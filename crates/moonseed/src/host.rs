//! Host functions are registry symbols, not serialized pointers.
//!
//! Two kinds exist. The legacy `HostFn` (one integer in, one out) is what
//! the proof kernel's `CallHost` instruction calls. A native function
//! ([`NativeFn`]) is an ordinary Lua value, called with `Call`, with any
//! number of arguments and results and a declared [`NativePolicy`].
//!
//! The journal is embedder state. Moonseed stores only the next sequence
//! number and the pending call. Replaying an [`EffectId`] must return the
//! outcome already stored in the journal.

use crate::heap::Heap;
use crate::id::WaitKey;
use crate::value::Value;

/// Host-owned lineage. Exact restore keeps it; a future fork would mint a new one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct EffectId {
    /// The host-selected journal lineage.
    pub domain: u64,
    /// The deterministic effect number within that lineage.
    pub sequence: u64,
}

/// One persisted journal outcome and its request identity.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EffectRecord {
    /// The effect identity.
    pub id: EffectId,
    /// The integer request metadata.
    pub arg: i64,
    /// Integer outcome; byte effects use `bytes` and leave this zero.
    pub outcome: i64,
    /// Byte outcome for byte effects; integer effects leave this unset.
    pub bytes: Option<Vec<u8>>,
    /// Exact general capability request identity (versioned bytes).
    pub request: Option<Vec<u8>>,
    /// Exact request bytes for a named module effect (legacy compatible).
    pub module: Option<Vec<u8>>,
}

/// A journal payload mismatch. No callback is repeated on a mismatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JournalError;

impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("journal payload mismatch")
    }
}
impl std::error::Error for JournalError {}

mod journal_payload {
    pub trait Sealed {}
    impl Sealed for i64 {}
    impl Sealed for Vec<u8> {}
}

/// Payloads accepted by [`Journal::commit`]: integers and byte vectors.
/// Both forms fail on a payload kind mismatch without repeating the callback.
pub trait JournalPayload: journal_payload::Sealed + Sized {
    /// Create the stored record for this payload kind.
    #[doc(hidden)]
    fn record(self, id: EffectId, arg: i64) -> EffectRecord;
    /// Read a matching outcome, refusing a different payload kind.
    #[doc(hidden)]
    fn replay(record: &EffectRecord) -> Result<Self, JournalError>;
}

impl JournalPayload for i64 {
    fn record(self, id: EffectId, arg: i64) -> EffectRecord {
        EffectRecord {
            id,
            arg,
            outcome: self,
            bytes: None,
            module: None,
            request: None,
        }
    }
    fn replay(record: &EffectRecord) -> Result<Self, JournalError> {
        if record.bytes.is_none() {
            Ok(record.outcome)
        } else {
            Err(JournalError)
        }
    }
}

impl JournalPayload for Vec<u8> {
    fn record(self, id: EffectId, arg: i64) -> EffectRecord {
        EffectRecord {
            id,
            arg,
            outcome: 0,
            bytes: Some(self),
            module: None,
            request: None,
        }
    }
    fn replay(record: &EffectRecord) -> Result<Self, JournalError> {
        record.bytes.clone().ok_or(JournalError)
    }
}

/// Map from effect id to the outcome that was committed.
///
/// A duplicate id returns the stored outcome and does not run `fresh` again.
#[derive(Clone, Debug, Default)]
pub struct Journal {
    entries: Vec<EffectRecord>,
    /// Position of each id in `entries`, so a commit is not a linear scan.
    index: std::collections::HashMap<EffectId, usize>,
}

impl PartialEq for Journal {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl Eq for Journal {}

impl Journal {
    /// Create an empty effect journal.
    pub fn new() -> Self {
        Self::default()
    }

    /// Borrow journal outcomes in their insertion order.
    pub fn entries(&self) -> &[EffectRecord] {
        &self.entries
    }

    /// Commit integer or byte outcomes exactly once per effect id.
    pub fn commit<P: JournalPayload>(
        &mut self,
        id: EffectId,
        arg: i64,
        fresh: impl FnOnce() -> P,
    ) -> Result<P, JournalError> {
        if let Some(&position) = self.index.get(&id) {
            // Legacy callers have no exact request to verify. They cannot
            // consume a record belonging to a different capability operation.
            if self.entries[position].request.is_some() {
                return Err(JournalError);
            }
            return P::replay(&self.entries[position]);
        }
        let record = fresh().record(id, arg);
        let outcome = P::replay(&record);
        self.index.insert(id, self.entries.len());
        self.entries.push(record);
        outcome
    }

    /// Insert a record without running a host. Used to simulate a torn journal.
    pub fn seed(&mut self, id: EffectId, arg: i64, outcome: i64) {
        let _ = self.seed_record(outcome.record(id, arg));
    }

    /// Reconstruct an integer or byte record persisted by the host, including
    /// its module or capability request. Duplicate ids keep the previously stored outcome.
    pub fn seed_record(&mut self, record: EffectRecord) -> Result<(), JournalError> {
        if record.bytes.is_some() && record.outcome != 0
            || record.module.is_some() && record.bytes.is_none()
            || record.module.is_some() && record.request.is_some()
        {
            return Err(JournalError);
        }
        if !self.index.contains_key(&record.id) {
            self.index.insert(record.id, self.entries.len());
            self.entries.push(record);
        }
        Ok(())
    }

    /// Replay an exact request or report a mismatch without invoking any host.
    /// Legacy records with no request remain valid only for legacy `commit`.
    pub fn replay_request<P: JournalPayload>(
        &self,
        id: EffectId,
        arg: i64,
        request: &[u8],
    ) -> Result<Option<P>, JournalError> {
        let Some(&position) = self.index.get(&id) else {
            return Ok(None);
        };
        let record = &self.entries[position];
        if record.arg != arg
            || record.module.is_some()
            || record.request.as_deref() != Some(request)
        {
            return Err(JournalError);
        }
        P::replay(record).map(Some)
    }

    /// Commit an outcome with exact request verification on replay, for reads
    /// and mutations alike. A mismatch never invokes `fresh` or rewrites a record.
    pub fn commit_request<P: JournalPayload>(
        &mut self,
        id: EffectId,
        arg: i64,
        request: &[u8],
        fresh: impl FnOnce() -> P,
    ) -> Result<P, JournalError> {
        if let Some(outcome) = self.replay_request(id, arg, request)? {
            return Ok(outcome);
        }
        let mut record = fresh().record(id, arg);
        record.request = Some(request.to_vec());
        let outcome = P::replay(&record);
        self.index.insert(id, self.entries.len());
        self.entries.push(record);
        outcome
    }

    pub(crate) fn module_result(&self, domain: u64, name: &[u8]) -> Option<(EffectId, &[u8])> {
        self.entries.iter().find_map(|record| {
            (record.id.domain == domain && record.module.as_deref() == Some(name))
                .then(|| record.bytes.as_deref().map(|bytes| (record.id, bytes)))?
        })
    }

    pub(crate) fn commit_module(
        &mut self,
        id: EffectId,
        name: &[u8],
        fresh: impl FnOnce() -> Vec<u8>,
    ) -> Result<Vec<u8>, JournalError> {
        if let Some(&position) = self.index.get(&id)
            && (self.entries[position].module.as_deref() != Some(name)
                || self.entries[position].request.is_some())
        {
            return Err(JournalError);
        }
        let outcome = self.commit(id, 0, fresh)?;
        let position = self.index[&id];
        let record = &mut self.entries[position];
        record.module = Some(name.to_vec());
        Ok(outcome)
    }
}

pub struct HostCtx<'a> {
    pub journal: &'a mut Journal,
    pub effect: EffectId,
}

pub enum HostResult {
    Ready(i64),
    Pending(WaitKey),
    /// The legacy proof host rejected an incompatible journal record.
    Fault,
}

#[deprecated(note = "use HostRegistry::function or HostRegistry::typed")]
pub type HostFn = fn(&mut HostCtx<'_>, i64) -> HostResult;

/// How a native function relates to state outside the VM.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NativePolicy {
    /// Reads its arguments and returns values. Anything it changes is VM
    /// state, which a checkpoint already restores. No effect id, no journal.
    VmLocal,
    /// Acts on the world outside the VM. Each call gets an [`EffectId`] and
    /// stops at a prepared safe point before it runs; the function commits
    /// through the journal, so a replayed id returns the stored outcome.
    External,
}

/// What a native function asks the VM to do after it returns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NativeOutcome {
    /// The pushed results are the call's results.
    Ready,
    /// The caller waits until [`crate::Runtime::complete_wait`] with this key.
    Pending(WaitKey),
    /// The call raises `LuaFault::Native`.
    Fault,
}

/// A native function. It must not call back into Lua.
pub type NativeFn = fn(&mut NativeCall<'_>) -> NativeOutcome;

/// The Lua type of a value, as `type()` names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LuaType {
    /// Lua nil.
    Nil,
    /// Lua boolean.
    Boolean,
    /// Lua integer or floating-point number.
    Number,
    /// Lua byte string.
    String,
    /// Lua table.
    Table,
    /// Lua callable function.
    Function,
    /// Lua coroutine.
    Thread,
    /// Full or light: `type()` names both `userdata`.
    /// `NativeCall::light` tells them apart.
    Userdata,
}

/// A Lua value inside one native call. It cannot outlive the call: the
/// lifetime ties it to the [`NativeCall`] it came from, and no collection
/// runs while a native function runs.
#[derive(Clone, Copy)]
pub struct NativeValue<'a> {
    pub(crate) value: Value,
    _call: std::marker::PhantomData<&'a ()>,
}

impl std::fmt::Debug for NativeValue<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ty = match self.value {
            Value::Nil => LuaType::Nil,
            Value::Bool(_) => LuaType::Boolean,
            Value::Integer(_) | Value::Float(_) => LuaType::Number,
            Value::String(_) => LuaType::String,
            Value::Table(_) => LuaType::Table,
            Value::Closure(_) | Value::Native(_) | Value::NativeClosure(_) => LuaType::Function,
            Value::Thread(_) => LuaType::Thread,
            Value::Userdata(_) | Value::LightUserdata(..) => LuaType::Userdata,
        };
        f.debug_tuple("NativeValue").field(&ty).finish()
    }
}

impl NativeValue<'_> {
    pub(crate) fn wrap(value: Value) -> Self {
        Self {
            value,
            _call: std::marker::PhantomData,
        }
    }
}

/// Why `NativeCall::raw_set` refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RawSetError {
    /// The target is not a table.
    NotATable,
    /// A nil or NaN key.
    InvalidKey,
    /// A new entry would pass the logical-heap quota. A native that returns
    /// `NativeOutcome::Fault` after this raises a memory error.
    Memory,
}

/// A Lua value as the host sees it or hands it in. Unstable API.
///
/// `Object` names a table, function, or thread that already exists in the
/// runtime; `Native` names a registered native function by symbol.
#[derive(Clone, Debug, PartialEq)]
#[deprecated(note = "use Value and IntoLua/FromLua with rooted object values")]
pub enum HostValue {
    /// Lua nil.
    Nil,
    /// Lua boolean.
    Boolean(bool),
    /// Lua integer.
    Integer(i64),
    /// Lua integer or floating-point number.
    Number(f64),
    /// Lua byte string.
    String(Vec<u8>),
    /// An unrooted logical object identity.
    Object(crate::id::ObjectId),
    /// A registered native symbol.
    Native(String),
    /// A light userdata (ADR 0043). A full userdata is an `Object`.
    LightUserdata(crate::userdata::LightUserdata),
}

/// Why a userdata operation of a [`NativeCall`] refused. Unstable API.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UserdataError {
    /// The value is not a full userdata of the kind asked for.
    NotUserdata,
    /// The host type is not registered with the runtime's registry.
    Unregistered,
    /// The logical-heap quota, or the payload or user-value bound, refused
    /// it. A native that returns `NativeOutcome::Fault` after this raises
    /// a memory error.
    Memory,
}

/// How the host ends a native call that is waiting (`Runtime::complete`).
/// Unstable API.
#[derive(Clone, Debug, PartialEq)]
#[deprecated(note = "use Completion with rooted Value objects")]
pub enum LegacyCompletion {
    /// The call returns these values.
    Return(Vec<HostValue>),
    /// The call raises this error object, as `error(value)` would.
    Error(HostValue),
}

/// Arguments in, results out, for one native call. Unstable API.
///
/// Values that are not numbers, booleans, or nil can be passed through with
/// `NativeCall::push_arg` but not inspected yet.
#[deprecated(note = "use NativeContext with HostRegistry::function or typed")]
pub type NativeCall<'a> = LegacyNativeCall<'a>;

/// Diagnostics a legacy native records only when it returns `Fault`.
pub(crate) enum NativeError {
    Argument(usize, Vec<u8>),
    Message(Vec<u8>, bool),
}

/// Compatibility storage for the deprecated function-pointer callbacks.
#[doc(hidden)]
pub struct LegacyNativeCall<'a> {
    pub(crate) heap: &'a mut Heap,
    pub(crate) args: &'a [Value],
    pub(crate) results: &'a mut Vec<Value>,
    pub(crate) effect: Option<EffectId>,
    pub(crate) journal: Option<&'a mut Journal>,
    /// A raw store was refused for the logical-heap quota; a `Fault` from
    /// this call is then a memory error.
    pub(crate) out_of_memory: bool,
    /// The host userdata types the runtime knows (ADR 0044).
    pub(crate) registry: &'a HostRegistry,
    /// `Config::max_objects`: a userdata is refused past it, as every
    /// other allocation is.
    pub(crate) max_objects: u32,
    /// An argument error [`NativeCall::type_error`] recorded: the
    /// argument's index and the message. A `Fault` raises it.
    pub(crate) error: Option<NativeError>,
}

impl<'a> LegacyNativeCall<'a> {
    pub fn arg_count(&self) -> usize {
        self.args.len()
    }

    /// Argument `index`, or nil past the end.
    pub fn arg(&self, index: usize) -> NativeValue<'a> {
        NativeValue::wrap(self.args.get(index).copied().unwrap_or(Value::Nil))
    }

    pub fn type_of(&self, value: NativeValue<'a>) -> LuaType {
        match value.value {
            Value::Nil => LuaType::Nil,
            Value::Bool(_) => LuaType::Boolean,
            Value::Integer(_) | Value::Float(_) => LuaType::Number,
            Value::String(_) => LuaType::String,
            Value::Table(_) => LuaType::Table,
            Value::Closure(_) | Value::Native(_) | Value::NativeClosure(_) => LuaType::Function,
            Value::Thread(_) => LuaType::Thread,
            Value::Userdata(_) | Value::LightUserdata(..) => LuaType::Userdata,
        }
    }

    /// A string's bytes, borrowed; any bytes, not only UTF-8.
    pub fn string_bytes(&self, value: NativeValue<'a>) -> Option<&[u8]> {
        match value.value {
            Value::String(handle) => self.heap.string_bytes(handle),
            _ => None,
        }
    }

    /// `rawget(table, key)`. `None` if `table` is not a table. A nil or NaN
    /// key reads nil.
    pub fn raw_get(&self, table: NativeValue<'a>, key: NativeValue<'a>) -> Option<NativeValue<'a>> {
        let Value::Table(handle) = table.value else {
            return None;
        };
        let value = match self.heap.key_view(key.value) {
            Ok(key) => self.heap.table_get_view(handle, key)?,
            Err(_) => Value::Nil,
        };
        Some(NativeValue::wrap(value))
    }

    /// `rawset(table, key, value)`, with the ordinary key rules. Nil
    /// deletes.
    pub fn raw_set(
        &mut self,
        table: NativeValue<'a>,
        key: NativeValue<'a>,
        value: NativeValue<'a>,
    ) -> Result<(), RawSetError> {
        let Value::Table(handle) = table.value else {
            return Err(RawSetError::NotATable);
        };
        if self.heap.update_string_key(handle, key.value, value.value) {
            return Ok(());
        }
        let normalized = self
            .heap
            .normalize_value(key.value)
            .map_err(|_| RawSetError::InvalidKey)?;
        match self
            .heap
            .table_insert(handle, normalized, key.value, value.value)
        {
            Ok(()) => Ok(()),
            Err(crate::heap::InsertError::NoTable) => Err(RawSetError::NotATable),
            Err(crate::heap::InsertError::Memory) => {
                self.out_of_memory = true;
                Err(RawSetError::Memory)
            }
        }
    }

    /// `rawlen(value)`: a table's border or a string's byte length.
    /// `a == b` without `__eq`: numbers by value, strings by bytes,
    /// everything else by identity.
    pub fn raw_equal(&self, a: NativeValue<'a>, b: NativeValue<'a>) -> bool {
        crate::compare::equal(self.heap, a.value, b.value)
    }

    pub fn raw_len(&self, value: NativeValue<'a>) -> Option<i64> {
        match value.value {
            Value::String(handle) => i64::try_from(self.heap.string_bytes(handle)?.len()).ok(),
            Value::Table(handle) => Some(self.heap.tables.get(handle)?.table.raw_border()),
            _ => None,
        }
    }

    /// A value's metatable, without `__metatable` protection: a table's
    /// own, or its type's (ADR 0034). `None` if it has none.
    pub fn metatable(&self, value: NativeValue<'a>) -> Option<NativeValue<'a>> {
        let metatable = self.heap.metatable_of(value.value)?;
        Some(NativeValue::wrap(Value::Table(metatable)))
    }

    /// Set or clear a table's or a full userdata's own metatable, without
    /// `__metatable` protection. `false` if `object` is neither, or
    /// `metatable` is neither a table nor absent.
    pub fn set_metatable(
        &mut self,
        object: NativeValue<'a>,
        metatable: Option<NativeValue<'a>>,
    ) -> bool {
        let metatable = match metatable.map(|value| value.value) {
            None => None,
            Some(Value::Table(metatable)) => Some(metatable),
            Some(_) => return false,
        };
        // The one place a metatable is set registers the object for
        // finalization when the metatable has `__gc` (ADR 0047).
        self.heap.set_metatable(object.value, metatable)
    }

    /// A new full userdata with `len` zeroed bytes Lua cannot read and
    /// `user_values` nil user values (ADR 0042).
    pub fn new_userdata(
        &mut self,
        len: usize,
        user_values: usize,
    ) -> Result<NativeValue<'a>, UserdataError> {
        if len > crate::userdata::MAX_USERDATA_BYTES {
            self.out_of_memory = true;
            return Err(UserdataError::Memory);
        }
        let made = self
            .heap
            .alloc_userdata(self.max_objects, user_values, len as u64, || {
                crate::userdata::Payload::Bytes(vec![0; len].into_boxed_slice())
            });
        self.made(made)
    }

    /// A new full userdata holding `value`, of a type registered with the
    /// runtime's registry, with `user_values` nil user values. It counts
    /// `value.logical_size()` bytes (ADR 0044).
    pub fn new_host_userdata<T: crate::userdata::HostUserdata>(
        &mut self,
        value: T,
        user_values: usize,
    ) -> Result<NativeValue<'a>, UserdataError> {
        if !self.registry.has_userdata_type::<T>() {
            return Err(UserdataError::Unregistered);
        }
        let charge = value.logical_size();
        let made = self
            .heap
            .alloc_userdata(self.max_objects, user_values, charge, || {
                crate::userdata::Payload::Host {
                    symbol: T::SYMBOL,
                    value: Box::new(value),
                }
            });
        self.made(made)
    }

    fn made(
        &mut self,
        made: Result<crate::id::Handle<crate::heap::UserdataObj>, crate::id::TerminationReason>,
    ) -> Result<NativeValue<'a>, UserdataError> {
        match made {
            Ok(handle) => Ok(NativeValue::wrap(Value::Userdata(handle))),
            Err(_) => {
                self.out_of_memory = true;
                Err(UserdataError::Memory)
            }
        }
    }

    /// A light userdata with the host's key (ADR 0043).
    pub fn light_userdata(&self, key: crate::userdata::HostLightKey) -> NativeValue<'a> {
        NativeValue::wrap(Value::LightUserdata(crate::value::LightDomain::Host, key.0))
    }

    /// A light userdata's identity; `None` for anything else.
    pub fn light(&self, value: NativeValue<'a>) -> Option<crate::userdata::LightUserdata> {
        match value.value {
            Value::LightUserdata(domain, bits) => {
                Some(crate::userdata::LightUserdata { domain, bits })
            }
            _ => None,
        }
    }

    /// Whether `value` is a full userdata.
    pub fn is_full_userdata(&self, value: NativeValue<'a>) -> bool {
        matches!(value.value, Value::Userdata(_))
    }

    fn userdata_object(&self, value: NativeValue<'a>) -> Option<&crate::heap::UserdataObj> {
        match value.value {
            Value::Userdata(handle) => self.heap.userdata.get(handle),
            _ => None,
        }
    }

    fn userdata_object_mut(
        &mut self,
        value: NativeValue<'a>,
    ) -> Option<&mut crate::heap::UserdataObj> {
        match value.value {
            Value::Userdata(handle) => self.heap.userdata.get_mut(handle),
            _ => None,
        }
    }

    /// A byte userdata's bytes, borrowed for no longer than the call can
    /// do nothing else.
    pub fn userdata_bytes(&self, value: NativeValue<'a>) -> Option<&[u8]> {
        self.userdata_object(value)?.payload.bytes()
    }

    pub fn userdata_bytes_mut(&mut self, value: NativeValue<'a>) -> Option<&mut [u8]> {
        self.userdata_object_mut(value)?.payload.bytes_mut()
    }

    /// The host value of a full userdata, if it is a `T`. The borrow holds
    /// the call: nothing that could run Lua or collect can happen while
    /// it lives, and a native function never calls Lua.
    pub fn userdata_ref<T: crate::userdata::HostUserdata>(
        &self,
        value: NativeValue<'a>,
    ) -> Option<&T> {
        self.userdata_object(value)?.payload.host::<T>()
    }

    /// The borrow is the call's: while it lives, no other borrow of any
    /// userdata, and no other use of the call, compiles.
    ///
    /// ```compile_fail
    /// use moonseed::{NativeCall, NativeOutcome, ProofCounter};
    /// fn twice(call: &mut NativeCall<'_>) -> NativeOutcome {
    ///     let this = call.arg(0);
    ///     let first = call.userdata_mut::<ProofCounter>(this);
    ///     let second = call.userdata_mut::<ProofCounter>(this);
    ///     first.map(|counter| counter.count += 1);
    ///     NativeOutcome::Ready
    /// }
    /// ```
    ///
    /// ```compile_fail
    /// use moonseed::{NativeCall, NativeOutcome, ProofCounter};
    /// fn held(call: &mut NativeCall<'_>) -> NativeOutcome {
    ///     let this = call.arg(0);
    ///     let counter = call.userdata_mut::<ProofCounter>(this).unwrap();
    ///     call.push_integer(1);
    ///     counter.count += 1;
    ///     NativeOutcome::Ready
    /// }
    /// ```
    pub fn userdata_mut<T: crate::userdata::HostUserdata>(
        &mut self,
        value: NativeValue<'a>,
    ) -> Option<&mut T> {
        self.userdata_object_mut(value)?.payload.host_mut::<T>()
    }

    /// User value `n` (1-based) of a full userdata; `None` if `value` is
    /// not one or has no such user value.
    pub fn user_value(&self, value: NativeValue<'a>, n: usize) -> Option<NativeValue<'a>> {
        let object = self.userdata_object(value)?;
        let held = *object.user_values.get(n.checked_sub(1)?)?;
        Some(NativeValue::wrap(held))
    }

    /// Set user value `n` (1-based); `false` if there is no such slot.
    pub fn set_user_value(
        &mut self,
        value: NativeValue<'a>,
        n: usize,
        held: NativeValue<'a>,
    ) -> bool {
        let Some(slot) = n
            .checked_sub(1)
            .and_then(|index| self.userdata_object_mut(value)?.user_values.get_mut(index))
        else {
            return false;
        };
        *slot = held.value;
        true
    }

    /// Change the logical bytes a host value counts, as it grows or
    /// shrinks (ADR 0044). Growth past the logical-heap quota is refused
    /// before it is counted.
    pub fn set_userdata_charge(
        &mut self,
        value: NativeValue<'a>,
        bytes: u64,
    ) -> Result<(), UserdataError> {
        let Value::Userdata(handle) = value.value else {
            return Err(UserdataError::NotUserdata);
        };
        let object = self
            .heap
            .userdata
            .get(handle)
            .ok_or(UserdataError::NotUserdata)?;
        if object.payload.symbol().is_none() {
            return Err(UserdataError::NotUserdata);
        }
        let old = object.charge;
        if bytes > old && !self.heap.gc.fits(bytes - old) {
            self.out_of_memory = true;
            return Err(UserdataError::Memory);
        }
        // Growth is charged; a shrink leaves the logical heap at once, so
        // shrinking and growing again within one call does not count
        // twice.
        if bytes > old {
            self.heap.gc.charge(bytes - old);
        } else {
            self.heap.give_back(old - bytes);
        }
        if let Some(object) = self.heap.userdata.get_mut(handle) {
            object.charge = bytes;
        }
        Ok(())
    }

    /// Record Lua's argument error for argument `index` (0-based),
    /// The caller's name, argument number, and `message` are combined on
    /// the cold error path as `luaL_argerror` does. Return it to raise it.
    pub fn arg_error(&mut self, index: usize, message: &str) -> NativeOutcome {
        self.error = Some(NativeError::Argument(index, message.as_bytes().to_vec()));
        NativeOutcome::Fault
    }

    /// Raise a `luaL_error`-style native message.
    pub fn error(&mut self, message: &str) -> NativeOutcome {
        self.error = Some(NativeError::Message(message.as_bytes().to_vec(), true));
        NativeOutcome::Fault
    }

    /// Raise a VM primitive error, which has no `luaL_where` prefix.
    pub fn raw_error(&mut self, message: &str) -> NativeOutcome {
        self.error = Some(NativeError::Message(message.as_bytes().to_vec(), false));
        NativeOutcome::Fault
    }

    /// Record Lua's argument error for argument `index` (0-based), as
    /// `luaL_typeerror` words it using the call-site name and actual type.
    /// Return it to raise it.
    pub fn type_error(&mut self, index: usize, expected: &str) -> NativeOutcome {
        let got: Vec<u8> = if index >= self.args.len() {
            b"no value".to_vec()
        } else {
            let value = self.args[index];
            match crate::index::metamethod(self.heap, value, b"__name") {
                Some(Value::String(name)) => {
                    self.heap.string_bytes(name).unwrap_or_default().to_vec()
                }
                _ if matches!(value, Value::LightUserdata(..)) => b"light userdata".to_vec(),
                _ => crate::heap::type_name(value).as_bytes().to_vec(),
            }
        };
        let mut message = format!("{expected} expected, got ").into_bytes();
        message.extend(got);
        self.error = Some(NativeError::Argument(index, message));
        NativeOutcome::Fault
    }

    pub fn push(&mut self, value: NativeValue<'a>) {
        self.results.push(value.value);
    }

    /// Argument `index` if it is an integer.
    pub fn integer(&self, index: usize) -> Option<i64> {
        match self.args.get(index)? {
            Value::Integer(integer) => Some(*integer),
            _ => None,
        }
    }

    /// Argument `index` if it is a number, converted to a float.
    pub fn number(&self, index: usize) -> Option<f64> {
        match self.args.get(index)? {
            Value::Integer(integer) => Some(*integer as f64),
            Value::Float(float) => Some(*float),
            _ => None,
        }
    }

    pub fn boolean(&self, index: usize) -> Option<bool> {
        match self.args.get(index)? {
            Value::Bool(bit) => Some(*bit),
            _ => None,
        }
    }

    /// True for a nil argument and for one past the end.
    pub fn is_nil(&self, index: usize) -> bool {
        matches!(self.args.get(index), None | Some(Value::Nil))
    }

    pub fn push_integer(&mut self, value: i64) {
        self.results.push(Value::Integer(value));
    }

    pub fn push_number(&mut self, value: f64) {
        self.results.push(Value::Float(value));
    }

    pub fn push_boolean(&mut self, value: bool) {
        self.results.push(Value::Bool(value));
    }

    pub fn push_nil(&mut self) {
        self.results.push(Value::Nil);
    }

    /// Return argument `index` unchanged, whatever its type (nil if absent).
    pub fn push_arg(&mut self, index: usize) {
        self.results
            .push(self.args.get(index).copied().unwrap_or(Value::Nil));
    }

    /// This call's effect id. `Some` only for [`NativePolicy::External`].
    pub fn effect(&self) -> Option<EffectId> {
        self.effect
    }

    /// The embedder's journal. `Some` only for [`NativePolicy::External`].
    pub fn journal(&mut self) -> Option<&mut Journal> {
        self.journal.as_deref_mut()
    }
}

#[derive(Clone)]
pub(crate) struct NativeEntry {
    pub(crate) policy: NativePolicy,
    pub(crate) function: NativeFn,
    pub(crate) callback: Option<std::rc::Rc<crate::api::native::Callback>>,
    pub(crate) typed: Option<std::rc::Rc<crate::api::native::TypedCallback>>,
    /// A function the VM implements itself because it calls Lua or
    /// raises an error: `pcall`, `xpcall`, `error`. Its `function` is never
    /// called.
    pub(crate) builtin: Option<Builtin>,
}

/// The base functions that need the VM rather than a `NativeCall`: they
/// raise errors, call Lua, make strings or native values, or act on the
/// collector or the output (ADR 0024, ADR 0031).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Builtin {
    /// The runtime mechanism that the OS lane registers as `os.exit`.
    #[allow(dead_code)] // Registered by the OS lane; this lane installs no OS library.
    Exit,
    Os(crate::oslib::OsFn),
    Io(crate::iolib::IoFn),
    #[cfg(test)]
    CapabilitySmoke,
    Pcall,
    Xpcall,
    Error,
    Assert,
    Type,
    ToString,
    ToNumber,
    Print,
    Next,
    Pairs,
    Ipairs,
    /// The iterator `ipairs` returns.
    IpairsNext,
    CollectGarbage,
    Load,
    LoadFile,
    DoFile,
    /// A `math` function (ADR 0032).
    Math(crate::library::MathFn),
    /// A `table` function (ADR 0033).
    Table(crate::library::TableFn),
    /// A `string` function, or a string metamethod (ADR 0034).
    String(crate::strlib::StrFn),
    /// A UTF-8 function or stable iterator.
    Utf8(crate::utf8lib::Utf8Fn),
    /// `require` or a `package` searcher (ADR 0039).
    Package(crate::package::PkgFn),
    /// A `debug` function (ADR 0040).
    Debug(crate::debuglib::DbgFn),
    /// A `coroutine` function (ADR 0041).
    Coroutine(crate::corolib::CoFn),
    /// `warn` (ADR 0049).
    Warn,
}

/// Where `print` writes (ADR 0031). Host state, not snapshot state: a
/// restored runtime has none until the host sets one, and without one
/// `print` writes nothing. Each write is one external effect, committed
/// through the journal, so a replayed effect is not written twice.
/// Unstable API.
pub type Output = Box<dyn FnMut(&[u8])>;

/// Where warnings go (ADR 0049): `warn`'s and those of failed finalizers.
/// Host state, like the output sink. A warning is one external effect, given
/// to the sink as pieces; the flag is Lua's `tocont`, true for every
/// piece but the last. Control messages (a single piece starting with
/// `@`, such as `@on` and `@off`) are passed on: what they mean is the
/// sink's choice, as it is the warning function's in Lua. Unstable API.
pub type Warnings = Box<dyn FnMut(&[u8], bool)>;

/// Where `math.randomseed()` with no argument gets its two seed words
/// (ADR 0032). Host state, not snapshot state. Each word is an external
/// effect committed through the journal. Unstable API.
pub type Entropy = Box<dyn FnMut() -> i64>;

fn builtin_placeholder(_call: &mut NativeCall<'_>) -> NativeOutcome {
    NativeOutcome::Fault
}

type HookEntries = Vec<(String, std::rc::Rc<crate::api::hooks::HookCallback>)>;

/// Host native callbacks and userdata registrations, bound by stable symbols.
/// Cloning shares callback objects, not mutable registration maps. A runtime
/// owns its registry; Lua captures belong in [`crate::Runtime::make_closure`],
/// while Rust callback state remains host-owned and is not checkpointed.
/// Rebuild host registrations before [`crate::Runtime::restore`]; Moonseed
/// supplies its allowed library symbols through [`crate::Host`]. Missing symbols
/// fail with [`crate::SnapshotError::UnknownHostSymbol`]; userdata codecs and
/// policies must match. Typed argument failures become catchable Lua errors;
/// other misuse remains [`crate::ApiError`]. Callback panics propagate and poison
/// execution/snapshots: discard the runtime if the host catches one.
///
/// ```
/// use moonseed::{HostRegistry, NativePolicy, Runtime};
/// let mut registry = HostRegistry::new();
/// registry.typed("example.double", NativePolicy::VmLocal, |_, n: i64| Ok(n.wrapping_mul(2)));
/// let mut rt = Runtime::builder().registry(registry).build()?;
/// let double = rt.make_closure("example.double", ())?;
/// rt.globals().raw_set(&mut rt, "double", double)?;
/// # Ok::<(), moonseed::Error>(())
/// ```
#[derive(Clone)]
pub struct HostRegistry {
    entries: Vec<(String, HostFn)>,
    natives: Vec<(String, NativeEntry)>,
    /// Host userdata types, by symbol (ADR 0044).
    userdata_types: Vec<crate::userdata::HostType>,
    hooks: Option<Box<HookEntries>>,
}

impl HostRegistry {
    /// Create an empty native and userdata registry.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            natives: Vec::new(),
            userdata_types: Vec::new(),
            hooks: None,
        }
    }

    /// Register a synchronous debug hook under a stable snapshot symbol.
    /// Re-registering a symbol replaces its callback without changing its slot.
    pub fn register_hook<F>(&mut self, symbol: impl Into<String>, callback: F)
    where
        F: Fn(&mut crate::HookContext<'_>) -> crate::Result<crate::HookAction> + 'static,
    {
        let symbol = symbol.into();
        let hooks = self.hooks.get_or_insert_with(Box::default);
        let entry = (
            symbol.clone(),
            std::rc::Rc::new(callback) as std::rc::Rc<crate::api::hooks::HookCallback>,
        );
        if let Some(slot) = hooks.iter().position(|(name, _)| name == &symbol) {
            hooks[slot] = entry;
        } else {
            hooks.push(entry);
        }
    }
    pub(crate) fn hook_slot(&self, symbol: &str) -> Option<u32> {
        self.hooks
            .as_ref()?
            .iter()
            .position(|(name, _)| name == symbol)
            .and_then(|i| i.try_into().ok())
    }
    pub(crate) fn hook(
        &self,
        slot: u32,
    ) -> Option<(&str, &std::rc::Rc<crate::api::hooks::HookCallback>)> {
        self.hooks
            .as_ref()?
            .get(slot as usize)
            .map(|(name, callback)| (name.as_str(), callback))
    }

    /// Register a host userdata type that snapshots refuse (ADR 0045): a
    /// snapshot of a heap holding one fails with
    /// [`crate::SnapshotError::NonPortableUserdata`]. The safe choice for
    /// anything holding a handle, a lock, or an address. A later
    /// registration of the same symbol replaces it.
    pub fn register_userdata<T: crate::userdata::HostUserdata>(&mut self) {
        self.insert_userdata_type(crate::userdata::HostType::of::<T>());
    }

    /// Register a host userdata type that snapshots carry through its
    /// codec (ADR 0045). A restore needs the same symbol registered as
    /// portable.
    pub fn register_portable_userdata<T: crate::userdata::PortableUserdata>(&mut self) {
        self.insert_userdata_type(crate::userdata::HostType::portable::<T>());
    }

    /// Register a type whose snapshots contain only a bounded external key.
    /// Restore requires the same policy and resources in the host environment.
    pub fn register_rebind_userdata<T: crate::userdata::RebindUserdata>(&mut self) {
        self.insert_userdata_type(crate::userdata::HostType::rebind::<T>());
    }

    /// Inspect the snapshot policy registered under this host type symbol.
    pub fn userdata_policy(&self, symbol: &str) -> Option<crate::UserdataPolicy> {
        self.userdata_type(symbol).map(|entry| entry.policy.kind())
    }

    fn insert_userdata_type(&mut self, entry: crate::userdata::HostType) {
        self.userdata_types
            .retain(|held| held.symbol != entry.symbol && held.type_id != entry.type_id);
        self.userdata_types.push(entry);
    }

    pub(crate) fn has_userdata_type<T: 'static>(&self) -> bool {
        let type_id = std::any::TypeId::of::<T>();
        self.userdata_types
            .iter()
            .any(|entry| entry.type_id == type_id)
    }

    pub(crate) fn userdata_type(&self, symbol: &str) -> Option<crate::userdata::HostType> {
        self.userdata_types
            .iter()
            .find(|entry| entry.symbol == symbol)
            .copied()
    }

    /// Register a native function under a stable symbol. The symbol is its
    /// identity in Lua and in snapshots; a later registration of the same
    /// symbol replaces the implementation.
    pub fn register_native(
        &mut self,
        symbol: impl Into<String>,
        policy: NativePolicy,
        function: NativeFn,
    ) {
        self.insert_native(
            symbol.into(),
            NativeEntry {
                policy,
                function,
                callback: None,
                typed: None,
                builtin: None,
            },
        );
    }

    /// Register a callback under its stable snapshot symbol. Rust callback
    /// state is host state; Lua captures belong in a native closure.
    pub fn function<F>(&mut self, symbol: impl Into<String>, policy: NativePolicy, f: F)
    where
        F: Fn(&mut crate::NativeContext<'_>) -> crate::Result<crate::NativeReturn> + 'static,
    {
        self.insert_native(
            symbol.into(),
            NativeEntry {
                policy,
                function: builtin_placeholder,
                callback: Some(std::rc::Rc::new(f)),
                typed: None,
                builtin: None,
            },
        );
    }

    /// Register a typed callback. Argument conversion failures raise Lua's
    /// positioned argument error; other API failures remain host errors.
    pub fn typed<A, R, F>(&mut self, symbol: impl Into<String>, policy: NativePolicy, f: F)
    where
        A: crate::FromLuaMulti,
        R: crate::IntoLuaMulti,
        F: Fn(&mut crate::NativeContext<'_>, A) -> crate::Result<R> + 'static,
    {
        let fast: std::rc::Rc<crate::api::native::TypedCallback> = std::rc::Rc::new(move |cx| {
            let args = cx
                .arguments::<A>()
                .map_err(|error| cx.argument_error(error))?;
            let result = f(cx, args)?;
            cx.write_results(result)
        });
        let callback = fast.clone();
        self.insert_native(
            symbol.into(),
            NativeEntry {
                policy,
                function: builtin_placeholder,
                builtin: None,
                typed: Some(fast),
                callback: Some(std::rc::Rc::new(move |cx| {
                    callback(cx)?;
                    Ok(crate::NativeReturn::Return(cx.take_results()?))
                })),
            },
        );
    }

    pub(crate) fn register_builtin(&mut self, symbol: &str, builtin: Builtin) {
        self.insert_native(
            symbol.to_string(),
            NativeEntry {
                policy: NativePolicy::VmLocal,
                function: builtin_placeholder,
                callback: None,
                typed: None,
                builtin: Some(builtin),
            },
        );
    }

    fn insert_native(&mut self, symbol: String, entry: NativeEntry) {
        if let Some(slot) = self.natives.iter().position(|(name, _)| name == &symbol) {
            self.natives[slot] = (symbol, entry);
        } else {
            self.natives.push((symbol, entry));
        }
    }

    pub(crate) fn fill_missing_natives(&mut self, source: &Self) {
        for (symbol, entry) in &source.natives {
            if self.native_slot(symbol).is_none() {
                self.insert_native(symbol.clone(), entry.clone());
            }
        }
    }

    pub(crate) fn native_slot(&self, symbol: &str) -> Option<usize> {
        self.natives.iter().position(|(name, _)| name == symbol)
    }

    pub(crate) fn native(&self, slot: usize) -> Option<&NativeEntry> {
        self.natives.get(slot).map(|(_, entry)| entry)
    }

    /// Register an integer-only legacy proof callback.
    #[deprecated(note = "use HostRegistry::function or HostRegistry::typed")]
    pub(crate) fn register(&mut self, symbol: impl Into<String>, function: HostFn) {
        let symbol = symbol.into();
        if let Some(slot) = self.entries.iter().position(|(name, _)| name == &symbol) {
            self.entries[slot] = (symbol, function);
        } else {
            self.entries.push((symbol, function));
        }
    }

    #[doc(hidden)] // Kernel proof fixture; outside the embedding API.
    pub fn proof() -> Self {
        let mut registry = Self::new();
        registry.register("mark", mark);
        registry.register("park", park);
        registry.register_native("add", NativePolicy::VmLocal, native_add);
        registry.register_native("sub", NativePolicy::VmLocal, native_sub);
        registry.register_native("many", NativePolicy::VmLocal, native_many);
        registry.register_native("none", NativePolicy::VmLocal, native_none);
        registry.register_native("mark", NativePolicy::External, native_mark);
        registry.register_native("park", NativePolicy::VmLocal, native_park);
        registry.register_native("second", NativePolicy::VmLocal, native_second);
        registry.register_native("upto", NativePolicy::VmLocal, native_upto);
        register_userdata_proof(&mut registry);
        crate::library::register_standard(&mut registry);
        crate::debuglib::register_debug(&mut registry);
        registry
    }

    pub(crate) fn contains(&self, symbol: &str) -> bool {
        self.entries.iter().any(|(name, _)| name == symbol)
    }

    pub(crate) fn call(&self, symbol: &str, ctx: &mut HostCtx<'_>, arg: i64) -> Option<HostResult> {
        let function = self.entries.iter().find(|(name, _)| name == symbol)?.1;
        Some(function(ctx, arg))
    }
}

impl Default for HostRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Bounded host effect. The returned outcome is the sequence, unless the
/// journal already committed a different outcome for this [`EffectId`].
pub fn mark(ctx: &mut HostCtx<'_>, arg: i64) -> HostResult {
    let effect = ctx.effect;
    let outcome = ctx.journal.commit(effect, arg, || effect.sequence as i64);
    match outcome {
        Ok(outcome) => HostResult::Ready(outcome),
        Err(_) => HostResult::Fault,
    }
}

/// Parks the caller until [`crate::Runtime::complete_wait`]. Does not commit.
pub fn park(_ctx: &mut HostCtx<'_>, _arg: i64) -> HostResult {
    HostResult::Pending(WaitKey(1))
}

/// `add(a, b)`: integer sum, wrapping. Faults unless both are integers.
pub fn native_add(call: &mut NativeCall<'_>) -> NativeOutcome {
    match (call.integer(0), call.integer(1)) {
        (Some(a), Some(b)) => {
            call.push_integer(a.wrapping_add(b));
            NativeOutcome::Ready
        }
        _ => NativeOutcome::Fault,
    }
}

/// `sub(a, b)`: like `add`, a different symbol for identity tests.
pub fn native_sub(call: &mut NativeCall<'_>) -> NativeOutcome {
    match (call.integer(0), call.integer(1)) {
        (Some(a), Some(b)) => {
            call.push_integer(a.wrapping_sub(b));
            NativeOutcome::Ready
        }
        _ => NativeOutcome::Fault,
    }
}

/// `many()`: returns `10, nil, 30`.
pub fn native_many(call: &mut NativeCall<'_>) -> NativeOutcome {
    call.push_integer(10);
    call.push_nil();
    call.push_integer(30);
    NativeOutcome::Ready
}

/// `none()`: returns nothing.
pub fn native_none(_call: &mut NativeCall<'_>) -> NativeOutcome {
    NativeOutcome::Ready
}

/// `mark(x)`: the proof kernel's effect as a native function. Commits
/// through the journal, so a replayed effect id returns the stored outcome.
pub fn native_mark(call: &mut NativeCall<'_>) -> NativeOutcome {
    let (Some(effect), Some(arg)) = (call.effect(), call.integer(0)) else {
        return NativeOutcome::Fault;
    };
    let Some(journal) = call.journal() else {
        return NativeOutcome::Fault;
    };
    let Ok(outcome) = journal.commit(effect, arg, || effect.sequence as i64) else {
        return NativeOutcome::Fault;
    };
    call.push_integer(outcome);
    NativeOutcome::Ready
}

/// `park()`: waits for [`crate::Runtime::complete_wait`] with key 1.
pub fn native_park(_call: &mut NativeCall<'_>) -> NativeOutcome {
    NativeOutcome::Pending(WaitKey(1))
}

/// `second(a, b)`: returns `b`. As `__index`, returns the key.
pub fn native_second(call: &mut NativeCall<'_>) -> NativeOutcome {
    call.push_arg(1);
    NativeOutcome::Ready
}

/// `upto(n, i)`: an iterator. Returns `i + 1`, or 1 for a nil `i`, while
/// that is at most `n`; then nothing.
pub fn native_upto(call: &mut NativeCall<'_>) -> NativeOutcome {
    let last = if call.is_nil(1) {
        Some(0)
    } else {
        call.integer(1)
    };
    match (call.integer(0), last.and_then(|last| last.checked_add(1))) {
        (Some(n), Some(next)) => {
            if next <= n {
                call.push_integer(next);
            }
            NativeOutcome::Ready
        }
        _ => NativeOutcome::Fault,
    }
}

/// Userdata proof natives (ADR 0042 to ADR 0045): what a host gives Lua to
/// make and use userdata, as the reference harness gives Lua 5.4.9 through
/// its C API. `newud(size [, nuv])` makes a byte userdata, `light(n)` a
/// light userdata with host key `n`, `udpeek(u, i)` and `udpoke(u, i, b)`
/// read and write byte `i` (0-based).
#[doc(hidden)] // Kernel proof fixture; outside the embedding API.
pub const USERDATA_NATIVES: [(&str, NativeFn); 10] = [
    ("newud", native_newud),
    ("light", native_light),
    ("udpeek", native_udpeek),
    ("udpoke", native_udpoke),
    ("counter_new", native_counter_new),
    ("counter_get", native_counter_get),
    ("counter_add", native_counter_add),
    ("counter_grow", native_counter_grow),
    ("handle_new", native_handle_new),
    ("handle_get", native_handle_get),
];

/// Register the userdata proof natives and their host types: the
/// portable `ProofCounter` and the snapshot-refusing `ProofHandle`.
#[doc(hidden)] // Kernel proof fixture; outside the embedding API.
pub fn register_userdata_proof(registry: &mut HostRegistry) {
    for (symbol, function) in USERDATA_NATIVES {
        registry.register_native(symbol, NativePolicy::VmLocal, function);
    }
    registry.register_portable_userdata::<ProofCounter>();
    registry.register_userdata::<ProofHandle>();
}

/// A portable host userdata for proofs: a count, and the logical bytes it
/// claims, which `counter_grow` changes.
#[derive(Debug, PartialEq)]
#[doc(hidden)] // Kernel proof fixture; outside the embedding API.
pub struct ProofCounter {
    pub count: i64,
    pub size: u64,
}

impl crate::userdata::HostUserdata for ProofCounter {
    const SYMBOL: &'static str = "moonseed.Counter";
    fn logical_size(&self) -> u64 {
        self.size
    }
}

impl crate::userdata::PortableUserdata for ProofCounter {
    fn encode(&self) -> Vec<u8> {
        let mut bytes = self.count.to_le_bytes().to_vec();
        bytes.extend(self.size.to_le_bytes());
        bytes
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let (count, size) = bytes.split_at_checked(8)?;
        Some(Self {
            count: i64::from_le_bytes(count.try_into().ok()?),
            size: u64::from_le_bytes(size.try_into().ok()?),
        })
    }
}

/// A host userdata no snapshot carries, as a file handle or a lock would
/// be.
#[derive(Debug)]
#[doc(hidden)] // Kernel proof fixture; outside the embedding API.
pub struct ProofHandle(pub i64);

impl crate::userdata::HostUserdata for ProofHandle {
    const SYMBOL: &'static str = "moonseed.Handle";
    fn logical_size(&self) -> u64 {
        8
    }
}

fn native_newud(call: &mut NativeCall<'_>) -> NativeOutcome {
    let size = call.integer(0).and_then(|size| usize::try_from(size).ok());
    let user_values = if call.is_nil(1) {
        Some(0)
    } else {
        call.integer(1).and_then(|n| usize::try_from(n).ok())
    };
    let (Some(size), Some(user_values)) = (size, user_values) else {
        return NativeOutcome::Fault;
    };
    match call.new_userdata(size, user_values) {
        Ok(userdata) => {
            call.push(userdata);
            NativeOutcome::Ready
        }
        Err(_) => NativeOutcome::Fault,
    }
}

fn native_light(call: &mut NativeCall<'_>) -> NativeOutcome {
    let Some(key) = call.integer(0) else {
        return NativeOutcome::Fault;
    };
    let light = call.light_userdata(crate::userdata::HostLightKey(key as u64));
    call.push(light);
    NativeOutcome::Ready
}

fn native_udpeek(call: &mut NativeCall<'_>) -> NativeOutcome {
    let userdata = call.arg(0);
    let byte = call
        .integer(1)
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| call.userdata_bytes(userdata)?.get(index).copied());
    match byte {
        Some(byte) => {
            call.push_integer(i64::from(byte));
            NativeOutcome::Ready
        }
        None => NativeOutcome::Fault,
    }
}

fn native_udpoke(call: &mut NativeCall<'_>) -> NativeOutcome {
    let userdata = call.arg(0);
    let (Some(index), Some(byte)) = (call.integer(1), call.integer(2)) else {
        return NativeOutcome::Fault;
    };
    let slot = usize::try_from(index)
        .ok()
        .and_then(|index| call.userdata_bytes_mut(userdata)?.get_mut(index));
    match slot {
        Some(slot) => {
            *slot = byte as u8;
            NativeOutcome::Ready
        }
        None => NativeOutcome::Fault,
    }
}

/// `counter_new(n [, mt [, size]])`: a counter at `n` with metatable `mt`,
/// claiming `size` logical bytes (default 16).
fn native_counter_new(call: &mut NativeCall<'_>) -> NativeOutcome {
    let Some(count) = call.integer(0) else {
        return call.type_error(0, "number");
    };
    let size = call.integer(2).map_or(16, |size| size.max(0) as u64);
    let metatable = call.arg(1);
    let counter = match call.new_host_userdata(ProofCounter { count, size }, 1) {
        Ok(counter) => counter,
        Err(_) => return NativeOutcome::Fault,
    };
    if !call.is_nil(1) && !call.set_metatable(counter, Some(metatable)) {
        return call.type_error(1, "table");
    }
    call.push(counter);
    NativeOutcome::Ready
}

fn native_counter_get(call: &mut NativeCall<'_>) -> NativeOutcome {
    let this = call.arg(0);
    let Some(counter) = call.userdata_ref::<ProofCounter>(this) else {
        return call.type_error(0, ProofCounter::SYMBOL_NAME);
    };
    let count = counter.count;
    call.push_integer(count);
    NativeOutcome::Ready
}

fn native_counter_add(call: &mut NativeCall<'_>) -> NativeOutcome {
    let this = call.arg(0);
    let Some(by) = call.integer(1) else {
        return call.type_error(1, "number");
    };
    let Some(counter) = call.userdata_mut::<ProofCounter>(this) else {
        return call.type_error(0, ProofCounter::SYMBOL_NAME);
    };
    counter.count = counter.count.wrapping_add(by);
    call.push_arg(0);
    NativeOutcome::Ready
}

/// `counter_grow(c, size)`: the counter now claims `size` logical bytes,
/// as a host value that grew would report.
fn native_counter_grow(call: &mut NativeCall<'_>) -> NativeOutcome {
    let this = call.arg(0);
    let Some(size) = call.integer(1).and_then(|size| u64::try_from(size).ok()) else {
        return call.type_error(1, "number");
    };
    if call.userdata_ref::<ProofCounter>(this).is_none() {
        return call.type_error(0, ProofCounter::SYMBOL_NAME);
    }
    if call.set_userdata_charge(this, size).is_err() {
        return NativeOutcome::Fault;
    }
    if let Some(counter) = call.userdata_mut::<ProofCounter>(this) {
        counter.size = size;
    }
    NativeOutcome::Ready
}

fn native_handle_new(call: &mut NativeCall<'_>) -> NativeOutcome {
    let Some(n) = call.integer(0) else {
        return call.type_error(0, "number");
    };
    match call.new_host_userdata(ProofHandle(n), 0) {
        Ok(handle) => {
            call.push(handle);
            NativeOutcome::Ready
        }
        Err(_) => NativeOutcome::Fault,
    }
}

fn native_handle_get(call: &mut NativeCall<'_>) -> NativeOutcome {
    let this = call.arg(0);
    let Some(handle) = call.userdata_ref::<ProofHandle>(this) else {
        return call.type_error(0, ProofHandle::SYMBOL_NAME);
    };
    let n = handle.0;
    call.push_integer(n);
    NativeOutcome::Ready
}

impl ProofCounter {
    const SYMBOL_NAME: &'static str = <Self as crate::userdata::HostUserdata>::SYMBOL;
}

impl ProofHandle {
    const SYMBOL_NAME: &'static str = <Self as crate::userdata::HostUserdata>::SYMBOL;
}
