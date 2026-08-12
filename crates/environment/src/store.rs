//! Bytes that do not travel through the prefix.
//!
//! Output is a [`Span`], not a string. stdout/stderr land in storage and
//! the exchange carries a ref, so a 40 MB build log is a storage event rather
//! than a prefix event, and how much gets materialized into the transcript is
//! harness policy rather than a lossy fork in the road at capture time.
//!
//! The store is a trait because content addressing is the storage layer's
//! decision, not this crate's — [`MemoryBlobs`] hands out opaque refs and is
//! for tests and single-process runs. Nothing here reads a [`BlobRef`]'s
//! contents; a ref is a handle the harness redeems.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// An opaque handle to stored bytes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlobRef(pub String);

/// Bytes on the way *in* — a `write` body, a `stdin` payload.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blob(pub Vec<u8>);

impl Blob {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<&str> for Blob {
    fn from(text: &str) -> Self {
        Blob(text.as_bytes().to_vec())
    }
}

impl From<Vec<u8>> for Blob {
    fn from(bytes: Vec<u8>) -> Self {
        Blob(bytes)
    }
}

/// Bytes on the way *out*: a ref, the size behind it, and whether capture was
/// capped.
///
/// `truncated` is a field rather than a convention (A5). A capped result the
/// model reads as complete is worse than no result, because it terminates the
/// search.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub blob: BlobRef,
    /// Bytes stored. Not necessarily the bytes produced — see `truncated`.
    pub len: u64,
    pub truncated: bool,
}

/// Somewhere to put bytes that should not become prefix.
pub trait Blobs: Send + Sync {
    fn put(&self, bytes: &[u8]) -> BlobRef;
    fn get(&self, blob: &BlobRef) -> Option<Vec<u8>>;
}

impl<T: Blobs + ?Sized> Blobs for Arc<T> {
    fn put(&self, bytes: &[u8]) -> BlobRef {
        (**self).put(bytes)
    }

    fn get(&self, blob: &BlobRef) -> Option<Vec<u8>> {
        (**self).get(blob)
    }
}

/// An in-process store. Refs are sequence numbers, not digests: deduplication
/// and durability are the real store's problem.
#[derive(Debug, Default)]
pub struct MemoryBlobs {
    entries: Mutex<Vec<Vec<u8>>>,
}

impl MemoryBlobs {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Blobs for MemoryBlobs {
    fn put(&self, bytes: &[u8]) -> BlobRef {
        let mut entries = self.entries.lock().expect("blob store poisoned");
        entries.push(bytes.to_vec());
        BlobRef(format!("mem:{}", entries.len() - 1))
    }

    fn get(&self, blob: &BlobRef) -> Option<Vec<u8>> {
        let index: usize = blob.0.strip_prefix("mem:")?.parse().ok()?;
        let entries = self.entries.lock().expect("blob store poisoned");
        entries.get(index).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_bytes_come_back_by_ref() {
        let blobs = MemoryBlobs::new();
        let first = blobs.put(b"one");
        let second = blobs.put(b"two");
        assert_ne!(first, second);
        assert_eq!(blobs.get(&first).unwrap(), b"one");
        assert_eq!(blobs.get(&second).unwrap(), b"two");
        assert_eq!(blobs.get(&BlobRef("mem:99".into())), None);
    }
}
