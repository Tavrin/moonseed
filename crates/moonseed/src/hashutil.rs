//! Stable hasher for lookup maps.
//!
//! Enumeration never walks this map. The hasher exists so lookup does not
//! follow `std`'s per-process or per-target randomization. It is not a
//! HashDoS defense; that decision is deferred.

use std::hash::{BuildHasher, Hasher};

#[derive(Clone, Default)]
pub(crate) struct StableBuildHasher;

impl BuildHasher for StableBuildHasher {
    type Hasher = StableHasher;

    fn build_hasher(&self) -> Self::Hasher {
        StableHasher {
            state: 0x517c_c1b7_2722_0a95,
        }
    }
}

pub(crate) struct StableHasher {
    state: u64,
}

impl Hasher for StableHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.state = self
                .state
                .wrapping_mul(0x0100_0000_01b3)
                .wrapping_add(u64::from(byte));
        }
    }

    fn finish(&self) -> u64 {
        self.state
    }
}

/// Derived, target-independent string hash. Never canonical snapshot state.
#[inline]
pub(crate) fn string_hash(bytes: &[u8]) -> u64 {
    count!("string_hashes");
    count!("string_bytes_hashed", bytes.len());
    let mut hash = StableBuildHasher.build_hasher();
    hash.write(bytes);
    hash.finish()
}

/// Table keys supply a cached hash for strings; other key kinds retain the
/// stable byte writer. Enumeration never depends on this index.
#[derive(Clone, Default)]
pub(crate) struct TableBuildHasher;
impl BuildHasher for TableBuildHasher {
    type Hasher = TableHasher;
    fn build_hasher(&self) -> Self::Hasher {
        TableHasher(StableBuildHasher.build_hasher())
    }
}
pub(crate) struct TableHasher(StableHasher);
impl Hasher for TableHasher {
    fn write(&mut self, bytes: &[u8]) {
        self.0.write(bytes);
    }
    fn write_u64(&mut self, hash: u64) {
        self.0.state = hash;
    }
    fn finish(&self) -> u64 {
        self.0.finish()
    }
}
