#![no_main]
//! Restore of mutated snapshots taken mid-collection, then a full collection.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    moonseed_fuzz::restore_round_trip(data, true);
});
