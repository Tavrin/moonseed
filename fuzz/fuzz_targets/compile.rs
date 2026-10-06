#![no_main]
//! Source to compile: lexer, parser, compiler and diagnostic rendering.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    match moonseed::compile_with_limits(data, &moonseed_fuzz::compile_limits()) {
        Ok(mut chunk) => {
            chunk.set_chunk_name(b"@fuzz.lua");
            let _ = (chunk.prototype_count(), chunk.instruction_count());
        }
        Err(error) => {
            let _ = format!("{error} {error:?}");
        }
    }
});
