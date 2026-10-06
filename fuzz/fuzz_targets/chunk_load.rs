#![no_main]
//! Moonseed binary chunk decode, validation and load, then a bounded call.
use libfuzzer_sys::fuzz_target;

const DRIVER: &[u8] = br#"
local f = load(INPUT, "=bin", "b")
if f then
  pcall(f)
  local ok, d = pcall(string.dump, f)
  if ok then assert(load(d, "=again", "b")) end
end
"#;

fuzz_target!(|data: &[u8]| {
    moonseed_fuzz::run_driver(DRIVER, &[], data, 200_000);
});
