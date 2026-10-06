#![no_main]
//! Restore of mutated snapshots holding VFS file handles, buffered writes and
//! pending capability operations; waits are completed with typed values.
//! The last input byte selects the host: VFS (high bit clear) or pending.
use libfuzzer_sys::fuzz_target;
use moonseed::{CapabilityValue, Journal, Runtime, StepOutcome};

fuzz_target!(|data: &[u8]| {
    let Some((&choice, bytes)) = data.split_last() else {
        return;
    };
    let host = if choice & 0x80 == 0 {
        moonseed_fuzz::host(moonseed_fuzz::hostcap_filesystem())
    } else {
        moonseed_fuzz::pending_host()
    };
    let Ok(mut rt) = Runtime::restore(bytes, &host) else {
        return;
    };
    let mut journal = Journal::new();
    for step in 0..12u8 {
        match rt.run(moonseed_fuzz::QUANTUM, &mut journal) {
            Ok(StepOutcome::Paused(_)) => {}
            Ok(StepOutcome::Waiting(key)) => {
                let value = match choice.wrapping_add(step) % 6 {
                    0 => CapabilityValue::Bytes(bytes.iter().take(64).copied().collect()),
                    1 => CapabilityValue::Unsigned(u64::from(choice)),
                    2 => CapabilityValue::Unit,
                    3 => CapabilityValue::Boolean(choice & 1 == 0),
                    4 => CapabilityValue::Integer(-i64::from(choice)),
                    _ => CapabilityValue::OptionalBytes(None),
                };
                let _ = rt.wait(key);
                let _ = rt.complete_capability(key, Ok(value));
            }
            _ => break,
        }
    }
    if let Ok(again) = rt.snapshot() {
        let _ = Runtime::restore(&again, &host);
    }
    let _ = rt.begin_close();
    moonseed_fuzz::drive(&mut rt, 2);
});
