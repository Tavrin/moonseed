#![allow(deprecated)] // Legacy proof and compatibility call sites.
//! Scalar ABI around the proof kernel.
//!
//! The only `unsafe` in this crate is the edition-2024 export attribute on
//! each function. There are no unsafe blocks: byte-wise exports remain
//! available, and bulk exports lend Vec buffers to the host's linear
//! memory API without constructing slices from pointers.

use std::cell::RefCell;

use moonseed::{Config, HostRegistry, Journal, PauseReason, Runtime, StepOutcome};

struct Probe {
    runtime: Option<Runtime>,
    journal: Journal,
    snapshot: Vec<u8>,
    restore: Vec<u8>,
    diagnostic: Vec<u8>,
}

impl Probe {
    fn new() -> Self {
        Self {
            runtime: None,
            journal: Journal::new(),
            snapshot: Vec::new(),
            restore: Vec::new(),
            diagnostic: Vec::new(),
        }
    }
}

thread_local! {
    static PROBE: RefCell<Probe> = RefCell::new(Probe::new());
}

fn with_probe<T>(body: impl FnOnce(&mut Probe) -> T) -> T {
    PROBE.with(|probe| body(&mut probe.borrow_mut()))
}

/// `0` completed, `1` paused, `2` yielded, `3` waiting, `4` lua error,
/// `5` terminated, `6` engine error, `7` no runtime, `8` exit requested.
fn outcome_code(outcome: Result<StepOutcome, moonseed::VmError>) -> u32 {
    match outcome {
        Ok(StepOutcome::Completed) => 0,
        Ok(StepOutcome::Paused(PauseReason::FuelExhausted)) => 1,
        Ok(StepOutcome::LuaYielded) => 2,
        Ok(StepOutcome::Waiting(_)) => 3,
        Ok(StepOutcome::LuaError(_)) => 4,
        Ok(StepOutcome::Terminated(_)) => 5,
        Ok(StepOutcome::ExitRequested { .. }) => 8,
        Ok(_) | Err(_) => 6,
    }
}

fn probe_registry() -> HostRegistry {
    let mut registry = HostRegistry::proof();
    registry.register_hook("wasm.hook", |cx| {
        let globals = cx.globals();
        let count: i64 = cx.raw_get(&globals, "hook_events")?;
        cx.raw_set(&globals, "hook_events", count + 1)?;
        Ok(moonseed::HookAction::Continue)
    });
    registry
}

/// Exported for the wasm32 probe. The attribute is unsafe in edition 2024;
/// the function body is safe.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_reset() -> u32 {
    with_probe(
        |probe| match Runtime::boot_canonical(Config::default(), HostRegistry::proof()) {
            Ok(runtime) => {
                probe.runtime = Some(runtime);
                probe.journal = Journal::new();
                probe.snapshot.clear();
                probe.restore.clear();
                0
            }
            Err(_) => 6,
        },
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn moonseed_run(quantum: u64) -> u32 {
    with_probe(|probe| {
        let Some(runtime) = probe.runtime.as_mut() else {
            return 7;
        };
        outcome_code(runtime.run_until_terminal(quantum, &mut probe.journal))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn moonseed_snapshot() -> u32 {
    with_probe(|probe| {
        let Some(runtime) = probe.runtime.as_ref() else {
            return 7;
        };
        match runtime.snapshot() {
            Ok(bytes) => {
                probe.snapshot = bytes;
                0
            }
            Err(_) => 6,
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn moonseed_snapshot_len() -> u32 {
    with_probe(|probe| probe.snapshot.len() as u32)
}

#[unsafe(no_mangle)]
pub extern "C" fn moonseed_snapshot_byte(index: u32) -> u32 {
    with_probe(|probe| probe.snapshot.get(index as usize).copied().unwrap_or(0) as u32)
}

/// Read `moonseed_snapshot_len` bytes here before changing the snapshot.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_snapshot_ptr() -> u32 {
    with_probe(|probe| probe.snapshot.as_ptr() as usize as u32)
}

/// Allocate the restore buffer; write `len` bytes here before any other
/// restore call. Zero means the requested length exceeds the snapshot cap.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_restore_alloc(len: u32) -> u32 {
    with_probe(|probe| {
        if u64::from(len) > Config::default().max_snapshot_bytes {
            return 0;
        }
        probe.restore.clear();
        probe.restore.resize(len as usize, 0);
        probe.restore.as_mut_ptr() as usize as u32
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn moonseed_restore_clear() -> u32 {
    with_probe(|probe| {
        probe.restore.clear();
        0
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn moonseed_restore_push_byte(byte: u32) {
    with_probe(|probe| probe.restore.push(byte as u8));
}

#[unsafe(no_mangle)]
pub extern "C" fn moonseed_restore_finish(domain: u64) -> u32 {
    with_probe(|probe| {
        let bytes = probe.restore.clone();
        match Runtime::from_snapshot(&bytes, &probe_registry(), domain) {
            Ok(runtime) => {
                probe.runtime = Some(runtime);
                probe.journal = Journal::new();
                0
            }
            Err(moonseed::SnapshotError::BadMagic) => 1,
            Err(moonseed::SnapshotError::BadVersion) => 2,
            Err(moonseed::SnapshotError::Truncated) => 3,
            Err(moonseed::SnapshotError::Checksum) => 4,
            Err(moonseed::SnapshotError::DuplicateObjectId) => 5,
            Err(moonseed::SnapshotError::DanglingReference) => 6,
            Err(moonseed::SnapshotError::InvalidProgramCounter) => 7,
            Err(moonseed::SnapshotError::UnknownHostSymbol) => 8,
            Err(moonseed::SnapshotError::EffectDomainMismatch) => 9,
            Err(moonseed::SnapshotError::LimitExceeded) => 10,
            Err(_) => 11,
        }
    })
}

/// Capture the exact string error object after a diagnostic run. Zero means
/// there is no string error; one means the byte accessors are valid.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_diag_error_capture() -> u32 {
    with_probe(|probe| {
        probe.diagnostic.clear();
        let Some(runtime) = probe.runtime.as_ref() else {
            return 0;
        };
        let Some((_, moonseed::HostValue::String(bytes))) = runtime.lua_error() else {
            return 0;
        };
        probe.diagnostic = bytes;
        1
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn moonseed_diag_error_len() -> u32 {
    with_probe(|probe| probe.diagnostic.len() as u32)
}

#[unsafe(no_mangle)]
pub extern "C" fn moonseed_diag_error_byte(index: u32) -> u32 {
    with_probe(|probe| probe.diagnostic.get(index as usize).copied().unwrap_or(0) as u32)
}

/// Packed traversal and border result. `i64::MIN` means the fixture faulted.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_table_fingerprint() -> i64 {
    moonseed::table_semantics_fingerprint().unwrap_or(i64::MIN)
}

/// Packed closure-fixture result. `i64::MIN` means compilation or execution failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_source_fingerprint() -> i64 {
    moonseed::source_closure_fingerprint().unwrap_or(i64::MIN)
}

/// Packed branch-close result, checkpointed after the close. `i64::MIN`
/// means compilation or execution failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_branch_fingerprint() -> i64 {
    moonseed::source_branch_fingerprint().unwrap_or(i64::MIN)
}

/// Folded `while` / `repeat` / `break` / numeric `for` results. `i64::MIN` means a fixture
/// failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_loops_fingerprint() -> i64 {
    moonseed::source_loops_fingerprint().unwrap_or(i64::MIN)
}

/// Folded constructor / indexing / globals / `_ENV` results. `i64::MIN`
/// means a fixture failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_tables_fingerprint() -> i64 {
    moonseed::source_tables_fingerprint().unwrap_or(i64::MIN)
}

/// Folded native-function fixture results. `i64::MIN` means a fixture failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_natives_fingerprint() -> i64 {
    moonseed::source_natives_fingerprint().unwrap_or(i64::MIN)
}

/// Folded operator fixtures and waiting operator handlers. `i64::MIN`
/// means a program failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_operators_fingerprint() -> i64 {
    moonseed::source_operators_fingerprint().unwrap_or(i64::MIN)
}

/// Folded error, `pcall` and `xpcall` fixtures and waiting protected calls.
/// `i64::MIN` means a program failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_errors_fingerprint() -> i64 {
    moonseed::source_errors_fingerprint().unwrap_or(i64::MIN)
}

/// Folded `<close>` fixtures, coroutine close cases, and waiting closes.
/// `i64::MIN` means a program failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_close_fingerprint() -> i64 {
    moonseed::source_close_fingerprint().unwrap_or(i64::MIN)
}

/// Folded generic `for` fixtures, the yielding iterator, and waiting
/// iterators and closing values. `i64::MIN` means a program failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_generic_for_fingerprint() -> i64 {
    moonseed::source_generic_for_fingerprint().unwrap_or(i64::MIN)
}

/// Folded vararg fixtures and programs, waiting vararg frames, and a chunk
/// given arguments. `i64::MIN` means a program failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_varargs_fingerprint() -> i64 {
    moonseed::source_varargs_fingerprint().unwrap_or(i64::MIN)
}

/// Folded tail-call fixtures and tail calls to waiting natives.
/// `i64::MIN` means a program failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_tail_calls_fingerprint() -> i64 {
    moonseed::source_tail_calls_fingerprint().unwrap_or(i64::MIN)
}

/// Folded `and` / `or` / `not` and method fixtures, and waits inside the
/// new forms. `i64::MIN` means a program failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_syntax_fingerprint() -> i64 {
    moonseed::source_syntax_fingerprint().unwrap_or(i64::MIN)
}

/// Folded `goto` fixtures and waits during goto cleanup and goto loops.
/// `i64::MIN` means a program failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_goto_fingerprint() -> i64 {
    moonseed::source_goto_fingerprint().unwrap_or(i64::MIN)
}

/// Folded base-library fixtures and their output, and waits inside base
/// functions. `i64::MIN` means a program failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_base_fingerprint() -> i64 {
    moonseed::source_base_fingerprint().unwrap_or(i64::MIN)
}

/// Folded math and table fixtures and their output, and waits inside
/// library functions. `i64::MIN` means a program failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_library_fingerprint() -> i64 {
    moonseed::source_library_fingerprint().unwrap_or(i64::MIN)
}

/// Folded string fixtures and their output, dumped chunks, packed and
/// formatted numbers, and waits inside string functions. `i64::MIN` means
/// a program failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_string_fingerprint() -> i64 {
    moonseed::source_string_fingerprint().unwrap_or(i64::MIN)
}

/// Folded debug and package corpora and their output, dumped debug
/// information, and waits inside `require` and a traceback. `i64::MIN`
/// means a program failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_debug_fingerprint() -> i64 {
    moonseed::source_debug_fingerprint().unwrap_or(i64::MIN)
}

/// Folded coroutine corpus and its output, and waits inside coroutines.
/// `i64::MIN` means a program failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_coroutine_fingerprint() -> i64 {
    moonseed::source_coroutine_fingerprint().unwrap_or(i64::MIN)
}

/// The bits of every transcendental function over a fixed corpus.
/// `i64::MIN` means a program failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_gc_semantics_fingerprint() -> i64 {
    moonseed::source_gc_semantics_fingerprint().unwrap_or(i64::MIN)
}

#[unsafe(no_mangle)]
pub extern "C" fn moonseed_userdata_fingerprint() -> i64 {
    moonseed::source_userdata_fingerprint().unwrap_or(i64::MIN)
}

/// Fill the snapshot buffer with the userdata exchange program, waiting.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_userdata_snapshot() -> u32 {
    with_probe(|probe| match moonseed::userdata_exchange_snapshot() {
        Ok(bytes) => {
            probe.snapshot = bytes;
            0
        }
        Err(_) => 6,
    })
}

/// Finish the userdata exchange program from the restore buffer.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_userdata_finish() -> i64 {
    with_probe(|probe| moonseed::userdata_exchange_finish(&probe.restore).unwrap_or(i64::MIN))
}

/// Fill the snapshot buffer with the large exchange program, waiting.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_large_snapshot() -> u32 {
    with_probe(|probe| match moonseed::large_exchange_snapshot() {
        Ok(bytes) => {
            probe.snapshot = bytes;
            0
        }
        Err(_) => 6,
    })
}

/// Finish the large exchange program from the restore buffer.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_large_finish() -> i64 {
    with_probe(|probe| moonseed::large_exchange_finish(&probe.restore).unwrap_or(i64::MIN))
}

#[unsafe(no_mangle)]
pub extern "C" fn moonseed_math_bits_fingerprint() -> i64 {
    moonseed::math_bits_fingerprint().unwrap_or(i64::MIN)
}

/// Folded automatic-collection schedule. `i64::MIN` means a loop failed.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_gc_fingerprint() -> i64 {
    moonseed::gc_schedule_fingerprint().unwrap_or(i64::MIN)
}

#[unsafe(no_mangle)]
pub extern "C" fn moonseed_field(field: u32) -> i64 {
    with_probe(|probe| {
        let Some(runtime) = probe.runtime.as_ref() else {
            return i64::MIN;
        };
        let Ok(observation) = runtime.observe(&probe.journal) else {
            return i64::MIN;
        };
        match field {
            0 => observation.tag,
            1 => observation.mark,
            2 => observation.inc_result,
            3 => observation.get_result,
            4 => observation.yielded,
            5 => observation.upvalue,
            6 => observation.a_id as i64,
            7 => observation.b_id as i64,
            8 => observation.a_b as i64,
            9 => observation.b_a as i64,
            10 => observation.inc_upvalue as i64,
            11 => observation.get_upvalue as i64,
            12 => observation.fuel_consumed as i64,
            13 => observation.journal.len() as i64,
            14 => observation
                .journal
                .first()
                .map(|entry| entry.id.sequence as i64)
                .unwrap_or(0),
            15 => observation
                .journal
                .first()
                .map(|entry| entry.outcome)
                .unwrap_or(0),
            16 => i64::from(observation.yielder_status),
            _ => i64::MIN,
        }
    })
}

/// VFS IO/loading/package and mocked OS, with mid-operation checkpoints.
#[unsafe(no_mangle)]
pub extern "C" fn moonseed_host_capabilities_fingerprint() -> i64 {
    moonseed::host_capabilities_fingerprint().unwrap_or(i64::MIN)
}
