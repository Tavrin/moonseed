#![no_main]
//! Fixed Lua driver over fuzzed fields; see `moonseed_fuzz::UTF8_DRIVER`.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let args = moonseed_fuzz::fields(data, 2);
    moonseed_fuzz::run_driver(moonseed_fuzz::UTF8_DRIVER, &args, data, 200_000);
});
