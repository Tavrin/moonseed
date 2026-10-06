use std::fmt;

use crate::{LuaFault, LuaType, Runtime, VmError};

use super::Value;

/// An embedding operation's result (ADR 0053).
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A strict conversion failure. Positions are one-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConversionError {
    /// The Rust value or Lua type required.
    pub expected: &'static str,
    /// The Lua type supplied.
    pub actual: LuaType,
    /// Argument or result index, when converting several values.
    pub position: Option<usize>,
}

impl ConversionError {
    /// Describe a failed conversion outside a positional argument/result list.
    /// Set `position` to a one-based index when a host conversion knows it.
    pub const fn new(expected: &'static str, actual: LuaType) -> Self {
        Self {
            expected,
            actual,
            position: None,
        }
    }
}

/// Host misuse, separate from catchable Lua failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ApiError {
    /// A reference belongs to another runtime instance.
    WrongRuntime,
    /// The main thread is already executing.
    Busy,
    /// No wait matches the request.
    NotWaiting,
    /// A wait has already completed.
    AlreadyCompleted,
    /// The operation is not valid in the current call state.
    InvalidCallState,
    /// A strict conversion failed.
    Conversion(ConversionError),
    /// A root slot has been released.
    Released,
    /// No native symbol matches the request.
    UnknownSymbol,
    /// A host type has not been registered.
    Unregistered,
    /// A raw key is nil or NaN, or a traversal anchor is invalid.
    InvalidKey,
    /// A full userdata does not hold the requested Rust type.
    WrongType,
}

/// A catchable Lua failure with its unchanged error object.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct LuaError {
    /// The error object, rooted when it is collectable.
    pub value: Value,
    /// Lua's failure class.
    pub class: LuaFault,
    /// Traceback bytes, when one was made.
    pub traceback: Option<Vec<u8>>,
    message: Vec<u8>,
}

impl LuaError {
    /// Capture the display text without changing the error object.
    pub fn new(value: Value, class: LuaFault, runtime: &Runtime) -> Result<Self> {
        value.check(runtime)?;
        let message = match &value {
            Value::String(string) => string.as_bytes(runtime)?.to_vec(),
            Value::Integer(integer) => integer.to_string().into_bytes(),
            Value::Number(number) => {
                crate::concat::number_text(crate::value::Value::Float(*number))
                    .unwrap_or_default()
                    .into_bytes()
            }
            other => {
                format!("error object is a {} value", type_name(other.lua_type())).into_bytes()
            }
        };
        Ok(Self {
            value,
            class,
            traceback: None,
            message,
        })
    }

    pub(crate) fn construction(class: LuaFault) -> Self {
        // A failed construction has no runtime in which to root a message.
        Self {
            value: Value::Nil,
            class,
            traceback: None,
            message: class.text().as_bytes().to_vec(),
        }
    }
}

/// Lua failure, VM failure, or host misuse (ADR 0053).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Error {
    /// A catchable Lua failure.
    Lua(LuaError),
    /// An execution or VM integrity failure.
    Vm(VmError),
    /// Host misuse.
    Api(ApiError),
}

pub(crate) fn type_name(ty: LuaType) -> &'static str {
    match ty {
        LuaType::Nil => "nil",
        LuaType::Boolean => "boolean",
        LuaType::Number => "number",
        LuaType::String => "string",
        LuaType::Table => "table",
        LuaType::Function => "function",
        LuaType::Thread => "thread",
        LuaType::Userdata => "userdata",
    }
}

impl fmt::Display for ConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(position) = self.position {
            write!(f, "value #{position}: ")?;
        }
        write!(
            f,
            "{} expected, got {}",
            self.expected,
            type_name(self.actual)
        )
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conversion(error) => error.fmt(f),
            other => write!(f, "{other:?}"),
        }
    }
}

impl fmt::Display for LuaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match std::str::from_utf8(&self.message) {
            Ok(text) => f.write_str(text),
            Err(_) => {
                for &byte in &self.message {
                    if byte.is_ascii_graphic() || byte == b' ' {
                        write!(f, "{}", char::from(byte))?;
                    } else {
                        write!(f, "\\x{byte:02x}")?;
                    }
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lua(error) => error.fmt(f),
            Self::Vm(error) => write!(f, "VM error: {error:?}"),
            Self::Api(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ConversionError {}
impl std::error::Error for ApiError {}
impl std::error::Error for LuaError {}
impl std::error::Error for Error {}

impl From<ApiError> for Error {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}
impl From<ConversionError> for Error {
    fn from(error: ConversionError) -> Self {
        Self::Api(ApiError::Conversion(error))
    }
}
impl From<LuaError> for Error {
    fn from(error: LuaError) -> Self {
        Self::Lua(error)
    }
}
impl From<VmError> for Error {
    fn from(error: VmError) -> Self {
        match error {
            VmError::Api(error) => Self::Api(error),
            other => Self::Vm(other),
        }
    }
}
