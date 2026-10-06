//! Versioned checkpoint codec.
//!
//! Object references are [`ObjectId`](crate::id::ObjectId)s. Arena indexes and
//! generations are not written. The CRC detects accidental corruption; it does
//! not authenticate the sender.

use std::collections::HashSet;

use crate::check;
use crate::heap::{
    AssignTarget, ClosureObj, Frame, Heap, NativeClosureObj, Pending, Proto, Status, StringObj,
    TableObj, ThreadObj, UpvalueObj, UpvalueState,
};
use crate::host::HostRegistry;
use crate::id::{Handle, Kind, ObjectId, SnapshotError, TerminationReason};
use crate::opcode::{self, Op};
use crate::runtime::Limits;
use crate::runtime::Runtime;
use crate::table::{Slot, Table, TableKey};
use crate::value::Value;

/// Default bound on a snapshot, written or read (`Config::max_snapshot_bytes`,
/// `Limits::max_snapshot_bytes`): twice the default logical-heap quota. A
/// snapshot measured 0.33 to 0.92 bytes per logical byte (threads to
/// userdata) at every heap size up to the quota (ADR 0052).
pub(crate) const MAX_SNAPSHOT_BYTES: u64 = 128 << 20;

/// What a snapshot bound is clamped to: lengths and offsets stay `u32`
/// on every target, and a 32-bit host can hold the bytes.
pub(crate) const SNAPSHOT_BYTES_RANGE: std::ops::RangeInclusive<u64> = 4_096..=1 << 31;

/// Byte offset of the string-object count when no VM wait has completed.
/// Kept in lockstep with [`write_image`].
#[cfg(test)]
pub(crate) const STRING_COUNT_OFFSET: usize = 276;
/// Structural ceilings: what any runtime could hold. A snapshot is also
/// held to the limits it is restored into ([`Limits`]), and every count to
/// what is left of its bytes and to the decode budget.
const MAX_OBJECTS: u32 = crate::heap::OBJECTS_CEILING;
const MAX_STRING_BYTES: u32 = crate::heap::STRING_CEILING as u32;
/// Slots of one table: each is an entry of the logical heap.
const MAX_TABLE_ENTRIES: u32 = 1 << 28;
/// Values, and numbers, a native closure keeps (ADR 0035).
const MAX_CLOSURE_VALUES: u32 = 16;
const MAX_USER_VALUES: u32 = crate::userdata::MAX_USER_VALUES as u32;
const MAX_USERDATA_BYTES: u32 = crate::userdata::MAX_USERDATA_BYTES as u32;
const MAX_FRAMES: u32 = crate::runtime::MAX_FRAMES as u32;
/// The largest stack a thread may have: the top of the range
/// `Config::max_stack_slots` is clamped to (ADR 0028).
const MAX_STACK_SLOTS: u32 = *crate::runtime::STACK_SLOTS_RANGE.end();
const MAX_INSTRUCTIONS: u32 = crate::limits::MAX_INSTRUCTIONS as u32;
const MAX_CONSTS: u32 = crate::limits::MAX_CONSTS as u32;
const MAX_PROTOS: u32 = crate::limits::MAX_PROTOS as u32;
const MAX_UPVALUES: u32 = crate::limits::MAX_UPVALUES as u32;
/// The most source a `load` frame holds while it reads (ADR 0031): what
/// `load` accepts.
const MAX_SOURCE_BYTES: u32 = crate::limits::DEFAULT_SOURCE_BYTES as u32;

const MAGIC: &[u8; 4] = b"MNSD";
/// Container schema: the wire layout of the object graph. Schema 3: table
/// slots are live entries or dead traversal anchors. `next_live` links are
/// rebuilt from slot order and are not written. Schema 4: a native-function
/// symbol section, native values and keys (by index into it), and the
/// native pending-call states. Schema 5: a table's metatable, and a
/// frame's in-progress metamethod call. Schema 6: a prototype's constants
/// are references to its string objects; the GC policy revision and the
/// automatic-collection state. Schema 7: a metamethod call's event names how
/// it finishes: store (1), drop (2), drop and step the assignment (3), truth
/// (4), negated truth (5). Schema 8: the logical-heap quota, the reserved
/// error strings, boundary frames, a thread's unwind state, and a failed
/// thread's error (ADR 0024, ADR 0025). Schema 9: a thread's to-be-closed
/// list and coroutine and closing flags, a frame's `Close` event and idle
/// phase, and an unwind whose error may be absent (ADR 0026). Older
/// schemas are not restored. Adding an instruction does not change this
/// number. Schema 10: a vararg frame's extra arguments sit below its
/// registers, so a frame records only their count; the thread stack bound
/// (ADR 0028). Schema 11: the reserved strings also hold the type names
/// and the booleans' text, after five new error classes; a boundary frame
/// may be a base function waiting on a call it made, with its task
/// (ADR 0031). Schema 12: the standard libraries' state (the random
/// generator and the entropy stream), a library function's task in a
/// base-function frame, a call deferred to the next step, and the reserved
/// names `integer` and `float` after four new error classes (ADR 0032,
/// ADR 0033). Schema 13: each basic type's shared metatable, native
/// closures, string functions' tasks, and new error classes (ADR 0034,
/// ADR 0035). Schema 14: the Lua registry, package tasks, prototypes'
/// debug information and chunk names, the tail-call mark of a frame, and
/// new error classes (ADR 0039, ADR 0040). Schema 15: full userdata with
/// their metatables, user values, and byte or host payloads, light
/// userdata values and keys, and no shared metatable for full userdata
/// (ADR 0042 to ADR 0045). Schema 16: the finalization lists and flags,
/// finalizer frames, and `collectgarbage`'s frame waiting for them
/// (ADR 0047, ADR 0048). Schema 17: the incremental collector's state:
/// its parameters and schedule, its phase, every object's mark but the
/// current white, its gray, scanned, weak, ephemeron and waiting lists, and
/// the sweep's count; dead objects waiting for the sweep are not written
/// (ADR 0050). Schema 18: generational collection: the mode and its
/// multipliers, the last major collection's base, falling back, every
/// object's age, the young lists, the objects remembered for the next
/// young collection, and a young collection's progress; a `collectgarbage`
/// frame's result is a tag (ADR 0051). Schema 19: what each thread is
/// charged beyond its object, the bytes a sweep has freed, and where the
/// registered objects are known old; the logical heap is counted again,
/// never read (ADR 0051). Schema 20: the string limit, after the stack
/// bound; no bound on all strings' bytes together, and wider structural
/// ceilings (ADR 0052). Schema 21: native continuation frames, host wait
/// payloads, completed VM wait keys, main-thread host calls, and halted
/// callback misuse; rebindable host userdata as payload tag 3, a type
/// symbol and an external key of at most 4 KiB (ADR 0053). Schema 22:
/// native continuation frames retain the original external effect sequence.
// Schema 25: pending exit status, close policy and shutdown phase; typed
// capability pending/completed work (pending tag 8).
const VERSION: u16 = 25;
/// Instruction-set revision of the prototypes inside the snapshot. It changes
/// when an opcode is added or an existing opcode's encoding or meaning
/// changes. A decoder accepts exactly its own revision, so an older runtime
/// rejects newer code at the header, before any prototype is decoded.
/// Revision 1: tags 1-30. Revision 2: `LoadBool` (31), `CloseUpvalues` (32),
/// `JumpIfFalse` (33). Revision 3: `Compare` (34). Revision 4: `Neg` (35),
/// `ForPrep` (36), `ForLoop` (37). Revision 5: `Index` (38), `SetIndex` (39),
/// `GetField` (40), `SetField` (41), `SetList` (42); `AssignField` stores
/// with language-level assignment; a chunk's one capture is its `_ENV`.
/// Revision 6: `Index`, `SetIndex`, `GetField`, `SetField`, and assignment
/// stores follow `__index` / `__newindex`; `Len` (43).
/// Revision 7: `Len` passes `__len` its operand twice, as Lua 5.4 does, and
/// calls any `__len` value; `Call` follows `__call`; `Add`, `Neg`, and
/// `Compare` take numeric strings and metamethods; `Arith` (44), `BNot`
/// (45), `Concat` (46).
/// Revision 8: `MarkClose` (47), `CloseScope` (48), `CloseThread` (49);
/// `Return` closes the frame's to-be-closed values before it returns.
/// Revision 9: `GenericForLoop` (50).
/// Revision 10: `Vararg` reads a frame's extras below its registers
/// (ADR 0028); `Vararg` and `VarargLen` appear only in vararg prototypes.
/// Revision 11: `TailCall` (51), always followed by the `Return` of its
/// open window (ADR 0029). Revision 12: jump offsets are signed 32-bit;
/// constant, field-name, and child indexes are unsigned 32-bit. Registers
/// and host symbols keep their widths. Revision 13: `ArithK` (52), with
/// an exact integer immediate and the original operand order. Revision 14:
/// `CompareBranch` (53); Truth's reserved destination 255 finishes its branch.
pub(crate) const BYTECODE_REVISION: u16 = 14;
/// Table semantics revision: insertion-order `next`, dead anchors, and the
/// smallest-border raw length (ADR 0010). Revision 2: the float 2^63 is a
/// float key, not `math.maxinteger`; a float key stored as an integer is
/// returned by `next` as that integer; an update keeps the entry's key object.
/// Revision 3: tables have metatables (ADR 0018). Revision 4: an insert
/// keeps the dead anchors until they outnumber half the live entries, so a
/// key may have anchors before its live slot (Phase 3.30).
const TABLES_REVISION: u16 = 4;
/// Fuel revision: one logical unit per executed instruction. Revision 2:
/// and one per step of a base function that called Lua, when it goes on
/// from the call's result (ADR 0031). Revision 3: a math or table
/// function's step runs up to 32 semantic operations, and each step after
/// the first costs one unit; a function the VM implements, called by
/// another's frame, is called in its own step, which costs one unit
/// (ADR 0033). Revision 4: a string function's step does a bounded amount of
/// work (bytes built, pattern, format, or pack steps), and a string that
/// takes part in arithmetic does so through the string metatable's
/// metamethods, called as functions (ADR 0034). Revision 5: starting a
/// finalizer costs one unit, as a call does (ADR 0048). Revision 6: the
/// collector's work costs one unit per `gc::WORK_PER_FUEL` units of it,
/// what a unit paid for and was not used carrying over (ADR 0050).
const FUEL_REVISION: u16 = 7;
/// GC policy revision: the logical costs and the threshold rule that decide
/// where automatic collections run (ADR 0021). A decoder accepts exactly its
/// own, so a restored run collects where the original would have.
/// Revision 2: the threshold is also capped at half the room under the
/// heap quota; a table store the quota refuses collects and tries again;
/// a protected call that caught a memory error collects before it returns
/// (ADR 0025). Revision 3: the source a `load` reader has given so far is
/// charged to the logical heap and counted by every collection (ADR 0031).
/// Revision 4: so is the text `table.concat` has built (ADR 0033).
/// Revision 5: so are the bytes a string function has built, and native
/// closures have a logical size (ADR 0034, ADR 0035).
/// Revision 6: a prototype's debug information is charged to the logical
/// heap, and a `require` task's searcher messages (ADR 0039, ADR 0040).
/// Revision 7: so is every slot of a thread's stack, now that source can
/// make threads (ADR 0041). Revision 8: full userdata count their user
/// values and their payload's bytes, or what their host type declares
/// (ADR 0042, ADR 0044). Revision 9: tables with `__mode` are weak,
/// weak-key tables are ephemerons, and registered objects found dead are
/// queued for their finalizers and kept until they have run (ADR 0046,
/// ADR 0047); no collection starts while a finalizer runs. Revision 10:
/// the collector is incremental: cycles start after `pause` percent of
/// what the last kept, steps every 2^`stepsize` bytes do `stepmul` units of
/// work per KiB, and the work units are counted exactly (ADR 0050).
/// Revision 11: generational mode, the default: young collections every
/// `minormul` percent of what the last kept (at least the minimum debt),
/// a major collection once memory grows `majormul` percent past the last
/// one's, and whole cycles after a bad major (ADR 0051). Revision 12: the
/// quota is on the exact logical heap; a thread is charged for the stack
/// slots and builtin bytes it has held since a collection last traced it;
/// a young collection looks only at the objects registered since the one
/// before it (ADR 0051).
const GC_REVISION: u16 = 12;

/// Positions of an image's strings and threads, by id (see [`Image::index`]).
#[derive(Clone, Debug, Default)]
pub(crate) struct ImageIndex {
    strings: std::cell::OnceCell<std::collections::HashMap<u64, usize>>,
    threads: std::cell::OnceCell<std::collections::HashMap<u64, usize>>,
}

impl Image {
    /// The bytes of the string `id`.
    pub(crate) fn string(&self, id: u64) -> Option<&[u8]> {
        let at = self.index.strings.get_or_init(|| {
            self.strings
                .iter()
                .enumerate()
                .map(|(at, (id, _))| (*id, at))
                .collect()
        });
        at.get(&id).map(|at| self.strings[*at].1.as_slice())
    }

    /// The thread `id`.
    pub(crate) fn thread(&self, id: u64) -> Option<&ThreadImage> {
        let at = self.index.threads.get_or_init(|| {
            self.threads
                .iter()
                .enumerate()
                .map(|(at, thread)| (thread.id, at))
                .collect()
        });
        at.get(&id).map(|at| &self.threads[*at])
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Image {
    pub(crate) effect_domain: u64,
    pub(crate) next_object_id: u64,
    pub(crate) next_sequence: u64,
    pub(crate) fuel_consumed: u64,
    pub(crate) fuel_limit: Option<u64>,
    pub(crate) last_completed_wait: Option<u64>,
    pub(crate) completed_waits: Vec<u64>,
    pub(crate) host_call: bool,
    pub(crate) callback_failed: bool,
    pub(crate) trap: u8,
    pub(crate) max_objects: u32,
    /// `Config::max_stack_slots`: every thread's stack and slot index
    /// stays within it.
    pub(crate) max_stack_slots: u32,
    /// `Config::max_string_bytes` (ADR 0052).
    pub(crate) max_string: u32,
    pub(crate) gc: crate::heap::GcState,
    /// The standard libraries' state (ADR 0032).
    pub(crate) library: crate::library::LibraryState,
    /// Each basic type's shared metatable, by `heap::basic_type`, or 0
    /// (ADR 0034).
    pub(crate) type_metatables: [u64; crate::heap::BASIC_TYPES],
    pub(crate) globals: u64,
    /// Lua's registry, or 0 (ADR 0039).
    pub(crate) registry: u64,
    pub(crate) active: u64,
    pub(crate) entry: u64,
    pub(crate) strings: Vec<(u64, Vec<u8>)>,
    /// Native-function symbols; `EncValue::Native(i)` is `natives[i]`.
    pub(crate) natives: Vec<String>,
    /// Ids of the reserved strings, in `heap::reserved_texts` order.
    pub(crate) reserved: Vec<u64>,
    pub(crate) protos: Vec<ProtoImage>,
    pub(crate) tables: Vec<TableImage>,
    pub(crate) upvalues: Vec<UpImage>,
    pub(crate) closures: Vec<ClosureImage>,
    pub(crate) native_closures: Vec<NativeClosureImage>,
    pub(crate) threads: Vec<ThreadImage>,
    pub(crate) userdata: Vec<UserdataImage>,
    pub(crate) finalizers: FinalizersImage,
    pub(crate) collector: CollectorImage,
    /// Where each string and each thread is, by id: made once, on first
    /// use, so validation looks objects up in constant time.
    pub(crate) index: ImageIndex,
}

/// The incremental collector's state (ADR 0050), objects by id.
#[derive(Clone, Debug, Default)]
pub(crate) struct CollectorImage {
    pub(crate) phase: u8,
    pub(crate) atomic: u8,
    pub(crate) white: u8,
    pub(crate) gray: Vec<u64>,
    /// The object being scanned, its position, and how (0 a thread or a
    /// prototype, 1 table entries, 2 an ephemeron table) with the mode.
    pub(crate) scan: Option<(u64, u32, u8, bool, bool)>,
    pub(crate) weak: Vec<(u64, bool, bool)>,
    pub(crate) late: u32,
    pub(crate) ephemerons: Vec<u64>,
    /// By key id, ascending.
    pub(crate) waiting: Vec<(u64, Vec<EncValue>)>,
    pub(crate) cursor: u32,
    pub(crate) inner: u32,
    pub(crate) marked_bytes: u64,
    pub(crate) debt_base: u64,
    pub(crate) marking_debt: u64,
    pub(crate) work_base: u64,
    pub(crate) sweep_left: u64,
    pub(crate) reset: bool,
    /// Every object whose mark is not the default, with its age: the
    /// default is black and old off the young lists in generational form,
    /// the current white otherwise.
    pub(crate) marks: Vec<(u64, u8, u8)>,
    /// The arenas' lists of objects gray again, in kind order.
    pub(crate) again: Vec<u64>,
    /// Generational form, a young collection running, a sweep making
    /// survivors old, what the cycle decides (ADR 0051).
    pub(crate) generational: bool,
    pub(crate) minor: bool,
    pub(crate) to_old: bool,
    pub(crate) decide: u8,
    /// What the sweep running has freed and has still to free, logical
    /// bytes: left in the logical heap until it ends.
    pub(crate) unreleased: u64,
    pub(crate) promoted: u64,
    /// White objects whose mark or age is not the default, with the age.
    pub(crate) ages: Vec<(u64, u8)>,
    /// The arenas' young lists, in kind order, but entries freed or
    /// promoted.
    pub(crate) young: Vec<u64>,
    pub(crate) revisit: Vec<u64>,
    pub(crate) touched: Vec<u64>,
}

/// Finalization state (ADR 0047): object ids, in order.
#[derive(Clone, Debug, Default)]
pub(crate) struct FinalizersImage {
    /// Registered, oldest first.
    pub(crate) registered: Vec<u64>,
    /// Where the registered objects are known old, and where those
    /// registered since the last young collection begin (ADR 0051).
    pub(crate) old_until: u32,
    pub(crate) new_from: u32,
    /// Waiting for their finalizers, in the order they run.
    pub(crate) pending: Vec<u64>,
    pub(crate) running: bool,
    pub(crate) closing: bool,
    /// The entry thread's status before closing began, while it closes
    /// with no frames of its own, or 0.
    pub(crate) closed: u8,
    /// The closure of the finalizer frames while closing, or 0.
    pub(crate) close_closure: u64,
    pub(crate) exit: Option<crate::heap::ExitState>,
}

/// A full userdata (ADR 0042, ADR 0045).
#[derive(Clone, Debug)]
pub(crate) struct UserdataImage {
    pub(crate) id: u64,
    /// The metatable's id, or 0.
    pub(crate) metatable: u64,
    pub(crate) user_values: Vec<EncValue>,
    pub(crate) payload: PayloadImage,
    /// The logical bytes the payload counts.
    pub(crate) charge: u64,
}

#[derive(Clone, Debug)]
pub(crate) enum PayloadImage {
    Bytes(Vec<u8>),
    File(crate::iolib::FileState),
    /// A portable host value: its type's symbol and its codec's bytes.
    Host {
        symbol: String,
        bytes: Vec<u8>,
    },
    /// A host resource identified by a bounded external key.
    Rebind {
        symbol: String,
        key: Vec<u8>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct ProtoImage {
    pub(crate) id: u64,
    pub(crate) max_reg: u8,
    pub(crate) params: u8,
    pub(crate) vararg: bool,
    pub(crate) ops: Vec<Op>,
    /// Ids of the prototype's constant strings; this is what is written.
    pub(crate) const_ids: Vec<u64>,
    /// Their bytes, filled from the string section when decoding.
    pub(crate) byte_consts: Vec<Vec<u8>>,
    pub(crate) captures: Vec<crate::opcode::Capture>,
    pub(crate) children: Vec<u64>,
    /// The chunk name's string id, or 0 (ADR 0040).
    pub(crate) source: u64,
    pub(crate) debug: Option<Box<crate::debuginfo::DebugInfo>>,
}

#[derive(Clone, Debug)]
pub(crate) struct TableImage {
    pub(crate) id: u64,
    /// The metatable's id, or 0.
    pub(crate) metatable: u64,
    pub(crate) slots: Vec<SlotImage>,
}

#[derive(Clone, Debug)]
pub(crate) struct SlotImage {
    pub(crate) ordinal: u32,
    pub(crate) body: SlotBody,
}

#[derive(Clone, Debug)]
pub(crate) enum SlotBody {
    Live { key: EncValue, value: EncValue },
    Dead(DeadKey),
}

/// Identity of a deleted key. Not a GC edge. Object ids may name an object
/// that was collected before the snapshot.
#[derive(Clone, Debug)]
pub(crate) enum DeadKey {
    Bool(bool),
    Integer(i64),
    Float(u64),
    Bytes(Vec<u8>),
    Object(u64),
    Native(u32),
    Light(crate::value::LightDomain, u64),
}

#[derive(Clone, Debug)]
pub(crate) struct UpImage {
    pub(crate) id: u64,
    pub(crate) state: UpImageState,
}

#[derive(Clone, Debug)]
pub(crate) enum UpImageState {
    Closed(EncValue),
    Open { thread: u64, slot: u32 },
}

/// A native closure (ADR 0035): its builtin by native index, its values,
/// and its numbers.
#[derive(Clone, Debug)]
pub(crate) struct NativeClosureImage {
    pub(crate) id: u64,
    pub(crate) native: u32,
    pub(crate) values: Vec<EncValue>,
    pub(crate) state: Vec<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct ClosureImage {
    pub(crate) id: u64,
    pub(crate) proto: u64,
    pub(crate) upvalues: Vec<u64>,
}

#[derive(Clone, Debug)]
pub(crate) struct UnwindImage {
    /// Class tag and object; `None` only while a thread that had not
    /// failed is being closed.
    pub(crate) error: Option<(u8, EncValue)>,
    pub(crate) phase: crate::heap::UnwindPhase,
}

/// A frame's metamethod call (ADR 0019), or its closes (ADR 0026).
#[derive(Clone, Debug)]
pub(crate) struct MetaImage {
    pub(crate) event: EventImage,
    pub(crate) slot: u32,
    pub(crate) nargs: u8,
    pub(crate) phase: crate::heap::MetaPhase,
}

#[derive(Clone, Debug)]
pub(crate) enum EventImage {
    /// Every event but `Close`: plain numbers.
    Plain(crate::heap::MetaEvent),
    Close {
        from: u32,
        next: NextImage,
    },
}

#[derive(Clone, Debug)]
pub(crate) enum NextImage {
    Advance,
    Return { src: u32, produced: u32 },
    Unwind(UnwindImage),
}

#[derive(Clone, Debug)]
pub(crate) enum BoundaryImage {
    Protect {
        func: u32,
        advance_caller: bool,
        handler: Option<EncValue>,
    },
    Handler {
        slot: u32,
        protect: u32,
        target: u32,
        depth: u8,
        fault: u8,
    },
    /// A base function waiting on the call it made (ADR 0031). Its task
    /// holds no values.
    Builtin {
        func: u32,
        passed: u32,
        advance_caller: bool,
        task: crate::heap::Task,
    },
    Native {
        func: u32,
        passed: u32,
        advance_caller: bool,
        symbol: u32,
        tag: u32,
        kept: u32,
        sequence: Option<u64>,
        error: Option<(u8, EncValue)>,
        resuming: bool,
    },
    Hook {
        func: u32,
        saved_top: u32,
        target: u32,
        instruction: Option<(usize, u32, u8)>,
        after: crate::runtime::hooks::AfterHook,
    },
    HookNative {
        func: u32,
        passed: u32,
        callee: EncValue,
        advance_caller: bool,
        phase: u8,
        produced: u32,
        result: u32,
    },
    /// A finalizer's call (ADR 0048).
    Finalizer { func: u32, saved_top: u32 },
}

impl BoundaryImage {
    /// The slot of the call the frame stands for.
    fn call_slot(&self) -> u32 {
        match self {
            Self::Protect { func, .. } => func.saturating_add(1),
            Self::Handler { slot, .. } => *slot,
            Self::Builtin {
                func, passed, task, ..
            } => func
                .saturating_add(1)
                .saturating_add(*passed)
                .saturating_add(task.scratch()),
            Self::Native {
                func, passed, kept, ..
            } => func
                .saturating_add(1)
                .saturating_add(*passed)
                .saturating_add(*kept),
            Self::Finalizer { func, .. }
            | Self::Hook { func, .. }
            | Self::HookNative { func, .. } => *func,
        }
    }

    /// The results that call wants.
    fn call_wants(&self) -> u8 {
        match self {
            Self::Protect { .. } | Self::Native { .. } => crate::opcode::COUNT_OPEN,
            Self::Handler { .. } => 1,
            Self::Builtin { task, .. } => task.wants(),
            Self::Finalizer { .. } | Self::Hook { .. } => 0,
            Self::HookNative { .. } => crate::opcode::COUNT_OPEN,
        }
    }

    /// Whether an error raised above stops here: a protected call, or
    /// `load` reading.
    fn catches(&self) -> bool {
        match self {
            Self::Protect { .. } | Self::Native { .. } => true,
            Self::Handler { .. } => false,
            Self::Builtin { task, .. } => task.catches(),
            Self::Finalizer { .. } => true,
            Self::Hook { .. } | Self::HookNative { .. } => false,
        }
    }

    /// The slot of its caller's `Call` that the frame's end moves past.
    fn advances(&self) -> Option<u32> {
        match self {
            Self::Protect {
                func,
                advance_caller: true,
                ..
            }
            | Self::Builtin {
                func,
                advance_caller: true,
                ..
            }
            | Self::Native {
                func,
                advance_caller: true,
                ..
            } => Some(*func),
            _ => None,
        }
    }
}

/// Whether an unwind with no target may pass a frame: it holds no
/// boundary that catches or handles errors.
fn passes(frame: &FrameImage) -> bool {
    match &frame.boundary {
        None => true,
        Some(BoundaryImage::Handler { .. }) => false,
        Some(boundary) => !boundary.catches(),
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ThreadImage {
    pub(crate) id: u64,
    pub(crate) status: u8,
    pub(crate) resumed_by: u64,
    pub(crate) top: u32,
    pub(crate) stack: Vec<EncValue>,
    pub(crate) results: Vec<EncValue>,
    pub(crate) frames: Vec<FrameImage>,
    /// A failed thread's error: class tag and object.
    pub(crate) error: Option<(u8, EncValue)>,
    pub(crate) unwind: Option<UnwindImage>,
    pub(crate) coroutine: bool,
    pub(crate) closing: bool,
    /// Active to-be-closed slots, strictly increasing.
    pub(crate) tbc: Vec<u32>,
    /// What the thread is charged beyond its object: stack slots, and
    /// bytes its builtins hold (ADR 0051).
    pub(crate) charged_slots: u32,
    pub(crate) charged_held: u64,
    pub(crate) hook: Option<HookImage>,
}

#[derive(Clone, Debug)]
pub(crate) enum HookTargetImage {
    None,
    Lua(EncValue),
    Host(String),
    InheritedLua,
}
#[derive(Clone, Debug)]
pub(crate) struct HookImage {
    pub(crate) target: HookTargetImage,
    pub(crate) mask: u8,
    pub(crate) base_count: i32,
    pub(crate) remaining_count: i32,
    pub(crate) allow_hook: bool,
    pub(crate) old_pc: Option<(usize, u32, Option<u32>)>,
    pub(crate) pending: Option<crate::runtime::hooks::PendingEvent>,
    pub(crate) hook_yield: bool,
    pub(crate) names: Vec<EncValue>,
    pub(crate) instruction: Option<(usize, u32, u8)>,
    pub(crate) after: crate::runtime::hooks::AfterHook,
    pub(crate) transfer: Option<(u32, u32, u32)>,
    pub(crate) restore_cursor: Option<(usize, u32, Option<u32>)>,
}

#[derive(Clone, Debug)]
pub(crate) struct FrameImage {
    pub(crate) closure: u64,
    pub(crate) pc: u32,
    pub(crate) base: u32,
    pub(crate) limit: u32,
    pub(crate) nresults: u8,
    pub(crate) vararg_len: u32,
    pub(crate) tail: bool,
    pub(crate) return_hook: bool,
    pub(crate) pending: PendingImage,
    pub(crate) wait_request: Option<(String, Vec<EncValue>)>,
    pub(crate) targets: Vec<TargetImage>,
    pub(crate) meta: Option<MetaImage>,
    pub(crate) boundary: Option<BoundaryImage>,
}

#[derive(Clone, Debug)]
pub(crate) enum TargetImage {
    Register(u32),
    Field { table: EncValue, key: EncValue },
}

#[derive(Clone, Debug)]
pub(crate) enum PendingImage {
    None,
    Prepared {
        sequence: u64,
        symbol: String,
        arg: i64,
        dest: u8,
    },
    Waiting {
        sequence: u64,
        symbol: String,
        arg: i64,
        dest: u8,
        wait_key: u64,
    },
    Resuming {
        child: u64,
        dest: u8,
        nresults: u8,
    },
    Assigning {
        src: u32,
        nvalues: u16,
        next: u16,
    },
    NativePrepared {
        sequence: u64,
    },
    NativeWaiting {
        sequence: Option<u64>,
        wait_key: u64,
    },
    Capability {
        sequence: u64,
        wait_key: u64,
        completed: bool,
    },
    /// A builtin's call left for the next step (ADR 0033).
    Deferred,
}

#[derive(Clone, Debug)]
pub(crate) enum EncValue {
    Nil,
    Bool(bool),
    Integer(i64),
    Float(u64),
    String(u64),
    Table(u64),
    Closure(u64),
    Thread(u64),
    Native(u32),
    NativeClosure(u64),
    Userdata(u64),
    Light(crate::value::LightDomain, u64),
}

impl Runtime {
    /// The checkpoint of this runtime, refused with
    /// `SnapshotError::LimitExceeded` past `Config::max_snapshot_bytes`.
    /// Host userdata growth stays charged past the quota (ADR 0044);
    /// Userdata charges or collection accounting past it refuse the
    /// snapshot (ADR 0052).
    pub fn snapshot(&self) -> Result<Vec<u8>, SnapshotError> {
        if self.in_callback() {
            return Err(SnapshotError::InvalidStructure);
        }
        let image = self.to_image()?;
        check_pending_resources(&image, None)?;
        if !slots_fit(&image, image.max_stack_slots) {
            return Err(SnapshotError::LimitExceeded);
        }
        let userdata_bytes = image.userdata.iter().fold(0u64, |total, object| {
            total.saturating_add(crate::heap::userdata_cost(
                object.user_values.len(),
                object.charge,
            ))
        });
        if userdata_bytes > image.gc.quota
            || image.gc.major_base > image.gc.quota
            || image.collector.unreleased > image.gc.quota
        {
            return Err(SnapshotError::LimitExceeded);
        }
        encode_within(&image, self.max_snapshot())
    }

    /// Restore a checkpoint into the default [`Limits`]: see
    /// [`Runtime::from_snapshot_with_limits`].
    pub fn from_snapshot(
        bytes: &[u8],
        registry: &HostRegistry,
        expected_domain: u64,
    ) -> Result<Self, SnapshotError> {
        Self::from_snapshot_with_limits(bytes, registry, expected_domain, Limits::default())
    }

    /// Restore a checkpoint into the host's `limits` (ADR 0052). The
    /// restored runtime runs under, for each limit, the smaller of the
    /// snapshot's and the host's: equal or larger host limits restore the
    /// runtime exactly as it was, and a host limit is never raised. A
    /// smaller host limit the state does not fit (more live objects, a
    /// longer stack or string, a larger logical heap) refuses the
    /// snapshot with `SnapshotError::LimitExceeded` before any runtime
    /// exists. A snapshot longer than `limits.max_snapshot_bytes` is
    /// refused unread, and decoding allocates at most about four times
    /// the logical heap it may restore, plus the snapshot's length.
    pub fn from_snapshot_with_limits(
        bytes: &[u8],
        registry: &HostRegistry,
        expected_domain: u64,
        limits: Limits,
    ) -> Result<Self, SnapshotError> {
        Self::restore(
            bytes,
            &crate::Host::new(registry.clone())
                .limits(limits)
                .effect_domain(expected_domain),
        )
        .map_err(|error| match error {
            crate::Error::Vm(crate::VmError::Snapshot(error)) => error,
            _ => SnapshotError::InvalidStructure,
        })
    }

    /// Restore against the host's registrations, policies, resources, and limits.
    /// Validation, codecs, and rebinds finish before a runtime is constructed.
    /// Moonseed library symbols allowed by [`crate::Host::libraries`] are supplied
    /// automatically; the host registry needs only host callbacks, hooks and
    /// userdata types. A denied library or absent host symbol returns
    /// [`SnapshotError::UnknownHostSymbol`] wrapped in [`crate::Error::Vm`].
    /// Execution capabilities are optional: `print` without a sink writes nowhere.
    /// Old rooted [`crate::Value`] objects belong to the original runtime;
    /// reacquire them through globals or [`Runtime::object`] after restore.
    /// The host retains the journal separately and supplies it to `run` again;
    /// the expected effect domain identifies that journal's run lineage.
    pub fn restore(bytes: &[u8], host: &crate::Host) -> crate::Result<Self> {
        Self::restore_snapshot(bytes, host)
            .map_err(|error| crate::Error::Vm(crate::VmError::Snapshot(error)))
    }

    fn restore_snapshot(bytes: &[u8], host: &crate::Host) -> Result<Self, SnapshotError> {
        let limits = host.limits.clamped();
        let image = decode_within(bytes, &limits)?;
        if image.effect_domain != host.effect_domain {
            return Err(SnapshotError::EffectDomainMismatch);
        }
        let registry = host.restore_registry(&image.natives)?;
        check_pending_resources(&image, Some(&host.capabilities))?;
        let mut runtime = realize(&image, &registry, &limits, &host.capabilities)?;
        runtime.apply_capabilities(&host.capabilities);
        runtime.refresh_hook_trap();
        Ok(runtime)
    }

    pub(crate) fn to_image(&self) -> Result<Image, SnapshotError> {
        let heap = self.heap();
        let mut strings = Vec::new();
        for (_, _, object) in heap.strings.iter() {
            strings.push((object.id.raw(), object.bytes.to_vec()));
        }
        let mut protos = Vec::new();
        for (_, _, proto) in heap.protos.iter() {
            protos.push(proto_image(heap, proto)?);
        }
        let mut tables = Vec::new();
        for (_, _, table) in heap.tables.iter() {
            tables.push(table_image(heap, table)?);
        }
        let mut upvalues = Vec::new();
        for (_, _, upvalue) in heap.upvalues.iter() {
            upvalues.push(up_image(heap, upvalue)?);
        }
        let mut closures = Vec::new();
        for (_, _, closure) in heap.closures.iter() {
            closures.push(closure_image(heap, closure)?);
        }
        let mut threads = Vec::new();
        for (_, _, thread) in heap.threads.iter() {
            threads.push(thread_image(heap, self.host_registry(), thread)?);
        }
        let mut native_closures = Vec::new();
        for (_, _, closure) in heap.native_closures.iter() {
            native_closures.push(NativeClosureImage {
                id: closure.id.raw(),
                native: closure.native,
                values: closure
                    .values
                    .iter()
                    .map(|value| enc_value(heap, *value))
                    .collect::<Result<_, _>>()?,
                state: closure.state.clone(),
            });
        }
        let mut userdata = Vec::new();
        for (_, _, object) in heap.userdata.iter() {
            userdata.push(self.userdata_image(object)?);
        }
        Ok(Image {
            effect_domain: self.effect_domain(),
            next_object_id: heap.next_object_id,
            next_sequence: self.next_sequence(),
            fuel_consumed: self.fuel_consumed(),
            fuel_limit: self.fuel_limit(),
            last_completed_wait: self.last_completed_wait(),
            completed_waits: self.completed_waits(),
            host_call: self.host_call(),
            callback_failed: self.callback_failed(),
            trap: trap_tag(self.trap_reason()),
            max_objects: self.max_objects(),
            max_stack_slots: self.max_stack_slots_public(),
            max_string: heap.max_string as u32,
            gc: heap.gc.clone(),
            library: heap.library.clone(),
            type_metatables: {
                let mut ids = [0; crate::heap::BASIC_TYPES];
                for (id, handle) in ids.iter_mut().zip(heap.type_metatables) {
                    *id = optional_table_id(heap, handle)?;
                }
                ids
            },
            globals: optional_table_id(heap, heap.globals)?,
            registry: optional_table_id(heap, heap.registry)?,
            active: optional_thread_id(heap, heap.active)?,
            entry: optional_thread_id(heap, heap.entry)?,
            strings,
            natives: heap.natives.clone(),
            reserved: heap
                .reserved
                .iter()
                .map(|handle| {
                    heap.strings
                        .get(*handle)
                        .map(|string| string.id.raw())
                        .ok_or(SnapshotError::DanglingReference)
                })
                .collect::<Result<_, _>>()?,
            protos,
            tables,
            upvalues,
            closures,
            threads,
            native_closures,
            userdata,
            finalizers: {
                let fin = &heap.finalizers;
                let ids = |list: &mut dyn Iterator<Item = &crate::heap::FinRef>| {
                    list.map(|fin| {
                        heap.object_id_of_value(fin.value())
                            .map(ObjectId::raw)
                            .ok_or(SnapshotError::DanglingReference)
                    })
                    .collect::<Result<Vec<_>, _>>()
                };
                FinalizersImage {
                    registered: ids(&mut fin.registered.iter())?,
                    old_until: fin.old_until,
                    new_from: fin.new_from,
                    pending: ids(&mut fin.pending.iter())?,
                    running: fin.running,
                    closing: fin.closing,
                    exit: fin.exit,
                    closed: fin.closed.map_or(0, Status::tag),
                    close_closure: match fin.close_closure {
                        Some(handle) => heap
                            .closures
                            .get(handle)
                            .ok_or(SnapshotError::DanglingReference)?
                            .id
                            .raw(),
                        None => 0,
                    },
                }
            },
            collector: collector_image(heap)?,
            index: ImageIndex::default(),
        })
    }

    /// A full userdata's image. A host value goes through its type's
    /// codec; a type without one refuses the snapshot (ADR 0045).
    fn userdata_image(
        &self,
        object: &crate::heap::UserdataObj,
    ) -> Result<UserdataImage, SnapshotError> {
        use crate::userdata::Payload;
        let heap = self.heap();
        let payload = match &object.payload {
            Payload::File(file) => {
                if !file.closed && file.policy == crate::hostcaps::HandlePolicy::Refuse {
                    return Err(SnapshotError::NonPortableResource { object: object.id });
                }
                PayloadImage::File((**file).clone())
            }
            Payload::Bytes(bytes) => PayloadImage::Bytes(bytes.to_vec()),
            Payload::Host { symbol, value } => {
                let host = self
                    .registry()
                    .userdata_type(symbol)
                    .ok_or(SnapshotError::UnknownUserdataType)?;
                // Restore refuses a value that counts more than its charge:
                // refuse it here, so no checkpoint is written that cannot
                // be restored.
                if (host.size)(value.as_ref()).is_none_or(|size| size > object.charge) {
                    return Err(SnapshotError::UserdataCharge);
                }
                match host.policy {
                    crate::userdata::Policy::Refuse => {
                        return Err(SnapshotError::NonPortableUserdata);
                    }
                    crate::userdata::Policy::Portable(codec) => {
                        let bytes = (codec.encode)(value.as_ref())
                            .ok_or(SnapshotError::UnknownUserdataType)?;
                        if bytes.len() > crate::userdata::MAX_USERDATA_BYTES {
                            return Err(SnapshotError::LimitExceeded);
                        }
                        PayloadImage::Host {
                            symbol: (*symbol).to_string(),
                            bytes,
                        }
                    }
                    crate::userdata::Policy::Rebind { key, .. } => {
                        let key = key(value.as_ref()).ok_or(SnapshotError::UnknownUserdataType)?;
                        if key.len() > crate::MAX_REBIND_KEY {
                            return Err(SnapshotError::LimitExceeded);
                        }
                        PayloadImage::Rebind {
                            symbol: (*symbol).to_string(),
                            key,
                        }
                    }
                }
            }
        };
        Ok(UserdataImage {
            id: object.id.raw(),
            metatable: optional_table_id(heap, object.metatable)?,
            user_values: object
                .user_values
                .iter()
                .map(|value| enc_value(heap, *value))
                .collect::<Result<_, _>>()?,
            payload,
            charge: object.charge,
        })
    }
}

impl Runtime {
    fn trap_reason(&self) -> Option<TerminationReason> {
        self.trap_public()
    }

    fn max_objects(&self) -> u32 {
        self.max_objects_public()
    }
}

/// The collector's state, objects by id (ADR 0050). Dead objects waiting
/// for the sweep are not in the image, nor their marks.
/// The logical size of the objects a snapshot writes: every live one.
fn live_bytes(heap: &Heap) -> u64 {
    use crate::heap::LogicalSize;
    let mut total = 0u64;
    for kind in crate::gc::KINDS {
        let bytes: u64 = crate::heap::on_arena!(heap, kind, arena => {
            arena.iter().map(|(_, _, object)| object.logical_size()).sum()
        });
        total = total.saturating_add(bytes);
    }
    total
}

fn collector_image(heap: &Heap) -> Result<CollectorImage, SnapshotError> {
    use crate::gc::{How, Phase};
    use crate::heap::TraceRef;
    let collector = &heap.collector;
    let id = |object: TraceRef| {
        heap.id_of(object)
            .map(ObjectId::raw)
            .ok_or(SnapshotError::DanglingReference)
    };
    let table = |index| TraceRef {
        kind: Kind::Table,
        index,
    };
    let (phase, atomic) = match collector.phase {
        Phase::Pause => (0, 0),
        Phase::Begin => (1, 0),
        Phase::Propagate => (2, 0),
        Phase::Atomic(step) => (3, step.tag()),
        Phase::Sweep => (4, 0),
        Phase::Touched => (5, 0),
    };
    let scan = match collector.scan {
        Some(scan) => {
            let (how, keys, values) = match scan.how {
                How::Object => (0, false, false),
                How::Entries { keys, values } => (1, keys, values),
                How::Ephemeron => (2, false, false),
            };
            Some((id(scan.object)?, scan.pos, how, keys, values))
        }
        None => None,
    };
    let mut waiting = collector
        .waiting
        .iter()
        .map(|(&(kind, index), values)| {
            Ok((
                id(TraceRef { kind, index })?,
                values
                    .iter()
                    .map(|value| enc_value(heap, *value))
                    .collect::<Result<Vec<_>, _>>()?,
            ))
        })
        .collect::<Result<Vec<_>, SnapshotError>>()?;
    waiting.sort_by_key(|(key, _)| *key);
    let mut marks = Vec::new();
    let mut again = Vec::new();
    let mut ages = Vec::new();
    let mut young = Vec::new();
    macro_rules! arena {
        ($arena:ident, $kind:expr) => {
            // A young collection's sweep leaves freed and promoted entries
            // behind it; what it passed is told by marks and ages.
            let mut listed = HashSet::new();
            for &index in heap.$arena.young() {
                // Dead ones a young collection's sweep has not reached
                // are not written, like any dead object.
                let dead = heap.$arena.mark_of(index).is_none_or(|found| {
                    crate::heap::mark::is_white(found) && found != collector.white
                });
                if index == crate::gc::TOMB
                    || dead
                    || heap.$arena.age_of(index) > crate::heap::age::SURVIVAL
                {
                    continue;
                }
                if let Some(found) = heap.id_of(TraceRef { kind: $kind, index }) {
                    listed.insert(index);
                    young.push(found.raw());
                }
            }
            // Only what differs from the default is written: in
            // generational form, black and old off the young lists; white
            // and new otherwise.
            for (index, _, object) in heap.$arena.iter() {
                let mark = heap.$arena.mark_of(index).unwrap_or(collector.white);
                let age = heap.$arena.age_of(index);
                let default = if collector.generational && !listed.contains(&index) {
                    (crate::heap::mark::BLACK, crate::heap::age::OLD)
                } else {
                    (collector.white, crate::heap::age::NEW)
                };
                if (mark, age) == default {
                    continue;
                }
                if mark != collector.white {
                    marks.push((object.id.raw(), mark, age));
                } else {
                    ages.push((object.id.raw(), age));
                }
            }
            // An entry no longer gray (a young object a young collection's
            // sweep made white) costs nothing when taken, so is left out.
            for &index in heap.$arena.again() {
                if heap.$arena.mark_of(index) == Some(crate::heap::mark::GRAY) {
                    again.push(id(TraceRef { kind: $kind, index })?);
                }
            }
        };
    }
    arena!(strings, Kind::String);
    arena!(tables, Kind::Table);
    arena!(protos, Kind::Proto);
    arena!(upvalues, Kind::Upvalue);
    arena!(closures, Kind::Closure);
    arena!(threads, Kind::Thread);
    arena!(native_closures, Kind::NativeClosure);
    arena!(userdata, Kind::Userdata);
    Ok(CollectorImage {
        phase,
        atomic,
        white: collector.white,
        gray: collector
            .gray
            .iter()
            .map(|object| id(*object))
            .collect::<Result<_, _>>()?,
        scan,
        weak: collector
            .weak
            .iter()
            .map(|entry| Ok((id(table(entry.table))?, entry.keys, entry.values)))
            .collect::<Result<_, SnapshotError>>()?,
        late: collector.late,
        ephemerons: collector
            .ephemerons
            .iter()
            .map(|index| id(table(*index)))
            .collect::<Result<_, _>>()?,
        waiting,
        cursor: collector.cursor,
        inner: collector.inner,
        marked_bytes: collector.marked_bytes,
        debt_base: collector.debt_base,
        marking_debt: collector.marking_debt,
        work_base: collector.work_base,
        sweep_left: collector.sweep_left,
        reset: collector.reset,
        marks,
        again,
        generational: collector.generational,
        minor: collector.minor,
        to_old: collector.to_old,
        decide: collector.decide.tag(),
        // The dead objects are not written: what they count is.
        unreleased: heap.gc.used.saturating_sub(live_bytes(heap)),
        promoted: collector.promoted,
        ages,
        young,
        revisit: collector
            .revisit
            .iter()
            .map(|object| id(*object))
            .collect::<Result<_, _>>()?,
        touched: collector
            .touched
            .iter()
            .map(|object| id(*object))
            .collect::<Result<_, _>>()?,
    })
}

fn trap_tag(trap: Option<TerminationReason>) -> u8 {
    match trap {
        None => 0,
        Some(TerminationReason::FuelLimitExceeded) => 1,
        Some(TerminationReason::MemoryLimit) => 2,
        Some(TerminationReason::ObjectIdExhausted) => 3,
    }
}

fn trap_from(tag: u8) -> Result<Option<TerminationReason>, SnapshotError> {
    Ok(match tag {
        0 => None,
        1 => Some(TerminationReason::FuelLimitExceeded),
        2 => Some(TerminationReason::MemoryLimit),
        3 => Some(TerminationReason::ObjectIdExhausted),
        _ => return Err(SnapshotError::InvalidTag),
    })
}

fn optional_table_id(heap: &Heap, handle: Option<Handle<TableObj>>) -> Result<u64, SnapshotError> {
    match handle {
        Some(handle) => Ok(heap
            .tables
            .get(handle)
            .ok_or(SnapshotError::DanglingReference)?
            .id
            .raw()),
        None => Ok(0),
    }
}

fn optional_thread_id(
    heap: &Heap,
    handle: Option<Handle<ThreadObj>>,
) -> Result<u64, SnapshotError> {
    match handle {
        Some(handle) => Ok(heap
            .threads
            .get(handle)
            .ok_or(SnapshotError::DanglingReference)?
            .id
            .raw()),
        None => Ok(0),
    }
}

fn proto_image(heap: &Heap, proto: &Proto) -> Result<ProtoImage, SnapshotError> {
    let mut children = Vec::new();
    for child in &proto.children {
        children.push(
            heap.protos
                .get(*child)
                .ok_or(SnapshotError::DanglingReference)?
                .id
                .raw(),
        );
    }
    let mut const_ids = Vec::with_capacity(proto.const_strings.len());
    for string in &proto.const_strings {
        const_ids.push(
            heap.strings
                .get(*string)
                .ok_or(SnapshotError::DanglingReference)?
                .id
                .raw(),
        );
    }
    Ok(ProtoImage {
        id: proto.id.raw(),
        max_reg: proto.max_reg,
        params: proto.params,
        vararg: proto.vararg,
        ops: proto.ops.clone(),
        const_ids,
        byte_consts: proto.byte_consts.clone(),
        captures: proto.captures.clone(),
        children,
        source: match proto.source {
            Some(handle) => heap
                .strings
                .get(handle)
                .ok_or(SnapshotError::DanglingReference)?
                .id
                .raw(),
            None => 0,
        },
        debug: proto.debug.clone(),
    })
}

fn table_image(heap: &Heap, table: &TableObj) -> Result<TableImage, SnapshotError> {
    let mut slots = Vec::new();
    for (index, slot) in table.table.slots().iter().enumerate() {
        let body = match slot {
            Slot::Live {
                key_value, value, ..
            } => SlotBody::Live {
                key: enc_value(heap, *key_value)?,
                value: enc_value(heap, *value)?,
            },
            Slot::Dead { key, .. } => SlotBody::Dead(dead_key(key)?),
        };
        slots.push(SlotImage {
            ordinal: u32::try_from(index).map_err(|_| SnapshotError::LimitExceeded)?,
            body,
        });
    }
    let metatable = match table.metatable {
        Some(handle) => heap
            .tables
            .get(handle)
            .ok_or(SnapshotError::DanglingReference)?
            .id
            .raw(),
        None => 0,
    };
    Ok(TableImage {
        id: table.id.raw(),
        metatable,
        slots,
    })
}

fn dead_key(key: &TableKey) -> Result<DeadKey, SnapshotError> {
    Ok(match key {
        TableKey::Bool(bit) => DeadKey::Bool(*bit),
        TableKey::Integer(integer) => DeadKey::Integer(*integer),
        TableKey::Float(bits) => DeadKey::Float(*bits),
        TableKey::String(key) => DeadKey::Bytes(key.bytes().to_vec()),
        TableKey::Object(id) => DeadKey::Object(id.raw()),
        TableKey::Native(index) => DeadKey::Native(*index),
        TableKey::Light(domain, bits) => DeadKey::Light(*domain, *bits),
    })
}

fn up_image(heap: &Heap, upvalue: &UpvalueObj) -> Result<UpImage, SnapshotError> {
    let state = match &upvalue.state {
        UpvalueState::Closed(value) => UpImageState::Closed(enc_value(heap, *value)?),
        UpvalueState::Open { thread, slot } => UpImageState::Open {
            thread: heap
                .threads
                .get(*thread)
                .ok_or(SnapshotError::DanglingReference)?
                .id
                .raw(),
            slot: *slot,
        },
    };
    Ok(UpImage {
        id: upvalue.id.raw(),
        state,
    })
}

fn closure_image(heap: &Heap, closure: &ClosureObj) -> Result<ClosureImage, SnapshotError> {
    let proto = heap
        .protos
        .get(closure.proto)
        .ok_or(SnapshotError::DanglingReference)?
        .id
        .raw();
    let mut upvalues = Vec::new();
    for upvalue in &closure.upvalues {
        upvalues.push(
            heap.upvalues
                .get(*upvalue)
                .ok_or(SnapshotError::DanglingReference)?
                .id
                .raw(),
        );
    }
    Ok(ClosureImage {
        id: closure.id.raw(),
        proto,
        upvalues,
    })
}

fn thread_image(
    heap: &Heap,
    registry: &HostRegistry,
    thread: &ThreadObj,
) -> Result<ThreadImage, SnapshotError> {
    let mut stack = Vec::new();
    for value in &thread.stack {
        stack.push(enc_value(heap, *value)?);
    }
    let mut results = Vec::new();
    for value in &thread.host_results {
        results.push(enc_value(heap, *value)?);
    }
    let resumed_by = match thread.resumed_by {
        Some(parent) => heap
            .threads
            .get(parent)
            .ok_or(SnapshotError::DanglingReference)?
            .id
            .raw(),
        None => 0,
    };
    let mut frames = Vec::new();
    for frame in &thread.frames {
        frames.push(frame_image(heap, frame)?);
    }
    let error = match thread.error {
        Some((fault, value)) => Some((fault.tag(), enc_value(heap, value)?)),
        None => None,
    };
    let unwind = thread
        .unwind
        .as_deref()
        .map(|unwind| unwind_image(heap, unwind))
        .transpose()?;
    Ok(ThreadImage {
        id: thread.id.raw(),
        status: thread.status.tag(),
        resumed_by,
        top: thread.top,
        stack,
        results,
        frames,
        error,
        unwind,
        coroutine: thread.coroutine,
        closing: thread.closing,
        tbc: thread.tbc.clone(),
        charged_slots: thread.charged_slots,
        charged_held: thread.charged_held,
        hook: heap
            .hooks
            .get(thread.id)
            .map(|h| hook_image(heap, registry, h))
            .transpose()?,
    })
}

fn unwind_image(heap: &Heap, unwind: &crate::heap::Unwind) -> Result<UnwindImage, SnapshotError> {
    Ok(UnwindImage {
        error: unwind
            .error
            .map(|(fault, value)| Ok::<_, SnapshotError>((fault.tag(), enc_value(heap, value)?)))
            .transpose()?,
        phase: unwind.phase,
    })
}

fn meta_image(heap: &Heap, meta: &crate::heap::MetaCall) -> Result<MetaImage, SnapshotError> {
    use crate::heap::{CloseNext, MetaEvent};
    let event = match (meta.event, meta.close.as_deref()) {
        (MetaEvent::Close, Some(closing)) => EventImage::Close {
            from: closing.from,
            next: match closing.next {
                CloseNext::Advance => NextImage::Advance,
                CloseNext::Return { src, produced } => NextImage::Return { src, produced },
                CloseNext::Unwind(unwind) => NextImage::Unwind(unwind_image(heap, &unwind)?),
            },
        },
        (MetaEvent::Close, None) | (_, Some(_)) => return Err(SnapshotError::InvalidStructure),
        (event, None) => EventImage::Plain(event),
    };
    Ok(MetaImage {
        event,
        slot: meta.slot,
        nargs: meta.nargs,
        phase: meta.phase,
    })
}

fn frame_image(heap: &Heap, frame: &Frame) -> Result<FrameImage, SnapshotError> {
    let closure = heap
        .closures
        .get(frame.closure)
        .ok_or(SnapshotError::DanglingReference)?
        .id
        .raw();
    let pending = match frame.pending() {
        None => PendingImage::None,
        Some(Pending::Deferred) => PendingImage::Deferred,
        Some(Pending::Prepared {
            sequence,
            symbol,
            arg,
            dest,
        }) => PendingImage::Prepared {
            sequence: *sequence,
            symbol: symbol.clone(),
            arg: *arg,
            dest: *dest,
        },
        Some(Pending::Waiting {
            sequence,
            symbol,
            arg,
            dest,
            wait_key,
        }) => PendingImage::Waiting {
            sequence: *sequence,
            symbol: symbol.clone(),
            arg: *arg,
            dest: *dest,
            wait_key: *wait_key,
        },
        Some(Pending::Resuming {
            child,
            dest,
            nresults,
        }) => PendingImage::Resuming {
            child: heap
                .threads
                .get(*child)
                .ok_or(SnapshotError::DanglingReference)?
                .id
                .raw(),
            dest: *dest,
            nresults: *nresults,
        },
        Some(Pending::Assigning { src, nvalues, next }) => PendingImage::Assigning {
            src: *src,
            nvalues: *nvalues,
            next: *next,
        },
        Some(Pending::Capability {
            sequence,
            wait_key,
            completed,
        }) => PendingImage::Capability {
            sequence: *sequence,
            wait_key: *wait_key,
            completed: *completed,
        },
        Some(Pending::NativePrepared { sequence }) => PendingImage::NativePrepared {
            sequence: *sequence,
        },
        Some(Pending::NativeWaiting { sequence, wait_key }) => PendingImage::NativeWaiting {
            sequence: *sequence,
            wait_key: *wait_key,
        },
    };
    let mut targets = Vec::new();
    {
        for target in frame.targets() {
            targets.push(match target {
                AssignTarget::Register(slot) => TargetImage::Register(*slot),
                AssignTarget::Field { table, key } => TargetImage::Field {
                    table: enc_value(heap, *table)?,
                    key: enc_value(heap, *key)?,
                },
            });
        }
    }
    Ok(FrameImage {
        closure,
        pc: frame.pc,
        base: frame.base,
        limit: frame.limit,
        nresults: frame.nresults,
        vararg_len: frame.vararg_len,
        tail: frame.is_tail(),
        return_hook: frame.flags & 2 != 0,
        pending,
        wait_request: frame
            .wait_request()
            .map(|wait| {
                Ok::<_, SnapshotError>((
                    wait.operation.clone(),
                    wait.payload
                        .iter()
                        .map(|value| enc_value(heap, *value))
                        .collect::<Result<Vec<_>, _>>()?,
                ))
            })
            .transpose()?,
        targets,
        meta: frame
            .meta()
            .map(|meta| meta_image(heap, meta))
            .transpose()?,
        boundary: match frame.boundary() {
            None => None,
            Some(crate::heap::Boundary::Protect {
                func,
                advance_caller,
                handler,
            }) => Some(BoundaryImage::Protect {
                func: *func,
                advance_caller: *advance_caller,
                handler: handler.map(|value| enc_value(heap, value)).transpose()?,
            }),
            Some(crate::heap::Boundary::Handler {
                slot,
                protect,
                target,
                depth,
                fault,
            }) => Some(BoundaryImage::Handler {
                slot: *slot,
                protect: *protect,
                target: *target,
                depth: *depth,
                fault: fault.tag(),
            }),
            Some(crate::heap::Boundary::Builtin {
                func,
                passed,
                advance_caller,
                task,
            }) => Some(BoundaryImage::Builtin {
                func: *func,
                passed: *passed,
                advance_caller: *advance_caller,
                task: task.clone(),
            }),
            Some(crate::heap::Boundary::Native {
                func,
                passed,
                advance_caller,
                symbol,
                tag,
                kept,
                sequence,
                error,
                resuming,
            }) => Some(BoundaryImage::Native {
                func: *func,
                passed: *passed,
                advance_caller: *advance_caller,
                symbol: *symbol,
                tag: *tag,
                kept: *kept,
                sequence: *sequence,
                error: error
                    .map(|(class, value)| {
                        Ok::<_, SnapshotError>((class.tag(), enc_value(heap, value)?))
                    })
                    .transpose()?,
                resuming: *resuming,
            }),
            Some(crate::heap::Boundary::Hook {
                func,
                saved_top,
                target,
                instruction,
                after,
            }) => Some(BoundaryImage::Hook {
                func: *func,
                saved_top: *saved_top,
                target: *target,
                instruction: *instruction,
                after: *after,
            }),
            Some(crate::heap::Boundary::HookNative {
                func,
                passed,
                callee,
                advance_caller,
                phase,
                produced,
                result,
            }) => Some(BoundaryImage::HookNative {
                func: *func,
                passed: *passed,
                callee: enc_value(heap, *callee)?,
                advance_caller: *advance_caller,
                phase: *phase,
                produced: *produced,
                result: *result,
            }),
            Some(crate::heap::Boundary::Finalizer { func, saved_top }) => {
                Some(BoundaryImage::Finalizer {
                    func: *func,
                    saved_top: *saved_top,
                })
            }
        },
    })
}

fn enc_value(heap: &Heap, value: Value) -> Result<EncValue, SnapshotError> {
    Ok(match value {
        Value::Nil => EncValue::Nil,
        Value::Bool(bit) => EncValue::Bool(bit),
        Value::Integer(integer) => EncValue::Integer(integer),
        Value::Float(number) => EncValue::Float(number.to_bits()),
        Value::String(handle) => EncValue::String(
            heap.strings
                .get(handle)
                .ok_or(SnapshotError::DanglingReference)?
                .id
                .raw(),
        ),
        Value::Table(handle) => EncValue::Table(
            heap.tables
                .get(handle)
                .ok_or(SnapshotError::DanglingReference)?
                .id
                .raw(),
        ),
        Value::Closure(handle) => EncValue::Closure(
            heap.closures
                .get(handle)
                .ok_or(SnapshotError::DanglingReference)?
                .id
                .raw(),
        ),
        Value::Thread(handle) => EncValue::Thread(
            heap.threads
                .get(handle)
                .ok_or(SnapshotError::DanglingReference)?
                .id
                .raw(),
        ),
        Value::Native(index) => EncValue::Native(index),
        Value::NativeClosure(handle) => EncValue::NativeClosure(
            heap.native_closures
                .get(handle)
                .ok_or(SnapshotError::DanglingReference)?
                .id
                .raw(),
        ),
        Value::Userdata(handle) => EncValue::Userdata(
            heap.userdata
                .get(handle)
                .ok_or(SnapshotError::DanglingReference)?
                .id
                .raw(),
        ),
        Value::LightUserdata(domain, bits) => EncValue::Light(domain, bits),
    })
}

#[cfg(test)]
pub(crate) fn encode(image: &Image) -> Result<Vec<u8>, SnapshotError> {
    encode_within(image, MAX_SNAPSHOT_BYTES)
}

/// The snapshot of `image`, refused once it passes `limit` bytes: checked
/// after every object, so the output never grows past the limit by more
/// than one object.
pub(crate) fn encode_within(image: &Image, limit: u64) -> Result<Vec<u8>, SnapshotError> {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let mut out = Vec::new();
    write_image(&mut out, image, limit)?;
    let crc = crc32(&out);
    out.extend(crc.to_le_bytes());
    within(&out, limit)?;
    Ok(out)
}

fn within(out: &[u8], limit: usize) -> Result<(), SnapshotError> {
    if out.len() > limit {
        return Err(SnapshotError::LimitExceeded);
    }
    Ok(())
}

fn write_image(out: &mut Vec<u8>, image: &Image, limit: usize) -> Result<(), SnapshotError> {
    out.extend(MAGIC);
    out.extend(VERSION.to_le_bytes());
    out.extend(BYTECODE_REVISION.to_le_bytes());
    out.extend(TABLES_REVISION.to_le_bytes());
    out.extend(FUEL_REVISION.to_le_bytes());
    out.extend(GC_REVISION.to_le_bytes());
    out.extend(image.effect_domain.to_le_bytes());
    out.extend(image.next_object_id.to_le_bytes());
    out.extend(image.next_sequence.to_le_bytes());
    out.extend(image.fuel_consumed.to_le_bytes());
    match image.fuel_limit {
        Some(limit) => {
            out.push(1);
            out.extend(limit.to_le_bytes());
        }
        None => {
            out.push(0);
            out.extend(0u64.to_le_bytes());
        }
    }
    match image.last_completed_wait {
        Some(key) => {
            out.push(1);
            out.extend(key.to_le_bytes());
        }
        None => {
            out.push(0);
            out.extend(0u64.to_le_bytes());
        }
    }
    out.push(u8::from(image.host_call));
    out.push(u8::from(image.callback_failed));
    write_count(out, image.completed_waits.len(), MAX_OBJECTS)?;
    for key in &image.completed_waits {
        out.extend(key.to_le_bytes());
    }
    out.push(image.trap);
    out.extend(image.max_objects.to_le_bytes());
    out.extend(image.max_stack_slots.to_le_bytes());
    out.extend(image.max_string.to_le_bytes());
    out.push(u8::from(image.gc.auto));
    for field in [
        image.gc.debt,
        image.gc.threshold,
        image.gc.min_debt,
        image.gc.live,
        image.gc.collections,
        image.gc.quota,
    ] {
        out.extend(field.to_le_bytes());
    }
    for word in image.library.rng {
        out.extend(word.to_le_bytes());
    }
    out.extend(image.library.entropy.to_le_bytes());
    for id in image.type_metatables {
        out.extend(id.to_le_bytes());
    }
    out.extend(image.globals.to_le_bytes());
    out.extend(image.registry.to_le_bytes());
    out.extend(image.active.to_le_bytes());
    out.extend(image.entry.to_le_bytes());
    write_count(out, image.strings.len(), MAX_OBJECTS)?;
    for (id, bytes) in &image.strings {
        within(out, limit)?;
        out.extend(id.to_le_bytes());
        write_count(out, bytes.len(), MAX_STRING_BYTES)?;
        out.extend(bytes);
    }
    write_count(out, image.natives.len(), MAX_OBJECTS)?;
    for symbol in &image.natives {
        write_str(out, symbol)?;
    }
    write_count(
        out,
        image.reserved.len(),
        crate::heap::reserved_texts().count() as u32,
    )?;
    for id in &image.reserved {
        out.extend(id.to_le_bytes());
    }
    write_count(out, image.protos.len(), MAX_OBJECTS)?;
    for proto in &image.protos {
        within(out, limit)?;
        write_proto(out, proto)?;
    }
    write_count(out, image.tables.len(), MAX_OBJECTS)?;
    for table in &image.tables {
        within(out, limit)?;
        out.extend(table.id.to_le_bytes());
        out.extend(table.metatable.to_le_bytes());
        let live = table
            .slots
            .iter()
            .filter(|slot| matches!(slot.body, SlotBody::Live { .. }))
            .count();
        let dead = table.slots.len() - live;
        write_count(out, live, MAX_TABLE_ENTRIES)?;
        write_count(out, dead, MAX_TABLE_ENTRIES)?;
        if table.slots.len() > MAX_TABLE_ENTRIES as usize {
            return Err(SnapshotError::LimitExceeded);
        }
        for slot in &table.slots {
            out.extend(slot.ordinal.to_le_bytes());
            match &slot.body {
                SlotBody::Live { key, value } => {
                    out.push(1);
                    write_value(out, key);
                    write_value(out, value);
                }
                SlotBody::Dead(key) => {
                    out.push(2);
                    write_dead_key(out, key)?;
                }
            }
        }
    }
    write_count(out, image.upvalues.len(), MAX_OBJECTS)?;
    for upvalue in &image.upvalues {
        within(out, limit)?;
        out.extend(upvalue.id.to_le_bytes());
        match &upvalue.state {
            UpImageState::Closed(value) => {
                out.push(1);
                write_value(out, value);
            }
            UpImageState::Open { thread, slot } => {
                out.push(2);
                out.extend(thread.to_le_bytes());
                out.extend(slot.to_le_bytes());
            }
        }
    }
    write_count(out, image.closures.len(), MAX_OBJECTS)?;
    for closure in &image.closures {
        within(out, limit)?;
        out.extend(closure.id.to_le_bytes());
        out.extend(closure.proto.to_le_bytes());
        write_count(out, closure.upvalues.len(), MAX_OBJECTS)?;
        for id in &closure.upvalues {
            out.extend(id.to_le_bytes());
        }
    }
    write_count(out, image.threads.len(), MAX_OBJECTS)?;
    for thread in &image.threads {
        within(out, limit)?;
        write_thread(out, thread)?;
    }
    write_count(out, image.native_closures.len(), MAX_OBJECTS)?;
    for closure in &image.native_closures {
        within(out, limit)?;
        out.extend(closure.id.to_le_bytes());
        out.extend(closure.native.to_le_bytes());
        write_count(out, closure.values.len(), MAX_CLOSURE_VALUES)?;
        for value in &closure.values {
            write_value(out, value);
        }
        write_count(out, closure.state.len(), MAX_CLOSURE_VALUES)?;
        for word in &closure.state {
            out.extend(word.to_le_bytes());
        }
    }
    write_count(out, image.userdata.len(), MAX_OBJECTS)?;
    for userdata in &image.userdata {
        within(out, limit)?;
        out.extend(userdata.id.to_le_bytes());
        out.extend(userdata.metatable.to_le_bytes());
        write_count(out, userdata.user_values.len(), MAX_USER_VALUES)?;
        for value in &userdata.user_values {
            write_value(out, value);
        }
        match &userdata.payload {
            PayloadImage::File(file) => {
                out.push(4);
                file.encode(out);
            }
            PayloadImage::Bytes(bytes) => {
                out.push(1);
                write_count(out, bytes.len(), MAX_USERDATA_BYTES)?;
                out.extend(bytes);
            }
            PayloadImage::Host { symbol, bytes } => {
                out.push(2);
                write_str(out, symbol)?;
                write_count(out, bytes.len(), MAX_USERDATA_BYTES)?;
                out.extend(bytes);
            }
            PayloadImage::Rebind { symbol, key } => {
                out.push(3);
                write_str(out, symbol)?;
                write_count(out, key.len(), crate::MAX_REBIND_KEY as u32)?;
                out.extend(key);
            }
        }
        out.extend(userdata.charge.to_le_bytes());
    }
    let fin = &image.finalizers;
    for list in [&fin.registered, &fin.pending] {
        write_count(out, list.len(), MAX_OBJECTS)?;
        for id in list {
            out.extend(id.to_le_bytes());
        }
    }
    out.extend(fin.old_until.to_le_bytes());
    out.extend(fin.new_from.to_le_bytes());
    out.push(u8::from(fin.running));
    out.push(u8::from(fin.closing));
    out.push(fin.closed);
    out.extend(fin.close_closure.to_le_bytes());
    match fin.exit {
        None => out.push(0),
        Some(exit) => {
            out.push(exit.phase as u8);
            out.push(u8::from(exit.close));
            let (tag, code) = match exit.status {
                crate::ExitStatus::Success => (0, 0),
                crate::ExitStatus::Failure => (1, 0),
                crate::ExitStatus::Code(code) => (2, code),
            };
            out.push(tag);
            out.extend(code.to_le_bytes());
        }
    }
    write_collector(out, image)
}

/// Bound on each of the collector's lists in a snapshot.
/// Entries of one collector list: an object can be on a list once, or
/// twice counting dead slots not yet swept.
const MAX_GC_LIST: u32 = 2 * MAX_OBJECTS;

fn write_ids(out: &mut Vec<u8>, ids: &[u64]) -> Result<(), SnapshotError> {
    write_count(out, ids.len(), MAX_GC_LIST)?;
    for id in ids {
        out.extend(id.to_le_bytes());
    }
    Ok(())
}

fn write_collector(out: &mut Vec<u8>, image: &Image) -> Result<(), SnapshotError> {
    let gc = &image.gc;
    out.push(gc.pause);
    out.push(gc.stepmul);
    out.push(gc.stepsize);
    out.extend(gc.sched.to_le_bytes());
    out.extend(gc.owed.to_le_bytes());
    match gc.full {
        Some(target) => {
            out.push(1);
            out.extend(target.to_le_bytes());
        }
        None => {
            out.push(0);
            out.extend(0u64.to_le_bytes());
        }
    }
    out.extend(gc.prepaid.to_le_bytes());
    out.extend(gc.work.to_le_bytes());
    out.extend(gc.cycle_work.to_le_bytes());
    out.extend(gc.trace.to_le_bytes());
    out.push(u8::from(gc.generational));
    out.push(gc.minormul);
    out.push(gc.majormul);
    for word in [gc.major_base, gc.bad, gc.minors] {
        out.extend(word.to_le_bytes());
    }
    out.extend(gc.major_objects.to_le_bytes());
    let c = &image.collector;
    out.push(c.phase);
    out.push(c.atomic);
    out.push(c.white);
    write_ids(out, &c.gray)?;
    match c.scan {
        Some((id, pos, how, keys, values)) => {
            out.push(1);
            out.extend(id.to_le_bytes());
            out.extend(pos.to_le_bytes());
            out.push(how);
            out.push(u8::from(keys));
            out.push(u8::from(values));
        }
        None => out.push(0),
    }
    write_count(out, c.weak.len(), MAX_GC_LIST)?;
    for (id, keys, values) in &c.weak {
        out.extend(id.to_le_bytes());
        out.push(u8::from(*keys));
        out.push(u8::from(*values));
    }
    out.extend(c.late.to_le_bytes());
    write_ids(out, &c.ephemerons)?;
    write_count(out, c.waiting.len(), MAX_GC_LIST)?;
    for (key, values) in &c.waiting {
        out.extend(key.to_le_bytes());
        write_count(out, values.len(), MAX_GC_LIST)?;
        for value in values {
            write_value(out, value);
        }
    }
    for word in [c.cursor, c.inner] {
        out.extend(word.to_le_bytes());
    }
    for word in [
        c.marked_bytes,
        c.debt_base,
        c.marking_debt,
        c.work_base,
        c.sweep_left,
    ] {
        out.extend(word.to_le_bytes());
    }
    out.push(u8::from(c.reset));
    write_count(out, c.marks.len(), MAX_GC_LIST)?;
    for (id, mark, age) in &c.marks {
        out.extend(id.to_le_bytes());
        out.push(*mark);
        out.push(*age);
    }
    write_ids(out, &c.again)?;
    for flag in [c.generational, c.minor, c.to_old] {
        out.push(u8::from(flag));
    }
    out.push(c.decide);
    out.extend(c.unreleased.to_le_bytes());
    out.extend(c.promoted.to_le_bytes());
    write_count(out, c.ages.len(), MAX_GC_LIST)?;
    for (id, age) in &c.ages {
        out.extend(id.to_le_bytes());
        out.push(*age);
    }
    write_ids(out, &c.young)?;
    write_ids(out, &c.revisit)?;
    write_ids(out, &c.touched)
}

fn read_ids(input: &mut &[u8]) -> Result<Vec<u64>, SnapshotError> {
    let count = checked_count(input, MAX_GC_LIST)?;
    let mut ids = Vec::with_capacity((count as usize).min(4096));
    for _ in 0..count {
        ids.push(opcode::read_u64(input)?);
    }
    Ok(ids)
}

/// The collector section: the schedule into `gc`, the rest returned.
fn read_collector(
    input: &mut &[u8],
    gc: &mut crate::heap::GcState,
) -> Result<CollectorImage, SnapshotError> {
    gc.pause = opcode::read_u8(input)?;
    gc.stepmul = opcode::read_u8(input)?;
    gc.stepsize = opcode::read_u8(input)?;
    gc.sched = opcode::read_u64(input)? as i64;
    gc.owed = opcode::read_u64(input)?;
    let full = read_flag(input)?;
    let target = opcode::read_u64(input)?;
    gc.full = full.then_some(target);
    gc.prepaid = opcode::read_u32(input)?;
    gc.work = opcode::read_u64(input)?;
    gc.cycle_work = opcode::read_u64(input)?;
    gc.trace = opcode::read_u64(input)?;
    gc.generational = read_flag(input)?;
    gc.minormul = opcode::read_u8(input)?;
    gc.majormul = opcode::read_u8(input)?;
    gc.major_base = opcode::read_u64(input)?;
    gc.bad = opcode::read_u64(input)?;
    gc.minors = opcode::read_u64(input)?;
    gc.major_objects = opcode::read_u32(input)?;
    let phase = opcode::read_u8(input)?;
    let atomic = opcode::read_u8(input)?;
    let white = opcode::read_u8(input)?;
    let gray = read_ids(input)?;
    let scan = if read_flag(input)? {
        Some((
            opcode::read_u64(input)?,
            opcode::read_u32(input)?,
            opcode::read_u8(input)?,
            read_flag(input)?,
            read_flag(input)?,
        ))
    } else {
        None
    };
    let count = checked_count(input, MAX_GC_LIST)?;
    let mut weak = Vec::new();
    for _ in 0..count {
        weak.push((
            opcode::read_u64(input)?,
            read_flag(input)?,
            read_flag(input)?,
        ));
    }
    let late = opcode::read_u32(input)?;
    let ephemerons = read_ids(input)?;
    let count = checked_count(input, MAX_GC_LIST)?;
    let mut waiting = Vec::new();
    for _ in 0..count {
        let key = opcode::read_u64(input)?;
        let values = checked_count(input, MAX_GC_LIST)?;
        let mut list = Vec::new();
        for _ in 0..values {
            list.push(read_value(input)?);
        }
        waiting.push((key, list));
    }
    let cursor = opcode::read_u32(input)?;
    let inner = opcode::read_u32(input)?;
    let marked_bytes = opcode::read_u64(input)?;
    let debt_base = opcode::read_u64(input)?;
    let marking_debt = opcode::read_u64(input)?;
    let work_base = opcode::read_u64(input)?;
    let sweep_left = opcode::read_u64(input)?;
    let reset = read_flag(input)?;
    let count = checked_count(input, MAX_GC_LIST)?;
    let mut marks = Vec::new();
    for _ in 0..count {
        marks.push((
            opcode::read_u64(input)?,
            opcode::read_u8(input)?,
            opcode::read_u8(input)?,
        ));
    }
    let again = read_ids(input)?;
    let generational = read_flag(input)?;
    let minor = read_flag(input)?;
    let to_old = read_flag(input)?;
    let decide = opcode::read_u8(input)?;
    let unreleased = opcode::read_u64(input)?;
    let promoted = opcode::read_u64(input)?;
    let count = checked_count(input, MAX_GC_LIST)?;
    let mut ages = Vec::new();
    for _ in 0..count {
        ages.push((opcode::read_u64(input)?, opcode::read_u8(input)?));
    }
    let young = read_ids(input)?;
    let revisit = read_ids(input)?;
    let touched = read_ids(input)?;
    Ok(CollectorImage {
        phase,
        atomic,
        white,
        gray,
        scan,
        weak,
        late,
        ephemerons,
        waiting,
        cursor,
        inner,
        marked_bytes,
        debt_base,
        marking_debt,
        work_base,
        sweep_left,
        reset,
        marks,
        again,
        generational,
        minor,
        to_old,
        decide,
        unreleased,
        promoted,
        ages,
        young,
        revisit,
        touched,
    })
}

fn write_dead_key(out: &mut Vec<u8>, key: &DeadKey) -> Result<(), SnapshotError> {
    match key {
        DeadKey::Bool(bit) => {
            out.push(1);
            out.push(u8::from(*bit));
        }
        DeadKey::Integer(integer) => {
            out.push(2);
            out.extend(integer.to_le_bytes());
        }
        DeadKey::Float(bits) => {
            out.push(3);
            out.extend(bits.to_le_bytes());
        }
        DeadKey::Bytes(bytes) => {
            out.push(4);
            write_count(out, bytes.len(), MAX_STRING_BYTES)?;
            out.extend(bytes);
        }
        DeadKey::Object(id) => {
            out.push(5);
            out.extend(id.to_le_bytes());
        }
        DeadKey::Native(index) => {
            out.push(6);
            out.extend(index.to_le_bytes());
        }
        DeadKey::Light(domain, bits) => {
            out.push(7);
            out.push(*domain as u8);
            out.extend(bits.to_le_bytes());
        }
    }
    Ok(())
}

/// Writes a count the reader accepts: at most `max`, the bound `checked_count`
/// applies to the same field, so a state restore would refuse fails here.
fn write_count(out: &mut Vec<u8>, len: usize, max: u32) -> Result<(), SnapshotError> {
    let count = u32::try_from(len)
        .ok()
        .filter(|count| *count <= max)
        .ok_or(SnapshotError::LimitExceeded)?;
    out.extend(count.to_le_bytes());
    Ok(())
}

fn write_proto(out: &mut Vec<u8>, proto: &ProtoImage) -> Result<(), SnapshotError> {
    out.extend(proto.id.to_le_bytes());
    out.push(proto.max_reg);
    out.push(proto.params);
    out.push(u8::from(proto.vararg));
    write_count(out, proto.ops.len(), MAX_INSTRUCTIONS)?;
    for op in &proto.ops {
        op.encode(out);
    }
    write_count(out, proto.const_ids.len(), MAX_CONSTS)?;
    for id in &proto.const_ids {
        out.extend(id.to_le_bytes());
    }
    write_count(out, proto.captures.len(), MAX_UPVALUES)?;
    for capture in &proto.captures {
        opcode::encode_capture(*capture, out);
    }
    write_count(out, proto.children.len(), MAX_PROTOS)?;
    for id in &proto.children {
        out.extend(id.to_le_bytes());
    }
    out.extend(proto.source.to_le_bytes());
    match &proto.debug {
        None => out.push(0),
        Some(debug) => {
            out.push(1);
            debug.encode(out);
        }
    }
    Ok(())
}

/// A thread's stack, and every stack slot it names, its open upvalues'
/// slots aside, lie within `bound`, the snapshot's stack bound
/// (`Config::max_stack_slots`, ADR 0028). A running thread grows its stack
/// lazily, so a slot index can pass the stack's length; the bound keeps a
/// restored index from growing it past what the runtime allows.
/// [`Runtime::snapshot`] and restore apply the same test.
fn thread_slots_fit(thread: &ThreadImage, bound: u32) -> bool {
    let fits = |end: u64| end <= u64::from(bound);
    fits(thread.stack.len() as u64)
        && fits(thread.results.len() as u64)
        && fits(u64::from(thread.top))
        && thread.tbc.iter().all(|slot| fits(u64::from(*slot) + 1))
        && thread.frames.iter().all(|frame| {
            fits(u64::from(frame.base))
                && fits(u64::from(frame.limit))
                && frame.meta.as_ref().is_none_or(|meta| {
                    fits(u64::from(meta.slot) + 1 + u64::from(meta.nargs))
                        && match &meta.event {
                            EventImage::Close {
                                from,
                                next: NextImage::Return { src, produced },
                            } => {
                                fits(u64::from(*from))
                                    && fits(u64::from(*src) + u64::from(*produced))
                            }
                            EventImage::Close { from, .. } => fits(u64::from(*from)),
                            EventImage::Plain(_) => true,
                        }
                })
                && match frame.pending {
                    PendingImage::Assigning { src, nvalues, .. } => {
                        fits(u64::from(src) + u64::from(nvalues))
                    }
                    _ => true,
                }
                && frame.targets.iter().all(|target| match target {
                    TargetImage::Register(slot) => fits(u64::from(*slot) + 1),
                    TargetImage::Field { .. } => true,
                })
        })
}

/// The slot bounds restore enforces, checked when a snapshot is taken.
fn slots_fit(image: &Image, max_stack_slots: u32) -> bool {
    image
        .threads
        .iter()
        .all(|thread| thread_slots_fit(thread, max_stack_slots))
        && image.upvalues.iter().all(|upvalue| match upvalue.state {
            UpImageState::Open { slot, .. } => slot < max_stack_slots,
            UpImageState::Closed(_) => true,
        })
}

fn write_thread(out: &mut Vec<u8>, thread: &ThreadImage) -> Result<(), SnapshotError> {
    out.extend(thread.id.to_le_bytes());
    out.push(thread.status);
    out.extend(thread.resumed_by.to_le_bytes());
    out.extend(thread.top.to_le_bytes());
    write_count(out, thread.stack.len(), MAX_STACK_SLOTS)?;
    for value in &thread.stack {
        write_value(out, value);
    }
    write_count(out, thread.results.len(), MAX_STACK_SLOTS)?;
    for value in &thread.results {
        write_value(out, value);
    }
    write_count(out, thread.frames.len(), MAX_FRAMES)?;
    for frame in &thread.frames {
        out.extend(frame.closure.to_le_bytes());
        out.extend(frame.pc.to_le_bytes());
        out.extend(frame.base.to_le_bytes());
        out.extend(frame.limit.to_le_bytes());
        out.push(frame.nresults);
        out.extend(frame.vararg_len.to_le_bytes());
        out.push(u8::from(frame.tail));
        out.push(u8::from(frame.return_hook));
        match &frame.pending {
            PendingImage::None => out.push(0),
            PendingImage::Prepared {
                sequence,
                symbol,
                arg,
                dest,
            } => {
                out.push(1);
                out.extend(sequence.to_le_bytes());
                write_str(out, symbol)?;
                out.extend(arg.to_le_bytes());
                out.push(*dest);
            }
            PendingImage::Waiting {
                sequence,
                symbol,
                arg,
                dest,
                wait_key,
            } => {
                out.push(2);
                out.extend(sequence.to_le_bytes());
                write_str(out, symbol)?;
                out.extend(arg.to_le_bytes());
                out.push(*dest);
                out.extend(wait_key.to_le_bytes());
            }
            PendingImage::Resuming {
                child,
                dest,
                nresults,
            } => {
                out.push(3);
                out.extend(child.to_le_bytes());
                out.push(*dest);
                out.push(*nresults);
            }
            PendingImage::Assigning { src, nvalues, next } => {
                out.push(4);
                out.extend(src.to_le_bytes());
                out.extend(nvalues.to_le_bytes());
                out.extend(next.to_le_bytes());
            }
            PendingImage::NativePrepared { sequence } => {
                out.push(5);
                out.extend(sequence.to_le_bytes());
            }
            PendingImage::Capability {
                sequence,
                wait_key,
                completed,
            } => {
                out.push(8);
                out.extend(sequence.to_le_bytes());
                out.extend(wait_key.to_le_bytes());
                out.push(u8::from(*completed));
            }
            PendingImage::Deferred => out.push(7),
            PendingImage::NativeWaiting { sequence, wait_key } => {
                out.push(6);
                match sequence {
                    Some(sequence) => {
                        out.push(1);
                        out.extend(sequence.to_le_bytes());
                    }
                    None => {
                        out.push(0);
                        out.extend(0u64.to_le_bytes());
                    }
                }
                out.extend(wait_key.to_le_bytes());
            }
        }
        match &frame.wait_request {
            None => out.push(0),
            Some((operation, payload)) => {
                out.push(1);
                write_str(out, operation)?;
                write_count(out, payload.len(), MAX_STACK_SLOTS)?;
                for value in payload {
                    write_value(out, value);
                }
            }
        }
        write_count(out, frame.targets.len(), MAX_STACK_SLOTS)?;
        for target in &frame.targets {
            match target {
                TargetImage::Register(slot) => {
                    out.push(1);
                    out.extend(slot.to_le_bytes());
                }
                TargetImage::Field { table, key } => {
                    out.push(2);
                    write_value(out, table);
                    write_value(out, key);
                }
            }
        }
        write_meta(out, frame.meta.as_ref());
        match &frame.boundary {
            None => out.push(0),
            Some(BoundaryImage::Protect {
                func,
                advance_caller,
                handler,
            }) => {
                out.push(1);
                out.extend(func.to_le_bytes());
                out.push(u8::from(*advance_caller));
                match handler {
                    Some(value) => {
                        out.push(1);
                        write_value(out, value);
                    }
                    None => out.push(0),
                }
            }
            Some(BoundaryImage::Handler {
                slot,
                protect,
                target,
                depth,
                fault,
            }) => {
                out.push(2);
                out.extend(slot.to_le_bytes());
                out.extend(protect.to_le_bytes());
                out.extend(target.to_le_bytes());
                out.push(*depth);
                out.push(*fault);
            }
            Some(BoundaryImage::Builtin {
                func,
                passed,
                advance_caller,
                task,
            }) => {
                use crate::heap::Task;
                out.push(3);
                out.extend(func.to_le_bytes());
                out.extend(passed.to_le_bytes());
                out.push(u8::from(*advance_caller));
                match task {
                    Task::HostLoad(work) => {
                        out.push(8);
                        work.encode(out);
                    }
                    Task::DoFile => out.push(9),
                    Task::ToString => out.push(1),
                    Task::Print { next } => {
                        out.push(2);
                        out.extend(next.to_le_bytes());
                    }
                    Task::Pairs => out.push(3),
                    Task::Ipairs { index } => {
                        out.push(4);
                        out.extend(index.to_le_bytes());
                    }
                    Task::Load { source } => {
                        out.push(5);
                        write_count(out, source.len(), MAX_SOURCE_BYTES)?;
                        out.extend_from_slice(source);
                    }
                    Task::Lib(task) => {
                        out.push(6);
                        write_lib_task(out, task)?;
                    }
                    Task::Io(work) => {
                        out.push(10);
                        work.encode(out);
                    }
                    Task::Collect {
                        result,
                        wait,
                        ended,
                        left,
                    } => {
                        out.push(7);
                        out.push(*result);
                        out.push(u8::from(*wait));
                        out.push(u8::from(*ended));
                        out.extend(left.to_le_bytes());
                    }
                }
            }
            Some(BoundaryImage::Native {
                func,
                passed,
                advance_caller,
                symbol,
                tag,
                kept,
                sequence,
                error,
                resuming,
            }) => {
                out.push(5);
                out.extend(func.to_le_bytes());
                out.extend(passed.to_le_bytes());
                out.push(u8::from(*advance_caller));
                out.extend(symbol.to_le_bytes());
                out.extend(tag.to_le_bytes());
                out.extend(kept.to_le_bytes());
                out.extend(sequence.unwrap_or(0).to_le_bytes());
                out.push(u8::from(*resuming));
                match error {
                    None => out.push(0),
                    Some((class, value)) => {
                        out.push(1);
                        out.push(*class);
                        write_value(out, value);
                    }
                }
            }
            Some(BoundaryImage::Hook {
                func,
                saved_top,
                target,
                instruction,
                after,
            }) => {
                out.push(6);
                out.extend(func.to_le_bytes());
                out.extend(saved_top.to_le_bytes());
                out.extend(target.to_le_bytes());
                write_hook_instruction(out, *instruction)?;
                write_hook_after(out, *after);
            }
            Some(BoundaryImage::HookNative {
                func,
                passed,
                callee,
                advance_caller,
                phase,
                produced,
                result,
            }) => {
                out.push(7);
                out.extend(func.to_le_bytes());
                out.extend(passed.to_le_bytes());
                write_value(out, callee);
                out.push(u8::from(*advance_caller));
                out.push(*phase);
                out.extend(produced.to_le_bytes());
                out.extend(result.to_le_bytes());
            }
            Some(BoundaryImage::Finalizer { func, saved_top }) => {
                out.push(4);
                out.extend(func.to_le_bytes());
                out.extend(saved_top.to_le_bytes());
            }
        }
    }
    match &thread.error {
        Some((fault, value)) => {
            out.push(1);
            out.push(*fault);
            write_value(out, value);
        }
        None => out.push(0),
    }
    match &thread.unwind {
        Some(unwind) => {
            out.push(1);
            write_unwind(out, unwind);
        }
        None => out.push(0),
    }
    out.push(u8::from(thread.coroutine));
    out.push(u8::from(thread.closing));
    write_count(out, thread.tbc.len(), MAX_STACK_SLOTS)?;
    for slot in &thread.tbc {
        out.extend(slot.to_le_bytes());
    }
    out.extend(thread.charged_slots.to_le_bytes());
    out.extend(thread.charged_held.to_le_bytes());
    write_hook(out, thread.hook.as_ref())?;
    Ok(())
}

fn write_unwind(out: &mut Vec<u8>, unwind: &UnwindImage) {
    match &unwind.error {
        Some((fault, value)) => {
            out.push(1);
            out.push(*fault);
            write_value(out, value);
        }
        None => out.push(0),
    }
    match unwind.phase {
        crate::heap::UnwindPhase::Raised => out.push(0),
        crate::heap::UnwindPhase::Popping { target } => {
            out.push(1);
            match target {
                Some(index) => {
                    out.push(1);
                    out.extend(index.to_le_bytes());
                }
                None => {
                    out.push(0);
                    out.extend(0u32.to_le_bytes());
                }
            }
        }
    }
}

fn read_unwind(input: &mut &[u8]) -> Result<UnwindImage, SnapshotError> {
    let error = if read_flag(input)? {
        Some((read_fault(input)?, read_value(input)?))
    } else {
        None
    };
    let phase = match opcode::read_u8(input)? {
        0 => crate::heap::UnwindPhase::Raised,
        1 => {
            let flag = read_flag(input)?;
            let index = opcode::read_u32(input)?;
            crate::heap::UnwindPhase::Popping {
                target: if flag {
                    Some(index)
                } else if index == 0 {
                    None
                } else {
                    return Err(SnapshotError::InvalidTag);
                },
            }
        }
        _ => return Err(SnapshotError::InvalidTag),
    };
    Ok(UnwindImage { error, phase })
}

fn write_meta(out: &mut Vec<u8>, meta: Option<&MetaImage>) {
    use crate::heap::{MetaEvent, MetaPhase};
    let Some(meta) = meta else {
        out.push(0);
        return;
    };
    match &meta.event {
        EventImage::Plain(event) => {
            let (tag, dst) = match event {
                MetaEvent::Store { dst } => (1, *dst),
                MetaEvent::NewIndex => (2, 0),
                MetaEvent::NewIndexAssign => (3, 0),
                MetaEvent::Truth { dst, negate: false } => (4, *dst),
                MetaEvent::Truth { dst, negate: true } => (5, *dst),
                // Closes are `EventImage::Close`.
                MetaEvent::Close => (0, 0),
            };
            out.push(tag);
            out.push(dst);
        }
        EventImage::Close { from, next } => {
            out.push(6);
            out.push(0);
            out.extend(from.to_le_bytes());
            match next {
                NextImage::Advance => out.push(1),
                NextImage::Return { src, produced } => {
                    out.push(2);
                    out.extend(src.to_le_bytes());
                    out.extend(produced.to_le_bytes());
                }
                NextImage::Unwind(unwind) => {
                    out.push(3);
                    write_unwind(out, unwind);
                }
            }
        }
    }
    out.extend(meta.slot.to_le_bytes());
    out.push(meta.nargs);
    match meta.phase {
        MetaPhase::Running => out.push(0),
        MetaPhase::NativePrepared { sequence } => {
            out.push(1);
            out.extend(sequence.to_le_bytes());
        }
        MetaPhase::NativeWaiting { sequence, wait_key } => {
            out.push(2);
            out.push(u8::from(sequence.is_some()));
            out.extend(sequence.unwrap_or(0).to_le_bytes());
            out.extend(wait_key.to_le_bytes());
        }
        MetaPhase::Idle => out.push(3),
    }
}

fn read_meta(input: &mut &[u8]) -> Result<Option<MetaImage>, SnapshotError> {
    use crate::heap::{MetaEvent, MetaPhase};
    let tag = opcode::read_u8(input)?;
    if tag == 0 {
        return Ok(None);
    }
    let dst = opcode::read_u8(input)?;
    let event = match tag {
        1 => EventImage::Plain(MetaEvent::Store { dst }),
        2 if dst == 0 => EventImage::Plain(MetaEvent::NewIndex),
        3 if dst == 0 => EventImage::Plain(MetaEvent::NewIndexAssign),
        4 => EventImage::Plain(MetaEvent::Truth { dst, negate: false }),
        5 => EventImage::Plain(MetaEvent::Truth { dst, negate: true }),
        6 if dst == 0 => {
            let from = opcode::read_u32(input)?;
            let next = match opcode::read_u8(input)? {
                1 => NextImage::Advance,
                2 => NextImage::Return {
                    src: opcode::read_u32(input)?,
                    produced: opcode::read_u32(input)?,
                },
                3 => NextImage::Unwind(read_unwind(input)?),
                _ => return Err(SnapshotError::InvalidTag),
            };
            EventImage::Close { from, next }
        }
        _ => return Err(SnapshotError::InvalidTag),
    };
    let slot = opcode::read_u32(input)?;
    let nargs = opcode::read_u8(input)?;
    let phase = match opcode::read_u8(input)? {
        0 => MetaPhase::Running,
        1 => MetaPhase::NativePrepared {
            sequence: opcode::read_u64(input)?,
        },
        2 => {
            let flag = opcode::read_u8(input)?;
            let raw = opcode::read_u64(input)?;
            let sequence = match flag {
                0 if raw == 0 => None,
                1 => Some(raw),
                _ => return Err(SnapshotError::InvalidTag),
            };
            MetaPhase::NativeWaiting {
                sequence,
                wait_key: opcode::read_u64(input)?,
            }
        }
        3 => MetaPhase::Idle,
        _ => return Err(SnapshotError::InvalidTag),
    };
    Ok(Some(MetaImage {
        event,
        slot,
        nargs,
        phase,
    }))
}

fn write_str(out: &mut Vec<u8>, text: &str) -> Result<(), SnapshotError> {
    write_count(out, text.len(), MAX_STRING_BYTES)?;
    out.extend(text.as_bytes());
    Ok(())
}

fn write_value(out: &mut Vec<u8>, value: &EncValue) {
    match value {
        EncValue::Nil => out.push(0),
        EncValue::Bool(bit) => {
            out.push(1);
            out.push(u8::from(*bit));
        }
        EncValue::Integer(integer) => {
            out.push(2);
            out.extend(integer.to_le_bytes());
        }
        EncValue::Float(bits) => {
            out.push(3);
            out.extend(bits.to_le_bytes());
        }
        EncValue::String(id) => {
            out.push(4);
            out.extend(id.to_le_bytes());
        }
        EncValue::Table(id) => {
            out.push(5);
            out.extend(id.to_le_bytes());
        }
        EncValue::Closure(id) => {
            out.push(6);
            out.extend(id.to_le_bytes());
        }
        EncValue::Thread(id) => {
            out.push(7);
            out.extend(id.to_le_bytes());
        }
        EncValue::Native(index) => {
            out.push(8);
            out.extend(index.to_le_bytes());
        }
        EncValue::NativeClosure(id) => {
            out.push(9);
            out.extend(id.to_le_bytes());
        }
        EncValue::Userdata(id) => {
            out.push(10);
            out.extend(id.to_le_bytes());
        }
        EncValue::Light(domain, bits) => {
            out.push(11);
            out.push(*domain as u8);
            out.extend(bits.to_le_bytes());
        }
    }
}

/// The image of `bytes`, decoded within `limits`: no longer than its
/// snapshot bound, no more objects than its object limit, and no more
/// decoded structure than the budget (see [`budget`]).
pub(crate) fn decode_within(bytes: &[u8], limits: &Limits) -> Result<Image, SnapshotError> {
    if bytes.len() as u64 > limits.max_snapshot_bytes {
        return Err(SnapshotError::LimitExceeded);
    }
    if bytes.len() < 4 {
        return Err(SnapshotError::Truncated);
    }
    if &bytes[..4] != MAGIC {
        return Err(SnapshotError::BadMagic);
    }
    if bytes.len() < 6 {
        return Err(SnapshotError::Truncated);
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if version != VERSION {
        return Err(SnapshotError::BadVersion);
    }
    if bytes.len() < 10 {
        return Err(SnapshotError::Truncated);
    }
    let (body, crc_bytes) = bytes.split_at(bytes.len() - 4);
    let crc = u32::from_le_bytes(crc_bytes.try_into().map_err(|_| SnapshotError::Truncated)?);
    if crc32(body) != crc {
        return Err(SnapshotError::Checksum);
    }
    // Magic and schema were checked above; the body resumes after them.
    let mut cursor = &body[6..];
    parse_after_version(&mut cursor, limits)
}

fn parse_after_version(input: &mut &[u8], limits: &Limits) -> Result<Image, SnapshotError> {
    let length = input.len() as u64;
    let bytecode = opcode::read_u16(input)?;
    let tables = opcode::read_u16(input)?;
    let fuel = opcode::read_u16(input)?;
    let gc_revision = opcode::read_u16(input)?;
    if bytecode != BYTECODE_REVISION
        || tables != TABLES_REVISION
        || fuel != FUEL_REVISION
        || gc_revision != GC_REVISION
    {
        return Err(SnapshotError::BadVersion);
    }
    let effect_domain = opcode::read_u64(input)?;
    let next_object_id = opcode::read_u64(input)?;
    let next_sequence = opcode::read_u64(input)?;
    let fuel_consumed = opcode::read_u64(input)?;
    let fuel_flag = opcode::read_u8(input)?;
    let fuel_raw = opcode::read_u64(input)?;
    let fuel_limit = match fuel_flag {
        0 => None,
        1 => Some(fuel_raw),
        _ => return Err(SnapshotError::InvalidTag),
    };
    let wait_flag = opcode::read_u8(input)?;
    let wait_raw = opcode::read_u64(input)?;
    let last_completed_wait = match wait_flag {
        0 => None,
        1 => Some(wait_raw),
        _ => return Err(SnapshotError::InvalidTag),
    };
    let host_call = read_flag(input)?;
    let callback_failed = read_flag(input)?;
    let n_completed = checked_count(input, MAX_OBJECTS)?;
    let mut completed_waits = Vec::with_capacity(prealloc(n_completed));
    for _ in 0..n_completed {
        completed_waits.push(opcode::read_u64(input)?);
    }
    if completed_waits.windows(2).any(|pair| pair[0] >= pair[1])
        || completed_waits
            .iter()
            .any(|key| key & (1 << 63) == 0 || key & !(1 << 63) >= next_sequence)
    {
        return Err(SnapshotError::InvalidStructure);
    }
    let trap = opcode::read_u8(input)?;
    let _ = trap_from(trap)?;
    let max_objects = opcode::read_u32(input)?;
    if max_objects == 0 || max_objects > MAX_OBJECTS {
        return Err(SnapshotError::LimitExceeded);
    }
    let max_stack_slots = opcode::read_u32(input)?;
    if !crate::runtime::STACK_SLOTS_RANGE.contains(&max_stack_slots) {
        return Err(SnapshotError::LimitExceeded);
    }
    let max_string = opcode::read_u32(input)?;
    if !crate::heap::STRING_BYTES_RANGE.contains(&(max_string as usize)) {
        return Err(SnapshotError::LimitExceeded);
    }
    let auto = match opcode::read_u8(input)? {
        0 => false,
        1 => true,
        _ => return Err(SnapshotError::InvalidTag),
    };
    let mut gc = crate::heap::GcState {
        auto,
        debt: opcode::read_u64(input)?,
        threshold: opcode::read_u64(input)?,
        min_debt: opcode::read_u64(input)?,
        live: opcode::read_u64(input)?,
        collections: opcode::read_u64(input)?,
        quota: opcode::read_u64(input)?,
        pause: crate::heap::GC_PAUSE,
        stepmul: crate::heap::GC_STEPMUL,
        stepsize: crate::heap::GC_STEPSIZE,
        sched: 0,
        owed: 0,
        full: None,
        prepaid: 0,
        work: 0,
        cycle_work: 0,
        trace: 0,
        generational: false,
        minormul: crate::heap::GC_MINORMUL,
        majormul: crate::heap::GC_MAJORMUL,
        major_base: 0,
        major_objects: 0,
        bad: 0,
        minors: 0,
        used: 0,
    };
    if gc.threshold == 0 || gc.min_debt == 0 || gc.quota == 0 {
        return Err(SnapshotError::InvalidStructure);
    }
    // Everything past the header is held to the budget, and the objects
    // to the object limit the runtime will have.
    let quota = gc.quota.min(limits.max_logical_heap);
    let _budget = budget::Scope::set(quota.saturating_mul(4).saturating_add(length));
    let object_limit = u64::from(max_objects.min(limits.max_objects))
        + u64::from(crate::runtime::finalize_close_objects());
    let mut objects = 0u64;
    let mut count_objects = |n: usize| {
        objects += n as u64;
        if objects > object_limit {
            return Err(SnapshotError::LimitExceeded);
        }
        Ok(())
    };
    let mut rng = [0u64; 4];
    for word in &mut rng {
        *word = opcode::read_u64(input)?;
    }
    // xoshiro256** never reaches the all-zero state, and seeding avoids it.
    if rng == [0; 4] {
        return Err(SnapshotError::InvalidStructure);
    }
    let library = crate::library::LibraryState {
        rng,
        entropy: opcode::read_u64(input)?,
    };
    let mut type_metatables = [0; crate::heap::BASIC_TYPES];
    for id in &mut type_metatables {
        *id = opcode::read_u64(input)?;
    }
    let globals = opcode::read_u64(input)?;
    let registry = opcode::read_u64(input)?;
    let active = opcode::read_u64(input)?;
    let entry = opcode::read_u64(input)?;
    let mut seen = HashSet::new();
    let strings = read_strings(input, &mut seen)?;
    count_objects(strings.len())?;
    let natives = read_natives(input)?;
    let expected = crate::heap::reserved_texts().count();
    let n_faults = checked_count(input, expected as u32)?;
    if n_faults as usize != expected {
        return Err(SnapshotError::InvalidStructure);
    }
    let mut reserved = Vec::with_capacity(prealloc(n_faults));
    for _ in 0..n_faults {
        reserved.push(opcode::read_u64(input)?);
    }
    let protos = {
        let by_id: std::collections::HashMap<u64, &[u8]> = strings
            .iter()
            .map(|(id, bytes)| (*id, bytes.as_slice()))
            .collect();
        read_protos(input, &mut seen, &by_id)?
    };
    count_objects(protos.len())?;
    let tables = read_tables(input, &mut seen)?;
    count_objects(tables.len())?;
    let upvalues = read_upvalues(input, &mut seen)?;
    count_objects(upvalues.len())?;
    let closures = read_closures(input, &mut seen)?;
    count_objects(closures.len())?;
    let threads = read_threads(input, &mut seen)?;
    count_objects(threads.len())?;
    let native_closures = read_native_closures(input, &mut seen)?;
    count_objects(native_closures.len())?;
    let userdata = read_userdata(input, &mut seen)?;
    count_objects(userdata.len())?;
    let mut lists = [Vec::new(), Vec::new()];
    for list in &mut lists {
        let count = checked_count(input, MAX_OBJECTS)?;
        for _ in 0..count {
            list.push(opcode::read_u64(input)?);
        }
    }
    let [registered, pending] = lists;
    let finalizers = FinalizersImage {
        registered,
        pending,
        old_until: opcode::read_u32(input)?,
        new_from: opcode::read_u32(input)?,
        running: read_flag(input)?,
        closing: read_flag(input)?,
        closed: opcode::read_u8(input)?,
        close_closure: opcode::read_u64(input)?,
        exit: match opcode::read_u8(input)? {
            0 => None,
            phase => {
                use crate::heap::{ExitPhase, ExitState};
                let phase = match phase {
                    1 => ExitPhase::Scopes,
                    2 => ExitPhase::Finalizers,
                    3 => ExitPhase::Terminal,
                    _ => return Err(SnapshotError::InvalidTag),
                };
                let close = read_flag(input)?;
                let tag = opcode::read_u8(input)?;
                let code = opcode::read_u32(input)? as i32;
                let status = match (tag, code) {
                    (0, 0) => crate::ExitStatus::Success,
                    (1, 0) => crate::ExitStatus::Failure,
                    (2, code) => crate::ExitStatus::Code(code),
                    _ => return Err(SnapshotError::InvalidTag),
                };
                Some(ExitState {
                    status,
                    close,
                    phase,
                })
            }
        },
    };
    let collector = read_collector(input, &mut gc)?;
    if !input.is_empty() {
        return Err(SnapshotError::InvalidStructure);
    }
    let image = Image {
        effect_domain,
        next_object_id,
        next_sequence,
        fuel_consumed,
        fuel_limit,
        last_completed_wait,
        completed_waits,
        host_call,
        callback_failed,
        trap,
        max_objects,
        max_stack_slots,
        max_string,
        gc,
        library,
        type_metatables,
        globals,
        registry,
        active,
        entry,
        strings,
        natives,
        reserved,
        protos,
        tables,
        upvalues,
        closures,
        threads,
        native_closures,
        userdata,
        finalizers,
        collector,
        index: ImageIndex::default(),
    };
    validate_graph(&image, &seen)?;
    validate_code(&image)?;
    Ok(image)
}

fn code_view<'a>(
    proto: &'a ProtoImage,
    protos: &std::collections::HashMap<u64, &'a ProtoImage>,
) -> Result<check::CodeView<'a>, SnapshotError> {
    let children = proto
        .children
        .iter()
        .map(|id| {
            protos
                .get(id)
                .map(|child| child.captures.as_slice())
                .ok_or(SnapshotError::DanglingReference)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(check::CodeView {
        ops: &proto.ops,
        consts: proto.byte_consts.len(),
        captures: &proto.captures,
        children,
        max_reg: proto.max_reg,
        params: proto.params,
        vararg: proto.vararg,
    })
}

/// Run every restored prototype through the compiler's bytecode check, and
/// check each child's captures against every parent that lists it. Runs
/// after `validate_graph`, so child ids are known to be prototypes.
fn validate_code(image: &Image) -> Result<(), SnapshotError> {
    let protos: std::collections::HashMap<u64, &ProtoImage> =
        image.protos.iter().map(|proto| (proto.id, proto)).collect();
    for proto in &image.protos {
        let parent = code_view(proto, &protos)?;
        check::check_code(&parent).map_err(|_| SnapshotError::InvalidBytecode)?;
        for id in &proto.children {
            let child = protos.get(id).ok_or(SnapshotError::DanglingReference)?;
            check::check_captures(&child.captures, Some(&parent))
                .map_err(|_| SnapshotError::InvalidBytecode)?;
        }
    }
    for closure in &image.closures {
        let proto = protos
            .get(&closure.proto)
            .ok_or(SnapshotError::DanglingReference)?;
        if closure.upvalues.len() != proto.captures.len() {
            return Err(SnapshotError::InvalidBytecode);
        }
    }
    Ok(())
}

fn note_id(seen: &mut HashSet<u64>, id: u64) -> Result<(), SnapshotError> {
    if id == 0 || !seen.insert(id) {
        return Err(if id == 0 {
            SnapshotError::InvalidStructure
        } else {
            SnapshotError::DuplicateObjectId
        });
    }
    Ok(())
}

/// A count of items, each encoded in at least one byte: refused past
/// `max`, past what is left of the input (a count no input backs is a
/// lie, found before anything is reserved for it), and past the decode
/// budget, which it is charged [`ITEM_BYTES`] an item.
fn checked_count(input: &mut &[u8], max: u32) -> Result<u32, SnapshotError> {
    let count = opcode::read_u32(input)?;
    if count > max {
        return Err(SnapshotError::LimitExceeded);
    }
    if count as usize > input.len() {
        return Err(SnapshotError::Truncated);
    }
    budget::spend(u64::from(count).saturating_mul(ITEM_BYTES))?;
    Ok(count)
}

/// A count of items each encoded in at least `least` bytes, refused when
/// the input left cannot hold them.
fn checked_proto_count(input: &mut &[u8], max: u32, least: usize) -> Result<u32, SnapshotError> {
    let count = checked_count(input, max)?;
    let bytes = (count as usize)
        .checked_mul(least)
        .ok_or(SnapshotError::LimitExceeded)?;
    if bytes > input.len() {
        return Err(SnapshotError::Truncated);
    }
    Ok(count)
}

pub(crate) use budget::Scope as BudgetScope;

/// Charge `count` decoded items to the decode budget, for readers outside
/// this module (debug information).
pub(crate) fn spend_items(count: u64) -> Result<(), SnapshotError> {
    budget::spend(count.saturating_mul(ITEM_BYTES))
}

/// A byte length: refused past `max` and past what is left of the input,
/// and charged to the decode budget a byte a byte.
fn checked_len(input: &mut &[u8], max: u32) -> Result<u32, SnapshotError> {
    let len = opcode::read_u32(input)?;
    if len > max {
        return Err(SnapshotError::LimitExceeded);
    }
    if len as usize > input.len() {
        return Err(SnapshotError::Truncated);
    }
    budget::spend(u64::from(len))?;
    Ok(len)
}

/// What the decoder charges its budget for each counted item: about the
/// smallest decoded item (a value, an id, an instruction), so the budget
/// bounds what the counts make, however small their encoding.
const ITEM_BYTES: u64 = 16;

/// At most this many items are reserved for a count before they are
/// read; past it a list grows as its items decode.
const PREALLOC: usize = 1024;

fn prealloc(count: u32) -> usize {
    (count as usize).min(PREALLOC)
}

/// The decoder's allocation budget (ADR 0052): bytes of decoded
/// structure a snapshot may make before it is validated. Set for one
/// decode on this thread, to four times the logical heap the snapshot
/// may restore into plus its length; outside a decode it is unbounded.
mod budget {
    use std::cell::Cell;

    use crate::id::SnapshotError;

    thread_local! {
        static LEFT: Cell<u64> = const { Cell::new(u64::MAX) };
    }

    pub(super) fn spend(bytes: u64) -> Result<(), SnapshotError> {
        LEFT.with(|left| {
            let rest = left
                .get()
                .checked_sub(bytes)
                .ok_or(SnapshotError::LimitExceeded)?;
            left.set(rest);
            Ok(())
        })
    }

    /// The budget for one decode, until the scope is dropped.
    pub(crate) struct Scope(u64);

    impl Scope {
        pub(crate) fn set(bytes: u64) -> Self {
            Self(LEFT.with(|left| left.replace(bytes)))
        }
    }

    impl Drop for Scope {
        fn drop(&mut self) {
            LEFT.with(|left| left.set(self.0));
        }
    }
}

/// The native symbol section: unique, bounded symbols.
fn read_natives(input: &mut &[u8]) -> Result<Vec<String>, SnapshotError> {
    let count = checked_count(input, MAX_OBJECTS)?;
    let mut natives = Vec::with_capacity(prealloc(count));
    let mut unique = HashSet::new();
    for _ in 0..count {
        let symbol = read_string(input)?;
        if !unique.insert(symbol.clone()) {
            return Err(SnapshotError::InvalidStructure);
        }
        natives.push(symbol);
    }
    Ok(natives)
}

fn read_strings(
    input: &mut &[u8],
    seen: &mut HashSet<u64>,
) -> Result<Vec<(u64, Vec<u8>)>, SnapshotError> {
    let count = checked_count(input, MAX_OBJECTS)?;
    let mut out = Vec::with_capacity(prealloc(count));
    for _ in 0..count {
        let id = opcode::read_u64(input)?;
        note_id(seen, id)?;
        let len = checked_len(input, MAX_STRING_BYTES)?;
        if input.len() < len as usize {
            return Err(SnapshotError::Truncated);
        }
        let bytes = input[..len as usize].to_vec();
        *input = &input[len as usize..];
        out.push((id, bytes));
    }
    Ok(out)
}

fn read_protos(
    input: &mut &[u8],
    seen: &mut HashSet<u64>,
    strings: &std::collections::HashMap<u64, &[u8]>,
) -> Result<Vec<ProtoImage>, SnapshotError> {
    let count = checked_proto_count(input, MAX_OBJECTS, 36)?;
    let mut out = Vec::with_capacity(prealloc(count));
    // Each constant is its own string object (ADR 0020). Refusing a second
    // reference also bounds the bytes copied below by the string section.
    let mut constants = HashSet::new();
    for _ in 0..count {
        let id = opcode::read_u64(input)?;
        note_id(seen, id)?;
        let max_reg = opcode::read_u8(input)?;
        let params = opcode::read_u8(input)?;
        let vararg = match opcode::read_u8(input)? {
            0 => false,
            1 => true,
            _ => return Err(SnapshotError::InvalidTag),
        };
        let n_ops = checked_proto_count(input, MAX_INSTRUCTIONS, 1)?;
        let mut ops = Vec::with_capacity(prealloc(n_ops));
        for _ in 0..n_ops {
            ops.push(Op::decode(input)?);
        }
        let n_consts = checked_proto_count(input, MAX_CONSTS, 8)?;
        let mut const_ids = Vec::with_capacity(prealloc(n_consts));
        let mut byte_consts = Vec::with_capacity(prealloc(n_consts));
        for _ in 0..n_consts {
            let id = opcode::read_u64(input)?;
            let bytes = strings.get(&id).ok_or(SnapshotError::DanglingReference)?;
            if !constants.insert(id) {
                return Err(SnapshotError::InvalidStructure);
            }
            const_ids.push(id);
            byte_consts.push(bytes.to_vec());
        }
        let n_caps = checked_proto_count(input, MAX_UPVALUES, 2)?;
        let mut captures = Vec::with_capacity(prealloc(n_caps));
        for _ in 0..n_caps {
            captures.push(opcode::decode_capture(input)?);
        }
        let n_children = checked_proto_count(input, MAX_PROTOS, 8)?;
        let mut children = Vec::with_capacity(prealloc(n_children));
        for _ in 0..n_children {
            children.push(opcode::read_u64(input)?);
        }
        let source = opcode::read_u64(input)?;
        if source != 0 && !strings.contains_key(&source) {
            return Err(SnapshotError::DanglingReference);
        }
        let debug = match opcode::read_u8(input)? {
            0 => None,
            1 => Some(Box::new(crate::debuginfo::DebugInfo::decode(
                input,
                ops.len(),
                captures.len(),
                max_reg,
            )?)),
            _ => return Err(SnapshotError::InvalidTag),
        };
        out.push(ProtoImage {
            id,
            max_reg,
            params,
            vararg,
            ops,
            const_ids,
            byte_consts,
            captures,
            children,
            source,
            debug,
        });
    }
    Ok(out)
}

fn read_tables(
    input: &mut &[u8],
    seen: &mut HashSet<u64>,
) -> Result<Vec<TableImage>, SnapshotError> {
    let count = checked_count(input, MAX_OBJECTS)?;
    let mut out = Vec::with_capacity(prealloc(count));
    for _ in 0..count {
        let id = opcode::read_u64(input)?;
        note_id(seen, id)?;
        let metatable = opcode::read_u64(input)?;
        let live = checked_count(input, MAX_TABLE_ENTRIES)?;
        let dead = checked_count(input, MAX_TABLE_ENTRIES)?;
        let total = live
            .checked_add(dead)
            .filter(|total| *total <= MAX_TABLE_ENTRIES)
            .ok_or(SnapshotError::LimitExceeded)?;
        let mut slots = Vec::with_capacity(prealloc(total));
        let mut seen_live = 0u32;
        let mut seen_dead = 0u32;
        for index in 0..total {
            let ordinal = opcode::read_u32(input)?;
            if ordinal != index {
                return Err(SnapshotError::InvalidStructure);
            }
            let body = match opcode::read_u8(input)? {
                1 => {
                    seen_live += 1;
                    SlotBody::Live {
                        key: read_value(input)?,
                        value: read_value(input)?,
                    }
                }
                2 => {
                    seen_dead += 1;
                    SlotBody::Dead(read_dead_key(input)?)
                }
                _ => return Err(SnapshotError::InvalidTag),
            };
            slots.push(SlotImage { ordinal, body });
        }
        if seen_live != live || seen_dead != dead {
            return Err(SnapshotError::InvalidStructure);
        }
        out.push(TableImage {
            id,
            metatable,
            slots,
        });
    }
    Ok(out)
}

fn read_upvalues(
    input: &mut &[u8],
    seen: &mut HashSet<u64>,
) -> Result<Vec<UpImage>, SnapshotError> {
    let count = checked_count(input, MAX_OBJECTS)?;
    let mut out = Vec::with_capacity(prealloc(count));
    for _ in 0..count {
        let id = opcode::read_u64(input)?;
        note_id(seen, id)?;
        let state = match opcode::read_u8(input)? {
            1 => UpImageState::Closed(read_value(input)?),
            2 => UpImageState::Open {
                thread: opcode::read_u64(input)?,
                slot: opcode::read_u32(input)?,
            },
            _ => return Err(SnapshotError::InvalidTag),
        };
        out.push(UpImage { id, state });
    }
    Ok(out)
}

fn read_closures(
    input: &mut &[u8],
    seen: &mut HashSet<u64>,
) -> Result<Vec<ClosureImage>, SnapshotError> {
    let count = checked_count(input, MAX_OBJECTS)?;
    let mut out = Vec::with_capacity(prealloc(count));
    for _ in 0..count {
        let id = opcode::read_u64(input)?;
        note_id(seen, id)?;
        let proto = opcode::read_u64(input)?;
        let n = checked_count(input, MAX_OBJECTS)?;
        let mut upvalues = Vec::with_capacity(prealloc(n));
        for _ in 0..n {
            upvalues.push(opcode::read_u64(input)?);
        }
        out.push(ClosureImage {
            id,
            proto,
            upvalues,
        });
    }
    Ok(out)
}

fn read_native_closures(
    input: &mut &[u8],
    seen: &mut HashSet<u64>,
) -> Result<Vec<NativeClosureImage>, SnapshotError> {
    let count = checked_count(input, MAX_OBJECTS)?;
    let mut out = Vec::with_capacity(prealloc(count));
    for _ in 0..count {
        let id = opcode::read_u64(input)?;
        note_id(seen, id)?;
        let native = opcode::read_u32(input)?;
        let n = checked_count(input, MAX_CLOSURE_VALUES)?;
        let mut values = Vec::with_capacity(prealloc(n));
        for _ in 0..n {
            values.push(read_value(input)?);
        }
        let n = checked_count(input, MAX_CLOSURE_VALUES)?;
        let mut state = Vec::with_capacity(prealloc(n));
        for _ in 0..n {
            state.push(opcode::read_i64(input)?);
        }
        out.push(NativeClosureImage {
            id,
            native,
            values,
            state,
        });
    }
    Ok(out)
}

fn read_bytes(input: &mut &[u8], max: u32) -> Result<Vec<u8>, SnapshotError> {
    let len = checked_len(input, max)? as usize;
    if input.len() < len {
        return Err(SnapshotError::Truncated);
    }
    let bytes = input[..len].to_vec();
    *input = &input[len..];
    Ok(bytes)
}

fn read_userdata(
    input: &mut &[u8],
    seen: &mut HashSet<u64>,
) -> Result<Vec<UserdataImage>, SnapshotError> {
    let count = checked_count(input, MAX_OBJECTS)?;
    let mut out = Vec::with_capacity(prealloc(count));
    for _ in 0..count {
        let id = opcode::read_u64(input)?;
        note_id(seen, id)?;
        let metatable = opcode::read_u64(input)?;
        let n = checked_count(input, MAX_USER_VALUES)?;
        // Each value takes at least a byte: a count past what is left is
        // refused before anything is reserved for it.
        if n as usize > input.len() {
            return Err(SnapshotError::Truncated);
        }
        let mut user_values = Vec::with_capacity(prealloc(n));
        for _ in 0..n {
            user_values.push(read_value(input)?);
        }
        let payload = match opcode::read_u8(input)? {
            4 => PayloadImage::File(crate::iolib::FileState::decode(input)?),
            1 => PayloadImage::Bytes(read_bytes(input, MAX_USERDATA_BYTES)?),
            2 => PayloadImage::Host {
                symbol: read_string(input)?,
                bytes: read_bytes(input, MAX_USERDATA_BYTES)?,
            },
            3 => PayloadImage::Rebind {
                symbol: read_string(input)?,
                key: read_bytes(input, crate::MAX_REBIND_KEY as u32)?,
            },
            _ => return Err(SnapshotError::InvalidTag),
        };
        let charge = opcode::read_u64(input)?;
        out.push(UserdataImage {
            id,
            metatable,
            user_values,
            payload,
            charge,
        });
    }
    Ok(out)
}

fn read_threads(
    input: &mut &[u8],
    seen: &mut HashSet<u64>,
) -> Result<Vec<ThreadImage>, SnapshotError> {
    let count = checked_count(input, MAX_OBJECTS)?;
    let mut out = Vec::with_capacity(prealloc(count));
    for _ in 0..count {
        let id = opcode::read_u64(input)?;
        note_id(seen, id)?;
        let status = opcode::read_u8(input)?;
        if Status::from_u8(status).is_none() {
            return Err(SnapshotError::InvalidTag);
        }
        let resumed_by = opcode::read_u64(input)?;
        let top = opcode::read_u32(input)?;
        let n_stack = checked_count(input, MAX_STACK_SLOTS)?;
        let mut stack = Vec::with_capacity(prealloc(n_stack));
        for _ in 0..n_stack {
            stack.push(read_value(input)?);
        }
        let n_results = checked_count(input, MAX_STACK_SLOTS)?;
        let mut results = Vec::with_capacity(prealloc(n_results));
        for _ in 0..n_results {
            results.push(read_value(input)?);
        }
        let n_frames = checked_count(input, MAX_FRAMES)?;
        let mut frames = Vec::with_capacity(prealloc(n_frames));
        for _ in 0..n_frames {
            frames.push(read_frame(input)?);
        }
        let error = if read_flag(input)? {
            Some((read_fault(input)?, read_value(input)?))
        } else {
            None
        };
        let unwind = if read_flag(input)? {
            Some(read_unwind(input)?)
        } else {
            None
        };
        let coroutine = read_flag(input)?;
        let closing = read_flag(input)?;
        let n_tbc = checked_count(input, MAX_STACK_SLOTS)?;
        let mut tbc = Vec::with_capacity(prealloc(n_tbc));
        for _ in 0..n_tbc {
            tbc.push(opcode::read_u32(input)?);
        }
        let charged_slots = opcode::read_u32(input)?;
        let charged_held = opcode::read_u64(input)?;
        let hook = read_hook(input)?;
        out.push(ThreadImage {
            id,
            status,
            resumed_by,
            top,
            stack,
            results,
            frames,
            error,
            unwind,
            coroutine,
            closing,
            tbc,
            charged_slots,
            charged_held,
            hook,
        });
    }
    Ok(out)
}

fn read_frame(input: &mut &[u8]) -> Result<FrameImage, SnapshotError> {
    let closure = opcode::read_u64(input)?;
    let pc = opcode::read_u32(input)?;
    let base = opcode::read_u32(input)?;
    let limit = opcode::read_u32(input)?;
    let nresults = opcode::read_u8(input)?;
    let vararg_len = opcode::read_u32(input)?;
    let tail = match opcode::read_u8(input)? {
        0 => false,
        1 => true,
        _ => return Err(SnapshotError::InvalidTag),
    };
    let return_hook = read_flag(input)?;
    let pending = match opcode::read_u8(input)? {
        0 => PendingImage::None,
        1 => PendingImage::Prepared {
            sequence: opcode::read_u64(input)?,
            symbol: read_string(input)?,
            arg: opcode::read_i64(input)?,
            dest: opcode::read_u8(input)?,
        },
        2 => PendingImage::Waiting {
            sequence: opcode::read_u64(input)?,
            symbol: read_string(input)?,
            arg: opcode::read_i64(input)?,
            dest: opcode::read_u8(input)?,
            wait_key: opcode::read_u64(input)?,
        },
        3 => PendingImage::Resuming {
            child: opcode::read_u64(input)?,
            dest: opcode::read_u8(input)?,
            nresults: opcode::read_u8(input)?,
        },
        4 => PendingImage::Assigning {
            src: opcode::read_u32(input)?,
            nvalues: opcode::read_u16(input)?,
            next: opcode::read_u16(input)?,
        },
        5 => PendingImage::NativePrepared {
            sequence: opcode::read_u64(input)?,
        },
        8 => PendingImage::Capability {
            sequence: opcode::read_u64(input)?,
            wait_key: opcode::read_u64(input)?,
            completed: read_flag(input)?,
        },
        7 => PendingImage::Deferred,
        6 => {
            let flag = opcode::read_u8(input)?;
            let raw = opcode::read_u64(input)?;
            let sequence = match flag {
                0 if raw == 0 => None,
                1 => Some(raw),
                _ => return Err(SnapshotError::InvalidTag),
            };
            PendingImage::NativeWaiting {
                sequence,
                wait_key: opcode::read_u64(input)?,
            }
        }
        _ => return Err(SnapshotError::InvalidTag),
    };
    let wait_request = if read_flag(input)? {
        let operation = read_string(input)?;
        if operation.len() > 4096 {
            return Err(SnapshotError::LimitExceeded);
        }
        let count = checked_count(input, MAX_STACK_SLOTS)?;
        let mut payload = Vec::with_capacity(prealloc(count));
        for _ in 0..count {
            payload.push(read_value(input)?);
        }
        Some((operation, payload))
    } else {
        None
    };
    let n_targets = checked_count(input, MAX_STACK_SLOTS)?;
    let mut targets = Vec::with_capacity(prealloc(n_targets));
    for _ in 0..n_targets {
        targets.push(match opcode::read_u8(input)? {
            1 => TargetImage::Register(opcode::read_u32(input)?),
            2 => TargetImage::Field {
                table: read_value(input)?,
                key: read_value(input)?,
            },
            _ => return Err(SnapshotError::InvalidTag),
        });
    }
    let meta = read_meta(input)?;
    let boundary = match opcode::read_u8(input)? {
        0 => None,
        1 => {
            let func = opcode::read_u32(input)?;
            let advance_caller = read_flag(input)?;
            let handler = if read_flag(input)? {
                Some(read_value(input)?)
            } else {
                None
            };
            Some(BoundaryImage::Protect {
                func,
                advance_caller,
                handler,
            })
        }
        2 => Some(BoundaryImage::Handler {
            slot: opcode::read_u32(input)?,
            protect: opcode::read_u32(input)?,
            target: opcode::read_u32(input)?,
            depth: opcode::read_u8(input)?,
            fault: read_fault(input)?,
        }),
        3 => {
            use crate::heap::Task;
            let func = opcode::read_u32(input)?;
            let passed = opcode::read_u32(input)?;
            let advance_caller = read_flag(input)?;
            let task = match opcode::read_u8(input)? {
                8 => Task::HostLoad(Box::new(crate::runtime::hostload::HostLoad::decode(input)?)),
                9 => Task::DoFile,
                1 => Task::ToString,
                2 => Task::Print {
                    next: opcode::read_u32(input)?,
                },
                3 => Task::Pairs,
                4 => Task::Ipairs {
                    index: opcode::read_i64(input)?,
                },
                5 => {
                    let len = checked_len(input, MAX_SOURCE_BYTES)? as usize;
                    if input.len() < len {
                        return Err(SnapshotError::Truncated);
                    }
                    let (source, rest) = input.split_at(len);
                    *input = rest;
                    Task::Load {
                        source: source.to_vec(),
                    }
                }
                6 => Task::Lib(Box::new(read_lib_task(input)?)),
                10 => Task::Io(Box::new(crate::iolib::IoWork::decode(input)?)),
                7 => Task::Collect {
                    result: match opcode::read_u8(input)? {
                        result @ 0..=4 => result,
                        _ => return Err(SnapshotError::InvalidTag),
                    },
                    wait: read_flag(input)?,
                    ended: read_flag(input)?,
                    left: opcode::read_u32(input)?,
                },
                _ => return Err(SnapshotError::InvalidTag),
            };
            Some(BoundaryImage::Builtin {
                func,
                passed,
                advance_caller,
                task,
            })
        }
        5 => {
            let func = opcode::read_u32(input)?;
            let passed = opcode::read_u32(input)?;
            let advance_caller = read_flag(input)?;
            let symbol = opcode::read_u32(input)?;
            let tag = opcode::read_u32(input)?;
            let kept = opcode::read_u32(input)?;
            let sequence = match opcode::read_u64(input)? {
                0 => None,
                sequence => Some(sequence),
            };
            let resuming = read_flag(input)?;
            let error = if read_flag(input)? {
                Some((read_fault(input)?, read_value(input)?))
            } else {
                None
            };
            Some(BoundaryImage::Native {
                func,
                passed,
                advance_caller,
                symbol,
                tag,
                kept,
                sequence,
                error,
                resuming,
            })
        }
        6 => Some(BoundaryImage::Hook {
            func: opcode::read_u32(input)?,
            saved_top: opcode::read_u32(input)?,
            target: opcode::read_u32(input)?,
            instruction: read_hook_instruction(input)?,
            after: read_hook_after(input)?,
        }),
        7 => Some(BoundaryImage::HookNative {
            func: opcode::read_u32(input)?,
            passed: opcode::read_u32(input)?,
            callee: read_value(input)?,
            advance_caller: read_flag(input)?,
            phase: opcode::read_u8(input)?,
            produced: opcode::read_u32(input)?,
            result: opcode::read_u32(input)?,
        }),
        4 => Some(BoundaryImage::Finalizer {
            func: opcode::read_u32(input)?,
            saved_top: opcode::read_u32(input)?,
        }),
        _ => return Err(SnapshotError::InvalidTag),
    };
    Ok(FrameImage {
        closure,
        pc,
        base,
        limit,
        nresults,
        vararg_len,
        tail,
        return_hook,
        pending,
        wait_request,
        targets,
        meta,
        boundary,
    })
}

/// A library task (ADR 0033): its wait, then its work.
fn write_lib_task(out: &mut Vec<u8>, task: &crate::library::LibTask) -> Result<(), SnapshotError> {
    use crate::library::{Wait, Work};
    match task.wait {
        Wait::Nothing => out.push(0),
        Wait::Get { into } => {
            out.push(1);
            out.extend(into.to_le_bytes());
        }
        Wait::Set => out.push(2),
        Wait::Len => out.push(3),
        Wait::Truth => out.push(4),
        Wait::Pair { into } => {
            out.push(5);
            out.extend(into.to_le_bytes());
        }
    }
    let stage = |out: &mut Vec<u8>, stage: crate::library::Stage| out.push(stage as u8);
    let ints = |out: &mut Vec<u8>, values: &[i64]| {
        for value in values {
            out.extend(value.to_le_bytes());
        }
    };
    match &task.work {
        Work::Extreme { max, best, next } => {
            out.push(1);
            out.push(u8::from(*max));
            out.extend(best.to_le_bytes());
            out.extend(next.to_le_bytes());
        }
        Work::Insert { stage: at, pos, i } => {
            out.push(2);
            stage(out, *at);
            ints(out, &[*pos, *i]);
        }
        Work::Remove {
            stage: at,
            size,
            pos,
        } => {
            out.push(3);
            stage(out, *at);
            ints(out, &[*size, *pos]);
        }
        Work::Move {
            stage: at,
            f,
            t,
            n,
            i,
            backward,
        } => {
            out.push(4);
            stage(out, *at);
            ints(out, &[*f, *t, *n, *i]);
            out.push(u8::from(*backward));
        }
        Work::Concat {
            stage: at,
            i,
            last,
            text,
        } => {
            out.push(5);
            stage(out, *at);
            ints(out, &[*i, *last]);
            write_count(out, text.len(), MAX_STRING_BYTES)?;
            out.extend_from_slice(text);
        }
        Work::Pack { stage: at, next } => {
            out.push(6);
            stage(out, *at);
            out.extend(next.to_le_bytes());
        }
        Work::Unpack {
            stage: at,
            first,
            count,
            got,
        } => {
            out.push(7);
            stage(out, *at);
            ints(out, &[*first]);
            out.extend(count.to_le_bytes());
            out.extend(got.to_le_bytes());
        }
        Work::Sort(sort) => {
            out.push(8);
            out.push(sort.step as u8);
            for value in [sort.n, sort.lo, sort.up, sort.p, sort.i, sort.j, sort.rnd] {
                out.extend(value.to_le_bytes());
            }
            write_count(
                out,
                sort.pending.len(),
                crate::library::MAX_SORT_PENDING as u32,
            )?;
            for range in &sort.pending {
                for value in [range.lo, range.up, range.smaller, range.rnd] {
                    out.extend(value.to_le_bytes());
                }
            }
        }
        Work::Str(work) => {
            out.push(9);
            work.encode(out);
        }
        Work::Package(work) => {
            out.push(10);
            work.encode(out);
        }
        Work::Debug(work) => {
            out.push(11);
            work.encode(out);
        }
        Work::Utf8(work) => {
            out.push(12);
            work.encode(out);
        }
        Work::Os(work) => {
            out.push(13);
            work.encode(out);
        }
    }
    Ok(())
}

fn read_lib_task(input: &mut &[u8]) -> Result<crate::library::LibTask, SnapshotError> {
    use crate::library::{LibTask, SortRange, SortState, Stage, Wait, Work};
    let wait = match opcode::read_u8(input)? {
        0 => Wait::Nothing,
        1 => Wait::Get {
            into: opcode::read_u32(input)?,
        },
        2 => Wait::Set,
        3 => Wait::Len,
        4 => Wait::Truth,
        5 => Wait::Pair {
            into: opcode::read_u32(input)?,
        },
        _ => return Err(SnapshotError::InvalidTag),
    };
    // Every `Stage`, in tag order.
    const STAGES: [Stage; 8] = [
        Stage::Start,
        Stage::Length,
        Stage::First,
        Stage::Read,
        Stage::Write,
        Stage::Last,
        Stage::Equal,
        Stage::Done,
    ];
    let stage = |input: &mut &[u8]| -> Result<Stage, SnapshotError> {
        let tag = usize::from(opcode::read_u8(input)?);
        STAGES
            .get(tag)
            .copied()
            .filter(|stage| *stage as usize == tag)
            .ok_or(SnapshotError::InvalidTag)
    };
    let int = |input: &mut &[u8]| opcode::read_i64(input);
    let work = match opcode::read_u8(input)? {
        1 => Work::Extreme {
            max: read_flag(input)?,
            best: opcode::read_u32(input)?,
            next: opcode::read_u32(input)?,
        },
        2 => Work::Insert {
            stage: stage(input)?,
            pos: int(input)?,
            i: int(input)?,
        },
        3 => Work::Remove {
            stage: stage(input)?,
            size: int(input)?,
            pos: int(input)?,
        },
        4 => Work::Move {
            stage: stage(input)?,
            f: int(input)?,
            t: int(input)?,
            n: int(input)?,
            i: int(input)?,
            backward: read_flag(input)?,
        },
        5 => {
            let (at, i, last) = (stage(input)?, int(input)?, int(input)?);
            let len = checked_len(input, MAX_STRING_BYTES)? as usize;
            if input.len() < len {
                return Err(SnapshotError::Truncated);
            }
            let (text, rest) = input.split_at(len);
            *input = rest;
            Work::Concat {
                stage: at,
                i,
                last,
                text: text.to_vec(),
            }
        }
        6 => Work::Pack {
            stage: stage(input)?,
            next: opcode::read_u32(input)?,
        },
        7 => Work::Unpack {
            stage: stage(input)?,
            first: int(input)?,
            count: opcode::read_u32(input)?,
            got: opcode::read_u32(input)?,
        },
        8 => {
            let step = SORT_STEPS
                .get(usize::from(opcode::read_u8(input)?))
                .copied()
                .ok_or(SnapshotError::InvalidTag)?;
            let mut values = [0u32; 7];
            for value in &mut values {
                *value = opcode::read_u32(input)?;
            }
            let [n, lo, up, p, i, j, rnd] = values;
            let count = checked_count(input, crate::library::MAX_SORT_PENDING as u32)?;
            let mut pending = Vec::with_capacity(prealloc(count));
            for _ in 0..count {
                pending.push(SortRange {
                    lo: opcode::read_u32(input)?,
                    up: opcode::read_u32(input)?,
                    smaller: opcode::read_u32(input)?,
                    rnd: opcode::read_u32(input)?,
                });
            }
            Work::Sort(Box::new(SortState {
                step,
                n,
                lo,
                up,
                p,
                i,
                j,
                rnd,
                pending,
            }))
        }
        9 => Work::Str(Box::new(crate::strlib::StrWork::decode(input)?)),
        10 => Work::Package(Box::new(crate::package::PackageWork::decode(input)?)),
        11 => Work::Debug(Box::new(crate::debuglib::DebugWork::decode(input)?)),
        12 => Work::Utf8(Box::new(crate::utf8lib::Utf8Work::decode(input)?)),
        13 => Work::Os(Box::new(crate::oslib::OsWork::decode(input)?)),
        _ => return Err(SnapshotError::InvalidTag),
    };
    Ok(LibTask { work, wait })
}

/// Every `SortStep`, in tag order.
const SORT_STEPS: [crate::library::SortStep; 28] = {
    use crate::library::SortStep::*;
    [
        Length,
        Range,
        GetLo,
        GetUp,
        UpLessLo,
        SetLoUp,
        SetUpLo,
        GetP,
        GetLo2,
        PLessLo,
        SetPLo,
        SetLoP,
        GetUp2,
        UpLessP,
        SetPUp,
        SetUpP,
        GetPivot,
        GetUpMinus1,
        SetPToUpMinus1,
        SetUpMinus1ToPivot,
        GetI,
        ILessPivot,
        GetJ,
        PivotLessJ,
        SetIJ,
        SetJI,
        SetUpMinus1I,
        SetIPivot,
    ]
};

/// A library task restore accepts (ADR 0033): its counters within the
/// function's arguments and the stack, a wait its scratch can take, and
/// never a task that has not started, since a frame exists only after the
/// first step.
fn lib_task_fits(task: &crate::library::LibTask, passed: u32) -> bool {
    use crate::library::{MAX_SORT_PENDING, SortStep, Stage, Wait, Work};
    let wait_fits = match task.wait {
        Wait::Get { into } => into < task.work.scratch(),
        Wait::Pair { into } => into.saturating_add(1) < task.work.scratch(),
        _ => true,
    };
    let started = |stage: Stage| stage != Stage::Start;
    let work_fits = match &task.work {
        Work::Extreme { best, next, .. } => passed >= 1 && best < next && *next <= passed,
        Work::Insert { stage, .. } | Work::Remove { stage, .. } => started(*stage) && passed >= 1,
        Work::Move { stage, n, i, .. } => started(*stage) && *n >= 0 && *i >= -1 && *i <= *n,
        Work::Concat { stage, text, .. } => {
            started(*stage) && text.len() <= crate::heap::STRING_CEILING
        }
        Work::Pack { stage, next } => started(*stage) && *next <= passed,
        Work::Unpack {
            stage, count, got, ..
        } => started(*stage) && got <= count && *count <= MAX_STACK_SLOTS,
        Work::Sort(sort) => {
            let bound = sort.n.saturating_add(1);
            sort.n < i32::MAX as u32
                && sort.pending.len() <= MAX_SORT_PENDING
                && [sort.lo, sort.up, sort.p, sort.i, sort.j]
                    .iter()
                    .all(|index| *index <= bound)
                && sort
                    .pending
                    .iter()
                    .all(|range| range.lo <= bound && range.up <= bound)
                && (sort.step != SortStep::Length || sort.n == 0)
        }
        // Checked against its arguments' strings in `string_work_fits`.
        Work::Str(_) | Work::Utf8(_) | Work::Os(_) | Work::Package(_) | Work::Debug(_) => true,
    };
    wait_fits && work_fits
}

fn read_flag(input: &mut &[u8]) -> Result<bool, SnapshotError> {
    match opcode::read_u8(input)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(SnapshotError::InvalidTag),
    }
}

fn read_fault(input: &mut &[u8]) -> Result<u8, SnapshotError> {
    let tag = opcode::read_u8(input)?;
    crate::id::LuaFault::from_tag(tag).ok_or(SnapshotError::InvalidTag)?;
    Ok(tag)
}

fn read_string(input: &mut &[u8]) -> Result<String, SnapshotError> {
    let len = checked_len(input, MAX_STRING_BYTES)?;
    if input.len() < len as usize {
        return Err(SnapshotError::Truncated);
    }
    let bytes = input[..len as usize].to_vec();
    *input = &input[len as usize..];
    String::from_utf8(bytes).map_err(|_| SnapshotError::InvalidStructure)
}

fn read_dead_key(input: &mut &[u8]) -> Result<DeadKey, SnapshotError> {
    Ok(match opcode::read_u8(input)? {
        1 => DeadKey::Bool(opcode::read_u8(input)? != 0),
        2 => DeadKey::Integer(opcode::read_i64(input)?),
        3 => DeadKey::Float(opcode::read_u64(input)?),
        4 => {
            let len = checked_len(input, MAX_STRING_BYTES)?;
            if input.len() < len as usize {
                return Err(SnapshotError::Truncated);
            }
            let bytes = input[..len as usize].to_vec();
            *input = &input[len as usize..];
            DeadKey::Bytes(bytes)
        }
        5 => DeadKey::Object(opcode::read_u64(input)?),
        6 => DeadKey::Native(opcode::read_u32(input)?),
        7 => {
            let (domain, bits) = read_light(input)?;
            DeadKey::Light(domain, bits)
        }
        _ => return Err(SnapshotError::InvalidTag),
    })
}

fn read_value(input: &mut &[u8]) -> Result<EncValue, SnapshotError> {
    Ok(match opcode::read_u8(input)? {
        0 => EncValue::Nil,
        1 => EncValue::Bool(opcode::read_u8(input)? != 0),
        2 => EncValue::Integer(opcode::read_i64(input)?),
        3 => EncValue::Float(opcode::read_u64(input)?),
        4 => EncValue::String(opcode::read_u64(input)?),
        5 => EncValue::Table(opcode::read_u64(input)?),
        6 => EncValue::Closure(opcode::read_u64(input)?),
        7 => EncValue::Thread(opcode::read_u64(input)?),
        8 => EncValue::Native(opcode::read_u32(input)?),
        9 => EncValue::NativeClosure(opcode::read_u64(input)?),
        10 => EncValue::Userdata(opcode::read_u64(input)?),
        11 => {
            let (domain, bits) = read_light(input)?;
            EncValue::Light(domain, bits)
        }
        _ => return Err(SnapshotError::InvalidTag),
    })
}

fn read_light(input: &mut &[u8]) -> Result<(crate::value::LightDomain, u64), SnapshotError> {
    let domain = crate::value::LightDomain::from_tag(opcode::read_u8(input)?)
        .ok_or(SnapshotError::InvalidTag)?;
    Ok((domain, opcode::read_u64(input)?))
}

/// A light userdata token the VM made names an object that existed: an
/// upvalue cell, or a native closure's value (ADR 0043). It need not still
/// exist; ids are never reused, so it can never name another.
fn light_fits(domain: crate::value::LightDomain, bits: u64, next_object_id: u64) -> bool {
    use crate::value::LightDomain;
    let id_fits = |id: u64| id != 0 && id < next_object_id;
    match domain {
        LightDomain::Host => true,
        LightDomain::Upvalue => id_fits(bits),
        LightDomain::NativeValue => {
            id_fits(bits >> 8) && (bits & 0xff) < u64::from(MAX_CLOSURE_VALUES)
        }
    }
}

fn validate_graph(image: &Image, seen: &HashSet<u64>) -> Result<(), SnapshotError> {
    let mut kinds: std::collections::HashMap<u64, Kind> = std::collections::HashMap::new();
    for (id, _) in &image.strings {
        kinds.insert(*id, Kind::String);
    }
    let mut wait_keys = HashSet::new();
    for thread in &image.threads {
        for (frame_index, frame) in thread.frames.iter().enumerate() {
            if let PendingImage::Capability {
                sequence,
                wait_key,
                completed,
            } = frame.pending
            {
                let invalid = || SnapshotError::InvalidStructure;
                if frame_index + 1 != thread.frames.len()
                    || sequence >= image.next_sequence
                    || wait_key != (sequence | (1 << 63))
                    || completed != image.completed_waits.binary_search(&wait_key).is_ok()
                {
                    return Err(invalid());
                }
                let (operation, payload) = frame.wait_request.as_ref().ok_or_else(invalid)?;
                let [EncValue::String(request_id), EncValue::Integer(_), result] =
                    payload.as_slice()
                else {
                    return Err(invalid());
                };
                let bytes = image
                    .strings
                    .iter()
                    .find(|(id, _)| id == request_id)
                    .map(|(_, bytes)| bytes)
                    .ok_or_else(invalid)?;
                let request = crate::CapabilityRequest::from_bytes(bytes).map_err(|_| invalid())?;
                if !request.bounded() || request.operation() != operation {
                    return Err(invalid());
                }
                match (completed, result) {
                    (false, EncValue::Nil) => {}
                    (true, EncValue::String(id)) => {
                        let bytes = image
                            .strings
                            .iter()
                            .find(|(key, _)| key == id)
                            .map(|(_, bytes)| bytes)
                            .ok_or_else(invalid)?;
                        let result =
                            crate::hostcaps::protocol::decode(bytes).map_err(|_| invalid())?;
                        if !request.valid_result(&result) {
                            return Err(invalid());
                        }
                    }
                    _ => return Err(invalid()),
                }
            }
            let key = match frame.meta.as_ref().map(|meta| meta.phase) {
                Some(crate::heap::MetaPhase::NativeWaiting { wait_key, .. }) => Some(wait_key),
                _ => match frame.pending {
                    PendingImage::Waiting { wait_key, .. }
                    | PendingImage::NativeWaiting { wait_key, .. }
                    | PendingImage::Capability {
                        wait_key,
                        completed: false,
                        ..
                    } => Some(wait_key),
                    _ => None,
                },
            };
            if let Some(key) = key {
                if !wait_keys.insert(key) || image.completed_waits.binary_search(&key).is_ok() {
                    return Err(SnapshotError::InvalidStructure);
                }
                if frame.wait_request.is_some()
                    && (key & (1 << 63) == 0 || key & !(1 << 63) >= image.next_sequence)
                {
                    return Err(SnapshotError::InvalidStructure);
                }
            }
        }
    }
    if image.host_call && image.entry == 0 {
        return Err(SnapshotError::InvalidStructure);
    }
    for proto in &image.protos {
        kinds.insert(proto.id, Kind::Proto);
    }
    for table in &image.tables {
        kinds.insert(table.id, Kind::Table);
    }
    for upvalue in &image.upvalues {
        kinds.insert(upvalue.id, Kind::Upvalue);
    }
    for closure in &image.native_closures {
        kinds.insert(closure.id, Kind::NativeClosure);
    }
    for closure in &image.closures {
        kinds.insert(closure.id, Kind::Closure);
    }
    for thread in &image.threads {
        kinds.insert(thread.id, Kind::Thread);
    }
    for userdata in &image.userdata {
        kinds.insert(userdata.id, Kind::Userdata);
    }
    let require = |id: u64, kind: Kind| -> Result<(), SnapshotError> {
        if id == 0 {
            return Err(SnapshotError::DanglingReference);
        }
        match kinds.get(&id) {
            Some(found) if *found == kind => Ok(()),
            Some(_) => Err(SnapshotError::InvalidStructure),
            None => Err(SnapshotError::DanglingReference),
        }
    };
    if image.globals != 0 {
        require(image.globals, Kind::Table)?;
    }
    if image.registry != 0 {
        require(image.registry, Kind::Table)?;
    }
    for (tag, id) in image.type_metatables.iter().enumerate() {
        if *id == 0 {
            continue;
        }
        // Tables and userdata have per-object metatables.
        if crate::heap::PER_OBJECT_TYPES.contains(&tag) {
            return Err(SnapshotError::InvalidStructure);
        }
        require(*id, Kind::Table)?;
    }
    if image.active != 0 {
        require(image.active, Kind::Thread)?;
    }
    if image.entry != 0 {
        require(image.entry, Kind::Thread)?;
    }
    for id in &image.reserved {
        require(*id, Kind::String)?;
    }
    // The reserved strings are the boot-time strings, each with its
    // expected text, owned by no prototype.
    let reserved: HashSet<u64> = image.reserved.iter().copied().collect();
    if reserved.len() != image.reserved.len()
        || image
            .protos
            .iter()
            .any(|proto| proto.const_ids.iter().any(|id| reserved.contains(id)))
    {
        return Err(SnapshotError::InvalidStructure);
    }
    for (id, text) in image.reserved.iter().zip(crate::heap::reserved_texts()) {
        let bytes = image.string(*id);
        if bytes != Some(text.as_bytes()) {
            return Err(SnapshotError::InvalidStructure);
        }
    }
    for proto in &image.protos {
        if proto.ops.len() > MAX_INSTRUCTIONS as usize {
            return Err(SnapshotError::LimitExceeded);
        }
        for child in &proto.children {
            require(*child, Kind::Proto)?;
        }
        for op in &proto.ops {
            if let Op::CallHost { symbol, .. } = op {
                let bytes = proto
                    .byte_consts
                    .get(*symbol as usize)
                    .ok_or(SnapshotError::InvalidStructure)?;
                let _ = std::str::from_utf8(bytes).map_err(|_| SnapshotError::InvalidStructure)?;
            }
        }
    }
    for table in &image.tables {
        validate_table_slots(image, &require, &kinds, table)?;
        if table.metatable != 0 {
            require(table.metatable, Kind::Table)?;
        }
    }
    for upvalue in &image.upvalues {
        match &upvalue.state {
            UpImageState::Closed(value) => require_value(&require, value)?,
            UpImageState::Open { thread, slot } => {
                require(*thread, Kind::Thread)?;
                if *slot >= image.max_stack_slots {
                    return Err(SnapshotError::LimitExceeded);
                }
            }
        }
    }
    for closure in &image.native_closures {
        for value in &closure.values {
            require_value(&require, value)?;
        }
    }
    validate_userdata(image, &require)?;
    validate_finalizers(image, &kinds)?;
    validate_collector(image, &kinds, &require)?;
    for closure in &image.closures {
        require(closure.proto, Kind::Proto)?;
        for upvalue in &closure.upvalues {
            require(*upvalue, Kind::Upvalue)?;
        }
    }
    validate_resumers(image)?;
    for thread in &image.threads {
        if thread.resumed_by != 0 {
            require(thread.resumed_by, Kind::Thread)?;
        }
        for value in thread.stack.iter().chain(thread.results.iter()) {
            require_value(&require, value)?;
        }
        if !thread_slots_fit(thread, image.max_stack_slots) {
            return Err(SnapshotError::LimitExceeded);
        }
        let closing_failed = thread.id == image.entry
            && image.finalizers.closed == Status::Failed.tag()
            && (thread.status == Status::Ready.tag() || thread.status == Status::Waiting.tag());
        validate_error_state(thread, closing_failed, exit_scopes(image, thread))?;
        validate_waits(thread)?;
        validate_closes(image, thread)?;
        validate_hooks(image, thread, &require)?;
        // The running frame's extra arguments are on the stack: every
        // return, commit, and unwind keeps them.
        if let Some(frame) = thread.frames.last()
            && frame.vararg_len > 0
            && u64::from(frame.base) > thread.stack.len() as u64
        {
            return Err(SnapshotError::InvalidStructure);
        }
        if let Some((_, value)) = &thread.error {
            require_value(&require, value)?;
        }
        if let Some((_, value)) = thread
            .unwind
            .as_ref()
            .and_then(|unwind| unwind.error.as_ref())
        {
            require_value(&require, value)?;
        }
        for (index, frame) in thread.frames.iter().enumerate() {
            validate_boundary(thread, index)?;
            string_work_fits(image, thread, index)?;
            if let Some(MetaImage {
                event:
                    EventImage::Close {
                        next:
                            NextImage::Unwind(UnwindImage {
                                error: Some((_, value)),
                                ..
                            }),
                        ..
                    },
                ..
            }) = &frame.meta
            {
                require_value(&require, value)?;
            }
            if let Some((_, payload)) = &frame.wait_request {
                for value in payload {
                    require_value(&require, value)?;
                }
            }
            if let Some(BoundaryImage::Native { symbol, error, .. }) = &frame.boundary {
                if *symbol as usize >= image.natives.len() {
                    return Err(SnapshotError::UnknownHostSymbol);
                }
                if let Some((class, value)) = error {
                    if crate::id::LuaFault::from_tag(*class).is_none() {
                        return Err(SnapshotError::InvalidTag);
                    }
                    require_value(&require, value)?;
                }
            }
            if let Some(BoundaryImage::HookNative { callee, .. }) = &frame.boundary {
                require_value(&require, callee)?;
            }
            if let Some(BoundaryImage::Protect {
                handler: Some(handler),
                ..
            }) = &frame.boundary
            {
                require_value(&require, handler)?;
            }
        }
        for (index, frame) in thread.frames.iter().enumerate() {
            require(frame.closure, Kind::Closure)?;
            let proto = frame_proto(image, frame)?;
            if frame.pc as usize >= proto.ops.len() {
                return Err(SnapshotError::InvalidProgramCounter);
            }
            validate_frame_slots(thread, index, proto)?;
            validate_called(image, thread, index)?;
            // Source makes a tail call only outside every `<close>` scope,
            // and the runtime refuses one that would skip a close.
            if matches!(proto.ops[frame.pc as usize], Op::TailCall { .. })
                && thread
                    .tbc
                    .iter()
                    .any(|slot| (frame.base..frame.limit).contains(slot))
            {
                return Err(SnapshotError::InvalidStructure);
            }
            if let Some(meta) = &frame.meta {
                validate_meta(meta, proto.ops[frame.pc as usize], frame)?;
            }
            match &frame.pending {
                PendingImage::None => {}
                PendingImage::Resuming {
                    child,
                    dest,
                    nresults,
                } => {
                    require(*child, Kind::Thread)?;
                    // `Resume` waits on a coroutine it runs; `CloseThread`
                    // on one it closes.
                    let closing = image.thread(*child).is_some_and(|thread| thread.closing);
                    let fits = match proto.ops[frame.pc as usize] {
                        Op::Resume {
                            dest: op_dest,
                            nresults: op_nresults,
                            ..
                        } => !closing && op_dest == *dest && op_nresults == *nresults,
                        Op::CloseThread { dst, .. } => closing && dst == *dest && *nresults == 2,
                        _ => false,
                    };
                    if !fits {
                        return Err(SnapshotError::InvalidStructure);
                    }
                }
                // Only a boundary frame defers a call.
                PendingImage::Deferred if frame.boundary.is_none() => {
                    return Err(SnapshotError::InvalidStructure);
                }
                PendingImage::Prepared { .. }
                | PendingImage::Waiting { .. }
                | PendingImage::Assigning { .. }
                | PendingImage::NativePrepared { .. }
                | PendingImage::NativeWaiting { .. }
                | PendingImage::Capability { .. }
                | PendingImage::Deferred => {}
            }
            for target in &frame.targets {
                if let TargetImage::Field { table, key } = target {
                    require_value(&require, table)?;
                    require_value(&require, key)?;
                }
            }
        }
    }
    let _ = seen;
    Ok(())
}

/// The finalization lists name tables and full userdata, each at most
/// once over both; a finalizer runs only with its frame on some thread,
/// at most one at a time; a runtime closes only once closing began
/// (ADR 0047, ADR 0048).
/// The collector's state is one the collector reaches (ADR 0050): every
/// list names objects that exist, of the right kind, with marks that fit
/// the list; lists exist only in the phases that have them; positions lie
/// inside what they point into; the sweep has at least the objects not
/// yet passed left to pass.
fn validate_collector(
    image: &Image,
    kinds: &std::collections::HashMap<u64, Kind>,
    require: &dyn Fn(u64, Kind) -> Result<(), SnapshotError>,
) -> Result<(), SnapshotError> {
    use crate::heap::{age, mark};
    let c = &image.collector;
    let gc = &image.gc;
    let bad = || Err(SnapshotError::InvalidStructure);
    if c.white > 1 || c.phase > 5 {
        return bad();
    }
    let step_known = crate::gc::Atomic::from_tag(c.atomic).is_some();
    if (c.phase == 3 && !step_known) || (c.phase != 3 && c.atomic != 0) {
        return bad();
    }
    if gc.prepaid >= crate::gc::WORK_PER_FUEL {
        return bad();
    }
    let marking = matches!(c.phase, 2 | 3) || (c.minor && c.phase == 1);
    let sweeping = c.phase == 4;
    // The generational states (ADR 0051): a young collection runs only in
    // generational form, from its first units to its last, and a sweep
    // making survivors old is the one sweep in that form; nothing else
    // is, between collections, but generational mode chosen.
    let decide = crate::gc::Decide::from_tag(c.decide).ok_or(SnapshotError::InvalidStructure)?;
    let idle = c.generational && !c.minor && !c.to_old;
    if (c.minor && (!c.generational || !matches!(c.phase, 1 | 3 | 4 | 5) || !gc.generational))
        || (c.phase == 5 && !c.minor)
        || (c.to_old && (!c.generational || c.phase != 4 || c.minor))
        || (idle && (c.phase != 0 || !gc.generational))
        || (c.reset && (c.generational || !sweeping))
        || (c.generational && gc.bad != 0)
        || (gc.bad != 0 && !gc.generational)
        || (decide != crate::gc::Decide::None && (c.minor || (c.generational && !c.to_old)))
        || (decide == crate::gc::Decide::Fallback && matches!(c.phase, 1..=3) && gc.bad == 0)
        || (!c.minor && (c.promoted != 0 || !c.touched.is_empty()))
        || ((!sweeping || c.reset) && c.unreleased != 0)
        || c.unreleased > gc.quota
        || gc.major_base > gc.quota
        || gc.major_objects > 2 * crate::heap::MAX_OBJECTS
    {
        return bad();
    }
    // Lua does not run during an atomic phase or a young collection, so no
    // finalizer can be running then.
    if (c.phase == 3 || c.minor) && image.finalizers.running {
        return bad();
    }
    let mut marks = std::collections::HashMap::new();
    for (id, found, object_age) in &c.marks {
        if !kinds.contains_key(id)
            || !matches!(*found, mark::GRAY | mark::BLACK)
            || !age::valid(*object_age)
        {
            return bad();
        }
        if marks.insert(*id, (*found, *object_age)).is_some() {
            return bad();
        }
    }
    let mut ages = std::collections::HashMap::new();
    for (id, object_age) in &c.ages {
        if !kinds.contains_key(id)
            || marks.contains_key(id)
            || !age::valid(*object_age)
            || *object_age == age::NEW
            || ages.insert(*id, *object_age).is_some()
        {
            return bad();
        }
    }
    // In generational form an object off the young lists, and written in
    // neither list, is black and old.
    if c.generational {
        let listed: HashSet<u64> = c.young.iter().copied().collect();
        for id in kinds.keys() {
            if !listed.contains(id) && !marks.contains_key(id) && !ages.contains_key(id) {
                marks.insert(*id, (mark::BLACK, age::OLD));
            }
        }
    }
    // Ages: all new in incremental form (but a sweep leaving generational
    // form, which makes them so); young objects white and old ones marked
    // between collections in generational form, a touched one gray.
    for (id, (found, object_age)) in &marks {
        let fits = if c.reset {
            true
        } else if !c.generational {
            *object_age == age::NEW
        } else if c.to_old {
            matches!(
                (*found, *object_age),
                (_, age::NEW) | (mark::BLACK, age::OLD) | (mark::GRAY, age::TOUCHED1)
            )
        } else if c.minor {
            true
        } else {
            age::is_old(*object_age) && ((*found == mark::GRAY) == (*object_age == age::TOUCHED1))
        };
        if !fits || !kinds.contains_key(id) {
            return bad();
        }
    }
    for object_age in ages.values() {
        if !c.reset && (!c.generational || c.to_old || *object_age != age::SURVIVAL) {
            return bad();
        }
    }
    if !marking && !sweeping && !c.generational && !marks.is_empty() {
        return bad();
    }
    let marked = |id: &u64, want: u8| marks.get(id).is_some_and(|(found, _)| *found == want);
    let age_of = |id: &u64| {
        marks
            .get(id)
            .map(|(_, object_age)| *object_age)
            .or_else(|| ages.get(id).copied())
            .unwrap_or(age::NEW)
    };
    if !marking
        && (!c.gray.is_empty()
            || c.scan.is_some()
            || !c.weak.is_empty()
            || !c.ephemerons.is_empty()
            || !c.waiting.is_empty()
            || (!c.again.is_empty() && !c.generational))
    {
        return bad();
    }
    let mut seen = HashSet::new();
    for id in c.gray.iter().chain(&c.again) {
        if !marked(id, mark::GRAY) || !seen.insert(*id) {
            return bad();
        }
    }
    // While marking, every gray object waits to be traced, in one list or
    // the other; in generational form, every touched object waits in
    // `again`. (An incremental sweep makes gray objects white like black
    // ones.)
    if (marking || c.generational)
        && marks
            .iter()
            .any(|(id, (found, _))| *found == mark::GRAY && !seen.contains(id))
    {
        return bad();
    }
    // The young lists hold the young objects, each once, and every white
    // object in generational form; none outside it.
    let mut young = HashSet::new();
    for id in &c.young {
        if !kinds.contains_key(id)
            || !young.insert(*id)
            || age_of(id) > age::SURVIVAL
            || (idle && marks.contains_key(id))
        {
            return bad();
        }
    }
    if !c.generational && !c.young.is_empty() {
        return bad();
    }
    // Young objects are all listed, but the survivors a sweep making them
    // old has not reached yet.
    if c.generational
        && !c.to_old
        && marks
            .iter()
            .any(|(id, (_, object_age))| *object_age <= age::SURVIVAL && !young.contains(id))
    {
        return bad();
    }
    if c.generational
        && kinds
            .keys()
            .any(|id| !marks.contains_key(id) && !young.contains(id))
    {
        return bad();
    }
    // The objects the next young collection traces again: each once, old,
    // and every `OLD1` and `TOUCHED2` object between collections.
    let mut revisit = HashSet::new();
    for id in &c.revisit {
        let object_age = age_of(id);
        if !revisit.insert(*id)
            || !matches!(object_age, age::OLD1 | age::TOUCHED2 | age::TOUCHED1)
            || (object_age == age::TOUCHED1) != marked(id, mark::GRAY)
            || !marks.contains_key(id)
        {
            return bad();
        }
    }
    if (!c.revisit.is_empty() && (!c.generational || c.to_old || (c.minor && c.phase == 3)))
        || (idle
            && marks.iter().any(|(id, (_, object_age))| {
                matches!(*object_age, age::OLD1 | age::TOUCHED2) && !revisit.contains(id)
            }))
    {
        return bad();
    }
    for id in &c.touched {
        if !marks.contains_key(id) || !age::is_old(age_of(id)) || c.phase == 1 {
            return bad();
        }
    }
    // A full collection asks for at most the cycle running and one more.
    if gc
        .full
        .is_some_and(|target| target > gc.collections.saturating_add(2))
    {
        return bad();
    }
    // An object being scanned, or an ephemeron table waiting, was black
    // when reached, and gray again if written to since.
    if let Some((id, pos, how, keys, values)) = c.scan {
        if !marks.contains_key(&id) {
            return bad();
        }
        let total = match (how, kinds.get(&id)) {
            (1 | 2, Some(Kind::Table)) => image
                .tables
                .iter()
                .find(|table| table.id == id)
                .map(|table| table.slots.len()),
            (0, Some(Kind::Thread)) => {
                image
                    .threads
                    .iter()
                    .find(|thread| thread.id == id)
                    .map(|thread| {
                        thread.stack.len() + 1 + thread.frames.len() + thread.results.len() + 1
                    })
            }
            (0, Some(Kind::Proto)) => image
                .protos
                .iter()
                .find(|proto| proto.id == id)
                .map(|proto| 1 + proto.const_ids.len() + proto.children.len()),
            _ => None,
        };
        // Written to since it was reached, it may have shrunk below the
        // position; it is traced again in the atomic phase. An ephemeron
        // table is reached before its first slot is looked at, so may
        // have none.
        let written = marked(&id, mark::GRAY);
        let past = |total: usize| pos as usize > total || (pos as usize == total && how != 2);
        if total.is_none_or(|total| past(total) && !written)
            || (how != 1 && (keys || values))
            || (how == 1 && keys && !values)
        {
            return bad();
        }
    }
    for (id, _, _) in &c.weak {
        require(*id, Kind::Table)?;
        if !marks.contains_key(id) {
            return bad();
        }
    }
    for id in &c.ephemerons {
        require(*id, Kind::Table)?;
        if !marks.contains_key(id) {
            return bad();
        }
    }
    let mut last = 0;
    for (key, values) in &c.waiting {
        if *key <= last
            || values.is_empty()
            || matches!(
                kinds.get(key),
                None | Some(Kind::String | Kind::Proto | Kind::Upvalue)
            )
        {
            return bad();
        }
        last = *key;
        for value in values {
            require_value(require, value)?;
        }
    }
    let atomic = c.phase == 3;
    if !atomic && (c.cursor != 0 || c.inner != 0 || c.late != 0) {
        return bad();
    }
    if c.cursor as usize > c.weak.len() || c.late as usize > c.weak.len() {
        return bad();
    }
    if c.inner != 0 {
        let Some((id, _, _)) = c.weak.get(c.cursor as usize) else {
            return bad();
        };
        let slots = image
            .tables
            .iter()
            .find(|table| table.id == *id)
            .map_or(0, |table| table.slots.len());
        if c.inner as usize >= slots {
            return bad();
        }
    }
    if !sweeping && (c.sweep_left != 0 || c.reset) {
        return bad();
    }
    // The sweep has still to pass every object it has not: marked ones
    // (in a sweep making survivors old, those still new; in a young
    // collection, the marked young ones).
    let unswept = if c.minor {
        c.young.iter().filter(|id| marks.contains_key(id)).count()
    } else if c.to_old {
        marks
            .values()
            .filter(|(_, object_age)| *object_age == age::NEW)
            .count()
    } else {
        marks.len()
    };
    if sweeping && c.sweep_left < unswept as u64 {
        return bad();
    }
    let counting = marking || sweeping || (c.minor && c.phase == 5);
    if !counting
        && (c.marked_bytes != 0
            || c.debt_base != 0
            || c.marking_debt != 0
            || (c.work_base != 0 && !c.minor))
    {
        return bad();
    }
    Ok(())
}

fn validate_finalizers(
    image: &Image,
    kinds: &std::collections::HashMap<u64, Kind>,
) -> Result<(), SnapshotError> {
    let fin = &image.finalizers;
    if let Some(exit) = fin.exit {
        use crate::heap::ExitPhase;
        let main = image
            .thread(image.entry)
            .ok_or(SnapshotError::InvalidStructure)?;
        let fits = match exit.phase {
            ExitPhase::Scopes => {
                exit.close
                    && matches!(
                        Status::from_u8(main.status),
                        Some(Status::Ready | Status::Waiting)
                    )
                    && (main.unwind.is_some()
                        || main.frames.iter().any(|frame| {
                            matches!(
                                frame.meta.as_ref().map(|meta| &meta.event),
                                Some(EventImage::Close {
                                    next: NextImage::Unwind(_),
                                    ..
                                })
                            )
                        }))
            }
            ExitPhase::Finalizers => exit.close && fin.closing && fin.closed != 0,
            ExitPhase::Terminal => {
                !exit.close
                    || (fin.closing
                        && fin.closed == 0
                        && fin.pending.is_empty()
                        && !fin.running
                        && main.frames.is_empty()
                        && main.status == Status::Completed.tag()
                        && image.active == image.entry)
            }
        };
        if !fits {
            return Err(SnapshotError::InvalidStructure);
        }
    }
    let mut seen = HashSet::new();
    for id in fin.registered.iter().chain(fin.pending.iter()) {
        match kinds.get(id) {
            Some(Kind::Table | Kind::Userdata) => {}
            Some(_) => return Err(SnapshotError::InvalidStructure),
            None => return Err(SnapshotError::DanglingReference),
        }
        if !seen.insert(*id) {
            return Err(SnapshotError::InvalidStructure);
        }
    }
    let frames = image
        .threads
        .iter()
        .flat_map(|thread| thread.frames.iter())
        .filter(|frame| matches!(frame.boundary, Some(BoundaryImage::Finalizer { .. })))
        .count();
    if frames != usize::from(fin.running) {
        return Err(SnapshotError::InvalidStructure);
    }
    // Closing moved every registered object to the queue; one with no
    // frames of its own was the entry thread's, whose run had ended.
    if fin.closing && !fin.registered.is_empty() {
        return Err(SnapshotError::InvalidStructure);
    }
    // While the entry thread closes with no frames of its own, it is
    // ready, has finalizers left to run or running, and their frames name
    // the close's closure; no other thread starts with a finalizer frame.
    let entry = image.threads.iter().find(|thread| thread.id == image.entry);
    let bottom_finalizer = |thread: &ThreadImage| {
        matches!(
            thread
                .frames
                .first()
                .and_then(|frame| frame.boundary.as_ref()),
            Some(BoundaryImage::Finalizer { .. })
        )
    };
    match fin.closed {
        0 => {
            if fin.close_closure != 0 || image.threads.iter().any(bottom_finalizer) {
                return Err(SnapshotError::InvalidStructure);
            }
        }
        tag => {
            let fits = fin.closing
                && matches!(
                    Status::from_u8(tag),
                    Some(Status::Completed | Status::Failed)
                )
                && matches!(kinds.get(&fin.close_closure), Some(Kind::Closure))
                && entry.is_some_and(|entry| {
                    (entry.status == Status::Ready.tag() || entry.status == Status::Waiting.tag())
                        && (!fin.pending.is_empty() || !entry.frames.is_empty())
                        && entry.frames.first().is_none_or(|frame| {
                            frame.closure == fin.close_closure
                                && matches!(frame.boundary, Some(BoundaryImage::Finalizer { .. }))
                        })
                })
                && !image
                    .threads
                    .iter()
                    .any(|thread| thread.id != image.entry && bottom_finalizer(thread));
            if !fits {
                return Err(SnapshotError::InvalidStructure);
            }
        }
    }
    Ok(())
}

/// A full userdata's metatable is a table, its user values exist, a byte
/// payload counts its length, and every payload together fits the quota
/// it was charged against (ADR 0042, ADR 0045).
fn validate_userdata(
    image: &Image,
    require: &dyn Fn(u64, Kind) -> Result<(), SnapshotError>,
) -> Result<(), SnapshotError> {
    let mut total = 0u64;
    let mut file_resources = HashSet::new();
    for userdata in &image.userdata {
        if userdata.metatable != 0 {
            require(userdata.metatable, Kind::Table)?;
        }
        for value in &userdata.user_values {
            require_value(require, value)?;
        }
        match &userdata.payload {
            PayloadImage::File(file) => {
                if !file.closed && file.kind < 2 && !file_resources.insert(file.id.0) {
                    return Err(SnapshotError::InvalidStructure);
                }
                if userdata.charge != file.charge() {
                    return Err(SnapshotError::UserdataCharge);
                }
            }
            PayloadImage::Bytes(bytes) => {
                if userdata.charge != bytes.len() as u64 {
                    return Err(SnapshotError::InvalidStructure);
                }
            }
            PayloadImage::Host { symbol, .. } | PayloadImage::Rebind { symbol, .. } => {
                if symbol.is_empty() {
                    return Err(SnapshotError::InvalidStructure);
                }
            }
        }
        total = total.saturating_add(crate::heap::userdata_cost(
            userdata.user_values.len(),
            userdata.charge,
        ));
    }
    if total > image.gc.quota {
        return Err(SnapshotError::LimitExceeded);
    }
    Ok(())
}

fn validate_table_slots(
    image: &Image,
    require: &dyn Fn(u64, Kind) -> Result<(), SnapshotError>,
    kinds: &std::collections::HashMap<u64, Kind>,
    table: &TableImage,
) -> Result<(), SnapshotError> {
    // A key's slots: dead anchors, then at most one live slot, the last
    // (ADR 0010): a deleted key reinserted before the anchors went.
    let mut live = std::collections::HashMap::new();
    for slot in &table.slots {
        let (key, is_live) = match &slot.body {
            SlotBody::Live { key, value } => {
                if matches!(value, EncValue::Nil) {
                    return Err(SnapshotError::InvalidStructure);
                }
                require_value(require, key)?;
                require_value(require, value)?;
                (canonical_live_key(image, key)?, true)
            }
            SlotBody::Dead(key) => (canonical_dead_key(image, kinds, key)?, false),
        };
        if live.insert(key, is_live) == Some(true) {
            return Err(SnapshotError::InvalidStructure);
        }
    }
    Ok(())
}

fn canonical_live_key(image: &Image, value: &EncValue) -> Result<TableKey, SnapshotError> {
    Ok(match value {
        EncValue::Nil => return Err(SnapshotError::InvalidStructure),
        EncValue::Bool(bit) => TableKey::Bool(*bit),
        EncValue::Integer(integer) => TableKey::Integer(*integer),
        EncValue::Float(bits) => canonical_float_key(*bits)?,
        EncValue::String(id) => {
            let bytes = image.string(*id).ok_or(SnapshotError::DanglingReference)?;
            TableKey::string(bytes.to_vec())
        }
        EncValue::Table(id)
        | EncValue::Closure(id)
        | EncValue::Thread(id)
        | EncValue::NativeClosure(id)
        | EncValue::Userdata(id) => TableKey::Object(ObjectId(*id)),
        EncValue::Native(index) => native_key(image, *index)?,
        EncValue::Light(domain, bits) => light_key(image, *domain, *bits)?,
    })
}

fn light_key(
    image: &Image,
    domain: crate::value::LightDomain,
    bits: u64,
) -> Result<TableKey, SnapshotError> {
    if light_fits(domain, bits, image.next_object_id) {
        Ok(TableKey::Light(domain, bits))
    } else {
        Err(SnapshotError::InvalidStructure)
    }
}

fn native_key(image: &Image, index: u32) -> Result<TableKey, SnapshotError> {
    if (index as usize) < image.natives.len() {
        Ok(TableKey::Native(index))
    } else {
        Err(SnapshotError::DanglingReference)
    }
}

fn canonical_dead_key(
    image: &Image,
    kinds: &std::collections::HashMap<u64, Kind>,
    key: &DeadKey,
) -> Result<TableKey, SnapshotError> {
    Ok(match key {
        DeadKey::Bool(bit) => TableKey::Bool(*bit),
        DeadKey::Integer(integer) => TableKey::Integer(*integer),
        DeadKey::Float(bits) => canonical_float_key(*bits)?,
        DeadKey::Bytes(bytes) => TableKey::string(bytes.clone()),
        DeadKey::Object(id) => {
            if *id == 0 || *id >= image.next_object_id {
                return Err(SnapshotError::InvalidStructure);
            }
            if let Some(kind) = kinds.get(id) {
                match kind {
                    Kind::Table
                    | Kind::Closure
                    | Kind::Thread
                    | Kind::NativeClosure
                    | Kind::Userdata => {}
                    Kind::String | Kind::Proto | Kind::Upvalue => {
                        return Err(SnapshotError::InvalidStructure);
                    }
                }
            }
            TableKey::Object(ObjectId(*id))
        }
        DeadKey::Native(index) => native_key(image, *index)?,
        DeadKey::Light(domain, bits) => light_key(image, *domain, *bits)?,
    })
}

/// A float key is canonical when the table itself would store it as that
/// float: not NaN, and not a float that normalizes to an integer key.
fn canonical_float_key(bits: u64) -> Result<TableKey, SnapshotError> {
    match crate::table::normalize_key(Value::Float(f64::from_bits(bits)), None, None) {
        Ok(TableKey::Float(canonical)) if canonical == bits => Ok(TableKey::Float(bits)),
        _ => Err(SnapshotError::InvalidStructure),
    }
}

fn realized_dead_key(key: &DeadKey) -> Result<TableKey, SnapshotError> {
    Ok(match key {
        DeadKey::Bool(bit) => TableKey::Bool(*bit),
        DeadKey::Integer(integer) => TableKey::Integer(*integer),
        DeadKey::Float(bits) => canonical_float_key(*bits)?,
        DeadKey::Bytes(bytes) => TableKey::string(bytes.clone()),
        DeadKey::Object(id) => {
            if *id == 0 {
                return Err(SnapshotError::InvalidStructure);
            }
            TableKey::Object(ObjectId(*id))
        }
        // Range-checked by `canonical_dead_key` during validation.
        DeadKey::Native(index) => TableKey::Native(*index),
        DeadKey::Light(domain, bits) => TableKey::Light(*domain, *bits),
    })
}

fn require_value(
    require: &dyn Fn(u64, Kind) -> Result<(), SnapshotError>,
    value: &EncValue,
) -> Result<(), SnapshotError> {
    match value {
        EncValue::Nil | EncValue::Bool(_) | EncValue::Integer(_) | EncValue::Float(_) => Ok(()),
        EncValue::String(id) => require(*id, Kind::String),
        EncValue::Table(id) => require(*id, Kind::Table),
        EncValue::Closure(id) => require(*id, Kind::Closure),
        EncValue::Thread(id) => require(*id, Kind::Thread),
        EncValue::NativeClosure(id) => require(*id, Kind::NativeClosure),
        EncValue::Userdata(id) => require(*id, Kind::Userdata),
        // The index, and a token's range, are checked when the value is
        // decoded.
        EncValue::Native(_) | EncValue::Light(..) => Ok(()),
    }
}

/// The limits a restored runtime runs under (ADR 0052): for each, the
/// smaller of the snapshot's and the host's.
fn effective(image: &Image, limits: &Limits) -> Limits {
    Limits {
        max_logical_heap: image.gc.quota.min(limits.max_logical_heap),
        max_objects: image.max_objects.min(limits.max_objects),
        max_stack_slots: image.max_stack_slots.min(limits.max_stack_slots),
        max_string_bytes: u64::from(image.max_string).min(limits.max_string_bytes),
        max_snapshot_bytes: limits.max_snapshot_bytes,
    }
}

fn realize(
    image: &Image,
    registry: &HostRegistry,
    limits: &Limits,
    capabilities: &crate::HostCapabilities,
) -> Result<Runtime, SnapshotError> {
    check_symbols(image, registry)?;
    check_userdata_policies(image, registry)?;
    let within = effective(image, limits);
    // A smaller host bound the state does not fit refuses the snapshot.
    if !slots_fit(image, within.max_stack_slots)
        || image
            .strings
            .iter()
            .any(|(_, bytes)| bytes.len() as u64 > within.max_string_bytes)
    {
        return Err(SnapshotError::LimitExceeded);
    }
    let mut heap = Heap::new();
    heap.gc = image.gc.clone();
    heap.max_string = within.max_string_bytes as usize;
    heap.library = image.library.clone();
    // Objects come back with the current white; the marks that differ
    // are set once every object is made.
    heap.collector.white = image.collector.white;
    crate::gc::restore_policy(&mut heap);
    let mut strings = std::collections::HashMap::<u64, Handle<StringObj>>::new();
    let mut tables = std::collections::HashMap::<u64, Handle<TableObj>>::new();
    let mut protos = std::collections::HashMap::<u64, Handle<Proto>>::new();
    let mut upvalues = std::collections::HashMap::<u64, Handle<UpvalueObj>>::new();
    let mut closures = std::collections::HashMap::<u64, Handle<ClosureObj>>::new();
    let mut threads = std::collections::HashMap::<u64, Handle<ThreadObj>>::new();
    let mut native_closures = std::collections::HashMap::<u64, Handle<NativeClosureObj>>::new();

    for (id, bytes) in &image.strings {
        let handle = heap
            .strings
            .alloc(StringObj {
                id: ObjectId(*id),
                hash: std::cell::OnceCell::new(),
                bytes: bytes.clone(),
            })
            .map_err(|_| SnapshotError::LimitExceeded)?;
        strings.insert(*id, handle);
    }
    for table in &image.tables {
        let handle = heap
            .tables
            .alloc(TableObj {
                id: ObjectId(table.id),
                table: Table::new(),
                metatable: None,
                finalize: false,
            })
            .map_err(|_| SnapshotError::LimitExceeded)?;
        tables.insert(table.id, handle);
    }
    for proto in &image.protos {
        let handle = heap
            .protos
            .alloc(Proto {
                id: ObjectId(proto.id),
                ops: proto.ops.clone(),
                field_hints: Proto::empty_field_hints(&proto.ops),
                byte_consts: proto.byte_consts.clone(),
                byte_hashes: proto
                    .const_ids
                    .iter()
                    .map(|id| {
                        heap.strings
                            .get(strings[id])
                            .expect("restored string")
                            .hash()
                    })
                    .collect(),
                const_strings: proto
                    .const_ids
                    .iter()
                    .map(|id| {
                        strings
                            .get(id)
                            .copied()
                            .ok_or(SnapshotError::DanglingReference)
                    })
                    .collect::<Result<_, _>>()?,
                captures: proto.captures.clone(),
                children: Vec::new(),
                max_reg: proto.max_reg,
                params: proto.params,
                vararg: proto.vararg,
                debug: proto.debug.clone(),
                source: if proto.source == 0 {
                    None
                } else {
                    Some(
                        *strings
                            .get(&proto.source)
                            .ok_or(SnapshotError::DanglingReference)?,
                    )
                },
            })
            .map_err(|_| SnapshotError::LimitExceeded)?;
        protos.insert(proto.id, handle);
    }
    for upvalue in &image.upvalues {
        let handle = heap
            .upvalues
            .alloc(UpvalueObj {
                id: ObjectId(upvalue.id),
                state: UpvalueState::Closed(Value::Nil),
            })
            .map_err(|_| SnapshotError::LimitExceeded)?;
        upvalues.insert(upvalue.id, handle);
    }
    for closure in &image.closures {
        let proto = *protos
            .get(&closure.proto)
            .ok_or(SnapshotError::DanglingReference)?;
        let handle = heap
            .closures
            .alloc(ClosureObj {
                id: ObjectId(closure.id),
                proto,
                upvalues: Vec::new(),
            })
            .map_err(|_| SnapshotError::LimitExceeded)?;
        closures.insert(closure.id, handle);
    }
    for thread in &image.threads {
        let handle = heap
            .threads
            .alloc(ThreadObj {
                id: ObjectId(thread.id),
                status: Status::from_u8(thread.status).ok_or(SnapshotError::InvalidTag)?,
                stack: Default::default(),
                top: 0,
                frames: Default::default(),
                open_upvalues: Vec::new(),
                open_above: 0,
                resumed_by: None,
                host_results: Vec::new(),
                unwind: None,
                error: None,
                coroutine: false,
                closing: false,
                tbc: Vec::new(),
                charged_slots: thread.charged_slots,
                charged_held: thread.charged_held,
            })
            .map_err(|_| SnapshotError::LimitExceeded)?;
        threads.insert(thread.id, handle);
    }
    for closure in &image.native_closures {
        if closure.native as usize >= image.natives.len() {
            return Err(SnapshotError::DanglingReference);
        }
        let handle = heap
            .native_closures
            .alloc(NativeClosureObj {
                id: ObjectId(closure.id),
                native: closure.native,
                values: Vec::new(),
                state: closure.state.clone(),
            })
            .map_err(|_| SnapshotError::LimitExceeded)?;
        native_closures.insert(closure.id, handle);
    }
    // Host values are decoded into this heap only: a refusal anywhere
    // drops the heap and every value decoded so far (ADR 0045).
    let mut userdata = std::collections::HashMap::<u64, Handle<crate::heap::UserdataObj>>::new();
    for image_userdata in &image.userdata {
        let payload = realize_payload(
            &image_userdata.payload,
            image_userdata.charge,
            registry,
            capabilities,
            image_userdata.id,
            completed_file_close(image, image_userdata.id),
        )?;
        let handle = heap
            .userdata
            .alloc(crate::heap::UserdataObj {
                id: ObjectId(image_userdata.id),
                metatable: None,
                user_values: vec![Value::Nil; image_userdata.user_values.len()].into_boxed_slice(),
                payload,
                charge: image_userdata.charge,
                finalize: false,
            })
            .map_err(|_| SnapshotError::LimitExceeded)?;
        userdata.insert(image_userdata.id, handle);
    }
    let handles = Handles {
        strings: &strings,
        tables: &tables,
        closures: &closures,
        threads: &threads,
        native_closures: &native_closures,
        userdata: &userdata,
        natives: image.natives.len(),
        next_object_id: image.next_object_id,
    };
    for image_userdata in &image.userdata {
        let values = image_userdata
            .user_values
            .iter()
            .map(|value| dec_value(&handles, value))
            .collect::<Result<Vec<_>, _>>()?;
        let metatable = if image_userdata.metatable == 0 {
            None
        } else {
            Some(
                *tables
                    .get(&image_userdata.metatable)
                    .ok_or(SnapshotError::DanglingReference)?,
            )
        };
        let object = heap
            .userdata
            .get_mut(userdata[&image_userdata.id])
            .ok_or(SnapshotError::DanglingReference)?;
        object.user_values = values.into_boxed_slice();
        object.metatable = metatable;
    }
    for closure in &image.native_closures {
        let values = closure
            .values
            .iter()
            .map(|value| dec_value(&handles, value))
            .collect::<Result<Vec<_>, _>>()?;
        let entry = registry
            .native_slot(&image.natives[closure.native as usize])
            .and_then(|slot| registry.native(slot))
            .ok_or(SnapshotError::UnknownHostSymbol)?;
        let fits = match entry.builtin {
            Some(builtin) => crate::library::closure_fits(&heap, builtin, &values, &closure.state),
            None => values.len() <= 255 && closure.state.is_empty(),
        };
        if !fits {
            return Err(SnapshotError::InvalidStructure);
        }
        let handle = native_closures[&closure.id];
        heap.native_closures
            .get_mut(handle)
            .ok_or(SnapshotError::DanglingReference)?
            .values = values;
    }

    for proto in &image.protos {
        let handle = *protos
            .get(&proto.id)
            .ok_or(SnapshotError::DanglingReference)?;
        let children = proto
            .children
            .iter()
            .map(|id| {
                protos
                    .get(id)
                    .copied()
                    .ok_or(SnapshotError::DanglingReference)
            })
            .collect::<Result<Vec<_>, _>>()?;
        heap.protos
            .get_mut(handle)
            .ok_or(SnapshotError::DanglingReference)?
            .children = children;
    }
    for closure in &image.closures {
        let handle = *closures
            .get(&closure.id)
            .ok_or(SnapshotError::DanglingReference)?;
        let ups = closure
            .upvalues
            .iter()
            .map(|id| {
                upvalues
                    .get(id)
                    .copied()
                    .ok_or(SnapshotError::DanglingReference)
            })
            .collect::<Result<Vec<_>, _>>()?;
        heap.closures
            .get_mut(handle)
            .ok_or(SnapshotError::DanglingReference)?
            .upvalues = ups;
    }
    for upvalue in &image.upvalues {
        let handle = *upvalues
            .get(&upvalue.id)
            .ok_or(SnapshotError::DanglingReference)?;
        let state = match &upvalue.state {
            UpImageState::Closed(value) => UpvalueState::Closed(dec_value(&handles, value)?),
            UpImageState::Open { thread, slot } => {
                let thread = *threads
                    .get(thread)
                    .ok_or(SnapshotError::DanglingReference)?;
                UpvalueState::Open {
                    thread,
                    slot: *slot,
                }
            }
        };
        heap.upvalues
            .get_mut(handle)
            .ok_or(SnapshotError::DanglingReference)?
            .state = state;
    }
    for table in &image.tables {
        let handle = *tables
            .get(&table.id)
            .ok_or(SnapshotError::DanglingReference)?;
        if table.metatable != 0 {
            let metatable = *tables
                .get(&table.metatable)
                .ok_or(SnapshotError::DanglingReference)?;
            heap.tables
                .get_mut(handle)
                .ok_or(SnapshotError::DanglingReference)?
                .metatable = Some(metatable);
        }
        let mut slots = Vec::with_capacity(table.slots.len());
        for slot in &table.slots {
            match &slot.body {
                SlotBody::Live { key, value } => {
                    let key_value = dec_value(&handles, key)?;
                    let stored = dec_value(&handles, value)?;
                    if matches!(stored, Value::Nil) {
                        return Err(SnapshotError::InvalidStructure);
                    }
                    let normalized = heap
                        .normalize_value(key_value)
                        .map_err(|_| SnapshotError::InvalidStructure)?;
                    slots.push(Slot::Live {
                        key: normalized,
                        key_value,
                        value: stored,
                        next_live: None,
                        prev_live: None,
                    });
                }
                SlotBody::Dead(key) => slots.push(Slot::Dead {
                    key: realized_dead_key(key)?,
                    next_live: None,
                }),
            }
        }
        heap.tables
            .get_mut(handle)
            .ok_or(SnapshotError::DanglingReference)?
            .table
            .restore(slots);
    }
    for thread in &image.threads {
        let handle = *threads
            .get(&thread.id)
            .ok_or(SnapshotError::DanglingReference)?;
        let stack = thread
            .stack
            .iter()
            .map(|value| dec_value(&handles, value))
            .collect::<Result<Vec<_>, _>>()?;
        let results = thread
            .results
            .iter()
            .map(|value| dec_value(&handles, value))
            .collect::<Result<Vec<_>, _>>()?;
        let resumed_by = if thread.resumed_by == 0 {
            None
        } else {
            Some(
                *threads
                    .get(&thread.resumed_by)
                    .ok_or(SnapshotError::DanglingReference)?,
            )
        };
        let mut frames = Vec::new();
        for frame in &thread.frames {
            let closure = *closures
                .get(&frame.closure)
                .ok_or(SnapshotError::DanglingReference)?;
            let pending = match &frame.pending {
                PendingImage::None => None,
                PendingImage::Prepared {
                    sequence,
                    symbol,
                    arg,
                    dest,
                } => Some(Pending::Prepared {
                    sequence: *sequence,
                    symbol: symbol.clone(),
                    arg: *arg,
                    dest: *dest,
                }),
                PendingImage::Waiting {
                    sequence,
                    symbol,
                    arg,
                    dest,
                    wait_key,
                } => Some(Pending::Waiting {
                    sequence: *sequence,
                    symbol: symbol.clone(),
                    arg: *arg,
                    dest: *dest,
                    wait_key: *wait_key,
                }),
                PendingImage::Resuming {
                    child,
                    dest,
                    nresults,
                } => Some(Pending::Resuming {
                    child: *threads.get(child).ok_or(SnapshotError::DanglingReference)?,
                    dest: *dest,
                    nresults: *nresults,
                }),
                PendingImage::Assigning { src, nvalues, next } => Some(Pending::Assigning {
                    src: *src,
                    nvalues: *nvalues,
                    next: *next,
                }),
                PendingImage::NativePrepared { sequence } => Some(Pending::NativePrepared {
                    sequence: *sequence,
                }),
                PendingImage::Deferred => Some(Pending::Deferred),
                PendingImage::Capability {
                    sequence,
                    wait_key,
                    completed,
                } => Some(Pending::Capability {
                    sequence: *sequence,
                    wait_key: *wait_key,
                    completed: *completed,
                }),
                PendingImage::NativeWaiting { sequence, wait_key } => {
                    Some(Pending::NativeWaiting {
                        sequence: *sequence,
                        wait_key: *wait_key,
                    })
                }
            };
            let mut targets = Vec::new();
            for target in &frame.targets {
                targets.push(match target {
                    TargetImage::Register(slot) => AssignTarget::Register(*slot),
                    TargetImage::Field { table, key } => AssignTarget::Field {
                        table: dec_value(&handles, table)?,
                        key: dec_value(&handles, key)?,
                    },
                });
            }
            frames.push(Frame {
                closure,
                pc: frame.pc,
                base: frame.base,
                limit: frame.limit,
                nresults: frame.nresults,
                vararg_len: frame.vararg_len,
                flags: u8::from(frame.tail) | (u8::from(frame.return_hook) * 2),
                cold: crate::heap::FrameCold {
                    pending,
                    wait_request: frame
                        .wait_request
                        .as_ref()
                        .map(|(operation, payload)| {
                            Ok::<_, SnapshotError>(crate::heap::HostWait {
                                operation: operation.clone(),
                                payload: payload
                                    .iter()
                                    .map(|v| dec_value(&handles, v))
                                    .collect::<Result<Vec<_>, _>>()?,
                            })
                        })
                        .transpose()?,
                    targets,
                    meta: frame
                        .meta
                        .as_ref()
                        .map(|meta| realize_meta(meta, &|value| dec_value(&handles, value)))
                        .transpose()?,
                    boundary: match &frame.boundary {
                        None => None,
                        Some(BoundaryImage::Protect {
                            func,
                            advance_caller,
                            handler,
                        }) => Some(crate::heap::Boundary::Protect {
                            func: *func,
                            advance_caller: *advance_caller,
                            handler: handler
                                .as_ref()
                                .map(|value| dec_value(&handles, value))
                                .transpose()?,
                        }),
                        Some(BoundaryImage::Handler {
                            slot,
                            protect,
                            target,
                            depth,
                            fault,
                        }) => Some(crate::heap::Boundary::Handler {
                            slot: *slot,
                            protect: *protect,
                            target: *target,
                            depth: *depth,
                            fault: crate::id::LuaFault::from_tag(*fault)
                                .ok_or(SnapshotError::InvalidTag)?,
                        }),
                        Some(BoundaryImage::Builtin {
                            func,
                            passed,
                            advance_caller,
                            task,
                        }) => Some(crate::heap::Boundary::Builtin {
                            func: *func,
                            passed: *passed,
                            advance_caller: *advance_caller,
                            task: task.clone(),
                        }),
                        Some(BoundaryImage::Native {
                            func,
                            passed,
                            advance_caller,
                            symbol,
                            tag,
                            kept,
                            sequence,
                            error,
                            resuming,
                        }) => Some(crate::heap::Boundary::Native {
                            func: *func,
                            passed: *passed,
                            advance_caller: *advance_caller,
                            symbol: *symbol,
                            tag: *tag,
                            kept: *kept,
                            sequence: *sequence,
                            resuming: *resuming,
                            error: error
                                .as_ref()
                                .map(|(class, value)| {
                                    Ok::<_, SnapshotError>((
                                        crate::id::LuaFault::from_tag(*class)
                                            .ok_or(SnapshotError::InvalidTag)?,
                                        dec_value(&handles, value)?,
                                    ))
                                })
                                .transpose()?,
                        }),
                        Some(BoundaryImage::Hook {
                            func,
                            saved_top,
                            target,
                            instruction,
                            after,
                        }) => Some(crate::heap::Boundary::Hook {
                            func: *func,
                            saved_top: *saved_top,
                            target: *target,
                            instruction: *instruction,
                            after: *after,
                        }),
                        Some(BoundaryImage::HookNative {
                            func,
                            passed,
                            callee,
                            advance_caller,
                            phase,
                            produced,
                            result,
                        }) => Some(crate::heap::Boundary::HookNative {
                            func: *func,
                            passed: *passed,
                            callee: dec_value(&handles, callee)?,
                            advance_caller: *advance_caller,
                            phase: *phase,
                            produced: *produced,
                            result: *result,
                        }),
                        Some(BoundaryImage::Finalizer { func, saved_top }) => {
                            Some(crate::heap::Boundary::Finalizer {
                                func: *func,
                                saved_top: *saved_top,
                            })
                        }
                    },
                }
                .into_box(),
            });
        }
        let decode = |value: &EncValue| dec_value(&handles, value);
        let error = match &thread.error {
            Some((fault, value)) => Some((
                crate::id::LuaFault::from_tag(*fault).ok_or(SnapshotError::InvalidTag)?,
                decode(value)?,
            )),
            None => None,
        };
        let unwind = thread
            .unwind
            .as_ref()
            .map(|unwind| realize_unwind(unwind, &decode).map(Box::new))
            .transpose()?;
        let object = heap
            .threads
            .get_mut(handle)
            .ok_or(SnapshotError::DanglingReference)?;
        object.stack = stack.into();
        object.top = thread.top;
        object.host_results = results;
        object.resumed_by = resumed_by;
        object.frames = frames.into();
        object.error = error;
        object.unwind = unwind;
        object.coroutine = thread.coroutine;
        object.closing = thread.closing;
        object.tbc = thread.tbc.clone();
        // A thread is charged at least what it holds (ADR 0051).
        if object.extent() > object.charged_slots
            || object.charged_slots > image.max_stack_slots
            || object.held_bytes() > object.charged_held
            || object.charged_held > image.gc.quota
        {
            return Err(SnapshotError::InvalidStructure);
        }
    }
    for thread in &image.threads {
        if let Some(hook) = &thread.hook {
            let owner = *handles
                .threads
                .get(&thread.id)
                .ok_or(SnapshotError::DanglingReference)?;
            heap.hooks.insert(
                ObjectId(thread.id),
                realize_hook(hook, owner, registry, &|v| dec_value(&handles, v))?,
            );
        }
    }
    heap.reserved = image
        .reserved
        .iter()
        .map(|id| {
            strings
                .get(id)
                .copied()
                .ok_or(SnapshotError::DanglingReference)
        })
        .collect::<Result<_, _>>()?;
    // Rebuild open-upvalue lists from upvalue objects. Do it with copied triples
    // so we can mutate threads afterward.
    let opens: Vec<(u32, u32, Handle<ThreadObj>, u32)> = heap
        .upvalues
        .iter()
        .filter_map(|(index, generation, upvalue)| match upvalue.state {
            UpvalueState::Open { thread, slot } => Some((index, generation, thread, slot)),
            UpvalueState::Closed(_) => None,
        })
        .collect();
    for (index, generation, thread, slot) in opens {
        let up_handle = Handle::new(index, generation);
        heap.threads
            .get_mut(thread)
            .ok_or(SnapshotError::DanglingReference)?
            .push_open(slot, up_handle);
    }

    for (slot, id) in heap.type_metatables.iter_mut().zip(image.type_metatables) {
        *slot = if id == 0 {
            None
        } else {
            Some(*tables.get(&id).ok_or(SnapshotError::DanglingReference)?)
        };
    }
    heap.registry = if image.registry == 0 {
        None
    } else {
        Some(
            *tables
                .get(&image.registry)
                .ok_or(SnapshotError::DanglingReference)?,
        )
    };
    heap.globals = if image.globals == 0 {
        None
    } else {
        Some(
            *tables
                .get(&image.globals)
                .ok_or(SnapshotError::DanglingReference)?,
        )
    };
    heap.active = if image.active == 0 {
        None
    } else {
        Some(
            *threads
                .get(&image.active)
                .ok_or(SnapshotError::DanglingReference)?,
        )
    };
    heap.entry = if image.entry == 0 {
        None
    } else {
        Some(
            *threads
                .get(&image.entry)
                .ok_or(SnapshotError::DanglingReference)?,
        )
    };
    heap.next_object_id = image.next_object_id;
    heap.natives = image.natives.clone();
    let fin_ref = |heap: &mut Heap, id: &u64| -> Result<crate::heap::FinRef, SnapshotError> {
        let fin = if let Some(handle) = tables.get(id) {
            heap.tables
                .get_mut(*handle)
                .ok_or(SnapshotError::DanglingReference)?
                .finalize = true;
            crate::heap::FinRef::Table(*handle)
        } else {
            let handle = *userdata.get(id).ok_or(SnapshotError::DanglingReference)?;
            heap.userdata
                .get_mut(handle)
                .ok_or(SnapshotError::DanglingReference)?
                .finalize = true;
            crate::heap::FinRef::Userdata(handle)
        };
        Ok(fin)
    };
    for id in &image.finalizers.registered {
        let fin = fin_ref(&mut heap, id)?;
        heap.finalizers.registered.push(fin);
    }
    heap.finalizers.old_until = image.finalizers.old_until;
    heap.finalizers.new_from = image.finalizers.new_from;
    for id in &image.finalizers.pending {
        let fin = fin_ref(&mut heap, id)?;
        heap.finalizers.pending.push_back(fin);
    }
    heap.finalizers.running = image.finalizers.running;
    heap.finalizers.closing = image.finalizers.closing;
    heap.finalizers.exit = image.finalizers.exit;
    heap.finalizers.closed = Status::from_u8(image.finalizers.closed);
    heap.finalizers.close_closure = if image.finalizers.close_closure == 0 {
        None
    } else {
        Some(
            *closures
                .get(&image.finalizers.close_closure)
                .ok_or(SnapshotError::DanglingReference)?,
        )
    };
    // The runtime never holds more objects than its limit, but for the
    // closure closing makes (ADR 0048).
    let close_room = if image.finalizers.close_closure != 0 {
        crate::runtime::finalize_close_objects()
    } else {
        0
    };
    if heap.live_objects() > within.max_objects.saturating_add(close_room) {
        return Err(SnapshotError::LimitExceeded);
    }
    realize_collector(image, &mut heap, &handles, &protos, &upvalues)?;
    // A host may have grown its userdata past the quota (ADR 0042): that
    // heap restores under a quota as large, never under a smaller one.
    if within.max_logical_heap < image.gc.quota && heap.gc.used > within.max_logical_heap {
        return Err(SnapshotError::LimitExceeded);
    }
    heap.gc.quota = within.max_logical_heap;

    Ok(Runtime::from_restored(crate::runtime::RestoredParts {
        registry: registry.clone(),
        heap,
        effect_domain: image.effect_domain,
        next_sequence: image.next_sequence,
        fuel_consumed: image.fuel_consumed,
        fuel_limit: image.fuel_limit,
        max_objects: within.max_objects,
        max_snapshot: within.max_snapshot_bytes,
        max_stack_slots: within.max_stack_slots,
        last_completed_wait: image.last_completed_wait,
        completed_waits: image.completed_waits.iter().copied().collect(),
        host_call: image.host_call,
        callback_failed: image.callback_failed,
        trap: trap_from(image.trap)?,
    }))
}

/// The collector as it was (ADR 0050): marks, lists, and phase. Run once
/// every object is made, before the arenas take the phase's policy.
fn realize_collector(
    image: &Image,
    heap: &mut Heap,
    handles: &Handles<'_>,
    protos: &std::collections::HashMap<u64, Handle<Proto>>,
    upvalues: &std::collections::HashMap<u64, Handle<UpvalueObj>>,
) -> Result<(), SnapshotError> {
    use crate::gc::{Atomic, How, Phase, Scan, WeakTable};
    use crate::heap::TraceRef;
    let c = &image.collector;
    let object = |id: u64| -> Result<TraceRef, SnapshotError> {
        let found = |kind, index| Some(TraceRef { kind, index });
        handles
            .strings
            .get(&id)
            .and_then(|handle| found(Kind::String, handle.index))
            .or_else(|| {
                handles
                    .tables
                    .get(&id)
                    .and_then(|handle| found(Kind::Table, handle.index))
            })
            .or_else(|| {
                protos
                    .get(&id)
                    .and_then(|handle| found(Kind::Proto, handle.index))
            })
            .or_else(|| {
                upvalues
                    .get(&id)
                    .and_then(|handle| found(Kind::Upvalue, handle.index))
            })
            .or_else(|| {
                handles
                    .closures
                    .get(&id)
                    .and_then(|handle| found(Kind::Closure, handle.index))
            })
            .or_else(|| {
                handles
                    .threads
                    .get(&id)
                    .and_then(|handle| found(Kind::Thread, handle.index))
            })
            .or_else(|| {
                handles
                    .native_closures
                    .get(&id)
                    .and_then(|handle| found(Kind::NativeClosure, handle.index))
            })
            .or_else(|| {
                handles
                    .userdata
                    .get(&id)
                    .and_then(|handle| found(Kind::Userdata, handle.index))
            })
            .ok_or(SnapshotError::DanglingReference)
    };
    let table = |id: u64| -> Result<u32, SnapshotError> {
        handles
            .tables
            .get(&id)
            .map(|handle| handle.index)
            .ok_or(SnapshotError::DanglingReference)
    };
    let mut young = HashSet::new();
    for id in &c.young {
        let found = object(*id)?;
        young.insert(found);
        crate::heap::on_arena!(mut heap, found.kind, arena => arena.young_mut().push(found.index));
    }
    // In generational form, black and old off the young lists unless
    // written otherwise.
    if c.generational {
        for kind in crate::gc::KINDS {
            let indices: Vec<u32> = crate::heap::on_arena!(heap, kind, arena => {
                arena.iter().map(|(index, _, _)| index).collect()
            });
            for index in indices {
                let found = TraceRef { kind, index };
                if !young.contains(&found) {
                    heap.set_mark(found, crate::heap::mark::BLACK);
                    heap.set_age(found, crate::heap::age::OLD);
                }
            }
        }
    }
    for (id, mark, age) in &c.marks {
        let found = object(*id)?;
        heap.set_mark(found, *mark);
        heap.set_age(found, *age);
    }
    for (id, age) in &c.ages {
        let found = object(*id)?;
        heap.set_mark(found, c.white);
        heap.set_age(found, *age);
    }
    let revisit = c
        .revisit
        .iter()
        .map(|id| object(*id))
        .collect::<Result<Vec<_>, _>>()?;
    let touched = c
        .touched
        .iter()
        .map(|id| object(*id))
        .collect::<Result<Vec<_>, _>>()?;
    let collector = &mut heap.collector;
    collector.generational = c.generational;
    collector.minor = c.minor;
    collector.to_old = c.to_old;
    collector.decide =
        crate::gc::Decide::from_tag(c.decide).ok_or(SnapshotError::InvalidStructure)?;
    collector.unreleased = c.unreleased;
    collector.promoted = c.promoted;
    collector.revisit = revisit;
    collector.touched = touched;
    collector.phase = match (c.phase, Atomic::from_tag(c.atomic)) {
        (0, _) => Phase::Pause,
        (1, _) => Phase::Begin,
        (2, _) => Phase::Propagate,
        (3, Some(step)) => Phase::Atomic(step),
        (4, _) => Phase::Sweep,
        (5, _) => Phase::Touched,
        _ => return Err(SnapshotError::InvalidStructure),
    };
    collector.gray = c
        .gray
        .iter()
        .map(|id| object(*id))
        .collect::<Result<_, _>>()?;
    collector.scan = match c.scan {
        Some((id, pos, how, keys, values)) => Some(Scan {
            object: object(id)?,
            pos,
            how: match how {
                0 => How::Object,
                1 => How::Entries { keys, values },
                2 => How::Ephemeron,
                _ => return Err(SnapshotError::InvalidTag),
            },
        }),
        None => None,
    };
    collector.weak = c
        .weak
        .iter()
        .map(|(id, keys, values)| {
            Ok(WeakTable {
                table: table(*id)?,
                keys: *keys,
                values: *values,
            })
        })
        .collect::<Result<_, SnapshotError>>()?;
    collector.late = c.late;
    collector.ephemerons = c
        .ephemerons
        .iter()
        .map(|id| table(*id))
        .collect::<Result<_, _>>()?;
    collector.waiting = c
        .waiting
        .iter()
        .map(|(key, values)| {
            let key = object(*key)?;
            Ok((
                (key.kind, key.index),
                values
                    .iter()
                    .map(|value| dec_value(handles, value))
                    .collect::<Result<Vec<_>, _>>()?,
            ))
        })
        .collect::<Result<_, SnapshotError>>()?;
    collector.cursor = c.cursor;
    collector.inner = c.inner;
    collector.marked_bytes = c.marked_bytes;
    collector.debt_base = c.debt_base;
    collector.marking_debt = c.marking_debt;
    collector.work_base = c.work_base;
    collector.sweep_left = c.sweep_left;
    collector.reset = c.reset;
    for id in &c.again {
        let found = object(*id)?;
        match found.kind {
            Kind::String => heap.strings.push_again(found.index),
            Kind::Table => heap.tables.push_again(found.index),
            Kind::Proto => heap.protos.push_again(found.index),
            Kind::Upvalue => heap.upvalues.push_again(found.index),
            Kind::Closure => heap.closures.push_again(found.index),
            Kind::Thread => heap.threads.push_again(found.index),
            Kind::NativeClosure => heap.native_closures.push_again(found.index),
            Kind::Userdata => heap.userdata.push_again(found.index),
        }
    }
    // The logical heap is counted, never read: what the objects written
    // count, and what the sweep running frees of those not written. (A
    // host's userdata may have grown past the quota; the next allocation
    // sees it.)
    heap.gc.used = crate::gc::counted_bytes(heap);
    crate::gc::restore_policy(heap);
    // No black object refers to a white one: a sweep would free what it
    // still reaches. In generational form, no old object refers to a
    // young one but those the next young collection traces.
    crate::gc::check_invariant(heap).map_err(|_| SnapshotError::InvalidStructure)?;
    crate::gc::check_gen_invariant(heap).map_err(|_| SnapshotError::InvalidStructure)
}

// An acquisition can already own a host resource while its Lua userdata still
// says closed. Pending work must obey the same policy as installed live handles.
fn check_pending_resources(
    image: &Image,
    capabilities: Option<&crate::HostCapabilities>,
) -> Result<(), SnapshotError> {
    use crate::hostcaps::{CapabilityRequest as Request, CapabilityValue as Answer, HandlePolicy};
    for thread in &image.threads {
        for frame in &thread.frames {
            let PendingImage::Capability { completed, .. } = frame.pending else {
                continue;
            };
            let Some(BoundaryImage::Builtin {
                func,
                passed,
                task: crate::heap::Task::Io(work),
                ..
            }) = &frame.boundary
            else {
                continue;
            };
            if !matches!(work.as_ref(), crate::iolib::IoWork::Open { .. }) {
                continue;
            }
            let slot = u64::from(*func) + 1 + u64::from(*passed);
            let Some(EncValue::Userdata(file)) =
                usize::try_from(slot).ok().and_then(|i| thread.stack.get(i))
            else {
                return Err(SnapshotError::InvalidStructure);
            };
            let (_, payload) = frame
                .wait_request
                .as_ref()
                .ok_or(SnapshotError::InvalidStructure)?;
            let Some(EncValue::String(request)) = payload.first() else {
                return Err(SnapshotError::InvalidStructure);
            };
            let request = Request::from_bytes(
                image
                    .string(*request)
                    .ok_or(SnapshotError::DanglingReference)?,
            )
            .map_err(|_| SnapshotError::InvalidStructure)?;
            if !matches!(
                request,
                Request::FilesystemOpen { .. }
                    | Request::FilesystemTempFile
                    | Request::ProcessPopen { .. }
            ) {
                return Err(SnapshotError::InvalidStructure);
            }
            let resource = if completed {
                let Some(EncValue::String(result)) = payload.get(2) else {
                    return Err(SnapshotError::InvalidStructure);
                };
                match crate::hostcaps::protocol::decode(
                    image
                        .string(*result)
                        .ok_or(SnapshotError::DanglingReference)?,
                )
                .map_err(|_| SnapshotError::InvalidStructure)?
                {
                    Ok(Answer::Resource(id)) => Some(id),
                    Err(_) => continue,
                    _ => return Err(SnapshotError::InvalidStructure),
                }
            } else {
                None
            };
            let object = ObjectId(*file);
            let state = image
                .userdata
                .iter()
                .find(|u| u.id == *file)
                .and_then(|u| match &u.payload {
                    PayloadImage::File(f) => Some(f),
                    _ => None,
                })
                .ok_or(SnapshotError::InvalidStructure)?;
            let crate::iolib::IoWork::Open { path, action } = work.as_ref() else {
                unreachable!()
            };
            let expected = if *action == 4 {
                Request::ProcessPopen {
                    cmd: path.clone(),
                    mode: if state.mode.read {
                        crate::PipeMode::Read
                    } else {
                        crate::PipeMode::Write
                    },
                }
            } else if state.kind == 1 {
                Request::FilesystemTempFile
            } else {
                Request::FilesystemOpen {
                    path: path.clone(),
                    mode: state.mode,
                }
            };
            if request != expected {
                return Err(SnapshotError::InvalidStructure);
            }
            if let Some(id) = resource && image.userdata.iter().any(|u| matches!(&u.payload, PayloadImage::File(f) if !f.closed && f.kind < 2 && f.id == id)) {
                return Err(SnapshotError::InvalidStructure);
            }
            if state.policy == HandlePolicy::Refuse
                || matches!(request, Request::ProcessPopen { .. })
            {
                return Err(SnapshotError::NonPortableResource { object });
            }
            let failed = |error| SnapshotError::Rebind {
                symbol: "FILE*",
                object,
                error: crate::RebindError(error),
            };
            let Some(capabilities) = capabilities else {
                continue;
            };
            let fs = capabilities
                .filesystem
                .as_ref()
                .ok_or_else(|| failed("filesystem unavailable"))?;
            let policy = crate::hostcaps::filesystem_policy(fs.as_ref())
                .map_err(|_| failed("filesystem policy panicked"))?;
            if policy != HandlePolicy::Rebind {
                return Err(SnapshotError::NonPortableResource { object });
            }
            if let Some(id) = resource {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fs.rebind(id)))
                    .map_err(|_| failed("filesystem rebind panicked"))?
                    .map_err(|_| failed("file resource unavailable"))?;
            }
        }
    }
    Ok(())
}

// Completion may already have released a host resource while the builtin has
// not yet consumed its journaled answer and marked the Lua handle closed.
// Rebinding that completed close would incorrectly reject a valid checkpoint.
fn completed_file_close(image: &Image, object: u64) -> bool {
    image.threads.iter().any(|thread| thread.frames.iter().any(|frame| {
        let PendingImage::Capability { completed: true, .. } = frame.pending else { return false; };
        let Some(BoundaryImage::Builtin { func, passed, task: crate::heap::Task::Io(task), .. }) = &frame.boundary else { return false; };
        if !matches!(task.as_ref(), crate::iolib::IoWork::Close { .. }) { return false; }
        let slot = u64::from(*func) + 1 + u64::from(*passed);
        if !matches!(usize::try_from(slot).ok().and_then(|i| thread.stack.get(i)), Some(EncValue::Userdata(id)) if *id == object) { return false; }
        let Some(wait) = &frame.wait_request else { return false; };
        let Some(EncValue::String(request)) = wait.1.first() else { return false; };
        let Some(bytes) = image.string(*request) else { return false; };
        matches!(crate::CapabilityRequest::from_bytes(bytes), Ok(crate::CapabilityRequest::FilesystemClose { id })
            if image.userdata.iter().any(|u| u.id == object && matches!(&u.payload, PayloadImage::File(f) if f.id == id && f.kind < 2)))
    }))
}

/// A userdata's payload from its image. A host value's type must be
/// registered as portable under the same symbol; its codec must accept
/// the bytes and give a value that counts no more than was recorded.
fn realize_payload(
    payload: &PayloadImage,
    charge: u64,
    registry: &HostRegistry,
    capabilities: &crate::HostCapabilities,
    id: u64,
    released: bool,
) -> Result<crate::userdata::Payload, SnapshotError> {
    Ok(match payload {
        PayloadImage::File(file) => {
            if !file.closed && !released {
                if file.kind < 2 {
                    let fs = capabilities
                        .filesystem
                        .as_ref()
                        .ok_or(SnapshotError::Rebind {
                            symbol: "FILE*",
                            object: ObjectId(id),
                            error: crate::RebindError("filesystem unavailable"),
                        })?;
                    let policy = crate::hostcaps::filesystem_policy(fs.as_ref()).map_err(|_| {
                        SnapshotError::Rebind {
                            symbol: "FILE*",
                            object: ObjectId(id),
                            error: crate::RebindError("filesystem policy panicked"),
                        }
                    })?;
                    if policy != crate::hostcaps::HandlePolicy::Rebind {
                        return Err(SnapshotError::NonPortableResource {
                            object: ObjectId(id),
                        });
                    }
                    // Rebind is a validation/reacquisition boundary, never an IO effect.
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fs.rebind(file.id)))
                        .map_err(|_| SnapshotError::Rebind {
                            symbol: "FILE*",
                            object: ObjectId(id),
                            error: crate::RebindError("filesystem rebind panicked"),
                        })?
                        .map_err(|_| SnapshotError::Rebind {
                            symbol: "FILE*",
                            object: ObjectId(id),
                            error: crate::RebindError("file resource unavailable"),
                        })?;
                } else if capabilities.stdio.is_none() {
                    return Err(SnapshotError::Rebind {
                        symbol: "FILE*",
                        object: ObjectId(id),
                        error: crate::RebindError("standard streams unavailable"),
                    });
                }
            }
            crate::userdata::Payload::File(Box::new(file.clone()))
        }
        PayloadImage::Bytes(bytes) => {
            crate::userdata::Payload::Bytes(bytes.clone().into_boxed_slice())
        }
        PayloadImage::Host { symbol, bytes } => {
            let host = registry
                .userdata_type(symbol)
                .ok_or(SnapshotError::UnknownUserdataType)?;
            let crate::userdata::Policy::Portable(codec) = host.policy else {
                return Err(SnapshotError::UserdataPolicyMismatch);
            };
            let (value, size) = (codec.decode)(bytes).ok_or(SnapshotError::UserdataDecode)?;
            if size > charge {
                return Err(SnapshotError::UserdataCharge);
            }
            crate::userdata::Payload::Host {
                symbol: host.symbol,
                value,
            }
        }
        PayloadImage::Rebind { symbol, key } => {
            if key.len() > crate::MAX_REBIND_KEY {
                return Err(SnapshotError::LimitExceeded);
            }
            let host = registry
                .userdata_type(symbol)
                .ok_or(SnapshotError::UnknownUserdataType)?;
            let crate::userdata::Policy::Rebind { rebind, .. } = host.policy else {
                return Err(SnapshotError::UserdataPolicyMismatch);
            };
            let (value, size) =
                rebind(key, capabilities.env()).map_err(|error| SnapshotError::Rebind {
                    symbol: host.symbol,
                    object: ObjectId(id),
                    error,
                })?;
            if size > charge {
                return Err(SnapshotError::UserdataCharge);
            }
            crate::userdata::Payload::Host {
                symbol: host.symbol,
                value,
            }
        }
    })
}

fn check_userdata_policies(image: &Image, registry: &HostRegistry) -> Result<(), SnapshotError> {
    for userdata in &image.userdata {
        let (symbol, policy) = match &userdata.payload {
            PayloadImage::Bytes(_) | PayloadImage::File(_) => continue,
            PayloadImage::Host { symbol, .. } => (symbol, crate::UserdataPolicy::Portable),
            PayloadImage::Rebind { symbol, key } => {
                if key.len() > crate::MAX_REBIND_KEY {
                    return Err(SnapshotError::LimitExceeded);
                }
                (symbol, crate::UserdataPolicy::Rebind)
            }
        };
        let registered = registry
            .userdata_policy(symbol)
            .ok_or(SnapshotError::UnknownUserdataType)?;
        if registered != policy {
            return Err(SnapshotError::UserdataPolicyMismatch);
        }
    }
    Ok(())
}

type Decode<'a> = dyn Fn(&EncValue) -> Result<Value, SnapshotError> + 'a;

fn realize_unwind(
    unwind: &UnwindImage,
    decode: &Decode<'_>,
) -> Result<crate::heap::Unwind, SnapshotError> {
    Ok(crate::heap::Unwind {
        error: unwind
            .error
            .as_ref()
            .map(|(fault, value)| {
                Ok::<_, SnapshotError>((
                    crate::id::LuaFault::from_tag(*fault).ok_or(SnapshotError::InvalidTag)?,
                    decode(value)?,
                ))
            })
            .transpose()?,
        phase: unwind.phase,
    })
}

fn realize_meta(
    meta: &MetaImage,
    decode: &Decode<'_>,
) -> Result<crate::heap::MetaCall, SnapshotError> {
    use crate::heap::{CloseNext, Closing, MetaEvent};
    let (event, close) = match &meta.event {
        // A plain `Close` event has no close state; the reader never makes
        // one, and restore refuses it.
        EventImage::Plain(MetaEvent::Close) => return Err(SnapshotError::InvalidStructure),
        EventImage::Plain(event) => (*event, None),
        EventImage::Close { from, next } => (
            MetaEvent::Close,
            Some(Box::new(Closing {
                from: *from,
                next: match next {
                    NextImage::Advance => CloseNext::Advance,
                    NextImage::Return { src, produced } => CloseNext::Return {
                        src: *src,
                        produced: *produced,
                    },
                    NextImage::Unwind(unwind) => CloseNext::Unwind(realize_unwind(unwind, decode)?),
                },
            })),
        ),
    };
    Ok(crate::heap::MetaCall {
        event,
        slot: meta.slot,
        nargs: meta.nargs,
        phase: meta.phase,
        close,
    })
}

fn check_symbols(image: &Image, registry: &HostRegistry) -> Result<(), SnapshotError> {
    for symbol in &image.natives {
        if registry.native_slot(symbol).is_none() {
            return Err(SnapshotError::UnknownHostSymbol);
        }
    }
    for thread in &image.threads {
        if let Some(HookImage {
            target: HookTargetImage::Host(symbol),
            ..
        }) = &thread.hook
            && registry.hook_slot(symbol).is_none()
        {
            return Err(SnapshotError::UnknownHostSymbol);
        }
        for (index, frame) in thread.frames.iter().enumerate() {
            if let Some(BoundaryImage::Native {
                symbol,
                resuming,
                func,
                sequence,
                ..
            }) = &frame.boundary
            {
                let name = image
                    .natives
                    .get(*symbol as usize)
                    .ok_or(SnapshotError::UnknownHostSymbol)?;
                let entry = registry
                    .native_slot(name)
                    .and_then(|slot| registry.native(slot))
                    .ok_or(SnapshotError::UnknownHostSymbol)?;
                if entry.callback.is_none()
                    || entry.typed.is_some()
                    || slot_native(image, thread.stack.get(*func as usize)) != Some(*symbol)
                    || sequence.is_some() != (entry.policy == crate::NativePolicy::External)
                    || sequence
                        .is_some_and(|sequence| sequence == 0 || sequence >= image.next_sequence)
                {
                    return Err(SnapshotError::InvalidStructure);
                }
                if *resuming {
                    if entry.policy != crate::NativePolicy::External {
                        return Err(SnapshotError::InvalidStructure);
                    }
                    if !matches!(frame.pending, PendingImage::NativePrepared { sequence: pending } if Some(pending) == *sequence)
                    {
                        return Err(SnapshotError::InvalidStructure);
                    }
                    continue;
                }
            }
            if let Some(meta) = &frame.meta {
                use crate::heap::MetaPhase;
                let external = match meta.phase {
                    MetaPhase::Running | MetaPhase::Idle => continue,
                    MetaPhase::NativePrepared { .. } => true,
                    MetaPhase::NativeWaiting { sequence, .. } => sequence.is_some(),
                };
                let Some(index) = slot_native(image, thread.stack.get(meta.slot as usize)) else {
                    return Err(SnapshotError::InvalidStructure);
                };
                let policy = native_policy(image, registry, index)?;
                if external != (policy == crate::host::NativePolicy::External) {
                    return Err(SnapshotError::InvalidStructure);
                }
                continue;
            }
            let external = match &frame.pending {
                PendingImage::NativePrepared { .. } => true,
                PendingImage::NativeWaiting { sequence, .. } => sequence.is_some(),
                // A deferred call is of a function the VM implements.
                PendingImage::Deferred => {
                    let slot = frame
                        .boundary
                        .as_ref()
                        .map(BoundaryImage::call_slot)
                        .ok_or(SnapshotError::InvalidStructure)?;
                    let Some(native) = slot_native(image, thread.stack.get(slot as usize)) else {
                        return Err(SnapshotError::InvalidStructure);
                    };
                    let symbol = image
                        .natives
                        .get(native as usize)
                        .ok_or(SnapshotError::DanglingReference)?;
                    let builtin = registry
                        .native_slot(symbol)
                        .and_then(|slot| registry.native(slot))
                        .is_some_and(|entry| entry.builtin.is_some());
                    if (!builtin && !matches!(frame.boundary, Some(BoundaryImage::Native { .. })))
                        || thread.top <= slot
                    {
                        return Err(SnapshotError::InvalidStructure);
                    }
                    continue;
                }
                _ => continue,
            };
            let policy = native_call_policy(image, registry, thread, index)?;
            if external != (policy == crate::host::NativePolicy::External) {
                return Err(SnapshotError::InvalidStructure);
            }
        }
    }
    for proto in &image.protos {
        for op in &proto.ops {
            if let Op::CallHost { symbol, .. } = op {
                let bytes = proto
                    .byte_consts
                    .get(*symbol as usize)
                    .ok_or(SnapshotError::InvalidStructure)?;
                let name =
                    std::str::from_utf8(bytes).map_err(|_| SnapshotError::InvalidStructure)?;
                if !registry.contains(name) {
                    return Err(SnapshotError::UnknownHostSymbol);
                }
            }
        }
    }
    for thread in &image.threads {
        for frame in &thread.frames {
            if let PendingImage::Prepared { symbol, .. } | PendingImage::Waiting { symbol, .. } =
                &frame.pending
                && !registry.contains(symbol)
            {
                return Err(SnapshotError::UnknownHostSymbol);
            }
        }
    }
    Ok(())
}

/// The policy of the native a pending native call is stopped on: the frame's
/// instruction must be a `Call` whose callee register holds a native value.
/// A metamethod continuation must belong to the instruction at `pc`, carry
/// that event's argument count, and use scratch above the frame's registers.
/// A thread's error record and unwind state fit its status and frames:
/// a failed thread has an error, no frames, and nothing unwinding; an
/// unwind runs in a ready thread and pops toward a `Protect` frame.
/// A thread waits exactly when its top frame holds a wait, and only its top
/// frame may hold a prepared or waiting call. A frame that kept a wait on a
/// ready thread would run its instruction, or a boundary frame's borrowed
/// code, a second time.
fn validate_waits(thread: &ThreadImage) -> Result<(), SnapshotError> {
    use crate::heap::MetaPhase;
    let waiting = thread.status == crate::heap::Status::Waiting.tag();
    let top = thread.frames.len().checked_sub(1);
    let mut top_waits = false;
    for (index, frame) in thread.frames.iter().enumerate() {
        let phase = frame.meta.as_ref().map(|meta| meta.phase);
        let waits = matches!(
            frame.pending,
            PendingImage::Waiting { .. }
                | PendingImage::NativeWaiting { .. }
                | PendingImage::Capability {
                    completed: false,
                    ..
                }
        ) || matches!(phase, Some(MetaPhase::NativeWaiting { .. }));
        let prepared = matches!(
            frame.pending,
            PendingImage::Prepared { .. } | PendingImage::NativePrepared { .. }
        ) || matches!(phase, Some(MetaPhase::NativePrepared { .. }));
        let capability_ready = matches!(
            frame.pending,
            PendingImage::Capability {
                completed: true,
                ..
            }
        );
        if frame.wait_request.is_some() && !waits && !capability_ready {
            return Err(SnapshotError::InvalidStructure);
        }
        let on_top = Some(index) == top;
        let deferred = matches!(frame.pending, PendingImage::Deferred);
        if (waits && !(on_top && waiting)) || ((prepared || deferred) && (!on_top || waiting)) {
            return Err(SnapshotError::InvalidStructure);
        }
        top_waits |= waits && on_top;
    }
    if waiting != top_waits {
        return Err(SnapshotError::InvalidStructure);
    }
    Ok(())
}

/// The to-be-closed list names registers of Lua frames, in declaration
/// order. A frame's closes that wait on an unwind wait on one popping
/// toward the nearest protected call below the frame, or, only while
/// `CloseThread` closes the thread, past every boundary. A closing thread
/// is a coroutine whose resumer waits for it in `CloseThread`, and its
/// unwind is running or waiting in a frame.
fn exit_scopes(image: &Image, thread: &ThreadImage) -> bool {
    thread.id == image.entry
        && image
            .finalizers
            .exit
            .is_some_and(|exit| exit.phase == crate::heap::ExitPhase::Scopes)
}

fn validate_closes(image: &Image, thread: &ThreadImage) -> Result<(), SnapshotError> {
    let exit = exit_scopes(image, thread);
    if thread.tbc.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(SnapshotError::InvalidStructure);
    }
    for slot in &thread.tbc {
        let owner = thread
            .frames
            .iter()
            .rev()
            .find(|frame| frame.boundary.is_none() && frame.base <= *slot);
        if !owner.is_some_and(|frame| *slot < frame.limit) {
            return Err(SnapshotError::InvalidStructure);
        }
    }
    let is_protect =
        |frame: &FrameImage| frame.boundary.as_ref().is_some_and(BoundaryImage::catches);
    let mut paused = false;
    for (index, frame) in thread.frames.iter().enumerate() {
        let Some(MetaImage {
            event:
                EventImage::Close {
                    next: NextImage::Unwind(unwind),
                    ..
                },
            ..
        }) = &frame.meta
        else {
            continue;
        };
        let below = &thread.frames[..index];
        let fits = match unwind.phase {
            crate::heap::UnwindPhase::Popping {
                target: Some(target),
            } => {
                below.get(target as usize).is_some_and(is_protect)
                    && !below[target as usize + 1..].iter().any(is_protect)
            }
            crate::heap::UnwindPhase::Popping { target: None } => {
                paused |= thread.closing;
                thread.closing || exit || below.iter().all(passes)
            }
            crate::heap::UnwindPhase::Raised => false,
        };
        if !fits || (unwind.error.is_none() && !thread.closing && !exit) {
            return Err(SnapshotError::InvalidStructure);
        }
    }
    if thread.closing {
        let waits = image.thread(thread.resumed_by).is_some_and(|parent| {
            parent.frames.last().is_some_and(|frame| {
                matches!(frame.pending, PendingImage::Resuming { child, nresults: 2, .. }
                        if child == thread.id)
            }) || matches!(
                library_wait(image, parent),
                Some(crate::corolib::CoFn::Close | crate::corolib::CoFn::WrapCall)
            )
        });
        let unwinds = paused || thread.unwind.is_some();
        // A close may wait on the host while it runs.
        let running = thread.status == crate::heap::Status::Ready.tag()
            || thread.status == crate::heap::Status::Waiting.tag();
        // A new error in a close method stops at the frame being closed:
        // it may target a `pcall` only above that frame.
        let marker = thread.frames.iter().position(|frame| {
            matches!(
                &frame.meta,
                Some(MetaImage {
                    event: EventImage::Close {
                        next: NextImage::Unwind(UnwindImage {
                            phase: crate::heap::UnwindPhase::Popping { target: None },
                            ..
                        }),
                        ..
                    },
                    ..
                })
            )
        });
        let aimed = match thread.unwind.as_ref().map(|unwind| unwind.phase) {
            Some(crate::heap::UnwindPhase::Popping {
                target: Some(target),
            }) => marker.is_some_and(|marker| marker < target as usize),
            _ => true,
        };
        if !thread.coroutine || !running || !waits || !unwinds || !aimed {
            return Err(SnapshotError::InvalidStructure);
        }
    }
    // Between two closes nothing runs above a frame, unless the close
    // raised: an unwind, or a message handler it called, is then on top.
    let top = thread.frames.len().saturating_sub(1);
    for (index, frame) in thread.frames.iter().enumerate().take(top) {
        let idle = matches!(
            &frame.meta,
            Some(MetaImage {
                event: EventImage::Close { .. },
                phase: crate::heap::MetaPhase::Idle,
                ..
            })
        );
        let handled = thread.frames[index + 1..]
            .iter()
            .any(|frame| matches!(frame.boundary, Some(BoundaryImage::Handler { .. })));
        if idle && thread.unwind.is_none() && !handled {
            return Err(SnapshotError::InvalidStructure);
        }
    }
    // The entry thread is not a coroutine: its errors unwind it.
    if thread.coroutine && thread.id == image.entry {
        return Err(SnapshotError::InvalidStructure);
    }
    Ok(())
}

/// The coroutine function a thread waits in: the builtin at its top
/// frame's call site, when that frame has nothing else pending
/// (ADR 0041).
fn library_wait(image: &Image, thread: &ThreadImage) -> Option<crate::corolib::CoFn> {
    let frame = thread.frames.last()?;
    if !matches!(frame.pending, PendingImage::None) {
        return None;
    }
    let slot = match (&frame.boundary, &frame.meta) {
        (Some(boundary), _) => boundary.call_slot(),
        (None, Some(meta)) => meta.slot,
        (None, None) => {
            let proto = frame_proto(image, frame).ok()?;
            match proto.ops.get(frame.pc as usize)? {
                Op::Call { func, .. } | Op::TailCall { func, .. } => {
                    frame.base.checked_add(u32::from(*func))?
                }
                _ => return None,
            }
        }
    };
    let native = match thread.stack.get(slot as usize)? {
        EncValue::Native(index) => *index,
        EncValue::NativeClosure(id) => {
            image
                .native_closures
                .iter()
                .find(|closure| closure.id == *id)?
                .native
        }
        _ => return None,
    };
    let symbol = image.natives.get(native as usize)?;
    crate::corolib::COROUTINE_FUNCTIONS
        .iter()
        .find(|(_, name, _)| name == symbol)
        .map(|(_, _, function)| *function)
        .or((symbol == crate::corolib::WRAP_CALL).then_some(crate::corolib::CoFn::WrapCall))
}

/// The threads that run form one chain of resumes (ADR 0041): the active
/// thread, the thread that resumed it, and so on down to the entry
/// thread, each waiting on the one above in a `Resume` or `CloseThread`
/// instruction, `coroutine.resume`, `coroutine.close`, or a
/// `coroutine.wrap` function. No other thread runs, and a coroutine never
/// started has a function to start in.
fn validate_resumers(image: &Image) -> Result<(), SnapshotError> {
    use crate::corolib::CoFn;
    use crate::heap::Status;
    let invalid = || SnapshotError::InvalidStructure;
    let find = |id: u64| image.thread(id);
    let runs = |thread: &ThreadImage| {
        thread.status == Status::Ready.tag() || thread.status == Status::Waiting.tag()
    };
    let mut chain = Vec::new();
    let mut on_chain = HashSet::new();
    let mut at = (image.active != 0).then_some(image.active);
    while let Some(id) = at {
        if !on_chain.insert(id) || chain.len() > image.threads.len() {
            return Err(invalid());
        }
        let thread = find(id).ok_or_else(invalid)?;
        chain.push(id);
        at = (thread.resumed_by != 0).then_some(thread.resumed_by);
    }
    if chain.last().is_some_and(|root| *root != image.entry) {
        return Err(invalid());
    }
    for thread in &image.threads {
        let in_chain = on_chain.contains(&thread.id);
        // A resumer runs until its child answers; nothing outside the
        // chain runs. The active thread itself may have finished, failed,
        // or yielded to the host.
        let waits = in_chain && thread.id != image.active;
        let abandoned = image.finalizers.exit.is_some_and(|exit| exit.close)
            && thread.id != image.entry
            && !in_chain;
        if abandoned && runs(thread) {
            // The interrupted coroutine chain remains frozen. Every link
            // still reaches main, is acyclic and names a running resumer.
            let mut frozen = HashSet::new();
            let mut cursor = thread;
            while cursor.id != image.entry {
                if !runs(cursor) || !frozen.insert(cursor.id) || cursor.resumed_by == 0 {
                    return Err(invalid());
                }
                cursor = find(cursor.resumed_by).ok_or_else(invalid)?;
            }
        }
        if !abandoned && ((runs(thread) && !in_chain) || (waits && !runs(thread))) {
            return Err(invalid());
        }
        // A thread linked to a resumer is running: the link goes when it
        // yields, returns, or fails.
        if thread.resumed_by != 0 && ((!in_chain && !abandoned) || !runs(thread)) {
            return Err(invalid());
        }
        if thread.resumed_by != 0 {
            let parent = find(thread.resumed_by).ok_or_else(invalid)?;
            let op = parent.frames.last().is_some_and(|frame| {
                matches!(frame.pending, PendingImage::Resuming { child, .. } if child == thread.id)
            });
            // A `wrap` function closes its coroutine only after it failed,
            // so that close carries the error it will raise.
            let carries_error = thread
                .unwind
                .as_ref()
                .is_none_or(|unwind| unwind.error.is_some())
                && thread.frames.iter().all(|frame| match &frame.meta {
                    Some(MetaImage {
                        event:
                            EventImage::Close {
                                next: NextImage::Unwind(unwind),
                                ..
                            },
                        ..
                    }) => unwind.error.is_some(),
                    _ => true,
                });
            let library = match library_wait(image, parent) {
                Some(CoFn::Resume) => !thread.closing,
                Some(CoFn::Close) => thread.closing,
                Some(CoFn::WrapCall) => !thread.closing || carries_error,
                _ => false,
            };
            let interrupted_main = abandoned && parent.id == image.entry;
            if !op && !library && !interrupted_main {
                return Err(invalid());
            }
        }
        // A suspended coroutine with frames waits in `coroutine.yield`, or
        // after the `Yield` instruction.
        if !thread.frames.is_empty() && thread.status == Status::LuaSuspended.tag() {
            let top = thread.frames.last().ok_or_else(invalid)?;
            // Or it has not run at all: a thread `NewThread` made, or an
            // entry thread booted suspended.
            let unstarted = thread.frames.len() == 1 && top.pc == 0;
            let after_yield = top.boundary.is_none()
                && top.meta.is_none()
                && (unstarted
                    || frame_proto(image, top).is_ok_and(|proto| {
                        top.pc
                            .checked_sub(1)
                            .and_then(|pc| proto.ops.get(pc as usize))
                            .is_some_and(|op| matches!(op, Op::Yield { .. }))
                    }));
            if library_wait(image, thread) != Some(CoFn::Yield)
                && !after_yield
                && !thread.hook.as_ref().is_some_and(|h| h.hook_yield)
            {
                return Err(invalid());
            }
        }
        // A coroutine never resumed holds its body in slot 0.
        if thread.frames.is_empty() && thread.status == Status::LuaSuspended.tag() {
            let body = matches!(
                thread.stack.first(),
                Some(EncValue::Closure(_) | EncValue::Native(_) | EncValue::NativeClosure(_))
            );
            if !thread.coroutine || !body || thread.stack.len() != 1 || thread.top != 1 {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

fn validate_error_state(
    thread: &ThreadImage,
    closing_failed: bool,
    exit: bool,
) -> Result<(), SnapshotError> {
    let failed = thread.status == crate::heap::Status::Failed.tag();
    // A failed entry thread keeps its error while the runtime closes,
    // ready to run the finalizers (ADR 0048).
    if (failed || closing_failed) != thread.error.is_some() {
        return Err(SnapshotError::InvalidStructure);
    }
    // A failed coroutine keeps its frames until `CloseThread` closes it;
    // any other failed thread was unwound.
    if failed
        && (thread.unwind.is_some()
            || thread.closing
            || (!thread.coroutine && !thread.frames.is_empty()))
    {
        return Err(SnapshotError::InvalidStructure);
    }
    if let Some(unwind) = &thread.unwind {
        // Only a thread close runs with no error, and never while raising.
        let no_error = unwind.error.is_none()
            && ((!thread.closing && !exit) || unwind.phase == crate::heap::UnwindPhase::Raised);
        if thread.status != crate::heap::Status::Ready.tag() || no_error {
            return Err(SnapshotError::InvalidStructure);
        }
        // The unwind pops to the nearest protected call; with none, the
        // thread holds no boundary at all.
        if let crate::heap::UnwindPhase::Popping { target } = unwind.phase {
            let catches =
                |frame: &FrameImage| frame.boundary.as_ref().is_some_and(BoundaryImage::catches);
            let fits = match target {
                Some(target) => thread.frames.get(target as usize).is_some_and(|protect| {
                    catches(protect) && !thread.frames[target as usize + 1..].iter().any(catches)
                }),
                None => thread.closing || exit || thread.frames.iter().all(passes),
            };
            if !fits {
                return Err(SnapshotError::InvalidStructure);
            }
        }
    }
    Ok(())
}

/// A boundary frame sits on a frame that made the call it stands for, owns
/// no registers, and carries no Lua continuation of its own.
/// A string function's frame (ADR 0034): its work fits the strings its
/// arguments hold.
fn string_work_fits(
    image: &Image,
    thread: &ThreadImage,
    index: usize,
) -> Result<(), SnapshotError> {
    let Some(BoundaryImage::Builtin {
        func, passed, task, ..
    }) = &thread.frames[index].boundary
    else {
        return Ok(());
    };
    if let crate::heap::Task::Io(work) = task {
        let scratch = u64::from(*func) + 1 + u64::from(*passed);
        let file = usize::try_from(scratch)
            .ok()
            .and_then(|i| thread.stack.get(i));
        let state =
            match file {
                Some(EncValue::Userdata(id)) => image
                    .userdata
                    .iter()
                    .find(|u| u.id == *id)
                    .and_then(|u| match &u.payload {
                        PayloadImage::File(f) => Some(f),
                        _ => None,
                    }),
                _ => None,
            };
        let is_file = state.is_some_and(|f| match work.as_ref() {
            crate::iolib::IoWork::Open { .. } => {
                f.closed
                    || (thread.unwind.is_some()
                        && matches!(thread.frames[index].pending, PendingImage::None))
            }
            crate::iolib::IoWork::Close { .. } => {
                !f.closed || matches!(thread.frames[index].pending, PendingImage::None)
            }
            _ => !f.closed,
        });
        let counters = match work.as_ref() {
            crate::iolib::IoWork::Read {
                start,
                count,
                got,
                iterator,
                ..
            } => {
                *start <= 1
                    && *got <= (*count).max(1)
                    && work.held_bytes() <= image.max_string as usize
                    && if *iterator {
                        let closure = thread.stack.get(*func as usize);
                        matches!(closure, Some(EncValue::NativeClosure(id)) if image.native_closures.iter().any(|c| c.id == *id && c.values.len() == 1 + *count as usize && c.state.len() == 1))
                    } else {
                        *count == passed.saturating_sub(*start)
                    }
            }
            crate::iolib::IoWork::Write {
                start,
                next,
                offset,
                failed,
            } => {
                let slot = u64::from(*func) + 1 + u64::from(*next);
                let argument = usize::try_from(slot).ok().and_then(|i| thread.stack.get(i));
                let length = match argument {
                    Some(EncValue::String(id)) => image.string(*id).map(|s| s.len()),
                    Some(EncValue::Integer(n)) => Some(n.to_string().len()),
                    Some(EncValue::Float(_)) => Some(32),
                    _ => None,
                };
                *start <= 1
                    && *next >= *start
                    && *next <= *passed
                    && (*offset == 0
                        || (!*failed
                            && *next < *passed
                            && length.is_some_and(|n| *offset <= n as u64)))
            }
            _ => true,
        };
        return if is_file
            && counters
            && scratch + u64::from(work.scratch()) <= thread.stack.len() as u64
        {
            Ok(())
        } else {
            Err(SnapshotError::InvalidStructure)
        };
    }
    let crate::heap::Task::Lib(task) = task else {
        return Ok(());
    };
    let work = match &task.work {
        crate::library::Work::Str(work) => work,
        crate::library::Work::Package(work) => {
            let name = |arg: u32| -> Option<usize> {
                if arg >= *passed {
                    return None;
                }
                let slot = usize::try_from(u64::from(*func) + 1 + u64::from(arg)).ok()?;
                let Some(EncValue::String(id)) = thread.stack.get(slot) else {
                    return None;
                };
                image
                    .strings
                    .iter()
                    .find(|(string_id, _)| string_id == id)
                    .map(|(_, bytes)| bytes.len())
            };
            return if work.fits(&name) {
                Ok(())
            } else {
                Err(SnapshotError::InvalidStructure)
            };
        }
        crate::library::Work::Utf8(work) => {
            let len = match thread.stack.get((*func as usize).saturating_add(1)) {
                Some(EncValue::String(id)) => image.string(*id).map(|bytes| bytes.len()),
                _ => None,
            };
            let scratch = u64::from(*func) + 1 + u64::from(*passed);
            // Unlike a table callback's reserved scratch, every UTF-8
            // result counted here has already been written to the stack.
            let results_fit = scratch + u64::from(work.scratch()) <= thread.stack.len() as u64;
            return if work.fits(*passed, len)
                && results_fit
                && matches!(task.wait, crate::library::Wait::Nothing)
            {
                Ok(())
            } else {
                Err(SnapshotError::InvalidStructure)
            };
        }
        crate::library::Work::Os(work) => {
            use crate::oslib::OsFn;
            let arg = |n: usize| {
                if n < *passed as usize {
                    thread.stack.get(*func as usize + 1 + n)
                } else {
                    None
                }
            };
            let scratch = *func as usize + 1 + *passed as usize;
            let slot = |n: usize| thread.stack.get(scratch + n);
            let empty_wait = matches!(task.wait, crate::library::Wait::Nothing);
            let valid = match work.function {
                OsFn::Date => {
                    let format = match arg(0) {
                        None | Some(EncValue::Nil) => Some(b"%c".as_slice()),
                        Some(EncValue::String(id)) => image.string(*id),
                        _ if work.stage == 0 => Some(b"".as_slice()),
                        _ => None,
                    };
                    format.is_some_and(|format| {
                        let format = format.strip_prefix(b"!").unwrap_or(format);
                        let table = format.split(|b| *b == 0).next() == Some(b"*t");
                        work.pos as usize <= format.len()
                            && (work.stage < 4 || table)
                            && (work.stage < 2 || matches!(slot(7), Some(EncValue::Integer(n)) if i32::try_from(*n).is_ok()))
                            && (work.stage < 2 || work.stage >= 4 || matches!(slot(8), Some(EncValue::Bool(_))))
                            && (work.stage < 3 || matches!(slot(9), Some(EncValue::String(_)) | Some(EncValue::Table(_))))
                    }) && (empty_wait || work.stage >= 4 && matches!(task.wait, crate::library::Wait::Set))
                }
                OsFn::Time if work.stage > 0 => {
                    matches!(arg(0), Some(EncValue::Table(_)))
                        && (empty_wait
                            || match task.wait {
                                crate::library::Wait::Get { into } => {
                                    work.stage <= 7 && into == u32::from(work.stage - 1)
                                }
                                crate::library::Wait::Set => work.stage >= 12,
                                _ => false,
                            })
                }
                _ => empty_wait,
            };
            return if work.fits() && valid {
                Ok(())
            } else {
                Err(SnapshotError::InvalidStructure)
            };
        }
        crate::library::Work::Debug(work) => {
            let first_is_thread = *passed > 0
                && matches!(
                    usize::try_from(u64::from(*func) + 1)
                        .ok()
                        .and_then(|slot| thread.stack.get(slot)),
                    Some(EncValue::Thread(_))
                );
            return if work.fits(first_is_thread) {
                Ok(())
            } else {
                Err(SnapshotError::InvalidStructure)
            };
        }
        _ => return Ok(()),
    };
    let length = |value: Option<&EncValue>| -> Option<usize> {
        let Some(EncValue::String(id)) = value else {
            return None;
        };
        image
            .strings
            .iter()
            .find(|(string_id, _)| string_id == id)
            .map(|(_, bytes)| bytes.len())
    };
    let source = |source: crate::strlib::Source| -> Option<usize> {
        match source {
            crate::strlib::Source::Arg(arg) => {
                if arg >= *passed {
                    return None;
                }
                let slot = u64::from(*func) + 1 + u64::from(arg);
                length(thread.stack.get(usize::try_from(slot).ok()?))
            }
            // A value of the native closure being called.
            crate::strlib::Source::Closure(index) => {
                let Some(EncValue::NativeClosure(id)) = thread.stack.get(*func as usize) else {
                    return None;
                };
                let closure = image.native_closures.iter().find(|c| c.id == *id)?;
                length(closure.values.get(index as usize))
            }
        }
    };
    if work.fits(*passed, &source) {
        Ok(())
    } else {
        Err(SnapshotError::InvalidStructure)
    }
}

fn validate_boundary(thread: &ThreadImage, index: usize) -> Result<(), SnapshotError> {
    let frame = &thread.frames[index];
    let Some(boundary) = &frame.boundary else {
        return Ok(());
    };
    // A finalizer's frame (ADR 0048) sits above a frame between two
    // instructions, or `collectgarbage`'s waiting for it, or nothing while
    // the runtime closes; its call is above everything the frame below
    // holds.
    if let BoundaryImage::Finalizer { func, saved_top } = boundary {
        let clean_below = match index
            .checked_sub(1)
            .and_then(|below| thread.frames.get(below))
        {
            None => true,
            Some(below) => {
                below.meta.is_none()
                    && below.targets.is_empty()
                    && matches!(below.pending, PendingImage::None)
                    && *func >= below.limit
                    && match &below.boundary {
                        None => true,
                        Some(BoundaryImage::Builtin { task, .. }) => {
                            matches!(task, crate::heap::Task::Collect { .. })
                        }
                        Some(_) => false,
                    }
            }
        };
        let fits = clean_below
            && frame.meta.is_none()
            && frame.targets.is_empty()
            && matches!(
                frame.pending,
                PendingImage::None
                    | PendingImage::NativePrepared { .. }
                    | PendingImage::NativeWaiting { .. }
                    | PendingImage::Capability { .. }
                    | PendingImage::Deferred
            )
            && frame.base == *func
            && frame.limit == frame.base
            && frame.nresults == 0
            && saved_top <= func
            && u64::from(*func) <= thread.stack.len() as u64;
        return if fits {
            Ok(())
        } else {
            Err(SnapshotError::InvalidStructure)
        };
    }
    if let BoundaryImage::Hook {
        func,
        saved_top,
        target,
        instruction,
        after,
    } = boundary
    {
        let fits = *target as usize + 1 == index
            && frame.base == *func
            && frame.limit == *func
            && frame.nresults == 0
            && frame.meta.is_none()
            && frame.targets.is_empty()
            && matches!(
                frame.pending,
                PendingImage::None
                    | PendingImage::Deferred
                    | PendingImage::NativePrepared { .. }
                    | PendingImage::NativeWaiting { .. }
                    | PendingImage::Capability { .. }
            )
            && saved_top <= func
            && (*func as usize) <= thread.stack.len()
            && thread.frames.get(*target as usize).is_some_and(|f| {
                *func >= f.limit
                    && match after {
                        crate::runtime::hooks::AfterHook::Continue => true,
                        crate::runtime::hooks::AfterHook::Return { src, .. } => {
                            f.return_hook && f.boundary.is_none() && *src >= f.base
                        }
                    }
            })
            && instruction.is_none_or(|(depth, pc, stage)| {
                depth < MAX_FRAMES as usize && pc < MAX_INSTRUCTIONS && (1..=2).contains(&stage)
            })
            && hook_after_fits(thread, *after);
        return if fits {
            Ok(())
        } else {
            Err(SnapshotError::InvalidStructure)
        };
    }
    if let BoundaryImage::HookNative {
        func,
        passed,
        callee,
        advance_caller,
        phase,
        produced,
        result,
    } = boundary
    {
        let below = index.checked_sub(1).and_then(|i| thread.frames.get(i));
        let fits = frame.base == func.saturating_add(1)
            && u64::from(frame.limit) == u64::from(*func) + 1 + u64::from(*passed)
            && matches!(callee, EncValue::Native(_) | EncValue::NativeClosure(_))
            && (*phase != 1
                || match (callee, thread.stack.get(*func as usize)) {
                    (EncValue::Native(a), Some(EncValue::Native(b))) => a == b,
                    (EncValue::NativeClosure(a), Some(EncValue::NativeClosure(b))) => a == b,
                    _ => false,
                })
            && (1..=4).contains(phase)
            && frame.meta.is_none()
            && frame.targets.is_empty()
            && matches!(
                frame.pending,
                PendingImage::None
                    | PendingImage::Deferred
                    | PendingImage::NativePrepared { .. }
                    | PendingImage::NativeWaiting { .. }
                    | PendingImage::Capability { .. }
            )
            && u64::from(*func) + 1 + u64::from(*passed) <= thread.stack.len() as u64
            && (*phase >= 3 || (*produced == 0 && *result == 0))
            && (*phase < 3
                || u64::from(*result) + u64::from(*produced) <= thread.stack.len() as u64)
            && below.is_some_and(|f| {
                if *advance_caller {
                    f.boundary.is_none() && f.meta.is_none() && (f.base..f.limit).contains(func)
                } else {
                    f.boundary
                        .as_ref()
                        .map(BoundaryImage::call_slot)
                        .or_else(|| f.meta.as_ref().map(|m| m.slot))
                        == Some(*func)
                }
            });
        return if fits {
            Ok(())
        } else {
            Err(SnapshotError::InvalidStructure)
        };
    }
    let below = index
        .checked_sub(1)
        .and_then(|below| thread.frames.get(below))
        .ok_or(SnapshotError::InvalidStructure)?;
    if frame.meta.is_some()
        || !frame.targets.is_empty()
        || !matches!(
            frame.pending,
            PendingImage::None
                | PendingImage::NativePrepared { .. }
                | PendingImage::NativeWaiting { .. }
                | PendingImage::Capability { .. }
                | PendingImage::Deferred
        )
        || frame.limit != frame.base
    {
        return Err(SnapshotError::InvalidStructure);
    }
    // The call the frame below is making, when it is not its own `Call`.
    let called_at = match (&below.boundary, &below.meta) {
        (Some(boundary), _) => Some(boundary.call_slot()),
        (None, Some(meta)) => Some(meta.slot),
        (None, None) => None,
    };
    match boundary {
        BoundaryImage::Protect {
            func,
            advance_caller,
            handler,
        } => {
            let fits = frame.base == func.saturating_add(1)
                && handler.as_ref().is_none_or(|value| {
                    matches!(
                        value,
                        EncValue::Closure(_) | EncValue::Native(_) | EncValue::NativeClosure(_)
                    )
                })
                && match called_at {
                    Some(slot) => !advance_caller && slot == *func,
                    None => *advance_caller && (below.base..below.limit).contains(func),
                };
            if !fits {
                return Err(SnapshotError::InvalidStructure);
            }
        }
        BoundaryImage::Handler {
            slot,
            protect,
            target,
            depth,
            fault,
        } => {
            // Memory errors and "error in error handling" never reach a
            // handler.
            if matches!(
                crate::id::LuaFault::from_tag(*fault),
                Some(crate::id::LuaFault::Memory | crate::id::LuaFault::ErrorHandling) | None
            ) {
                return Err(SnapshotError::InvalidStructure);
            }
            let owner = thread
                .frames
                .get(*protect as usize)
                .filter(|_| (*protect as usize) < index)
                .ok_or(SnapshotError::InvalidStructure)?;
            let has_handler = matches!(
                owner.boundary,
                Some(BoundaryImage::Protect {
                    handler: Some(_),
                    ..
                })
            );
            // The unwind goes on to the `xpcall`, or to a `load` above it
            // with no protected call between them (ADR 0031).
            let catches =
                |frame: &FrameImage| frame.boundary.as_ref().is_some_and(BoundaryImage::catches);
            let target_fits = *target == *protect
                || ((*protect..index as u32).contains(target)
                    && matches!(
                        thread.frames[*target as usize].boundary,
                        Some(BoundaryImage::Builtin { .. })
                    )
                    && thread.frames[*target as usize]
                        .boundary
                        .as_ref()
                        .is_some_and(BoundaryImage::catches)
                    && !thread.frames[*protect as usize + 1..*target as usize]
                        .iter()
                        .any(catches));
            if !has_handler
                || !target_fits
                || *depth == 0
                || u32::from(*depth) > crate::runtime::MAX_HANDLER_DEPTH
                || frame.base != slot.saturating_add(1)
            {
                return Err(SnapshotError::InvalidStructure);
            }
        }
        // A base function's arguments are on the stack below the call it
        // makes; its task's cursor stays within them.
        BoundaryImage::Builtin {
            func,
            passed,
            advance_caller,
            task,
        } => {
            // The arguments are on the stack; scratch slots need not all
            // be yet, as a paused `table.unpack` has filled only some.
            let call = u64::from(*func) + 1 + u64::from(*passed);
            let task_fits = match task {
                crate::heap::Task::HostLoad(work) => work.fits(),
                crate::heap::Task::DoFile => true,
                crate::heap::Task::Print { next } => next < passed,
                crate::heap::Task::ToString => *passed >= 1,
                crate::heap::Task::Pairs => *passed >= 1,
                crate::heap::Task::Ipairs { .. } => *passed >= 2,
                crate::heap::Task::Load { source } => {
                    *passed >= 1 && source.len() <= crate::limits::DEFAULT_SOURCE_BYTES
                }
                crate::heap::Task::Lib(task) => lib_task_fits(task, *passed),
                crate::heap::Task::Io(_) => true, // Checked against roots/counters in string_work_fits.
                // `collectgarbage`'s arguments, any number of them.
                crate::heap::Task::Collect { .. } => true,
            };
            let fits = frame.base == func.saturating_add(1)
                && call <= thread.stack.len() as u64
                && task_fits
                && match called_at {
                    Some(slot) => !advance_caller && slot == *func,
                    None => *advance_caller && (below.base..below.limit).contains(func),
                };
            if !fits {
                return Err(SnapshotError::InvalidStructure);
            }
        }
        BoundaryImage::Native {
            func,
            passed,
            advance_caller,
            kept,
            error,
            resuming,
            ..
        } => {
            let call = u64::from(*func) + 1 + u64::from(*passed) + u64::from(*kept);
            let fits = frame.base == func.saturating_add(1)
                && call <= thread.stack.len() as u64
                && (!resuming || matches!(frame.pending, PendingImage::NativePrepared { .. }))
                && (error.is_none()
                    || (index + 1 == thread.frames.len()
                        && u64::from(thread.top) == call
                        && matches!(
                            frame.pending,
                            PendingImage::None | PendingImage::NativePrepared { .. }
                        )))
                && match called_at {
                    Some(slot) => !advance_caller && slot == *func,
                    None => *advance_caller && (below.base..below.limit).contains(func),
                };
            if !fits {
                return Err(SnapshotError::InvalidStructure);
            }
        }
        // Checked above.
        BoundaryImage::Finalizer { .. }
        | BoundaryImage::Hook { .. }
        | BoundaryImage::HookNative { .. } => {}
    }
    Ok(())
}

/// The prototype a frame runs.
fn frame_proto<'a>(image: &'a Image, frame: &FrameImage) -> Result<&'a ProtoImage, SnapshotError> {
    let proto_id = image
        .closures
        .iter()
        .find(|closure| closure.id == frame.closure)
        .map(|closure| closure.proto)
        .ok_or(SnapshotError::DanglingReference)?;
    image
        .protos
        .iter()
        .find(|proto| proto.id == proto_id)
        .ok_or(SnapshotError::DanglingReference)
}

/// A Lua frame above another answers the call that frame is making: its
/// call slot, just below its arguments, is that call's slot, and it
/// returns what that call wants. The call is an ordinary frame's `Call`,
/// its `pc` just past it; a metamethod or close call; or the call a
/// boundary stands for. A restored frame must answer the call it claims to;
/// old images where a native tail call already erased its Lua frame remain
/// valid (ADR 0029).
fn validate_called(image: &Image, thread: &ThreadImage, index: usize) -> Result<(), SnapshotError> {
    let frame = &thread.frames[index];
    let Some(below) = index.checked_sub(1).map(|below| &thread.frames[below]) else {
        return Ok(());
    };
    if frame.boundary.is_some() {
        return Ok(());
    }
    let slot = frame
        .base
        .checked_sub(frame.vararg_len)
        .and_then(|args| args.checked_sub(1))
        .ok_or(SnapshotError::InvalidStructure)?;
    let fits = match (&below.boundary, &below.meta) {
        (Some(boundary), _) => {
            boundary.call_slot() == slot && frame.nresults == boundary.call_wants()
        }
        (None, Some(meta)) => {
            meta.slot == slot
                && meta.phase == crate::heap::MetaPhase::Running
                && frame.nresults == 1
        }
        (None, None) => {
            let proto = frame_proto(image, below)?;
            let call = below
                .pc
                .checked_sub(1)
                .and_then(|pc| proto.ops.get(pc as usize));
            matches!(call, Some(Op::Call { func, nresults, .. })
                if u64::from(below.base) + u64::from(*func) == u64::from(slot)
                    && *nresults == frame.nresults)
        }
    };
    if fits {
        Ok(())
    } else {
        Err(SnapshotError::InvalidStructure)
    }
}

/// A frame's registers are its prototype's; its extra arguments, only for
/// a vararg prototype, sit just below them, and the call's slot below
/// those lies in the frame that called it (ADR 0028); an assignment's
/// cursor and register targets stay within the frame.
fn validate_frame_slots(
    thread: &ThreadImage,
    index: usize,
    proto: &ProtoImage,
) -> Result<(), SnapshotError> {
    let frame = &thread.frames[index];
    let registers_fit = frame.boundary.is_some()
        || u64::from(frame.limit) == u64::from(frame.base) + u64::from(proto.max_reg);
    // A message handler's function sits just below its boundary's base, so
    // the handler's frame starts where the boundary does.
    let start = frame.base.checked_sub(frame.vararg_len);
    let above_caller = start.is_some_and(|start| {
        index == 0 || {
            let below = &thread.frames[index - 1];
            start > below.base
                || (start == below.base
                    && matches!(below.boundary, Some(BoundaryImage::Handler { .. })))
                // A finalizer's frame starts at the first slot past what
                // the frame below holds, which is its base when it holds
                // nothing (ADR 0048).
                || (start == below.base
                    && matches!(frame.boundary, Some(BoundaryImage::Finalizer { .. })))
                // An empty interrupted native activation can put the hook
                // boundary exactly at its base/limit.
                || (start == below.base
                    && matches!(frame.boundary, Some(BoundaryImage::Hook { target, func, .. }) if target as usize + 1 == index && func == below.limit))
                || (start == below.base && matches!((&below.boundary, &frame.boundary),
                    (Some(BoundaryImage::HookNative { func: a, .. }), Some(BoundaryImage::Protect { func: b, .. } | BoundaryImage::Builtin { func: b, .. } | BoundaryImage::Native { func: b, .. })) if a == b))
        }
    });
    let varargs_fit = frame.vararg_len == 0 || (proto.vararg && frame.boundary.is_none());
    // Only a Lua activation is ever entered by a tail call.
    let varargs_fit =
        varargs_fit && !((frame.tail || frame.return_hook) && frame.boundary.is_some());
    let cursor_fits = match frame.pending {
        PendingImage::Assigning { next, .. } => {
            next >= 1 && usize::from(next) <= frame.targets.len()
        }
        _ => true,
    };
    let targets_fit = frame.targets.iter().all(|target| match target {
        TargetImage::Register(slot) => (frame.base..frame.limit).contains(slot),
        TargetImage::Field { .. } => true,
    });
    // A protected or base-function call the frame's `pc` moves past is the
    // frame's own `Call`.
    let call_fits = match thread.frames.get(index + 1).and_then(|above| {
        above
            .boundary
            .as_ref()
            .and_then(BoundaryImage::advances)
            .map(|func| (func, above.nresults))
    }) {
        None => true,
        Some((func, wanted)) => match proto.ops.get(frame.pc as usize) {
            Some(Op::Call {
                func: reg,
                nresults,
                ..
            }) => u64::from(frame.base) + u64::from(*reg) == u64::from(func) && *nresults == wanted,
            // A native tail call runs from its own Lua frame, for every
            // result, even when that frame has a caller (ADR 0029).
            Some(Op::TailCall { func: reg, .. }) => {
                u64::from(frame.base) + u64::from(*reg) == u64::from(func)
                    && wanted == crate::opcode::COUNT_OPEN
            }
            _ => false,
        },
    };
    if !(registers_fit && above_caller && varargs_fit && cursor_fits && targets_fit && call_fits) {
        return Err(SnapshotError::InvalidStructure);
    }
    Ok(())
}

fn validate_meta(meta: &MetaImage, op: Op, frame: &FrameImage) -> Result<(), SnapshotError> {
    use crate::heap::{MetaEvent, MetaPhase};
    // Two operands, plus one argument per `__call` step when the handler
    // was a callable value. `__index` / `__newindex` handlers are functions.
    let callable = |base: u8| {
        meta.nargs >= base && u32::from(meta.nargs - base) <= crate::runtime::MAX_CALL_CHAIN
    };
    let idle = meta.phase == MetaPhase::Idle;
    let event = match &meta.event {
        EventImage::Plain(event) => *event,
        EventImage::Close { from, next } => {
            // A close call gets the value and the error or nil; between
            // calls nothing runs. The frame's instruction is the one that
            // began the closes, or, for an unwind, any it stopped.
            let call_fits = if idle {
                meta.slot == 0 && meta.nargs == 0
            } else {
                callable(2) && meta.slot >= frame.limit
            };
            let at = |reg: u8| u64::from(frame.base) + u64::from(reg);
            let next_fits = match next {
                NextImage::Advance => {
                    matches!(op, Op::CloseScope { from: reg } if at(reg) == u64::from(*from))
                }
                NextImage::Return { src, produced } => {
                    *from == frame.base
                        && (idle || u64::from(meta.slot) >= u64::from(*src) + u64::from(*produced))
                        && matches!(op, Op::Return { base, count }
                            if at(base) == u64::from(*src)
                                && (count == crate::opcode::COUNT_OPEN
                                    || u32::from(count) == *produced))
                }
                NextImage::Unwind(unwind) => {
                    *from == frame.base
                        && matches!(unwind.phase, crate::heap::UnwindPhase::Popping { .. })
                }
            };
            let alone = matches!(
                frame.pending,
                PendingImage::None | PendingImage::Capability { .. }
            ) && frame.targets.is_empty();
            if !(call_fits && next_fits && alone) {
                return Err(SnapshotError::InvalidStructure);
            }
            return Ok(());
        }
    };
    if idle {
        return Err(SnapshotError::InvalidStructure);
    }
    let fits = match (event, op) {
        (
            MetaEvent::Store { dst },
            Op::Index { dst: op_dst, .. } | Op::GetField { dst: op_dst, .. },
        ) => dst == op_dst && meta.nargs == 2,
        (
            MetaEvent::Store { dst },
            Op::Len { dst: op_dst, .. }
            | Op::Add { dst: op_dst, .. }
            | Op::Arith { dst: op_dst, .. }
            | Op::ArithK { dst: op_dst, .. }
            | Op::Neg { dst: op_dst, .. }
            | Op::BNot { dst: op_dst, .. }
            | Op::Concat { dst: op_dst, .. },
        ) => dst == op_dst && callable(2),
        (
            MetaEvent::Truth { dst, negate },
            Op::Compare {
                kind, dst: op_dst, ..
            },
        ) => dst == op_dst && negate == (kind == crate::opcode::CmpKind::Ne) && callable(2),
        (MetaEvent::Truth { dst, negate }, Op::CompareBranch { kind, .. }) => {
            dst == crate::opcode::COUNT_OPEN
                && negate == (kind == crate::opcode::CmpKind::Ne)
                && callable(2)
        }
        (MetaEvent::NewIndex, Op::SetIndex { .. } | Op::SetField { .. }) => meta.nargs == 3,
        (MetaEvent::NewIndexAssign, Op::AssignCommit { .. }) => {
            meta.nargs == 3 && matches!(frame.pending, PendingImage::Assigning { .. })
        }
        _ => false,
    };
    if !fits || meta.slot < frame.limit {
        return Err(SnapshotError::InvalidStructure);
    }
    Ok(())
}

fn native_call_policy(
    image: &Image,
    registry: &HostRegistry,
    thread: &ThreadImage,
    index: usize,
) -> Result<crate::host::NativePolicy, SnapshotError> {
    let frame = &thread.frames[index];
    // A boundary frame calls the protected function, the message handler,
    // or what a base function calls.
    if let Some(boundary) = &frame.boundary {
        let slot = boundary.call_slot();
        let Some(index) = slot_native(image, thread.stack.get(slot as usize)) else {
            return Err(SnapshotError::InvalidStructure);
        };
        if thread.top <= slot {
            return Err(SnapshotError::InvalidStructure);
        }
        return native_policy(image, registry, index);
    }
    let proto_id = image
        .closures
        .iter()
        .find(|closure| closure.id == frame.closure)
        .map(|closure| closure.proto)
        .ok_or(SnapshotError::DanglingReference)?;
    let proto = image
        .protos
        .iter()
        .find(|proto| proto.id == proto_id)
        .ok_or(SnapshotError::DanglingReference)?;
    // A native tail call runs from the tail-calling Lua frame.
    let func = match proto.ops.get(frame.pc as usize) {
        Some(Op::Call { func, .. }) => func,
        Some(Op::TailCall { func, .. }) => func,
        _ => return Err(SnapshotError::InvalidStructure),
    };
    let slot = frame
        .base
        .checked_add(u32::from(*func))
        .ok_or(SnapshotError::InvalidStructure)?;
    let Some(index) = slot_native(image, thread.stack.get(slot as usize)) else {
        return Err(SnapshotError::InvalidStructure);
    };
    // The native's arguments end at `top`. Their count need not be the
    // `Call`'s: `__call` inserts arguments, and a native a callee tail-called
    // takes over the call with arguments of its own (ADR 0029).
    if thread.top <= slot {
        return Err(SnapshotError::InvalidStructure);
    }
    native_policy(image, registry, index)
}

/// The native a call slot's value runs: a native's own index, or a native
/// closure's builtin (ADR 0035).
fn slot_native(image: &Image, value: Option<&EncValue>) -> Option<u32> {
    match value? {
        EncValue::Native(index) => Some(*index),
        EncValue::NativeClosure(id) => image
            .native_closures
            .iter()
            .find(|closure| closure.id == *id)
            .map(|closure| closure.native),
        _ => None,
    }
}

fn native_policy(
    image: &Image,
    registry: &HostRegistry,
    index: u32,
) -> Result<crate::host::NativePolicy, SnapshotError> {
    let symbol = image
        .natives
        .get(index as usize)
        .ok_or(SnapshotError::DanglingReference)?;
    let slot = registry
        .native_slot(symbol)
        .ok_or(SnapshotError::UnknownHostSymbol)?;
    Ok(registry
        .native(slot)
        .ok_or(SnapshotError::UnknownHostSymbol)?
        .policy)
}

/// The restored object of every snapshot id, by kind, for [`dec_value`].
struct Handles<'a> {
    strings: &'a std::collections::HashMap<u64, Handle<StringObj>>,
    tables: &'a std::collections::HashMap<u64, Handle<TableObj>>,
    closures: &'a std::collections::HashMap<u64, Handle<ClosureObj>>,
    threads: &'a std::collections::HashMap<u64, Handle<ThreadObj>>,
    native_closures: &'a std::collections::HashMap<u64, Handle<NativeClosureObj>>,
    userdata: &'a std::collections::HashMap<u64, Handle<crate::heap::UserdataObj>>,
    natives: usize,
    next_object_id: u64,
}

fn dec_value(handles: &Handles<'_>, value: &EncValue) -> Result<Value, SnapshotError> {
    let Handles {
        strings,
        tables,
        closures,
        threads,
        native_closures,
        userdata,
        natives,
        next_object_id,
    } = handles;
    let natives = *natives;
    Ok(match value {
        EncValue::Nil => Value::Nil,
        EncValue::Bool(bit) => Value::Bool(*bit),
        EncValue::Integer(integer) => Value::Integer(*integer),
        EncValue::Float(bits) => Value::Float(f64::from_bits(*bits)),
        EncValue::String(id) => {
            Value::String(*strings.get(id).ok_or(SnapshotError::DanglingReference)?)
        }
        EncValue::Table(id) => {
            Value::Table(*tables.get(id).ok_or(SnapshotError::DanglingReference)?)
        }
        EncValue::Closure(id) => {
            Value::Closure(*closures.get(id).ok_or(SnapshotError::DanglingReference)?)
        }
        EncValue::Thread(id) => {
            Value::Thread(*threads.get(id).ok_or(SnapshotError::DanglingReference)?)
        }
        EncValue::Native(index) => {
            if *index as usize >= natives {
                return Err(SnapshotError::DanglingReference);
            }
            Value::Native(*index)
        }
        EncValue::NativeClosure(id) => Value::NativeClosure(
            *native_closures
                .get(id)
                .ok_or(SnapshotError::DanglingReference)?,
        ),
        EncValue::Userdata(id) => {
            Value::Userdata(*userdata.get(id).ok_or(SnapshotError::DanglingReference)?)
        }
        EncValue::Light(domain, bits) => {
            if !light_fits(*domain, *bits, *next_object_id) {
                return Err(SnapshotError::InvalidStructure);
            }
            Value::LightUserdata(*domain, *bits)
        }
    })
}

pub(crate) fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn hook_image(
    heap: &Heap,
    registry: &HostRegistry,
    h: &crate::runtime::hooks::HookState,
) -> Result<HookImage, SnapshotError> {
    use crate::runtime::hooks::HookTarget;
    // The registry index is resolved by the runtime below, never serialized.
    let target = match h.target {
        HookTarget::None => HookTargetImage::None,
        HookTarget::Lua(v) => HookTargetImage::Lua(enc_value(heap, v)?),
        HookTarget::InheritedLua => HookTargetImage::InheritedLua,
        HookTarget::Host(slot) => HookTargetImage::Host(
            registry
                .hook(slot)
                .ok_or(SnapshotError::UnknownHostSymbol)?
                .0
                .to_owned(),
        ),
    };
    Ok(HookImage {
        target,
        mask: h.mask,
        base_count: h.base_count,
        remaining_count: h.remaining_count,
        allow_hook: h.allow_hook,
        old_pc: h.old_pc,
        pending: h.pending,
        hook_yield: h.hook_yield,
        names: h
            .names
            .iter()
            .map(|v| enc_value(heap, *v))
            .collect::<Result<_, _>>()?,
        instruction: h.instruction,
        after: h.after,
        transfer: h.transfer,
        restore_cursor: h.restore_cursor,
    })
}
fn write_hook_after(out: &mut Vec<u8>, after: crate::runtime::hooks::AfterHook) {
    match after {
        crate::runtime::hooks::AfterHook::Continue => out.push(0),
        crate::runtime::hooks::AfterHook::Return { src, produced } => {
            out.push(1);
            out.extend(src.to_le_bytes());
            out.extend(produced.to_le_bytes());
        }
    }
}
fn read_hook_after(input: &mut &[u8]) -> Result<crate::runtime::hooks::AfterHook, SnapshotError> {
    Ok(match opcode::read_u8(input)? {
        0 => crate::runtime::hooks::AfterHook::Continue,
        1 => crate::runtime::hooks::AfterHook::Return {
            src: opcode::read_u32(input)?,
            produced: opcode::read_u32(input)?,
        },
        _ => return Err(SnapshotError::InvalidTag),
    })
}
fn write_hook_instruction(
    out: &mut Vec<u8>,
    instruction: Option<(usize, u32, u8)>,
) -> Result<(), SnapshotError> {
    out.push(u8::from(instruction.is_some()));
    if let Some((frame, pc, stage)) = instruction {
        write_count(out, frame, MAX_FRAMES)?;
        out.extend(pc.to_le_bytes());
        out.push(stage);
    }
    Ok(())
}
fn read_hook_instruction(input: &mut &[u8]) -> Result<Option<(usize, u32, u8)>, SnapshotError> {
    Ok(if read_flag(input)? {
        Some((
            checked_len(input, MAX_FRAMES)? as usize,
            opcode::read_u32(input)?,
            opcode::read_u8(input)?,
        ))
    } else {
        None
    })
}
fn write_hook_cursor(
    out: &mut Vec<u8>,
    cursor: Option<(usize, u32, Option<u32>)>,
) -> Result<(), SnapshotError> {
    out.push(u8::from(cursor.is_some()));
    if let Some((frame, pc, line)) = cursor {
        write_count(out, frame, MAX_FRAMES)?;
        out.extend(pc.to_le_bytes());
        out.push(u8::from(line.is_some()));
        if let Some(line) = line {
            out.extend(line.to_le_bytes());
        }
    }
    Ok(())
}
fn read_hook_cursor(input: &mut &[u8]) -> Result<Option<(usize, u32, Option<u32>)>, SnapshotError> {
    Ok(if read_flag(input)? {
        let frame = checked_len(input, MAX_FRAMES)? as usize;
        let pc = opcode::read_u32(input)?;
        let line = if read_flag(input)? {
            Some(opcode::read_u32(input)?)
        } else {
            None
        };
        Some((frame, pc, line))
    } else {
        None
    })
}
fn write_hook(out: &mut Vec<u8>, hook: Option<&HookImage>) -> Result<(), SnapshotError> {
    out.push(u8::from(hook.is_some()));
    let Some(h) = hook else { return Ok(()) };
    match &h.target {
        HookTargetImage::None => out.push(0),
        HookTargetImage::Lua(v) => {
            out.push(1);
            write_value(out, v);
        }
        HookTargetImage::Host(s) => {
            out.push(2);
            write_str(out, s)?;
        }
        HookTargetImage::InheritedLua => out.push(3),
    }
    out.push(h.mask);
    out.extend(h.base_count.to_le_bytes());
    out.extend(h.remaining_count.to_le_bytes());
    out.push(u8::from(h.allow_hook));
    write_hook_cursor(out, h.old_pc)?;
    out.push(u8::from(h.pending.is_some()));
    if let Some(e) = h.pending {
        out.push(e.event as u8);
        out.push(u8::from(e.line.is_some()));
        if let Some(line) = e.line {
            out.extend(line.to_le_bytes());
        }
        out.extend(e.frame.to_le_bytes());
        out.extend(e.transfer.0.to_le_bytes());
        out.extend(e.transfer.1.to_le_bytes());
        write_hook_after(out, e.after);
    }
    out.push(u8::from(h.hook_yield));
    for v in &h.names {
        write_value(out, v);
    }
    write_hook_instruction(out, h.instruction)?;
    write_hook_after(out, h.after);
    out.push(u8::from(h.transfer.is_some()));
    if let Some((frame, first, count)) = h.transfer {
        out.extend(frame.to_le_bytes());
        out.extend(first.to_le_bytes());
        out.extend(count.to_le_bytes());
    }
    write_hook_cursor(out, h.restore_cursor)?;
    Ok(())
}
fn read_hook(input: &mut &[u8]) -> Result<Option<HookImage>, SnapshotError> {
    use crate::runtime::hooks::{Event, PendingEvent};
    if !read_flag(input)? {
        return Ok(None);
    }
    let target = match opcode::read_u8(input)? {
        0 => HookTargetImage::None,
        1 => HookTargetImage::Lua(read_value(input)?),
        2 => HookTargetImage::Host(read_string(input)?),
        3 => HookTargetImage::InheritedLua,
        _ => return Err(SnapshotError::InvalidTag),
    };
    let mask = opcode::read_u8(input)?;
    let base_count = opcode::read_u32(input)? as i32;
    let remaining_count = opcode::read_u32(input)? as i32;
    let allow_hook = read_flag(input)?;
    let old_pc = read_hook_cursor(input)?;
    let pending = if read_flag(input)? {
        let event = match opcode::read_u8(input)? {
            0 => Event::Call,
            1 => Event::Return,
            2 => Event::Line,
            3 => Event::Count,
            4 => Event::TailCall,
            _ => return Err(SnapshotError::InvalidTag),
        };
        let line = if read_flag(input)? {
            Some(opcode::read_u32(input)?)
        } else {
            None
        };
        Some(PendingEvent {
            event,
            line,
            frame: opcode::read_u32(input)?,
            transfer: (opcode::read_u32(input)?, opcode::read_u32(input)?),
            after: read_hook_after(input)?,
        })
    } else {
        None
    };
    let hook_yield = read_flag(input)?;
    let mut names = Vec::with_capacity(5);
    for _ in 0..5 {
        names.push(read_value(input)?);
    }
    let instruction = read_hook_instruction(input)?;
    let after = read_hook_after(input)?;
    let transfer = if read_flag(input)? {
        Some((
            opcode::read_u32(input)?,
            opcode::read_u32(input)?,
            opcode::read_u32(input)?,
        ))
    } else {
        None
    };
    let restore_cursor = read_hook_cursor(input)?;
    Ok(Some(HookImage {
        target,
        mask,
        base_count,
        remaining_count,
        allow_hook,
        old_pc,
        pending,
        hook_yield,
        names,
        instruction,
        after,
        transfer,
        restore_cursor,
    }))
}
fn realize_hook(
    h: &HookImage,
    owner: Handle<ThreadObj>,
    registry: &HostRegistry,
    decode: &Decode<'_>,
) -> Result<crate::runtime::hooks::HookState, SnapshotError> {
    use crate::runtime::hooks::{HookState, HookTarget};
    let target = match &h.target {
        HookTargetImage::None => HookTarget::None,
        HookTargetImage::InheritedLua => HookTarget::InheritedLua,
        HookTargetImage::Lua(v) => HookTarget::Lua(decode(v)?),
        HookTargetImage::Host(s) => HookTarget::Host(
            registry
                .hook_slot(s)
                .ok_or(SnapshotError::UnknownHostSymbol)?,
        ),
    };
    let mut names = [Value::Nil; 5];
    for (to, from) in names.iter_mut().zip(&h.names) {
        *to = decode(from)?;
    }
    Ok(HookState {
        owner,
        target,
        mask: h.mask,
        base_count: h.base_count,
        remaining_count: h.remaining_count,
        allow_hook: h.allow_hook,
        old_pc: h.old_pc,
        pending: h.pending,
        hook_yield: h.hook_yield,
        names,
        instruction: h.instruction,
        after: h.after,
        transfer: h.transfer,
        restore_cursor: h.restore_cursor,
        boundary_spare: matches!(h.target, HookTargetImage::Lua(_)).then(Box::default),
    })
}

fn hook_after_fits(thread: &ThreadImage, after: crate::runtime::hooks::AfterHook) -> bool {
    match after {
        crate::runtime::hooks::AfterHook::Continue => true,
        crate::runtime::hooks::AfterHook::Return { src, produced } => {
            u64::from(src) + u64::from(produced) <= thread.stack.len() as u64
        }
    }
}
fn validate_hooks(
    image: &Image,
    thread: &ThreadImage,
    require: &dyn Fn(u64, Kind) -> Result<(), SnapshotError>,
) -> Result<(), SnapshotError> {
    use crate::runtime::hooks::{AfterHook, Event, HOOK_BYTES};
    let boundaries: Vec<_> = thread
        .frames
        .iter()
        .enumerate()
        .filter(|(_, f)| matches!(f.boundary, Some(BoundaryImage::Hook { .. })))
        .collect();
    let Some(h) = &thread.hook else {
        return if boundaries.is_empty() && !thread.frames.iter().any(|f| f.return_hook) {
            Ok(())
        } else {
            Err(SnapshotError::InvalidStructure)
        };
    };
    let invalid = || Err(SnapshotError::InvalidStructure);
    if h.mask & !15 != 0
        || h.names.len() != 5
        || ((h.base_count > 0) != (h.mask & 8 != 0)) && !matches!(h.target, HookTargetImage::None)
        || (h.mask & 8 != 0 && !(1..=h.base_count).contains(&h.remaining_count))
        || (h.mask & 8 == 0 && h.remaining_count != h.base_count)
        || thread.charged_held < HOOK_BYTES
        || thread.charged_held > image.gc.quota
        || !hook_after_fits(thread, h.after)
    {
        return invalid();
    }
    match &h.target {
        HookTargetImage::None if h.mask != 0 || h.base_count > 0 => return invalid(),
        HookTargetImage::Lua(value) => {
            if !matches!(
                value,
                EncValue::Closure(_) | EncValue::Native(_) | EncValue::NativeClosure(_)
            ) {
                return invalid();
            }
            require_value(require, value)?;
        }
        HookTargetImage::Host(symbol) if symbol.len() > MAX_STRING_BYTES as usize => {
            return invalid();
        }
        _ => {}
    }
    for (name, expected) in h
        .names
        .iter()
        .zip(["call", "return", "line", "count", "tail call"])
    {
        if !matches!(name, EncValue::Nil | EncValue::String(_)) {
            return invalid();
        }
        require_value(require, name)?;
        if let EncValue::String(id) = name
            && image.string(*id) != Some(expected.as_bytes())
        {
            return invalid();
        }
    }
    if matches!(h.target, HookTargetImage::Lua(_))
        && h.names.iter().any(|n| !matches!(n, EncValue::String(_)))
    {
        return invalid();
    }
    for cursor in [h.old_pc, h.restore_cursor].into_iter().flatten() {
        // The per-thread cursor may refer to a returned callback's historical
        // activation/prototype; traceexec clamps it in the next current proto.
        if cursor.0 >= MAX_FRAMES as usize || cursor.1 >= MAX_INSTRUCTIONS {
            return invalid();
        }
    }
    if h.instruction.is_some_and(|(depth, pc, stage)| {
        depth >= MAX_FRAMES as usize || pc >= MAX_INSTRUCTIONS || !(1..=2).contains(&stage)
    }) {
        return invalid();
    }
    let failed = thread.status == Status::Failed.tag();
    if let AfterHook::Return { src, .. } = h.after
        && (h.pending.is_some()
            || !h.allow_hook
            || thread
                .frames
                .last()
                .is_none_or(|f| !f.return_hook || f.boundary.is_some() || src < f.base))
    {
        return invalid();
    }
    for (index, frame) in thread.frames.iter().enumerate() {
        if frame.return_hook && !failed && !thread.closing {
            let pending_return = h.pending.is_some_and(|event| {
                event.frame as usize == index && matches!(event.after, AfterHook::Return { .. })
            });
            let delivering_return = boundaries.iter().any(|(_, boundary)| {
                matches!(boundary.boundary, Some(BoundaryImage::Hook { target, after: AfterHook::Return { .. }, .. }) if target as usize == index)
            });
            let finishing_return =
                index + 1 == thread.frames.len() && matches!(h.after, AfterHook::Return { .. });
            if !pending_return && !delivering_return && !finishing_return {
                return invalid();
            }
        }
    }
    // Failure retains the callback stack until coroutine.close. Those old
    // boundaries are abandoned, not active suppression. A close callback can
    // deliver a new hook above them; its transfer owner identifies the one
    // current boundary, which must be innermost.
    let active_boundary = if failed {
        None
    } else if thread.closing {
        h.transfer.and_then(|(activation, _, _)| boundaries.iter().find(|(_, frame)| {
            matches!(frame.boundary, Some(BoundaryImage::Hook { target, .. }) if target == activation)
        }))
    } else {
        boundaries.first()
    };
    if (!thread.closing && boundaries.len() > 1)
        || h.allow_hook != active_boundary.is_none()
        || h.transfer.is_some() != active_boundary.is_some()
        || (h.restore_cursor.is_some() && active_boundary.is_none())
        || active_boundary
            .is_some_and(|(index, _)| boundaries.last().is_none_or(|(last, _)| last != index))
    {
        return invalid();
    }
    if let Some((index, frame)) = active_boundary {
        let Some(BoundaryImage::Hook { target, .. }) = frame.boundary else {
            unreachable!()
        };
        if h.transfer
            .is_none_or(|(activation, _, _)| activation != target)
            || *index != target as usize + 1
        {
            return invalid();
        }
    }
    let transfer_fits = |frame: u32, first: u32, count: u32| {
        thread.frames.get(frame as usize).is_some_and(|f| {
            (first == 0) == (count == 0)
                && (count == 0
                    || u64::from(f.base) + u64::from(first) - 1 + u64::from(count)
                        <= thread.stack.len() as u64)
        })
    };
    if let Some((frame, first, count)) = h.transfer
        && !transfer_fits(frame, first, count)
    {
        return invalid();
    }
    if let Some(e) = h.pending {
        let bit = match e.event {
            Event::Call | Event::TailCall => 1,
            Event::Return => 2,
            Event::Line => 4,
            Event::Count => 8,
        };
        if !h.allow_hook
            || h.hook_yield
            || h.mask & bit == 0
            || matches!(
                h.target,
                HookTargetImage::None | HookTargetImage::InheritedLua
            )
            || !transfer_fits(e.frame, e.transfer.0, e.transfer.1)
            || e.frame as usize + 1 != thread.frames.len()
            || !hook_after_fits(thread, e.after)
            || (e.event != Event::Line && e.line.is_some())
            || (matches!(e.event, Event::Line | Event::Count)
                && (e.transfer != (0, 0) || !matches!(e.after, AfterHook::Continue)))
            || (matches!(e.event, Event::Call | Event::TailCall)
                && !matches!(e.after, AfterHook::Continue))
        {
            return invalid();
        }
        let frame = &thread.frames[e.frame as usize];
        match e.event {
            Event::Line | Event::Count => {
                if frame.boundary.is_some()
                    || h.instruction
                        .is_none_or(|(depth, pc, _)| depth != e.frame as usize || pc != frame.pc)
                    || (e.event == Event::Line
                        && (h.instruction != Some((e.frame as usize, frame.pc, 2))
                            || h.old_pc != Some((e.frame as usize, frame.pc, e.line))))
                {
                    return invalid();
                }
            }
            Event::Call | Event::TailCall => {
                let valid = match frame.boundary {
                    None => frame.pc == 0 && (e.event != Event::TailCall || frame.tail),
                    Some(BoundaryImage::HookNative { phase: 1, .. }) => e.event == Event::Call,
                    _ => false,
                };
                if !valid {
                    return invalid();
                }
            }
            Event::Return => {
                let valid = match (frame.boundary.as_ref(), e.after) {
                    (None, AfterHook::Return { src, produced }) => {
                        frame.return_hook
                            && src >= frame.base
                            && e.transfer
                                == if produced == 0 {
                                    (0, 0)
                                } else {
                                    (src - frame.base + 1, produced)
                                }
                    }
                    (
                        Some(BoundaryImage::HookNative {
                            phase: 4,
                            result,
                            produced,
                            ..
                        }),
                        AfterHook::Continue,
                    ) => {
                        *result >= frame.base
                            && e.transfer
                                == if *produced == 0 {
                                    (0, 0)
                                } else {
                                    (result - frame.base + 1, *produced)
                                }
                    }
                    _ => false,
                };
                if !valid {
                    return invalid();
                }
            }
        }
        if let AfterHook::Return { .. } = e.after
            && (e.event != Event::Return || !thread.frames[e.frame as usize].return_hook)
        {
            return invalid();
        }
    }
    if h.hook_yield {
        let current = thread
            .frames
            .len()
            .checked_sub(1)
            .and_then(|i| thread.frames.get(i).map(|f| (i, f)));
        // A suspended host yield survives clearing or replacing its hook.
        // The marker describes the interrupted instruction, not the new target.
        if !h.allow_hook
            || h.pending.is_some()
            || h.transfer.is_some()
            || !thread.coroutine
            || thread.id == image.entry
            || thread.closing
            || !matches!(thread.status, s if s == Status::LuaSuspended.tag() || s == Status::Ready.tag())
            || !matches!(h.after, AfterHook::Continue)
            || current
                .is_none_or(|(i, f)| f.boundary.is_some() || h.instruction != Some((i, f.pc, 2)))
        {
            return invalid();
        }
    }
    Ok(())
}

#[cfg(test)]
mod proto_count_tests {
    use super::*;

    #[test]
    fn forged_prototype_counts_are_checked_before_allocation() {
        for (field, ceiling) in [(0, MAX_INSTRUCTIONS), (1, MAX_CONSTS), (2, MAX_PROTOS)] {
            for count in [ceiling, u32::MAX] {
                let mut bytes = Vec::new();
                bytes.extend(1u32.to_le_bytes()); // One prototype.
                bytes.extend(1u64.to_le_bytes());
                bytes.extend([1, 0, 0]);
                if field == 0 {
                    bytes.extend(count.to_le_bytes());
                } else {
                    bytes.extend(1u32.to_le_bytes());
                    Op::Halt.encode(&mut bytes);
                    bytes.extend(if field == 1 { count } else { 0 }.to_le_bytes());
                    if field == 2 {
                        bytes.extend(0u32.to_le_bytes()); // Captures.
                        bytes.extend(count.to_le_bytes());
                    }
                }
                // Enough for the outer prototype count, too little for the
                // forged inner count. No vector reserves the declared size.
                bytes.resize(40, 0);
                let error = read_protos(
                    &mut bytes.as_slice(),
                    &mut HashSet::new(),
                    &std::collections::HashMap::new(),
                )
                .unwrap_err();
                assert!(matches!(
                    error,
                    SnapshotError::Truncated | SnapshotError::LimitExceeded
                ));
            }
        }
    }
}
