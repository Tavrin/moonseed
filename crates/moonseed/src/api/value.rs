use std::cell::RefCell;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;

use crate::heap::Heap;
use crate::id::{ObjectId, OwnerToken};
use crate::{HostUserdata, LightUserdata, LuaType, Runtime};

use super::roots::{RootTable, Rooted};
use super::{ConversionError, FromLua, IntoLua, Result};

/// An owned Lua value. Object variants keep their targets alive (ADR 0053).
/// Clones share one root; the last drop releases it without borrowing the VM.
/// Roots can outlive the [`Runtime`] as Rust objects, but using them in another
/// runtime (including after restore) returns [`super::ApiError::WrongRuntime`].
/// [`ObjectId`] survives checkpoints without keeping anything alive. Use
/// [`ValueRef`] for a borrowed view and [`super::Coerce`] for explicit coercion.
///
/// ```
/// use moonseed::{Runtime, Value};
/// let mut rt = Runtime::builder().build()?;
/// let table = rt.create_table()?;
/// let kept = Value::Table(table.clone());
/// drop(table); // kept still roots the table
/// assert!(kept.id().is_some());
/// let restored = Runtime::restore(&rt.snapshot().unwrap(), &moonseed::Host::default())?;
/// assert!(matches!(kept.as_ref(&restored), Err(moonseed::Error::Api(moonseed::ApiError::WrongRuntime))));
/// # Ok::<(), moonseed::Error>(())
/// ```
#[derive(Clone, Debug)]
pub enum Value {
    /// Lua nil.
    Nil,
    /// Lua boolean.
    Boolean(bool),
    /// Lua integer.
    Integer(i64),
    /// Lua floating-point number.
    Number(f64),
    /// An opaque identity token.
    LightUserdata(LightUserdata),
    /// A rooted byte string.
    String(LuaString),
    /// A rooted table.
    Table(Table),
    /// A function of any supported kind.
    Function(Function),
    /// A rooted Lua thread.
    Thread(Thread),
    /// A rooted full userdata.
    UserData(AnyUserData),
}

macro_rules! object_type {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        /// Clones share a root in one [`Runtime`]; the last drop releases it.
        /// Operations reject foreign runtime ownership. Restore preserves its
        /// logical [`ObjectId`], but roots must be reacquired in the new runtime.
        /// See [`Value`] for an ownership example and [`Runtime::snapshot`] for
        /// checkpoints. Borrowed string/payload views end before execution.
        #[derive(Clone)]
        pub struct $name(pub(crate) Rooted);

        impl $name {
            /// The logical identity, preserved across snapshots.
            pub fn id(&self) -> ObjectId {
                // These wrappers are only made for collectable objects.
                self.0.id.expect("collectable object identity")
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_tuple(stringify!($name)).field(&self.0.id).finish()
            }
        }
    };
}

object_type!(LuaString, "A rooted Lua byte string.");
object_type!(Table, "A rooted Lua table. Raw operations never call Lua.");
object_type!(Thread, "A rooted Lua thread.");
object_type!(
    AnyUserData,
    "A rooted full userdata, with checked Rust payload borrows."
);

/// A Lua, native, native closure, or builtin function.
/// Clones root the same collectable closure in one [`Runtime`]. Bare natives
/// have a stable registered symbol and no [`ObjectId`]. Foreign roots fail with
/// [`super::ApiError::WrongRuntime`]. Use [`Runtime::call`] outside callbacks and
/// [`super::NativeContext::call_lua`] inside them. Closures/captures are snapshot
/// state; callback code must be re-registered through [`crate::HostRegistry`].
/// See the crate quick start and [`super::Host`] for execution/restore examples.
#[derive(Clone)]
pub struct Function(pub(crate) Rooted);

impl fmt::Debug for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Function").field(&self.0.id).finish()
    }
}

/// A function's implementation kind, for introspection only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FunctionKind {
    /// A compiled Lua closure.
    Lua,
    /// A host native function.
    Native,
    /// A collectable native closure.
    NativeClosure,
    /// A function implemented by the VM.
    Builtin,
}

/// A thread's drive state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ThreadStatus {
    /// Ready to execute, including a fuel pause.
    Ready,
    /// Suspended by Lua yield, or not started yet.
    Suspended,
    /// Waiting for a host operation.
    Waiting,
    /// Completed successfully.
    Completed,
    /// Ended with an uncaught Lua error.
    Failed,
}

impl Value {
    /// A collectable object's identity; primitives and bare natives have none.
    pub fn id(&self) -> Option<ObjectId> {
        match self {
            Self::String(value) => Some(value.id()),
            Self::Table(value) => Some(value.id()),
            Self::Function(value) => value.id(),
            Self::Thread(value) => Some(value.id()),
            Self::UserData(value) => Some(value.id()),
            _ => None,
        }
    }

    /// Lua's language-level type.
    pub fn lua_type(&self) -> LuaType {
        match self {
            Self::Nil => LuaType::Nil,
            Self::Boolean(_) => LuaType::Boolean,
            Self::Integer(_) | Self::Number(_) => LuaType::Number,
            Self::LightUserdata(_) | Self::UserData(_) => LuaType::Userdata,
            Self::String(_) => LuaType::String,
            Self::Table(_) => LuaType::Table,
            Self::Function(_) => LuaType::Function,
            Self::Thread(_) => LuaType::Thread,
        }
    }

    /// Borrow this value without adding another root.
    pub fn as_ref<'a>(&self, runtime: &'a Runtime) -> Result<ValueRef<'a>> {
        let value = self.raw(runtime)?;
        Ok(ValueRef::new(runtime, value))
    }

    pub(crate) fn check(&self, runtime: &Runtime) -> Result<()> {
        self.raw(runtime).map(|_| ())
    }

    #[inline]
    pub(crate) fn raw(&self, runtime: &Runtime) -> Result<crate::value::Value> {
        use crate::value::Value as Raw;
        Ok(match self {
            Self::Nil => Raw::Nil,
            Self::Boolean(value) => Raw::Bool(*value),
            Self::Integer(value) => Raw::Integer(*value),
            Self::Number(value) => Raw::Float(*value),
            Self::LightUserdata(value) => Raw::LightUserdata(value.domain, value.bits),
            Self::String(value) => value.0.value(runtime.owner())?,
            Self::Table(value) => value.0.value(runtime.owner())?,
            Self::Function(value) => value.0.value(runtime.owner())?,
            Self::Thread(value) => value.0.value(runtime.owner())?,
            Self::UserData(value) => value.0.value(runtime.owner())?,
        })
    }

    #[inline]
    pub(crate) fn wrap(
        value: crate::value::Value,
        heap: &Heap,
        owner: OwnerToken,
        roots: Option<&Rc<RefCell<RootTable>>>,
    ) -> Result<Self> {
        use crate::value::Value as Raw;
        let root = || -> Result<Rooted> {
            let roots = roots.ok_or(super::ApiError::Released)?;
            let id = heap.object_id_of_value(value);
            if !matches!(value, Raw::Native(_)) && id.is_none() {
                return Err(super::ApiError::Released.into());
            }
            Ok(Rooted::new(roots, owner, id, value))
        };
        Ok(match value {
            Raw::Nil => Self::Nil,
            Raw::Bool(value) => Self::Boolean(value),
            Raw::Integer(value) => Self::Integer(value),
            Raw::Float(value) => Self::Number(value),
            Raw::LightUserdata(domain, bits) => Self::LightUserdata(LightUserdata { domain, bits }),
            Raw::String(_) => Self::String(LuaString(root()?)),
            Raw::Table(_) => Self::Table(Table(root()?)),
            Raw::Closure(_) | Raw::Native(_) | Raw::NativeClosure(_) => {
                Self::Function(Function(root()?))
            }
            Raw::Thread(_) => Self::Thread(Thread(root()?)),
            Raw::Userdata(_) => Self::UserData(AnyUserData(root()?)),
        })
    }
}

/// An ephemeral view tied to the runtime borrow. It adds no root.
///
/// ```compile_fail
/// use moonseed::{Runtime, ValueRef};
/// let view: ValueRef<'_>;
/// {
///     let mut runtime = Runtime::builder().build().unwrap();
///     let string = runtime.create_string(b"borrowed").unwrap();
///     view = runtime.object_ref(string.id()).unwrap();
/// }
/// let _ = view.lua_type();
/// ```
#[derive(Clone, Copy)]
pub struct ValueRef<'a> {
    pub(crate) value: crate::value::Value,
    heap: &'a Heap,
    owner: OwnerToken,
    roots: Option<&'a Rc<RefCell<RootTable>>>,
}

impl<'a> ValueRef<'a> {
    pub(crate) fn new(runtime: &'a Runtime, value: crate::value::Value) -> Self {
        Self {
            value,
            heap: runtime.heap(),
            owner: runtime.owner(),
            roots: runtime.heap().api_roots.as_ref(),
        }
    }

    /// Lua's language-level type.
    pub fn lua_type(&self) -> LuaType {
        use crate::value::Value as Raw;
        match self.value {
            Raw::Nil => LuaType::Nil,
            Raw::Bool(_) => LuaType::Boolean,
            Raw::Integer(_) | Raw::Float(_) => LuaType::Number,
            Raw::String(_) => LuaType::String,
            Raw::Table(_) => LuaType::Table,
            Raw::Closure(_) | Raw::Native(_) | Raw::NativeClosure(_) => LuaType::Function,
            Raw::Thread(_) => LuaType::Thread,
            Raw::Userdata(_) | Raw::LightUserdata(..) => LuaType::Userdata,
        }
    }

    /// The logical object identity, if collectable.
    pub fn id(&self) -> Option<ObjectId> {
        self.heap.object_id_of_value(self.value)
    }

    /// Root this view. The shared root table allows rooting during a borrow;
    /// no collection can advance while the view lives.
    pub fn to_owned_value(&self) -> Result<Value> {
        Value::wrap(self.value, self.heap, self.owner, self.roots)
    }

    /// Whether this value is nil.
    pub fn is_nil(&self) -> bool {
        matches!(self.value, crate::value::Value::Nil)
    }

    /// Read a light userdata token by value.
    pub fn as_light_userdata(&self) -> Option<LightUserdata> {
        if let crate::value::Value::LightUserdata(domain, bits) = self.value {
            Some(LightUserdata { domain, bits })
        } else {
            None
        }
    }

    /// Read an integer without coercion.
    pub fn as_integer(&self) -> Option<i64> {
        if let crate::value::Value::Integer(value) = self.value {
            Some(value)
        } else {
            None
        }
    }

    /// Read a number without string coercion.
    pub fn as_number(&self) -> Option<f64> {
        match self.value {
            crate::value::Value::Integer(value) => Some(value as f64),
            crate::value::Value::Float(value) => Some(value),
            _ => None,
        }
    }

    /// Read a boolean without truthiness coercion.
    pub fn as_boolean(&self) -> Option<bool> {
        if let crate::value::Value::Bool(value) = self.value {
            Some(value)
        } else {
            None
        }
    }

    /// Borrow a string's bytes without a root or allocation.
    pub fn as_string(&self) -> Option<StrRef<'a>> {
        if let crate::value::Value::String(handle) = self.value {
            self.heap.string_bytes(handle).map(StrRef)
        } else {
            None
        }
    }
}

impl fmt::Debug for ValueRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ValueRef")
            .field("type", &self.lua_type())
            .field("id", &self.id())
            .finish()
    }
}

/// A borrowed Lua byte string.
#[derive(Clone, Copy)]
pub struct StrRef<'a>(&'a [u8]);

impl<'a> StrRef<'a> {
    /// The exact bytes, including NUL and invalid UTF-8.
    pub fn as_bytes(&self) -> &'a [u8] {
        self.0
    }

    /// A strict UTF-8 view.
    pub fn to_str(&self) -> Result<&'a str> {
        std::str::from_utf8(self.0).map_err(|_| {
            ConversionError {
                expected: "UTF-8 string",
                actual: LuaType::String,
                position: None,
            }
            .into()
        })
    }
}

impl fmt::Debug for StrRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("StrRef").field(&self.0).finish()
    }
}

impl LuaString {
    /// Borrow the exact string bytes from its runtime.
    pub fn as_bytes<'a>(&self, runtime: &'a Runtime) -> Result<&'a [u8]> {
        runtime.api_string(self)
    }

    /// Borrow strict UTF-8, refusing invalid bytes.
    pub fn to_str<'a>(&self, runtime: &'a Runtime) -> Result<&'a str> {
        StrRef(self.as_bytes(runtime)?).to_str()
    }
}

impl Table {
    /// Read through `__index` on the idle main thread, driving at most
    /// `fuel` units. On wait or fuel exhaustion, continue with `run` and
    /// `finish_call`; starting another operation while it runs is `Busy`.
    pub fn get<K: IntoLua, R: super::FromLuaMulti>(
        &self,
        runtime: &mut Runtime,
        key: K,
        journal: &mut crate::Journal,
        fuel: u64,
    ) -> Result<super::CallOutcome<R>> {
        self.0.value(runtime.owner())?;
        let key = key.into_lua(runtime)?;
        runtime.api_table_get(self, key, journal, fuel)
    }

    /// Write through `__newindex`, with the same bounded driving and
    /// completion protocol as `get`.
    pub fn set<K: IntoLua, V: IntoLua>(
        &self,
        runtime: &mut Runtime,
        key: K,
        value: V,
        journal: &mut crate::Journal,
        fuel: u64,
    ) -> Result<super::CallOutcome<()>> {
        self.0.value(runtime.owner())?;
        let key = key.into_lua(runtime)?;
        let value = value.into_lua(runtime)?;
        runtime.api_table_set(self, key, value, journal, fuel)
    }

    /// Read without metamethods. Nil and NaN keys are refused.
    pub fn raw_get<K: IntoLua, V: FromLua>(&self, runtime: &mut Runtime, key: K) -> Result<V> {
        self.0.value(runtime.owner())?;
        let key = key.into_lua(runtime)?;
        let value = runtime.api_raw_get(self, &key)?;
        V::from_lua(value, runtime)
    }

    /// Write without metamethods. Nil deletes; nil and NaN keys are refused.
    pub fn raw_set<K: IntoLua, V: IntoLua>(
        &self,
        runtime: &mut Runtime,
        key: K,
        value: V,
    ) -> Result<()> {
        self.0.value(runtime.owner())?;
        let key = key.into_lua(runtime)?;
        let value = value.into_lua(runtime)?;
        runtime.api_raw_set(self, &key, &value)
    }

    /// The raw table border, without `__len`.
    pub fn raw_len(&self, runtime: &Runtime) -> Result<i64> {
        runtime.api_raw_len(self)
    }

    /// The next pair in raw traversal order. `None` starts traversal;
    /// nil, NaN, and invalid anchors are refused as `InvalidKey`.
    pub fn next(
        &self,
        runtime: &mut Runtime,
        key: Option<&Value>,
    ) -> Result<Option<(Value, Value)>> {
        runtime.api_next(self, key)
    }

    /// The actual metatable, bypassing `__metatable` protection.
    pub fn metatable(&self, runtime: &mut Runtime) -> Result<Option<Table>> {
        runtime.api_metatable(self)
    }

    /// Set or clear the actual metatable, bypassing protection.
    pub fn set_metatable(&self, runtime: &mut Runtime, table: Option<&Table>) -> Result<()> {
        runtime.api_set_metatable(self, table)
    }
}

impl Function {
    /// The identity of a collectable closure. Bare natives and builtins
    /// have no object identity; their binding still belongs to one runtime.
    pub fn id(&self) -> Option<ObjectId> {
        self.0.id
    }

    /// Inspect the implementation kind, checking runtime ownership.
    pub fn kind(&self, runtime: &Runtime) -> Result<FunctionKind> {
        runtime.api_function_kind(self)
    }
}

impl Thread {
    /// Inspect the thread's drive state, checking runtime ownership.
    pub fn status(&self, runtime: &Runtime) -> Result<ThreadStatus> {
        runtime.api_thread_status(self)
    }
}

impl AnyUserData {
    /// Borrow a payload of exactly the registered Rust type.
    pub fn borrow<'a, T: HostUserdata>(&self, runtime: &'a Runtime) -> Result<&'a T> {
        runtime.api_userdata(self)
    }

    /// Borrow a mutable payload. Dropping the guard updates its logical
    /// charge, including growth past the quota, as the existing host API does.
    pub fn borrow_mut<'a, T: HostUserdata>(
        &self,
        runtime: &'a mut Runtime,
    ) -> Result<UserDataRefMut<'a, T>> {
        runtime.api_userdata_mut(self)
    }
}

/// A mutable userdata payload borrow with automatic charge reconciliation.
pub struct UserDataRefMut<'a, T: HostUserdata> {
    pub(crate) runtime: &'a mut Runtime,
    // Validated before construction; the exclusive runtime borrow prevents
    // collection, slot reuse, or restore for the entire guard lifetime.
    pub(crate) handle: crate::id::Handle<crate::heap::UserdataObj>,
    pub(crate) marker: std::marker::PhantomData<T>,
}

impl<T: HostUserdata> Deref for UserDataRefMut<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.runtime
            .api_userdata_at(self.handle)
            .expect("borrowed userdata type")
    }
}

impl<T: HostUserdata> DerefMut for UserDataRefMut<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.runtime
            .api_userdata_at_mut(self.handle)
            .expect("borrowed userdata type")
    }
}

impl<T: HostUserdata> Drop for UserDataRefMut<'_, T> {
    fn drop(&mut self) {
        self.runtime.api_userdata_recharge::<T>(self.handle);
    }
}
