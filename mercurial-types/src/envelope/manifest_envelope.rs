// Copyright (c) 2018-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

//! Envelopes used for manifest nodes.

use bytes::Bytes;
use failure::{err_msg, SyncFailure};
use quickcheck::{empty_shrinker, Arbitrary, Gen};

use rust_thrift::compact_protocol;

use super::HgEnvelopeBlob;
use errors::*;
use nodehash::HgNodeHash;
use thrift;

/// A mutable representation of a Mercurial file node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HgManifestEnvelopeMut {
    pub node_id: HgNodeHash,
    pub p1: Option<HgNodeHash>,
    pub p2: Option<HgNodeHash>,
    pub computed_node_id: HgNodeHash,
    pub contents: Bytes,
}

impl HgManifestEnvelopeMut {
    pub fn freeze(self) -> HgManifestEnvelope {
        HgManifestEnvelope { inner: self }
    }
}

/// A serialized representation of a Mercurial manifest node in the blob store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HgManifestEnvelope {
    inner: HgManifestEnvelopeMut,
}

impl HgManifestEnvelope {
    pub(crate) fn from_thrift(fe: thrift::HgManifestEnvelope) -> Result<Self> {
        let catch_block = || {
            Ok(Self {
                inner: HgManifestEnvelopeMut {
                    node_id: HgNodeHash::from_thrift(fe.node_id)?,
                    p1: HgNodeHash::from_thrift_opt(fe.p1)?,
                    p2: HgNodeHash::from_thrift_opt(fe.p2)?,
                    computed_node_id: HgNodeHash::from_thrift(fe.computed_node_id)?,
                    contents: Bytes::from(fe.contents
                        .ok_or_else(|| err_msg("missing contents field"))?),
                },
            })
        };

        Ok(catch_block().with_context(|_: &Error| {
            ErrorKind::InvalidThrift(
                "HgManifestEnvelope".into(),
                "Invalid manifest envelope".into(),
            )
        })?)
    }

    pub fn from_blob(blob: HgEnvelopeBlob) -> Result<Self> {
        // TODO (T27336549) stop using SyncFailure once thrift is converted to failure
        let thrift_tc = compact_protocol::deserialize(blob.0.as_ref())
            .map_err(SyncFailure::new)
            .context(ErrorKind::BlobDeserializeError("HgManifestEnvelope".into()))?;
        Self::from_thrift(thrift_tc)
    }

    /// The ID for this manifest, as recorded by Mercurial. This might or might not match the
    /// actual hash computed from the contents.
    #[inline]
    pub fn node_id(&self) -> &HgNodeHash {
        &self.inner.node_id
    }

    /// The parent hashes for this node. The order matters.
    #[inline]
    pub fn parents(&self) -> (Option<&HgNodeHash>, Option<&HgNodeHash>) {
        (self.inner.p1.as_ref(), self.inner.p2.as_ref())
    }

    /// The computed ID for this manifest. This is primarily for consistency checks.
    #[inline]
    pub fn computed_node_id(&self) -> &HgNodeHash {
        &self.inner.computed_node_id
    }

    /// The manifest contents as raw bytes.
    #[inline]
    pub fn contents(&self) -> &Bytes {
        &self.inner.contents
    }

    /// Convert into a mutable representation.
    #[inline]
    pub fn into_mut(self) -> HgManifestEnvelopeMut {
        self.inner
    }

    pub(crate) fn into_thrift(self) -> thrift::HgManifestEnvelope {
        let inner = self.inner;
        thrift::HgManifestEnvelope {
            node_id: inner.node_id.into_thrift(),
            p1: inner.p1.map(HgNodeHash::into_thrift),
            p2: inner.p2.map(HgNodeHash::into_thrift),
            computed_node_id: inner.computed_node_id.into_thrift(),
            contents: Some(inner.contents.to_vec()),
        }
    }

    /// Serialize this structure into a blob.
    #[inline]
    pub fn into_blob(self) -> HgEnvelopeBlob {
        let thrift = self.into_thrift();
        HgEnvelopeBlob(compact_protocol::serialize(&thrift))
    }
}

impl Arbitrary for HgManifestEnvelope {
    fn arbitrary<G: Gen>(g: &mut G) -> Self {
        HgManifestEnvelope {
            inner: HgManifestEnvelopeMut {
                node_id: Arbitrary::arbitrary(g),
                p1: Arbitrary::arbitrary(g),
                p2: Arbitrary::arbitrary(g),
                // XXX this doesn't ensure that the computed node ID actually matches the contents.
                // Might want to do that.
                computed_node_id: Arbitrary::arbitrary(g),
                contents: Bytes::from(Vec::arbitrary(g)),
            },
        }
    }

    fn shrink(&self) -> Box<Iterator<Item = Self>> {
        empty_shrinker()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    quickcheck! {
        fn thrift_roundtrip(me: HgManifestEnvelope) -> bool {
            let thrift_me = me.clone().into_thrift();
            let me2 = HgManifestEnvelope::from_thrift(thrift_me)
                .expect("thrift roundtrips should always be valid");
            me == me2
        }

        fn blob_roundtrip(me: HgManifestEnvelope) -> bool {
            let blob = me.clone().into_blob();
            let me2 = HgManifestEnvelope::from_blob(blob)
                .expect("blob roundtrips should always be valid");
            me == me2
        }
    }

    #[test]
    fn bad_thrift() {
        let mut thrift_me = thrift::HgManifestEnvelope {
            node_id: thrift::HgNodeHash(thrift::Sha1(vec![1; 20])),
            p1: Some(thrift::HgNodeHash(thrift::Sha1(vec![2; 20]))),
            p2: None,
            computed_node_id: thrift::HgNodeHash(thrift::Sha1(vec![1; 20])),
            // contents must be present
            contents: None,
        };

        HgManifestEnvelope::from_thrift(thrift_me.clone())
            .expect_err("unexpected OK -- missing contents");

        thrift_me.contents = Some(b"abc".to_vec());
        thrift_me.node_id = thrift::HgNodeHash(thrift::Sha1(vec![1; 19]));

        HgManifestEnvelope::from_thrift(thrift_me).expect_err("unexpected OK -- wrong hash length");
    }
}
