//! Identity types.
//!
//! `ObjectId` is the only identity that survives a snapshot. Handles, slot
//! generations, and the runtime owner token do not.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};

/// Logical identity of a heap object. Never reused, never a memory address.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ObjectId(pub(crate) u64);

impl ObjectId {
    /// Return the logical identity bits; these bits do not root an object.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Checked arena lookup. Not a GC root and not serializable.
#[derive(Debug)]
pub(crate) struct Handle<T> {
    pub(crate) index: u32,
    pub(crate) generation: u32,
    _ty: PhantomData<fn() -> T>,
}

impl<T> Copy for Handle<T> {}
impl<T> Clone for Handle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> PartialEq for Handle<T> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && self.generation == other.generation
    }
}
impl<T> Eq for Handle<T> {}

impl<T> Handle<T> {
    pub(crate) fn new(index: u32, generation: u32) -> Self {
        Self {
            index,
            generation,
            _ty: PhantomData,
        }
    }
}

/// Process-local identity of one `Runtime` instance.
///
/// Minted from a counter so wasm does not need OS entropy. Not serialized.
/// A restored runtime always receives a new token.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct OwnerToken(u64);

impl OwnerToken {
    pub(crate) fn mint() -> Self {
        let raw = NEXT_OWNER.fetch_add(1, Ordering::Relaxed);
        // Relaxed is enough: the counter is only an identity, not a lock.
        // A process that creates 2^64 runtimes is outside Phase 1.
        Self(raw)
    }
}

static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);

/// Which arena a root or trace edge refers to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Kind {
    String = 1,
    Table = 2,
    Proto = 3,
    Upvalue = 4,
    Closure = 5,
    Thread = 6,
    NativeClosure = 7,
    Userdata = 8,
}

/// Strong host-owned GC root. Keeps its target alive until released.
///
/// Valid only against the runtime that issued it. Restore does not reissue it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Root {
    pub(crate) owner: OwnerToken,
    pub(crate) kind: Kind,
    pub(crate) index: u32,
    pub(crate) generation: u32,
    pub(crate) id: ObjectId,
}

impl std::fmt::Debug for Root {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Root")
            .field("type", &self.kind)
            .field("id", &self.id)
            .finish()
    }
}

/// A legacy explicit-root operation failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum RootError {
    /// The root belongs to another runtime.
    ForeignRuntime,
    /// The explicit root has been released.
    Released,
    /// The target object has been collected.
    Stale,
    /// The object has the wrong kind.
    WrongKind,
    /// No live object has this identity.
    NotFound,
}

/// A legacy wait completion failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum WaitError {
    /// No pending wait has this key.
    UnknownKey,
    /// The wait has already completed.
    AlreadyCompleted,
    /// The thread is no longer waiting for the host.
    NotWaiting,
    /// A completion value names no object or native this runtime has. The
    /// wait is left as it was.
    InvalidValue,
}

/// The identity of a pending host wait. Completion validates it against the runtime.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WaitKey(pub u64);

impl WaitKey {
    /// Return the wait identity bits; completion still validates the key.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Why execution paused without yielding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum PauseReason {
    /// The executor quantum is exhausted, including a quantum of zero.
    /// The program did not yield.
    FuelExhausted,
}

/// A fatal execution limit stopped the runtime.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum TerminationReason {
    /// The configured total fuel limit was reached.
    FuelLimitExceeded,
    /// The logical heap quota was exceeded.
    MemoryLimit,
    /// No further logical object identities can be allocated.
    ObjectIdExhausted,
}

/// The class of a Lua error: every error Lua code can catch with `pcall`
/// (ADR 0024). The error object itself is a Lua value; for every class but
/// `Error` it is the class's reserved message string ([`LuaFault::text`]).
/// Internal corruption and host misuse are [`VmError`], never a `LuaFault`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum LuaFault {
    /// An operation received a value of the wrong type.
    Type,
    /// A table assignment used nil as a key.
    NilKey,
    /// A table assignment used NaN as a key.
    NanKey,
    /// A value without a call implementation was called.
    BadCall,
    /// A coroutine was resumed in an invalid state.
    ResumeState,
    /// A native symbol was not bound.
    UnboundSymbol,
    /// `next` was given a key that is not a current traversal anchor.
    NextKey,
    /// `<` or `<=` on values that are not two numbers or two strings.
    Compare,
    /// A numeric `for` initial value, limit, or step is not a number.
    ForValue,
    /// A numeric `for` step is zero (or `-0.0`).
    ForZeroStep,
    /// Indexing, or assigning into, a value that is not a table.
    Index,
    /// A native function returned `NativeOutcome::Fault`.
    Native,
    /// An `__index` / `__newindex` chain longer than 2000 steps.
    MetaChain,
    /// `#` on a value that has no length.
    Length,
    /// Arithmetic on a value that is not a number or a numeric string, with
    /// no metamethod for it.
    Arith,
    /// A bitwise operator on a value that is not a number, with no
    /// metamethod for it.
    Bitwise,
    /// A bitwise operator on a float with no integer value.
    NoInteger,
    /// Integer `//` by zero.
    DivideByZero,
    /// `..` on a value that is not a string or a number, with no
    /// `__concat`.
    Concat,
    /// More `__call` steps than Moonseed follows for one call (ADR 0023).
    CallChain,
    /// A Lua call deeper than a thread's frame limit.
    StackOverflow,
    /// `error(value)`: the error object is `value`.
    Error,
    /// The logical-heap quota or the object limit (ADR 0025). An `xpcall`
    /// message handler is not called for it.
    Memory,
    /// An error kept escaping `xpcall`'s message handler.
    ErrorHandling,
    /// A yield inside an `xpcall` message handler, or in a close run by
    /// `CloseThread`.
    YieldAcross,
    /// A `<close>` local got a value that is not nil, false, or a value
    /// with a `__close` metamethod.
    Close,
    /// `CloseThread` on the running coroutine.
    CloseRunning,
    /// `CloseThread` on a coroutine that resumed the running one.
    CloseNormal,
    /// `assert(v)` with a false `v` and no message.
    Assert,
    /// A base function got an argument of the wrong type or value.
    Argument,
    /// A `__tostring` metamethod returned neither a string nor a number.
    ToString,
    /// A `load` reader returned neither a string, a number, nor nil. `load`
    /// returns this error rather than raising it.
    Reader,
    /// An operation Lua 5.4 has and Moonseed does not: a garbage-collector
    /// mode or tuning option.
    Unsupported,
    /// A length that is not an integer where the table library needs one.
    LengthType,
    /// `table.concat` met an element that is neither a string nor a number.
    ConcatValue,
    /// `table.unpack` asked for more results than the stack holds.
    Unpack,
    /// `table.sort`'s order function is inconsistent.
    OrderFunction,
    /// `string.byte` asked for more results than the stack holds.
    StringSlice,
    /// A pattern ends with `%`.
    PatternEnd,
    /// A pattern's set has no closing `]`.
    PatternBracket,
    /// `%b` without its two characters.
    PatternBalance,
    /// `%f` without a set.
    PatternFrontier,
    /// A capture reference or index names no closed capture.
    CaptureIndex,
    /// A `)` closes no capture.
    PatternCapture,
    /// A capture is used before it is closed.
    UnfinishedCapture,
    /// More captures than Lua's 32.
    TooManyCaptures,
    /// Matching went deeper than Lua's bound.
    PatternTooComplex,
    /// A `gsub` replacement string has `%` before a character other than a digit or `%`.
    ReplacementEscape,
    /// A `gsub` replacement function or table gave a value that is not a string, a number, false, or nil.
    ReplacementValue,
    /// A `string.format` item too long to be one.
    FormatString,
    /// A `string.format` item with an unknown conversion.
    FormatConversion,
    /// A `string.format` item with flags, width, or precision its conversion does not take.
    FormatSpecification,
    /// `%q` with modifiers.
    FormatQuote,
    /// `string.dump` of a function that is not a Lua function.
    DumpFunction,
    /// A `string.pack` format has an unknown option.
    PackOption,
    /// A `string.pack` size outside 1..=16.
    PackSize,
    /// `string.pack`'s `c` without a size.
    PackMissingSize,
    /// `string.unpack` of an integer wider than 64 bits that does not fit.
    PackIntegerFit,
    /// A string function's result would pass the string limit, found before it is made.
    StringTooLarge,
    /// Integer `%` by zero.
    ModuloByZero,
    /// `require` found no loader, or `package.searchers` is not a table.
    Require,
}

impl LuaFault {
    /// Every class, in tag order.
    pub(crate) const ALL: [LuaFault; 61] = [
        Self::Type,
        Self::NilKey,
        Self::NanKey,
        Self::BadCall,
        Self::ResumeState,
        Self::UnboundSymbol,
        Self::NextKey,
        Self::Compare,
        Self::ForValue,
        Self::ForZeroStep,
        Self::Index,
        Self::Native,
        Self::MetaChain,
        Self::Length,
        Self::Arith,
        Self::Bitwise,
        Self::NoInteger,
        Self::DivideByZero,
        Self::Concat,
        Self::CallChain,
        Self::StackOverflow,
        Self::Error,
        Self::Memory,
        Self::ErrorHandling,
        Self::YieldAcross,
        Self::Close,
        Self::CloseRunning,
        Self::CloseNormal,
        Self::Assert,
        Self::Argument,
        Self::ToString,
        Self::Reader,
        Self::Unsupported,
        Self::LengthType,
        Self::ConcatValue,
        Self::Unpack,
        Self::OrderFunction,
        Self::StringSlice,
        Self::PatternEnd,
        Self::PatternBracket,
        Self::PatternBalance,
        Self::PatternFrontier,
        Self::CaptureIndex,
        Self::PatternCapture,
        Self::UnfinishedCapture,
        Self::TooManyCaptures,
        Self::PatternTooComplex,
        Self::ReplacementEscape,
        Self::ReplacementValue,
        Self::FormatString,
        Self::FormatConversion,
        Self::FormatSpecification,
        Self::FormatQuote,
        Self::DumpFunction,
        Self::PackOption,
        Self::PackSize,
        Self::PackMissingSize,
        Self::PackIntegerFit,
        Self::StringTooLarge,
        Self::ModuloByZero,
        Self::Require,
    ];

    /// The stable tag snapshots use.
    pub(crate) fn tag(self) -> u8 {
        Self::ALL
            .iter()
            .position(|fault| *fault == self)
            .unwrap_or(0) as u8
    }

    pub(crate) fn from_tag(tag: u8) -> Option<Self> {
        Self::ALL.get(usize::from(tag)).copied()
    }

    /// The class's reserved message: the error object when a fault is raised
    /// without room to build Lua's full message, which `runtime/diag.rs`
    /// words with the source position and operand names (ADR 0058).
    pub fn text(self) -> &'static str {
        match self {
            Self::Type => "attempt to use a value of the wrong type",
            Self::NilKey => "index is nil",
            Self::NanKey => "index is NaN",
            Self::BadCall => "attempt to call a non-callable value",
            Self::ResumeState => "cannot resume a coroutine that is not suspended",
            Self::UnboundSymbol => "unknown host function",
            Self::NextKey => "invalid key to 'next'",
            Self::Compare => "attempt to compare incompatible values",
            Self::ForValue => "'for' value must be a number",
            Self::ForZeroStep => "'for' step is zero",
            Self::Index => "attempt to index a non-table value",
            Self::Native => "native function failed",
            Self::MetaChain => "'__index' chain too long; possible loop",
            Self::Length => "attempt to get length of a value without one",
            Self::Arith => "attempt to perform arithmetic on a non-number value",
            Self::Bitwise => "attempt to perform bitwise operation on a non-number value",
            Self::NoInteger => "number has no integer representation",
            Self::DivideByZero => "attempt to divide by zero",
            Self::Concat => "attempt to concatenate a non-string value",
            Self::CallChain => "'__call' chain too long",
            Self::StackOverflow => "stack overflow",
            Self::Error => "error",
            Self::Memory => "not enough memory",
            Self::ErrorHandling => "error in error handling",
            Self::YieldAcross => "attempt to yield across a C-call boundary",
            Self::Close => "variable got a non-closable value",
            Self::CloseRunning => "cannot close a running coroutine",
            Self::CloseNormal => "cannot close a normal coroutine",
            Self::Assert => "assertion failed!",
            Self::Argument => "bad argument to a base function",
            Self::ToString => "'__tostring' must return a string",
            Self::Reader => "reader function must return a string",
            Self::Unsupported => "not supported by Moonseed",
            Self::LengthType => "object length is not an integer",
            Self::ConcatValue => "invalid value in table for 'concat'",
            Self::Unpack => "too many results to unpack",
            Self::OrderFunction => "invalid order function for sorting",
            Self::StringSlice => "string slice too long",
            Self::PatternEnd => "malformed pattern (ends with '%')",
            Self::PatternBracket => "malformed pattern (missing ']')",
            Self::PatternBalance => "missing arguments to '%b'",
            Self::PatternFrontier => "missing '[' after '%f' in pattern",
            Self::CaptureIndex => "invalid capture index",
            Self::PatternCapture => "invalid pattern capture",
            Self::UnfinishedCapture => "unfinished capture",
            Self::TooManyCaptures => "too many captures",
            Self::PatternTooComplex => "pattern too complex",
            Self::ReplacementEscape => "invalid use of '%' in replacement string",
            Self::ReplacementValue => "invalid replacement value",
            Self::FormatString => "invalid format string to 'format'",
            Self::FormatConversion => "invalid conversion to 'format'",
            Self::FormatSpecification => "invalid conversion specification",
            Self::FormatQuote => "specifier '%q' cannot have modifiers",
            Self::DumpFunction => "unable to dump given function",
            Self::PackOption => "invalid format option",
            Self::PackSize => "integral size out of limits [1,16]",
            Self::PackMissingSize => "missing size for format option 'c'",
            Self::PackIntegerFit => "integer does not fit into Lua Integer",
            Self::StringTooLarge => "resulting string too large",
            Self::ModuloByZero => "attempt to perform 'n%0'",
            Self::Require => "module not found",
        }
    }
}

/// A requested process status, without imposing a host platform's exit constants.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExitStatus {
    /// Lua's default status and `os.exit(true)` (C `EXIT_SUCCESS`).
    Success,
    /// `os.exit(false)` (C `EXIT_FAILURE`).
    Failure,
    /// An integer converted to the signed 32-bit C status argument.
    Code(i32),
}

/// The state reached after driving the executor.
#[derive(Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum StepOutcome {
    /// Execution completed successfully.
    Completed,
    /// Execution consumed the quantum and may continue.
    Paused(PauseReason),
    /// A coroutine yielded to its Lua caller.
    LuaYielded,
    /// Execution is waiting for the host to complete this key.
    Waiting(WaitKey),
    /// Execution stopped with an uncaught catchable Lua failure.
    LuaError(LuaFault),
    /// Lua requested exit. Closing, when requested, has already finished.
    ExitRequested {
        /// The status for the embedder to interpret.
        status: ExitStatus,
        /// Whether Lua requested runtime closing before this outcome.
        close: bool,
    },
    /// Execution stopped at a fatal resource limit.
    Terminated(TerminationReason),
}

/// A snapshot could not be written or restored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum SnapshotError {
    /// Legacy hook refusal from schema 22; schema 23 encodes hook state.
    HooksNotPortable,
    /// The snapshot header is not recognized.
    BadMagic,
    /// The snapshot schema or semantic revision is unsupported.
    BadVersion,
    /// The snapshot ends before a required field.
    Truncated,
    /// The snapshot checksum does not match its bytes.
    Checksum,
    /// Two objects share one logical identity.
    DuplicateObjectId,
    /// A reference names an absent object.
    DanglingReference,
    /// A frame points outside its prototype.
    InvalidProgramCounter,
    /// A required native symbol is not registered.
    UnknownHostSymbol,
    /// The snapshot has another journal lineage.
    EffectDomainMismatch,
    /// Snapshot state or decoding exceeds a configured bound.
    LimitExceeded,
    /// A serialized enum tag is not recognized.
    InvalidTag,
    /// Serialized state violates VM invariants.
    InvalidStructure,
    /// A restored prototype failed the same bytecode check compiled code
    /// passes: an operand, window, jump, constant, child, or capture is
    /// outside its prototype.
    InvalidBytecode,
    /// The heap holds a full userdata whose host type has no codec
    /// (ADR 0045). Collecting first drops any nothing reaches.
    NonPortableUserdata,
    /// A retained open file or pipe has a refusing backend.
    NonPortableResource {
        /// Logical Lua file identity.
        object: ObjectId,
    },
    /// A full userdata's host type is not registered, or not as portable.
    UnknownUserdataType,
    /// A portable host type's codec refused the snapshot's bytes.
    UserdataDecode,
    /// A host value declares more logical bytes than it was charged: its
    /// growth was never reported (`NativeCall::set_userdata_charge`).
    /// Snapshots refuse it, and so does restore.
    UserdataCharge,
    /// The registered host type's policy differs from the image's policy.
    UserdataPolicyMismatch,
    /// A rebindable host type refused its external key before runtime creation.
    Rebind {
        /// The registered userdata type that failed to rebind.
        symbol: &'static str,
        /// The logical identity of the userdata being restored.
        object: ObjectId,
        /// The host rebind failure.
        error: crate::RebindError,
    },
}

/// An execution, integrity, snapshot, or host API failure.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum VmError {
    /// Snapshot decoding or host validation failed before runtime creation.
    Snapshot(SnapshotError),
    /// A callback misused the embedding API; never catchable in Lua.
    Api(crate::ApiError),
    /// Internal VM state violates an invariant.
    Corrupt,
    /// The requested legacy operation requires runnable or completed state.
    NotRunnable,
    /// An allocation exceeded the logical heap quota.
    MemoryLimit,
    /// No further logical object identities are available.
    ObjectIdExhausted,
    /// The total fuel limit was exceeded.
    FuelLimitExceeded,
    /// No native function is registered under the requested symbol.
    UnknownNative,
    /// A thread's stack would pass `Config::max_stack_slots`. Inside a
    /// step it becomes a Lua "stack overflow".
    StackLimit,
}

impl From<TerminationReason> for VmError {
    fn from(reason: TerminationReason) -> Self {
        match reason {
            TerminationReason::FuelLimitExceeded => Self::FuelLimitExceeded,
            TerminationReason::MemoryLimit => Self::MemoryLimit,
            TerminationReason::ObjectIdExhausted => Self::ObjectIdExhausted,
        }
    }
}
