//! Private tagged values. Handles, never Rust references.

use crate::heap::{ClosureObj, NativeClosureObj, StringObj, TableObj, ThreadObj, UserdataObj};
use crate::id::Handle;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Value {
    Nil,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(Handle<StringObj>),
    Table(Handle<TableObj>),
    Closure(Handle<ClosureObj>),
    Thread(Handle<ThreadObj>),
    /// A registered native function: an index into the runtime's interned
    /// native symbols (`Heap::natives`). Same symbol, same index. Not a
    /// collectable object.
    Native(u32),
    /// A function the VM implements that keeps values and state of its
    /// own, such as `string.gmatch`'s iterator (ADR 0035).
    NativeClosure(Handle<NativeClosureObj>),
    /// A full userdata (ADR 0042): an object with its own metatable, a
    /// fixed number of user values, and a payload Lua cannot read.
    Userdata(Handle<UserdataObj>),
    /// A light userdata (ADR 0043): an identity token, never an address
    /// and never an object. Equal when domain and bits are.
    LightUserdata(LightDomain, u64),
}

/// Who made a light userdata's token (ADR 0043). The domains keep a host
/// key from ever equalling a token the VM makes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub(crate) enum LightDomain {
    /// A host key, [`crate::HostLightKey`]: any `u64` the host chooses.
    Host = 0,
    /// `debug.upvalueid` of a Lua closure: the upvalue cell's `ObjectId`.
    Upvalue = 1,
    /// `debug.upvalueid` of a native closure: the closure's `ObjectId`
    /// shifted left 8 bits, plus the value's index.
    NativeValue = 2,
}

impl LightDomain {
    pub(crate) fn from_tag(tag: u8) -> Option<Self> {
        Some(match tag {
            0 => Self::Host,
            1 => Self::Upvalue,
            2 => Self::NativeValue,
            _ => return None,
        })
    }
}

// Two new value kinds must not make every value bigger.
const _: () = assert!(std::mem::size_of::<Value>() == 16);

impl Value {
    /// Whether Lua's type of the value is `function`.
    #[inline(always)]
    pub(crate) fn is_function(self) -> bool {
        matches!(
            self,
            Value::Closure(_) | Value::Native(_) | Value::NativeClosure(_)
        )
    }

    /// Lua truth: `nil` and `false` are false, every other value is true,
    /// `0`, `-0.0`, and `""` included. The one definition for conditions,
    /// `not`, `and`, `or`, a comparison metamethod's result, and which
    /// values `<close>` ignores.
    #[inline(always)]
    pub(crate) fn truthy(self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }
}

#[cfg(test)]
mod tests {
    use super::Value;

    #[test]
    fn only_nil_and_false_are_false() {
        assert!(!Value::Nil.truthy());
        assert!(!Value::Bool(false).truthy());
        for value in [
            Value::Bool(true),
            Value::Integer(0),
            Value::Float(0.0),
            Value::Float(-0.0),
            Value::Float(f64::NAN),
            Value::Native(0),
            // Any string, "" included, and any table or function.
            Value::String(crate::id::Handle::new(0, 0)),
            Value::Table(crate::id::Handle::new(0, 0)),
            Value::Closure(crate::id::Handle::new(0, 0)),
        ] {
            assert!(value.truthy(), "{value:?}");
        }
    }
}
