#![no_main]
//! `Runtime::restore` of mutated real snapshots, then a bounded run and a
//! second checkpoint round trip.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    moonseed_fuzz::restore_round_trip(data, false);
});
