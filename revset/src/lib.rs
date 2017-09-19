// Copyright (c) 2017-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

extern crate asyncmemo;
extern crate futures;
extern crate mercurial_types;
extern crate repoinfo;

use futures::stream::Stream;
use mercurial_types::NodeHash;

mod unionnodestream;
pub use unionnodestream::UnionNodeStream;

mod singlenodehash;
pub use singlenodehash::SingleNodeHash;

#[derive(Clone, Debug)]
pub enum RevsetError {
    NoSuchNode(NodeHash),
    GenerationFetchFailed,
}

pub type NodeStream = Stream<Item = NodeHash, Error = RevsetError> + 'static;

#[cfg(test)]
extern crate ascii;
#[cfg(test)]
extern crate blobrepo;
#[cfg(test)]
extern crate linear;

#[cfg(test)]
use ascii::AsciiString;
#[cfg(test)]
use futures::executor::spawn;
#[cfg(test)]
fn string_to_nodehash(hash: &'static str) -> NodeHash {
    NodeHash::from_ascii_str(&AsciiString::from_ascii(hash)
        .expect("Can't turn string to AsciiString"))
        .expect("Can't turn AsciiString to NodeHash")
}

#[cfg(test)]
fn assert_node_sequence<I>(hashes: I, stream: Box<NodeStream>)
where
    I: IntoIterator<Item = NodeHash>,
{
    let mut nodestream = spawn(stream);

    for expected in hashes {
        assert!(
            match nodestream.wait_stream() {
                Some(Ok(hash)) => hash == expected,
                Some(Err(e)) => panic!("Unexpected error {:?}", e),
                None => panic!("No node"),
            },
            "Wrong node"
        );
    }

    assert!(
        if let None = nodestream.wait_stream() {
            true
        } else {
            false
        },
        "Too many nodes"
    );
}