#![no_main]
//! Source to compile and run under tight fuel, heap, object and stack limits.
use libfuzzer_sys::fuzz_target;
use moonseed::GcMode;

fuzz_target!(|data: &[u8]| {
    let Ok(chunk) = moonseed::compile_with_limits(data, &moonseed_fuzz::compile_limits()) else {
        return;
    };
    let mode = if data.len().is_multiple_of(2) {
        GcMode::Generational
    } else {
        GcMode::Incremental
    };
    let mut rt = moonseed_fuzz::runtime(400_000, mode, moonseed_fuzz::filesystem());
    if rt.load_main(&chunk).is_ok() {
        moonseed_fuzz::drive(&mut rt, 25);
        let _ = rt.begin_close();
        moonseed_fuzz::drive(&mut rt, 2);
    }
});
