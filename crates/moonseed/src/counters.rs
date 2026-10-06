//! Unstable execution diagnostics, excluded from snapshots and default builds.
//! Independent of `native-host`; this feature grants no host capabilities.
//!
//! A runtime owns its sink. A thread-local scoped pointer lets leaf helpers
//! (including borrowed-key hashing) count without changing their signatures.
//! Scopes nest and restore on unwind; no global totals or atomic increments.
//! `run` installs a scope automatically. Use `Runtime::counter_scope` to include
//! host-side setup/API operations. Unscoped operations are deliberately omitted.
use crate::{Runtime, opcode::Op};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

pub(crate) type Sink = Rc<RefCell<Counters>>;
thread_local! { static ACTIVE: RefCell<Option<Sink>> = const { RefCell::new(None) }; }
/// Restores the previous measurement sink when dropped (including on panic).
#[must_use]
pub struct Scope {
    previous: Option<Sink>,
}
impl Drop for Scope {
    fn drop(&mut self) {
        ACTIVE.with(|slot| *slot.borrow_mut() = self.previous.take());
    }
}
pub(crate) fn enter(sink: &Sink) -> Scope {
    Scope {
        previous: ACTIVE.with(|slot| slot.replace(Some(sink.clone()))),
    }
}
fn update(f: impl FnOnce(&mut Counters)) {
    ACTIVE.with(|slot| {
        if let Some(sink) = slot.borrow().as_ref() {
            f(&mut sink.borrow_mut());
        }
    });
}
pub(crate) fn event(name: &'static str, value: u64) {
    update(|c| *c.events.entry(name).or_default() += value);
}
pub(crate) fn allocated(kind: &'static str, bytes: u64) {
    update(|c| {
        let a = c
            .allocations
            .entry(kind.rsplit("::").next().unwrap_or(kind))
            .or_default();
        a[0] += 1;
        a[1] += bytes;
    });
}
/// A copy of cumulative counts, with logical allocation bytes (not malloc bytes).
#[derive(Clone)]
pub struct Counters {
    opcodes: Vec<u64>,
    pairs: Vec<u64>,
    previous: Option<usize>,
    events: BTreeMap<&'static str, u64>,
    allocations: BTreeMap<&'static str, [u64; 2]>,
}
impl Default for Counters {
    fn default() -> Self {
        Self {
            opcodes: vec![0; NAMES.len()],
            pairs: vec![0; NAMES.len() * NAMES.len()],
            previous: None,
            events: BTreeMap::new(),
            allocations: BTreeMap::new(),
        }
    }
}
impl Counters {
    /// Number of fetched Lua instructions (faulting instructions included).
    pub fn instructions(&self) -> u64 {
        self.opcodes.iter().sum()
    }
    /// JSON object; opcode pairs span calls/coroutine switches in execution order.
    pub fn to_json(&self) -> String {
        let opcodes: BTreeMap<_, _> = NAMES
            .iter()
            .copied()
            .zip(self.opcodes.iter().copied())
            .collect();
        let pairs: BTreeMap<_, _> = self
            .pairs
            .iter()
            .enumerate()
            .filter(|(_, n)| **n != 0)
            .map(|(i, n)| {
                (
                    format!("{}->{}", NAMES[i / NAMES.len()], NAMES[i % NAMES.len()]),
                    *n,
                )
            })
            .collect();
        serde_json::json!({"instructions":self.instructions(), "opcodes":opcodes, "pairs":pairs, "events":self.events, "allocations":self.allocations}).to_string()
    }
}
impl Runtime {
    /// Scope measurements to this runtime, including host-side API operations.
    /// Drop the guard before measuring a different runtime's host-side work.
    pub fn counter_scope(&self) -> Scope {
        enter(&self.counters)
    }
    /// Copy this runtime's cumulative measurements. Counters are not snapshotted.
    pub fn counters(&self) -> Counters {
        self.counters.borrow().clone()
    }
    /// Start a new measurement interval, also breaking opcode-pair continuity.
    pub fn reset_counters(&self) {
        *self.counters.borrow_mut() = Counters::default();
    }
}
pub(crate) fn opcode(op: Op) {
    let id = match op {
        Op::LoadNil { .. } => 0,
        Op::LoadInt { .. } => 1,
        Op::LoadFloat { .. } => 2,
        Op::LoadBool { .. } => 3,
        Op::LoadBytes { .. } => 4,
        Op::Move { .. } => 5,
        Op::Add { .. } => 6,
        Op::NewTable { .. } => 7,
        Op::GetTable { .. } => 8,
        Op::SetTable { .. } => 9,
        Op::MakeClosure { .. } => 10,
        Op::GetUpvalue { .. } => 11,
        Op::SetUpvalue { .. } => 12,
        Op::Call { .. } => 13,
        Op::Return { .. } => 14,
        Op::Yield { .. } => 15,
        Op::CallHost { .. } => 16,
        Op::NewThread { .. } => 17,
        Op::Resume { .. } => 18,
        Op::GetGlobal { .. } => 19,
        Op::VarargLen { .. } => 20,
        Op::Vararg { .. } => 21,
        Op::OpenLen { .. } => 22,
        Op::AssignLocal { .. } => 23,
        Op::AssignField { .. } => 24,
        Op::AssignCommit { .. } => 25,
        Op::Jump { .. } => 26,
        Op::JumpIfFalse { .. } => 27,
        Op::CloseUpvalues { .. } => 28,
        Op::Neg { .. } => 29,
        Op::ForPrep { .. } => 30,
        Op::ForLoop { .. } => 31,
        Op::Index { .. } => 32,
        Op::SetIndex { .. } => 33,
        Op::GetField { .. } => 34,
        Op::SetField { .. } => 35,
        Op::SetList { .. } => 36,
        Op::Len { .. } => 37,
        Op::Compare { .. } => 38,
        Op::JumpIfLt { .. } => 39,
        Op::Next { .. } => 40,
        Op::RawLen { .. } => 41,
        Op::Arith { .. } => 42,
        Op::BNot { .. } => 43,
        Op::Concat { .. } => 44,
        Op::MarkClose { .. } => 45,
        Op::CloseScope { .. } => 46,
        Op::CloseThread { .. } => 47,
        Op::GenericForLoop { .. } => 48,
        Op::TailCall { .. } => 49,
        Op::Halt => 50,
        Op::ArithK { .. } => 51,
        Op::CompareBranch { .. } => 52,
    };
    update(|c| {
        c.opcodes[id] += 1;
        if let Some(prev) = c.previous {
            c.pairs[prev * NAMES.len() + id] += 1;
        }
        c.previous = Some(id);
    });
}
const NAMES: &[&str] = &[
    "LoadNil",
    "LoadInt",
    "LoadFloat",
    "LoadBool",
    "LoadBytes",
    "Move",
    "Add",
    "NewTable",
    "GetTable",
    "SetTable",
    "MakeClosure",
    "GetUpvalue",
    "SetUpvalue",
    "Call",
    "Return",
    "Yield",
    "CallHost",
    "NewThread",
    "Resume",
    "GetGlobal",
    "VarargLen",
    "Vararg",
    "OpenLen",
    "AssignLocal",
    "AssignField",
    "AssignCommit",
    "Jump",
    "JumpIfFalse",
    "CloseUpvalues",
    "Neg",
    "ForPrep",
    "ForLoop",
    "Index",
    "SetIndex",
    "GetField",
    "SetField",
    "SetList",
    "Len",
    "Compare",
    "JumpIfLt",
    "Next",
    "RawLen",
    "Arith",
    "BNot",
    "Concat",
    "MarkClose",
    "CloseScope",
    "CloseThread",
    "GenericForLoop",
    "TailCall",
    "Halt",
    "ArithK",
    "CompareBranch",
];

pub(crate) fn table_probe(hit: bool, string: bool) {
    event(
        if hit {
            "table_raw_hits"
        } else {
            "table_misses"
        },
        1,
    );
    if string {
        event("table_string_probes", 1);
        if hit {
            event("table_field_hits", 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, HostRegistry, Journal, StepOutcome, compile};

    #[test]
    fn loop_counts_are_plausible_and_runtime_local() {
        let chunk = compile(b"local n = 0; for i = 1, 10 do n = n + i end; return n").unwrap();
        let mut runtime =
            Runtime::load_chunk(Config::default(), HostRegistry::new(), &chunk).unwrap();
        let mut other =
            Runtime::load_chunk(Config::default(), HostRegistry::new(), &chunk).unwrap();
        let mut journal = Journal::new();
        assert_eq!(
            runtime.run_until_terminal(1, &mut journal).unwrap(),
            StepOutcome::Completed
        );
        let counts = runtime.counters();
        // This pure Lua loop has no builtin/collector work to charge.
        assert_eq!(counts.instructions(), runtime.fuel_consumed());
        assert_eq!(
            counts.opcodes[NAMES.iter().position(|n| *n == "Add").unwrap()],
            10
        );
        assert_eq!(counts.pairs.iter().sum::<u64>(), counts.instructions() - 1);
        assert_eq!(
            counts.events["dispatch_hot"] + counts.events["dispatch_common_entries"],
            counts.instructions()
        );
        assert_eq!(other.counters().instructions(), 0);
        let _outer = runtime.counter_scope();
        assert_eq!(
            other.run_until_terminal(u64::MAX, &mut journal).unwrap(),
            StepOutcome::Completed
        );
        assert_eq!(other.counters().opcodes, counts.opcodes);
        assert_eq!(runtime.counters().instructions(), counts.instructions());
        runtime.reset_counters();
        assert_eq!(runtime.counters().instructions(), 0);
    }
}
