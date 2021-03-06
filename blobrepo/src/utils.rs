// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use bytes::Bytes;

use bincode;

use mercurial_types::{HgBlobHash, HgNodeHash, HgParents};
use mononoke_types::BlobstoreBytes;

use errors::*;

#[derive(Debug, Copy, Clone)]
#[derive(Serialize, Deserialize)]
pub struct RawNodeBlob {
    pub parents: HgParents,
    pub blob: HgBlobHash,
}

impl RawNodeBlob {
    pub fn serialize(&self, nodeid: &HgNodeHash) -> Result<EnvelopeBlob> {
        let serialized = bincode::serialize(self)
            .map_err(|err| Error::from(ErrorKind::SerializationFailed(*nodeid, err)))?;
        Ok(EnvelopeBlob(serialized.into()))
    }

    pub fn deserialize(blob: &EnvelopeBlob) -> Result<Self> {
        Ok(bincode::deserialize(blob.0.as_ref())?)
    }
}

// XXX could possibly also compute and store an ID here
#[derive(Debug)]
pub struct EnvelopeBlob(Bytes);

impl AsRef<[u8]> for EnvelopeBlob {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl From<BlobstoreBytes> for EnvelopeBlob {
    #[inline]
    fn from(bytes: BlobstoreBytes) -> EnvelopeBlob {
        EnvelopeBlob(bytes.into_bytes())
    }
}

impl From<EnvelopeBlob> for BlobstoreBytes {
    #[inline]
    fn from(blob: EnvelopeBlob) -> BlobstoreBytes {
        BlobstoreBytes::from_bytes(blob.0)
    }
}
