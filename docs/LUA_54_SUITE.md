# Pinned Lua 5.4.9 official suite

The unmodified archive has SHA-256
`7d971845f545ffc09fbb3128a86b2c6524161c70d0fdf0154a16e8c00c343fca`.
The release-closure ledger is [lua54-0.1.0.json](../tests/compat/lua54-0.1.0.json).
All 33 files compile; 14 PASS and 19 FAIL, with zero UNKNOWN classifications.
A FAIL can be an accepted implementation boundary; it is still a failed
unmodified official test. No remaining failure is classified as a real Lua
semantic gap or a Moonseed bug: logical write buffering is implemented and
files.lua's buffering assertions pass.

Run `CARGO_TARGET_DIR=<assigned target> bash tools/lua_suite.sh run OUT`.
The native profile installs all libraries, explicit IO/OS capabilities, arg,
Lua filesystem loading and only `T.sethook`/`T.resume`. Each file runs in a
fresh process and writable scratch root under bwrap, with a 2 GB address-space
cap, 60-second deadline and 200 million fuel. The archive is verified and
re-extracted before each run. It is never edited for acceptance.

| File | Status | Classified first blocker | Rationale |
|---|---|---|---|
| `all.lua` | FAIL | REFERENCE C-API TEST ONLY | T.stacklevel inspects the reference C stack; the aggregate also needs all accepted component boundaries. |
| `api.lua` | FAIL | REFERENCE C-API TEST ONLY | T.testC is the reference C API command interpreter, not a Lua library. |
| `attrib.lua` | FAIL | UNSUPPORTED LUA C MODULE ABI | The expected require diagnostic includes C-searcher paths; Moonseed exposes Lua/preload/host searchers. |
| `big.lua` | FAIL | HOST/CAPABILITY DIFFERENCE | The suite driver resumes this chunk in a coroutine; isolated main-thread execution cannot yield. |
| `bitwise.lua` | PASS | PASS | The unmodified file completed under the native suite profile. |
| `bwcoercion.lua` | PASS | PASS | The unmodified file completed under the native suite profile. |
| `calls.lua` | FAIL | PUC BINARY-FORMAT DETAIL | Dump/load execution works; the assertion requires the PUC binary header. |
| `closure.lua` | PASS | PASS | The unmodified file completed under the native suite profile. |
| `code.lua` | FAIL | PUC INTERNAL/PRIVATE DETAIL | T.listk/listcode inspect PUC constants and opcodes; the public result checks pass separately. |
| `constructs.lua` | PASS | PASS | The unmodified file completed under the native suite profile. |
| `coroutine.lua` | FAIL | REFERENCE C-API TEST ONLY | The failing resume invokes absent T.testC; supported public coroutine and host-hook choreography passes. |
| `cstack.lua` | FAIL | RESOURCE/POLICY DIFFERENCE | Stackless close chains complete instead of exhausting the reference C stack. |
| `db.lua` | FAIL | PUC INTERNAL/PRIVATE DETAIL | The assertion requires the private registry table _HOOKKEY; public hooks are tested independently. |
| `errors.lua` | FAIL | REFERENCE C-API TEST ONLY | T.totalmem is a reference allocator-control hook; later Lua-visible diagnostics are fixed and checked. |
| `events.lua` | FAIL | REFERENCE C-API TEST ONLY | T.newuserdata creates C test userdata; native userdata semantics have separate tests. |
| `files.lua` | FAIL | STANDALONE-CLI DIFFERENCE | Logical write buffering passes (lines 675/689); line 760 runs the standalone `lua` executable through io.popen/os.execute and checks its exit status. |
| `gc.lua` | FAIL | PUC INTERNAL/PRIVATE DETAIL | T.gcstate exposes the reference collector phase; Lua-visible reachability/finalization is tested separately. |
| `gengc.lua` | FAIL | PUC INTERNAL/PRIVATE DETAIL | T.gcage exposes reference GC ages; public generational object/barrier tests pass. |
| `goto.lua` | PASS | PASS | The unmodified file completed under the native suite profile. |
| `heavy.lua` | PASS | PASS | The unmodified file completed under the native suite profile. |
| `literals.lua` | FAIL | IMPLEMENTATION-DEFINED IDENTITY | Equal long strings need not share one object/token across prototypes; value equality holds. |
| `locals.lua` | FAIL | PUC INTERNAL/PRIVATE DETAIL | T.querytab exposes PUC table layout; later local/close semantics pass after diagnostic fixes. |
| `main.lua` | FAIL | STANDALONE-CLI DIFFERENCE | The compat harness takes suite/output arguments and does not implement the standalone lua command line. |
| `math.lua` | PASS | PASS | The unmodified file completed under the native suite profile. |
| `nextvar.lua` | FAIL | PUC INTERNAL/PRIVATE DETAIL | T.querytab checks PUC array/hash capacities; public table iteration checks pass. |
| `pm.lua` | PASS | PASS | The unmodified file completed under the native suite profile. |
| `sort.lua` | PASS | PASS | The unmodified file completed under the native suite profile. |
| `strings.lua` | FAIL | REFERENCE C-API TEST ONLY | T.testC exercises lua_pushfstring; public string operations have independent oracle coverage. |
| `tpack.lua` | PASS | PASS | The unmodified file completed under the native suite profile. |
| `tracegc.lua` | PASS | PASS | The unmodified file completed under the native suite profile. |
| `utf8.lua` | PASS | PASS | The unmodified file completed under the native suite profile. |
| `vararg.lua` | PASS | PASS | The unmodified file completed under the native suite profile. |
| `verybig.lua` | PASS | PASS | The unmodified file completed under the native suite profile. |

## Explicit boundaries and later sections

- **Long-string `%p` (C):** `%p` identifies objects; equality by string bytes
  does not prescribe literal interning across prototypes. Moonseed does not
  promise PUC string-pointer identity for equal separately-created strings.
  Canonical constant sharing was rejected here: installer allocation forecasts,
  GC accounting and the snapshot decoder's anti-amplification ownership rules
  make it more than a small local fix. Global interning is outside scope.
- **Hooks (D):** `_HOOKKEY` is a PUC private registry detail. Moonseed keeps
  hook authority/roots with live threads. `debug.sethook`/`gethook`, per-thread
  settings, suppression, transfer, yields, weak ownership and restore are
  covered in `prove/hooks.rs`; registry mutation is not hook authority.
- **C helpers (E):** `T` is the suite's internal C-test library. The complete
  per-use inventory is in the lane's `T-uses.json` evidence (432 conservative
  textual occurrences, including comments). Each records file, line, source,
  classification, related public guarantee and exact Moonseed test names.
  The method-level coverage map below explains the accepted helper boundaries.
  Lua-visible operations remain requirements even when their C fixture is absent.
- **Modules (F):** arbitrary C modules, `package.loadlib` and C searchers are
  outside the supported ABI. Native Rust loaders and the host resolver are
  exercised by `prove/host_environment.rs::pure_resolver_native_loaders_use_registered_symbols`
  and `pure_resolver_source_keeps_preload_loader_data_and_loaded_protocol`.
  Filesystem Lua loading/searchpath, preload, loaded caching and custom searchers
  are covered by `prove/host_checkpoint.rs`, `prove/debug.rs` and the frozen host
  corpus. Creating `libs/P1` in the harness lets the sub-package checks run.
- **Binary chunks (G):** Moonseed binary chunks are Moonseed-specific and
  portable across Moonseed native/Wasm targets; not PUC chunks. Dump/load result
  semantics and malformed input are tested in `prove/string.rs` and the Wasm
  probe. PUC header bytes and diagnostic wording are separate format details.
- **Limits (H):** each thread allows 1,000 ordinary Lua/protected frames plus
  an 80-frame error/close reserve. `Config::max_stack_slots` defaults to 50,000,
  configurable from 1,024 to 100,000; normal calls use seven-eighths and error
  handling the reserve. Plain resume chains stop at 196 nested coroutines with
  catchable `C stack overflow`; PUC counts intervening C calls too. Closing
  uses heap continuations rather than that resume-depth budget and can close
  all 1,000 suite coroutines, bounded by heap/objects/fuel. A 240-deep close
  chain checkpoints at peak depth on a 256 KiB Rust stack in
  `prove/coroutine.rs::close_chains_past_the_resume_limit_checkpoint_safely`.
  `resume_chains_do_not_grow_the_rust_stack` and
  `transfers_past_the_stack_bound_fail_cleanly` cover resume/slot exhaustion.
- **Choreography (I):** `big.lua:56` deliberately yields `b` to all.lua's
  coroutine driver (`all.lua:182–186`). Running the original chunk through
  `coroutine.wrap(loadfile(...))` yields `b`, then returns `a` on both engines.
  Public behavior is correct. `coroutine.lua:656` follows an absent `T.testC`
  call at 654; it is a C-helper fixture failure, not a resume-result mismatch.
  Keeping the supported host-hook section while skipping the runC section and
  the later C-only tail completes the public coroutine checks.
- **CLI (J):** `main.lua` invokes a standalone executable with `-v`, `-e`,
  stdin, initialization and exit conventions. The compat harness is not that
  executable. The public crate supplies arg installation, terminal exit outcomes,
  loadfile, stdio and optional capabilities for a future CLI; `prove/exit.rs`,
  `prove/os.rs` and the frozen host corpus test those pieces.

Scratch copies preserve line numbers and are evidence only. With accepted
blockers disabled, later public sections of every failed component were checked.
`code.lua` also ran with only its opcode/constant observers removed, retaining
its public evaluation checks. The aggregate driver also completes with those
same scratch components, `T` disabled and the standalone CLI invocation omitted:
51,619,525 fuel and the final `>>> closing state <<<` message. Its missing-global
observer labels the run COMPLETED_WITHOUT because all.lua intentionally clears
then reads `debug`; this is modified execution evidence, not an unmodified PASS.
The original big.lua coroutine choreography is retained.
The wholly C-driven API portions are mapped to independent native tests rather
than pretending a fabricated T implementation tests the C ABI.

Later sections exposed six Moonseed diagnostic bugs: generic-for iterator
source lines, function-statement store lines, prefix-statement lookahead tokens,
non-closable local names, invalid close-call type/metamethod information, and
finalizer debug names. These are fixed in cold compiler/parser/diagnostic paths;
`prove/debug.rs::suite_later_section_diagnostics_survive_checkpoints` retains the
regression and exercises pauses/restores. Exact error prose/interning/GC phase
layout are not all manual promises; these diagnostic fixes enforce Moonseed's
stated diagnostic target. No revision, dispatch, ordinary call/return, IO
runtime or frozen corpus expectation changed.

Later `errors.lua` checks also encounter documented compiler capacities and
diagnostic precedence: 100 nested function definitions exceed Moonseed's
99-function structural bound; 500 invalid assignment targets are rejected
without PUC's recursive-parser C-stack overflow; 260 arguments exceed the
register budget; an unterminated function reports its missing `end` before
local-count lowering. Both engines reject an embedded nonprintable source byte
with the same escaped token but different prose (`unexpected symbol` versus
`syntax error`). These are accepted resource/private diagnostic differences,
with exact minimized outputs in the evidence, rather than semantic successes.

The normative reference is the [Lua 5.4 manual](https://www.lua.org/manual/5.4/manual.html),
especially source values (§2.1), coroutines (§2.6, §6.2), errors (§2.3),
formatting (§6.4), IO (§6.8), debug (§6.10) and the standalone program (§7).
Detailed Moonseed/PUC bytes, assertions, tracebacks, scratch exclusions, manual
dispositions and final execution receipts are retained in the lane report.

## T helper coverage

| Method | Uses | Classification | Lua-visible guarantee / independent coverage |
|---|---:|---|---|
| `T.alloccount` | 19 | REFERENCE C-API TEST ONLY | No promise of C allocator block counts or failure-injection API. Memory exhaustion is catchable, unwinds closes and must not corrupt restored state. Tests: `crates/moonseed/src/prove/errors.rs::memory_errors_are_catchable_and_skip_the_message_handler`, `crates/moonseed/src/prove/review.rs::review_quota_overshoot`, `crates/moonseed/src/prove/userdata.rs::userdata_count_against_the_heap_quota` |
| `T.allocfailnext` | 1 | REFERENCE C-API TEST ONLY | No promise of C allocator block counts or failure-injection API. Memory exhaustion is catchable, unwinds closes and must not corrupt restored state. Tests: `crates/moonseed/src/prove/errors.rs::memory_errors_are_catchable_and_skip_the_message_handler`, `crates/moonseed/src/prove/review.rs::review_quota_overshoot`, `crates/moonseed/src/prove/userdata.rs::userdata_count_against_the_heap_quota` |
| `T.checkmemory` | 3 | PUC INTERNAL/PRIVATE DETAIL | No promise of PUC GC color/age/phase or native allocation layout. Live objects, barriers, weak/ephemeron reachability and finalizer semantics must remain safe. Tests: `crates/moonseed/src/prove/incremental.rs::heap_changes_in_every_phase_keep_the_invariant`, `crates/moonseed/src/prove/generational.rs::heap_changes_keep_the_generational_invariant`, `crates/moonseed/src/prove/finalize.rs::gc_corpus_keeps_its_output_under_every_schedule` |
| `T.checkpanic` | 7 | REFERENCE C-API TEST ONLY | C state creation, panic callbacks and C stack execution have no Lua-source guarantee. Host runtime isolation, errors, shutdown finalizers and coroutine execution must stay safe. Tests: `crates/moonseed/src/prove/embed.rs::references_reject_other_and_restored_runtimes`, `crates/moonseed/src/prove/finalize.rs::closing_runs_finalizers_before_rust_drops`, `crates/moonseed/src/prove/coroutine.rs::coroutine_corpus_matches_lua`, `crates/moonseed/src/prove/errors.rs::checkpoint_before_named_fault_preserves_its_error_string` |
| `T.closestate` | 8 | REFERENCE C-API TEST ONLY | C state creation, panic callbacks and C stack execution have no Lua-source guarantee. Host runtime isolation, errors, shutdown finalizers and coroutine execution must stay safe. Tests: `crates/moonseed/src/prove/embed.rs::references_reject_other_and_restored_runtimes`, `crates/moonseed/src/prove/finalize.rs::closing_runs_finalizers_before_rust_drops`, `crates/moonseed/src/prove/coroutine.rs::coroutine_corpus_matches_lua`, `crates/moonseed/src/prove/errors.rs::checkpoint_before_named_fault_preserves_its_error_string` |
| `T.d2s` | 1 | REFERENCE C-API TEST ONLY | Raw C double-byte conversion is not a Lua-source facility. Public string.pack/unpack representation is covered on the documented ABI. Tests: `crates/moonseed/src/prove/string.rs::string_fixtures_match_lua_under_fuel_gc_and_checkpoints` |
| `T.doonnewstack` | 2 | REFERENCE C-API TEST ONLY | C state creation, panic callbacks and C stack execution have no Lua-source guarantee. Host runtime isolation, errors, shutdown finalizers and coroutine execution must stay safe. Tests: `crates/moonseed/src/prove/embed.rs::references_reject_other_and_restored_runtimes`, `crates/moonseed/src/prove/finalize.rs::closing_runs_finalizers_before_rust_drops`, `crates/moonseed/src/prove/coroutine.rs::coroutine_corpus_matches_lua`, `crates/moonseed/src/prove/errors.rs::checkpoint_before_named_fault_preserves_its_error_string` |
| `T.doremote` | 14 | REFERENCE C-API TEST ONLY | C state creation, panic callbacks and C stack execution have no Lua-source guarantee. Host runtime isolation, errors, shutdown finalizers and coroutine execution must stay safe. Tests: `crates/moonseed/src/prove/embed.rs::references_reject_other_and_restored_runtimes`, `crates/moonseed/src/prove/finalize.rs::closing_runs_finalizers_before_rust_drops`, `crates/moonseed/src/prove/coroutine.rs::coroutine_corpus_matches_lua`, `crates/moonseed/src/prove/errors.rs::checkpoint_before_named_fault_preserves_its_error_string` |
| `T.gcage` | 24 | PUC INTERNAL/PRIVATE DETAIL | No promise of PUC GC color/age/phase or native allocation layout. Live objects, barriers, weak/ephemeron reachability and finalizer semantics must remain safe. Tests: `crates/moonseed/src/prove/incremental.rs::heap_changes_in_every_phase_keep_the_invariant`, `crates/moonseed/src/prove/generational.rs::heap_changes_keep_the_generational_invariant`, `crates/moonseed/src/prove/finalize.rs::gc_corpus_keeps_its_output_under_every_schedule` |
| `T.gccolor` | 9 | PUC INTERNAL/PRIVATE DETAIL | No promise of PUC GC color/age/phase or native allocation layout. Live objects, barriers, weak/ephemeron reachability and finalizer semantics must remain safe. Tests: `crates/moonseed/src/prove/incremental.rs::heap_changes_in_every_phase_keep_the_invariant`, `crates/moonseed/src/prove/generational.rs::heap_changes_keep_the_generational_invariant`, `crates/moonseed/src/prove/finalize.rs::gc_corpus_keeps_its_output_under_every_schedule` |
| `T.gcstate` | 10 | PUC INTERNAL/PRIVATE DETAIL | No promise of PUC GC color/age/phase or native allocation layout. Live objects, barriers, weak/ephemeron reachability and finalizer semantics must remain safe. Tests: `crates/moonseed/src/prove/incremental.rs::heap_changes_in_every_phase_keep_the_invariant`, `crates/moonseed/src/prove/generational.rs::heap_changes_keep_the_generational_invariant`, `crates/moonseed/src/prove/finalize.rs::gc_corpus_keeps_its_output_under_every_schedule` |
| `T.getref` | 8 | REFERENCE C-API TEST ONLY | C registry-reference integers have no Lua-source API. Host roots must keep values live and dropping roots must release them. Tests: `crates/moonseed/src/prove/embed.rs::owned_roots_survive_both_collectors_and_last_drop_releases` |
| `T.listcode` | 3 | PUC INTERNAL/PRIVATE DETAIL | No guarantee of PUC array/hash sizes, string intern tables, constant lists or opcode selection. Public table values, string equality and source evaluation remain required. Tests: `crates/moonseed/src/prove/memory.rs::equal_strings_are_equal_whichever_object_holds_them`, `crates/moonseed/src/prove/language.rs::frontend_limits_hold_at_their_boundaries`, `crates/moonseed/src/prove/debug.rs::debug_and_package_corpora_match_lua` |
| `T.listk` | 3 | PUC INTERNAL/PRIVATE DETAIL | No guarantee of PUC array/hash sizes, string intern tables, constant lists or opcode selection. Public table values, string equality and source evaluation remain required. Tests: `crates/moonseed/src/prove/memory.rs::equal_strings_are_equal_whichever_object_holds_them`, `crates/moonseed/src/prove/language.rs::frontend_limits_hold_at_their_boundaries`, `crates/moonseed/src/prove/debug.rs::debug_and_package_corpora_match_lua` |
| `T.loadlib` | 4 | UNSUPPORTED LUA C MODULE ABI | Loading arbitrary C modules is outside the supported ABI. Native Rust loaders, host resolver, Lua/preload searchers and loaded cache retain their public contracts. Tests: `crates/moonseed/src/prove/host_environment.rs::pure_resolver_native_loaders_use_registered_symbols`, `crates/moonseed/src/prove/host_environment.rs::pure_resolver_source_keeps_preload_loader_data_and_loaded_protocol`, `crates/moonseed/src/prove/debug.rs::debug_and_package_corpora_match_lua` |
| `T.makeCfunc` | 14 | REFERENCE C-API TEST ONLY | C closures/upvalue storage are C API facilities. Native captures and Lua debug upvalue identity/value access must retain their semantics. Tests: `crates/moonseed/src/prove/embed.rs::native_closure_capture_writes_are_traced_and_snapshotted`, `crates/moonseed/src/prove/debug.rs::debug_and_package_corpora_match_lua`, `crates/moonseed/src/prove/userdata.rs::userdata_corpus_matches_lua` |
| `T.newstate` | 8 | REFERENCE C-API TEST ONLY | C state creation, panic callbacks and C stack execution have no Lua-source guarantee. Host runtime isolation, errors, shutdown finalizers and coroutine execution must stay safe. Tests: `crates/moonseed/src/prove/embed.rs::references_reject_other_and_restored_runtimes`, `crates/moonseed/src/prove/finalize.rs::closing_runs_finalizers_before_rust_drops`, `crates/moonseed/src/prove/coroutine.rs::coroutine_corpus_matches_lua`, `crates/moonseed/src/prove/errors.rs::checkpoint_before_named_fault_preserves_its_error_string` |
| `T.newuserdata` | 31 | REFERENCE C-API TEST ONLY | Creating/reading raw C userdata/pointer bytes has no Lua-source API. Supplied userdata must preserve identity, metatables, user values, GC and close semantics. Tests: `crates/moonseed/src/prove/userdata.rs::userdata_corpus_matches_lua`, `crates/moonseed/src/prove/userdata.rs::user_values_keep_what_they_hold`, `crates/moonseed/src/prove/userdata.rs::userdata_corpus_keeps_its_output_under_every_schedule` |
| `T.pushuserdata` | 8 | REFERENCE C-API TEST ONLY | Creating/reading raw C userdata/pointer bytes has no Lua-source API. Supplied userdata must preserve identity, metatables, user values, GC and close semantics. Tests: `crates/moonseed/src/prove/userdata.rs::userdata_corpus_matches_lua`, `crates/moonseed/src/prove/userdata.rs::user_values_keep_what_they_hold`, `crates/moonseed/src/prove/userdata.rs::userdata_corpus_keeps_its_output_under_every_schedule` |
| `T.querystr` | 2 | PUC INTERNAL/PRIVATE DETAIL | No guarantee of PUC array/hash sizes, string intern tables, constant lists or opcode selection. Public table values, string equality and source evaluation remain required. Tests: `crates/moonseed/src/prove/memory.rs::equal_strings_are_equal_whichever_object_holds_them`, `crates/moonseed/src/prove/language.rs::frontend_limits_hold_at_their_boundaries`, `crates/moonseed/src/prove/debug.rs::debug_and_package_corpora_match_lua` |
| `T.querytab` | 5 | PUC INTERNAL/PRIVATE DETAIL | No guarantee of PUC array/hash sizes, string intern tables, constant lists or opcode selection. Public table values, string equality and source evaluation remain required. Tests: `crates/moonseed/src/prove/memory.rs::equal_strings_are_equal_whichever_object_holds_them`, `crates/moonseed/src/prove/language.rs::frontend_limits_hold_at_their_boundaries`, `crates/moonseed/src/prove/debug.rs::debug_and_package_corpora_match_lua` |
| `T.ref` | 9 | REFERENCE C-API TEST ONLY | C registry-reference integers have no Lua-source API. Host roots must keep values live and dropping roots must release them. Tests: `crates/moonseed/src/prove/embed.rs::owned_roots_survive_both_collectors_and_last_drop_releases` |
| `T.resume` | 2 | REFERENCE C-API TEST ONLY | These reference helpers expose C hooks/resume choreography. Supported host-hook yield, thread inspection, suppression, transfer and Lua coroutine behavior are public and tested; instruction positions follow Moonseed bytecode. Tests: `crates/moonseed/src/prove/hooks.rs::host_preemption_checkpoint_transitions`, `crates/moonseed/src/prove/hooks.rs::hook_determinism_matrix`, `crates/moonseed/src/prove/hooks.rs::hook_side_table_does_not_root_dead_threads`, `crates/moonseed/src/prove/coroutine.rs::coroutine_corpus_keeps_its_output_under_every_schedule` |
| `T.s2d` | 1 | REFERENCE C-API TEST ONLY | Raw C double-byte conversion is not a Lua-source facility. Public string.pack/unpack representation is covered on the documented ABI. Tests: `crates/moonseed/src/prove/string.rs::string_fixtures_match_lua_under_fuel_gc_and_checkpoints` |
| `T.sethook` | 8 | REFERENCE C-API TEST ONLY | These reference helpers expose C hooks/resume choreography. Supported host-hook yield, thread inspection, suppression, transfer and Lua coroutine behavior are public and tested; instruction positions follow Moonseed bytecode. Tests: `crates/moonseed/src/prove/hooks.rs::host_preemption_checkpoint_transitions`, `crates/moonseed/src/prove/hooks.rs::hook_determinism_matrix`, `crates/moonseed/src/prove/hooks.rs::hook_side_table_does_not_root_dead_threads`, `crates/moonseed/src/prove/coroutine.rs::coroutine_corpus_keeps_its_output_under_every_schedule` |
| `T.stacklevel` | 8 | REFERENCE C-API TEST ONLY | No promise of C stack depth/bytes. Moonseed call/slot exhaustion is catchable and coroutine switching/closing is stackless. Tests: `crates/moonseed/src/prove/coroutine.rs::resume_chains_do_not_grow_the_rust_stack`, `crates/moonseed/src/prove/coroutine.rs::transfers_past_the_stack_bound_fail_cleanly`, `crates/moonseed/src/prove/coroutine.rs::close_chains_past_the_resume_limit_checkpoint_safely` |
| `T.testC` | 164 | REFERENCE C-API TEST ONLY | The runC command interpreter and C stack operators themselves have no Lua-source guarantee. Lua-visible results, calls/errors, metamethods, upvalues, userdata, close/yield behavior and native continuations are required independently of that command ABI. Tests: `crates/moonseed/src/prove/base.rs::base_fixtures_match_lua_under_fuel_gc_and_checkpoints`, `crates/moonseed/src/prove/userdata.rs::userdata_corpus_keeps_its_output_under_every_schedule`, `crates/moonseed/src/prove/coroutine.rs::coroutine_corpus_keeps_its_output_under_every_schedule`, `crates/moonseed/src/prove/close.rs::coroutine_closes_match_lua_under_every_schedule`, `crates/moonseed/src/prove/embed.rs::continuation_yields_waits_and_restores_at_every_quantum_once`, `crates/moonseed/src/prove/operators.rs::pending_native_operator_handlers_restore_and_finish_once`, `crates/moonseed/src/prove/operators.rs::arithk_operand_order_and_fallback_at_every_safe_point`, `crates/moonseed/src/prove/varargs.rs::the_count_of_extras_is_exact_frame_state`, `crates/moonseed/src/prove/debug.rs::suite_later_section_diagnostics_survive_checkpoints` |
| `T.totalmem` | 33 | REFERENCE C-API TEST ONLY | No promise of C allocator block counts or failure-injection API. Memory exhaustion is catchable, unwinds closes and must not corrupt restored state. Tests: `crates/moonseed/src/prove/errors.rs::memory_errors_are_catchable_and_skip_the_message_handler`, `crates/moonseed/src/prove/review.rs::review_quota_overshoot`, `crates/moonseed/src/prove/userdata.rs::userdata_count_against_the_heap_quota` |
| `T.udataval` | 9 | REFERENCE C-API TEST ONLY | Creating/reading raw C userdata/pointer bytes has no Lua-source API. Supplied userdata must preserve identity, metatables, user values, GC and close semantics. Tests: `crates/moonseed/src/prove/userdata.rs::userdata_corpus_matches_lua`, `crates/moonseed/src/prove/userdata.rs::user_values_keep_what_they_hold`, `crates/moonseed/src/prove/userdata.rs::userdata_corpus_keeps_its_output_under_every_schedule` |
| `T.unref` | 9 | REFERENCE C-API TEST ONLY | C registry-reference integers have no Lua-source API. Host roots must keep values live and dropping roots must release them. Tests: `crates/moonseed/src/prove/embed.rs::owned_roots_survive_both_collectors_and_last_drop_releases` |
| `T.upvalue` | 5 | REFERENCE C-API TEST ONLY | C closures/upvalue storage are C API facilities. Native captures and Lua debug upvalue identity/value access must retain their semantics. Tests: `crates/moonseed/src/prove/embed.rs::native_closure_capture_writes_are_traced_and_snapshotted`, `crates/moonseed/src/prove/debug.rs::debug_and_package_corpora_match_lua`, `crates/moonseed/src/prove/userdata.rs::userdata_corpus_matches_lua` |
