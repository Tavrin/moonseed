#![no_main]
//! Table operation sequences: raw sets, the table library, sort comparators,
//! metamethods, traversal with mutation, and weak tables.
use libfuzzer_sys::fuzz_target;
use moonseed::GcMode;

fuzz_target!(|data: &[u8]| {
    let source = moonseed_fuzz::table_program(&data[..data.len().min(512)]);
    let chunk = moonseed::compile(&source).expect("generated program compiles");
    let mut rt = moonseed_fuzz::runtime(100_000, GcMode::Generational, moonseed_fuzz::filesystem());
    rt.load_main(&chunk).expect("loads");
    moonseed_fuzz::drive(&mut rt, 8);
});
