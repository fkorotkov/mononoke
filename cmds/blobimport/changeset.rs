// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use bytes::Bytes;
use clap::ArgMatches;
use failure::err_msg;
use failure::prelude::*;
use futures::{Future, IntoFuture};
use futures::future::{self, SharedItem};
use futures::stream::{self, Stream};
use futures_cpupool::CpuPool;
use futures_ext::{BoxFuture, BoxStream, FutureExt, StreamExt};
use scuba_ext::ScubaSampleBuilder;

use blobrepo::{BlobChangeset, BlobRepo, ChangesetHandle, CreateChangeset, HgBlobEntry,
               UploadHgFileContents, UploadHgFileEntry, UploadHgNodeHash, UploadHgTreeEntry};
use mercurial::{manifest, RevlogChangeset, RevlogEntry, RevlogRepo};
use mercurial_types::{HgBlob, HgChangesetId, HgManifestId, HgNodeHash, MPath, RepoPath, Type,
                      NULL_HASH};

use super::get_usize;

struct ParseChangeset {
    revlogcs: BoxFuture<SharedItem<RevlogChangeset>, Error>,
    rootmf:
        BoxFuture<Option<(HgManifestId, HgBlob, Option<HgNodeHash>, Option<HgNodeHash>)>, Error>,
    entries: BoxStream<(Option<MPath>, RevlogEntry), Error>,
}

// Extracts all the data from revlog repo that commit API may need.
fn parse_changeset(revlog_repo: RevlogRepo, csid: HgChangesetId) -> ParseChangeset {
    let revlogcs = revlog_repo
        .get_changeset(&csid)
        .with_context(move |_| format!("While reading changeset {:?}", csid))
        .map_err(Fail::compat)
        .boxify()
        .shared();

    let rootmf = revlogcs
        .clone()
        .map_err(Error::from)
        .and_then({
            let revlog_repo = revlog_repo.clone();
            move |cs| {
                if cs.manifestid().into_nodehash() == NULL_HASH {
                    future::ok(None).boxify()
                } else {
                    revlog_repo
                        .get_root_manifest(cs.manifestid())
                        .map({
                            let manifest_id = *cs.manifestid();
                            move |rootmf| Some((manifest_id, rootmf))
                        })
                        .boxify()
                }
            }
        })
        .with_context(move |_| format!("While reading root manifest for {:?}", csid))
        .map_err(Fail::compat)
        .boxify()
        .shared();

    let entries = revlogcs
        .clone()
        .map_err(Error::from)
        .and_then({
            let revlog_repo = revlog_repo.clone();
            move |cs| {
                let mut parents = cs.parents()
                    .into_iter()
                    .map(HgChangesetId::new)
                    .map(|csid| {
                        let revlog_repo = revlog_repo.clone();
                        revlog_repo
                            .get_changeset(&csid)
                            .and_then(move |cs| {
                                if cs.manifestid().into_nodehash() == NULL_HASH {
                                    future::ok(None).boxify()
                                } else {
                                    revlog_repo
                                        .get_root_manifest(cs.manifestid())
                                        .map(Some)
                                        .boxify()
                                }
                            })
                            .boxify()
                    });

                let p1 = parents.next().unwrap_or(Ok(None).into_future().boxify());
                let p2 = parents.next().unwrap_or(Ok(None).into_future().boxify());

                p1.join(p2)
                    .with_context(move |_| format!("While reading parents of {:?}", csid))
                    .from_err()
            }
        })
        .join(rootmf.clone().from_err())
        .map(|((p1, p2), rootmf_shared)| match *rootmf_shared {
            None => stream::empty().boxify(),
            Some((_, ref rootmf)) => {
                manifest::new_entry_intersection_stream(&rootmf, p1.as_ref(), p2.as_ref())
            }
        })
        .flatten_stream()
        .with_context(move |_| format!("While reading entries for {:?}", csid))
        .from_err()
        .boxify();

    let revlogcs = revlogcs.map_err(Error::from).boxify();

    let rootmf = rootmf
        .map_err(Error::from)
        .and_then(move |rootmf_shared| match *rootmf_shared {
            None => Ok(None),
            Some((manifest_id, ref mf)) => {
                let mut bytes = Vec::new();
                mf.generate(&mut bytes).with_context(|_| {
                    format!("While generating root manifest blob for {:?}", csid)
                })?;

                let (p1, p2) = mf.parents().get_nodes();
                Ok(Some((
                    manifest_id,
                    HgBlob::from(Bytes::from(bytes)),
                    p1.cloned(),
                    p2.cloned(),
                )))
            }
        })
        .boxify();

    ParseChangeset {
        revlogcs,
        rootmf,
        entries,
    }
}

fn upload_entry(
    blobrepo: &BlobRepo,
    entry: RevlogEntry,
    path: Option<MPath>,
) -> BoxFuture<(HgBlobEntry, RepoPath), Error> {
    let blobrepo = blobrepo.clone();

    let ty = entry.get_type();

    let path = MPath::join_element_opt(path.as_ref(), entry.get_name());
    let path = match path {
        // XXX this shouldn't be possible -- encode this in the type system
        None => {
            return future::err(err_msg(
                "internal error: joined root path with root manifest",
            )).boxify()
        }
        Some(path) => path,
    };

    let content = entry.get_raw_content();
    let parents = entry.get_parents();

    content
        .join(parents)
        .and_then(move |(content, parents)| {
            let (p1, p2) = parents.get_nodes();
            let upload_node_id = UploadHgNodeHash::Checked(entry.get_hash().into_nodehash());
            match ty {
                Type::Tree => {
                    let upload = UploadHgTreeEntry {
                        upload_node_id,
                        contents: content
                            .into_inner()
                            .expect("contents should always be available"),
                        p1: p1.cloned(),
                        p2: p2.cloned(),
                        path: RepoPath::DirectoryPath(path),
                    };
                    let (_, upload_fut) = try_boxfuture!(upload.upload(&blobrepo));
                    upload_fut
                }
                Type::File(ft) => {
                    let upload = UploadHgFileEntry {
                        upload_node_id,
                        contents: UploadHgFileContents::RawBytes(
                            content
                                .into_inner()
                                .expect("contents should always be available"),
                        ),
                        file_type: ft,
                        p1: p1.cloned(),
                        p2: p2.cloned(),
                        path,
                    };
                    let (_, upload_fut) = try_boxfuture!(upload.upload(&blobrepo));
                    upload_fut
                }
            }
        })
        .boxify()
}

pub fn upload_changesets<'a>(
    matches: &ArgMatches<'a>,
    revlogrepo: RevlogRepo,
    blobrepo: Arc<BlobRepo>,
    cpupool_size: usize,
) -> BoxStream<BoxFuture<SharedItem<BlobChangeset>, Error>, Error> {
    let changesets = if let Some(hash) = matches.value_of("changeset") {
        future::result(HgNodeHash::from_str(hash))
            .into_stream()
            .boxify()
    } else {
        revlogrepo.changesets().boxify()
    };

    let changesets = if !matches.is_present("skip") {
        changesets
    } else {
        let skip = get_usize(matches, "skip", 0);
        changesets.skip(skip as u64).boxify()
    };

    let changesets = if !matches.is_present("commits-limit") {
        changesets
    } else {
        let limit = get_usize(matches, "commits-limit", 0);
        changesets.take(limit as u64).boxify()
    };

    let is_import_from_beggining = !matches.is_present("changeset") && !matches.is_present("skip");
    let mut parent_changeset_handles: HashMap<HgNodeHash, ChangesetHandle> = HashMap::new();

    changesets
        .map({
            let revlogrepo = revlogrepo.clone();
            let blobrepo = blobrepo.clone();
            move |csid| {
                let ParseChangeset {
                    revlogcs,
                    rootmf,
                    entries,
                } = parse_changeset(revlogrepo.clone(), HgChangesetId::new(csid));

                let rootmf = rootmf.map({
                    let blobrepo = blobrepo.clone();
                    move |rootmf| {
                        match rootmf {
                            None => future::ok(None).boxify(),
                            Some((manifest_id, blob, p1, p2)) => {
                                let upload = UploadHgTreeEntry {
                                    // The root tree manifest is expected to have the wrong hash in
                                    // hybrid mode. This will probably never go away for
                                    // compatibility with old repositories.
                                    upload_node_id: UploadHgNodeHash::Supplied(
                                        manifest_id.into_nodehash(),
                                    ),
                                    contents: blob.into_inner()
                                        .expect("contents should always be available"),
                                    p1,
                                    p2,
                                    path: RepoPath::root(),
                                };
                                upload
                                    .upload(&blobrepo)
                                    .into_future()
                                    .and_then(|(_, entry)| entry)
                                    .map(Some)
                                    .boxify()
                            }
                        }
                    }
                });

                let entries = entries.map({
                    let blobrepo = blobrepo.clone();
                    move |(path, entry)| upload_entry(&blobrepo, entry, path)
                });

                revlogcs
                    .join3(rootmf, entries.collect())
                    .map(move |(cs, rootmf, entries)| (csid, cs, rootmf, entries))
            }
        })
        .map({
            let cpupool = CpuPool::new(cpupool_size);
            move |fut| cpupool.spawn(fut)
        })
        .buffered(100)
        .map(move |(csid, cs, rootmf, entries)| {
            let entries = stream::futures_unordered(entries).boxify();

            let (p1handle, p2handle) = {
                let mut parents = cs.parents().into_iter().map(|p| {
                    let maybe_handle = parent_changeset_handles.get(&p).cloned();

                    if is_import_from_beggining {
                        maybe_handle.expect(&format!("parent {} not found for {}", p, csid))
                    } else {
                        maybe_handle.unwrap_or_else(|| {
                            ChangesetHandle::from(
                                blobrepo.get_changeset_by_changesetid(&HgChangesetId::new(p)),
                            )
                        })
                    }
                });

                (parents.next(), parents.next())
            };

            let create_changeset = CreateChangeset {
                expected_nodeid: Some(csid),
                expected_files: Some(Vec::from(cs.files())),
                p1: p1handle,
                p2: p2handle,
                root_manifest: rootmf,
                sub_entries: entries,
                user: String::from_utf8(Vec::from(cs.user()))
                    .expect(&format!("non-utf8 username for {}", csid)),
                time: cs.time().clone(),
                extra: cs.extra().clone(),
                comments: String::from_utf8(Vec::from(cs.comments()))
                    .expect(&format!("non-utf8 comments for {}", csid)),
            };
            let cshandle = create_changeset.create(&blobrepo, ScubaSampleBuilder::with_discard());
            parent_changeset_handles.insert(csid, cshandle.clone());
            cshandle
                .get_completed_changeset()
                .with_context(move |_| format!("While uploading changeset: {}", csid))
                .from_err()
                .boxify()
        })
        .boxify()
}
