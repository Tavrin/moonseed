use std::ops::{Deref, DerefMut};

use crate::{LightUserdata, LuaType, Runtime};

use super::{
    AnyUserData, ConversionError, Error, Function, LuaString, Result, Table, Thread, Value,
};

/// Convert one Rust value into Lua, checking ownership and numeric range.
pub trait IntoLua {
    /// Convert in the target runtime.
    fn into_lua(self, runtime: &mut Runtime) -> Result<Value>;
}

/// Read one Lua value without implicit coercions.
pub trait FromLua: Sized {
    /// Convert in the value's runtime.
    fn from_lua(value: Value, runtime: &mut Runtime) -> Result<Self>;
    /// Read a callback argument without rooting primitive values. Custom
    /// implementations may use the owned-value default.
    fn from_context(context: &mut super::NativeContext<'_>, index: usize) -> Result<Self> {
        let value = context.arg(index).to_owned_value()?;
        Self::from_lua(value, context.runtime)
    }
}

/// Convert a sequence of arguments or results, preserving nil holes.
pub trait IntoLuaMulti {
    /// Convert in the target runtime.
    fn into_lua_multi(self, runtime: &mut Runtime) -> Result<MultiValue>;
    /// Write an immediate typed callback's results. Scalar and tuple
    /// implementations reuse the native result window without an owned vector.
    fn write_native(self, context: &mut super::NativeContext<'_>) -> Result<()>
    where
        Self: Sized,
    {
        let values = self.into_lua_multi(context.runtime)?;
        for value in values {
            context.push_result(value)?;
        }
        Ok(())
    }
}

/// Read arguments or results. Missing tuple entries are nil; extras are
/// ignored except by `Variadic` and `MultiValue`.
pub trait FromLuaMulti: Sized {
    /// Convert in the values' runtime, with one-based failure positions.
    fn from_lua_multi(values: MultiValue, runtime: &mut Runtime) -> Result<Self>;
    /// Read callback arguments directly. The default supports custom
    /// conversions; scalar and tuple implementations avoid a temporary vector.
    fn from_context(context: &mut super::NativeContext<'_>) -> Result<Self> {
        let values = (0..context.arg_count())
            .map(|index| context.arg(index).to_owned_value())
            .collect::<Result<Vec<_>>>()?;
        Self::from_lua_multi(MultiValue(values), context.runtime)
    }
}

/// An exact sequence. Empty, a lone nil, and nil holes remain distinct.
#[derive(Clone, Debug, Default)]
pub struct MultiValue(pub Vec<Value>);

impl MultiValue {
    /// An empty sequence of values.
    pub fn new() -> Self {
        Self::default()
    }
    /// Take the exact underlying sequence.
    pub fn into_vec(self) -> Vec<Value> {
        self.0
    }
}

impl Deref for MultiValue {
    type Target = Vec<Value>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl DerefMut for MultiValue {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl From<Vec<Value>> for MultiValue {
    fn from(values: Vec<Value>) -> Self {
        Self(values)
    }
}
impl IntoIterator for MultiValue {
    type Item = Value;
    type IntoIter = std::vec::IntoIter<Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// An open number of values of one convertible type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Variadic<T>(pub Vec<T>);

impl<T> Deref for Variadic<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl<T> DerefMut for Variadic<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl<T> From<Vec<T>> for Variadic<T> {
    fn from(values: Vec<T>) -> Self {
        Self(values)
    }
}

/// Opt into Lua's numeric-string, integral-float, number-string, or
/// truthiness conversions when reading. UTF-8 and integer ranges stay strict.
/// Plain `f64` accepts Lua integers/numbers and rejects strings. The same rule
/// applies to [`crate::HostRegistry::typed`] arguments and [`Table::raw_get`].
/// This wrapper changes conversion only; it does not change stored Lua values
/// or snapshot contents.
///
/// ```
/// use moonseed::{ApiError, Coerce, Error, Runtime};
/// let mut rt = Runtime::builder().build()?;
/// let table = rt.create_table()?;
/// table.raw_set(&mut rt, "amount", "42.5")?;
/// assert!(matches!(table.raw_get::<_, f64>(&mut rt, "amount"),
///     Err(Error::Api(ApiError::Conversion(_)))));
/// let Coerce(amount): Coerce<f64> = table.raw_get(&mut rt, "amount")?;
/// assert_eq!(amount, 42.5);
/// # Ok::<(), moonseed::Error>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coerce<T>(pub T);

fn conversion(expected: &'static str, value: &Value) -> Error {
    ConversionError {
        expected,
        actual: value.lua_type(),
        position: None,
    }
    .into()
}

fn at(error: Error, position: usize) -> Error {
    match error {
        Error::Api(super::ApiError::Conversion(mut error)) => {
            error.position = Some(position);
            error.into()
        }
        other => other,
    }
}

impl IntoLua for Value {
    fn into_lua(self, runtime: &mut Runtime) -> Result<Value> {
        self.check(runtime)?;
        Ok(self)
    }
}
impl IntoLua for &Value {
    fn into_lua(self, runtime: &mut Runtime) -> Result<Value> {
        self.check(runtime)?;
        Ok(self.clone())
    }
}
impl FromLua for Value {
    fn from_lua(value: Value, runtime: &mut Runtime) -> Result<Self> {
        value.check(runtime)?;
        Ok(value)
    }
}

impl IntoLua for () {
    fn into_lua(self, _: &mut Runtime) -> Result<Value> {
        Ok(Value::Nil)
    }
}
impl FromLua for () {
    fn from_lua(value: Value, runtime: &mut Runtime) -> Result<Self> {
        value.check(runtime)?;
        if matches!(value, Value::Nil) {
            Ok(())
        } else {
            Err(conversion("nil", &value))
        }
    }
}
impl IntoLuaMulti for () {
    fn into_lua_multi(self, _: &mut Runtime) -> Result<MultiValue> {
        Ok(MultiValue::new())
    }
}
impl FromLuaMulti for () {
    fn from_lua_multi(values: MultiValue, runtime: &mut Runtime) -> Result<Self> {
        for value in &values.0 {
            value.check(runtime)?;
        }
        runtime.recycle_owned_buffer(values.into_vec());
        Ok(())
    }
}

impl IntoLua for bool {
    fn into_lua(self, _: &mut Runtime) -> Result<Value> {
        Ok(Value::Boolean(self))
    }
}
impl FromLua for bool {
    fn from_context(context: &mut super::NativeContext<'_>, index: usize) -> Result<Self> {
        let value = context.arg(index);
        value.as_boolean().ok_or_else(|| {
            ConversionError {
                expected: "boolean",
                actual: value.lua_type(),
                position: None,
            }
            .into()
        })
    }
    fn from_lua(value: Value, runtime: &mut Runtime) -> Result<Self> {
        value.check(runtime)?;
        if let Value::Boolean(value) = value {
            Ok(value)
        } else {
            Err(conversion("boolean", &value))
        }
    }
}

macro_rules! integers {
    ($($ty:ty),+) => { $(
        impl IntoLua for $ty {
            fn into_lua(self, _: &mut Runtime) -> Result<Value> {
                i64::try_from(self).map(Value::Integer).map_err(|_| ConversionError {
                    expected: "integer in Lua range", actual: LuaType::Number, position: None,
                }.into())
            }
        }
        impl FromLua for $ty {
            #[inline]
            fn from_context(context: &mut super::NativeContext<'_>, index: usize) -> Result<Self> {
                let value = context.arg(index);
                value.as_integer().and_then(|integer| <$ty>::try_from(integer).ok())
                    .ok_or_else(|| ConversionError { expected: stringify!($ty), actual: value.lua_type(), position: None }.into())
            }
            fn from_lua(value: Value, runtime: &mut Runtime) -> Result<Self> {
                value.check(runtime)?;
                let Value::Integer(integer) = value else { return Err(conversion(stringify!($ty), &value)); };
                <$ty>::try_from(integer).map_err(|_| conversion(stringify!($ty), &value))
            }
        }
        impl FromLua for Coerce<$ty> {
            fn from_lua(value: Value, runtime: &mut Runtime) -> Result<Self> {
                value.check(runtime)?;
                let integer = match numeric(&value, runtime)? {
                    Value::Integer(integer) => Some(integer),
                    Value::Number(number) => crate::compare::float_to_int(number),
                    _ => None,
                }.ok_or_else(|| conversion(stringify!($ty), &value))?;
                <$ty>::try_from(integer).map(Coerce).map_err(|_| conversion(stringify!($ty), &value))
            }
        }
    )+ };
}
integers!(i8, i16, i32, i64, u8, u16, u32, u64, isize, usize);

macro_rules! floats {
    ($($ty:ty),+) => { $(
        impl IntoLua for $ty {
            fn into_lua(self, _: &mut Runtime) -> Result<Value> { Ok(Value::Number(f64::from(self))) }
        }
        impl FromLua for $ty {
            #[inline]
            fn from_context(context: &mut super::NativeContext<'_>, index: usize) -> Result<Self> {
                let value = context.arg(index);
                let number = value.as_number().ok_or_else(|| Error::from(ConversionError { expected: "number", actual: value.lua_type(), position: None }))?;
                let result = number as $ty;
                if number.is_finite() && !result.is_finite() {
                    return Err(ConversionError { expected: stringify!($ty), actual: value.lua_type(), position: None }.into());
                }
                Ok(result)
            }
            fn from_lua(value: Value, runtime: &mut Runtime) -> Result<Self> {
                value.check(runtime)?;
                let number = match value {
                    Value::Integer(integer) => integer as f64,
                    Value::Number(number) => number,
                    _ => return Err(conversion("number", &value)),
                };
                let result = number as $ty;
                if number.is_finite() && !result.is_finite() { return Err(conversion(stringify!($ty), &value)); }
                Ok(result)
            }
        }
        impl FromLua for Coerce<$ty> {
            fn from_lua(value: Value, runtime: &mut Runtime) -> Result<Self> {
                value.check(runtime)?;
                let number = numeric(&value, runtime)?;
                <$ty>::from_lua(number, runtime).map(Coerce).map_err(|_| conversion("number", &value))
            }
        }
    )+ };
}
floats!(f32, f64);

macro_rules! objects {
    ($($ty:ident => $variant:ident),+) => { $(
        impl IntoLua for $ty {
            fn into_lua(self, runtime: &mut Runtime) -> Result<Value> {
                Value::$variant(self).into_lua(runtime)
            }
        }
        impl IntoLua for &$ty {
            fn into_lua(self, runtime: &mut Runtime) -> Result<Value> { self.clone().into_lua(runtime) }
        }
        impl FromLua for $ty {
            fn from_lua(value: Value, runtime: &mut Runtime) -> Result<Self> {
                value.check(runtime)?;
                if let Value::$variant(value) = value { Ok(value) } else { Err(conversion(stringify!($ty), &value)) }
            }
        }
    )+ };
}
objects!(LuaString => String, Table => Table, Function => Function, Thread => Thread,
    AnyUserData => UserData, LightUserdata => LightUserdata);

impl IntoLua for Vec<u8> {
    fn into_lua(self, runtime: &mut Runtime) -> Result<Value> {
        runtime.create_string(self).map(Value::String)
    }
}
impl IntoLua for &[u8] {
    fn into_lua(self, runtime: &mut Runtime) -> Result<Value> {
        runtime.create_string(self).map(Value::String)
    }
}
impl<const N: usize> IntoLua for &[u8; N] {
    fn into_lua(self, runtime: &mut Runtime) -> Result<Value> {
        self.as_slice().into_lua(runtime)
    }
}
impl IntoLua for String {
    fn into_lua(self, runtime: &mut Runtime) -> Result<Value> {
        self.into_bytes().into_lua(runtime)
    }
}
impl IntoLua for &str {
    fn into_lua(self, runtime: &mut Runtime) -> Result<Value> {
        self.as_bytes().into_lua(runtime)
    }
}
impl FromLua for Vec<u8> {
    fn from_lua(value: Value, runtime: &mut Runtime) -> Result<Self> {
        value.check(runtime)?;
        if let Value::String(string) = value {
            Ok(string.as_bytes(runtime)?.to_vec())
        } else {
            Err(conversion("string", &value))
        }
    }
}
impl FromLua for String {
    fn from_lua(value: Value, runtime: &mut Runtime) -> Result<Self> {
        value.check(runtime)?;
        if let Value::String(string) = value {
            Ok(string.to_str(runtime)?.to_owned())
        } else {
            Err(conversion("UTF-8 string", &value))
        }
    }
}

impl<T: IntoLua> IntoLua for Option<T> {
    fn into_lua(self, runtime: &mut Runtime) -> Result<Value> {
        match self {
            Some(value) => value.into_lua(runtime),
            None => Ok(Value::Nil),
        }
    }
}
impl<T: FromLua> FromLua for Option<T> {
    fn from_lua(value: Value, runtime: &mut Runtime) -> Result<Self> {
        value.check(runtime)?;
        if matches!(value, Value::Nil) {
            Ok(None)
        } else {
            T::from_lua(value, runtime).map(Some)
        }
    }
}
impl<T: IntoLua> IntoLua for Coerce<T> {
    fn into_lua(self, runtime: &mut Runtime) -> Result<Value> {
        self.0.into_lua(runtime)
    }
}

fn numeric(value: &Value, runtime: &Runtime) -> Result<Value> {
    if let Value::String(string) = value {
        match crate::lex::string_to_number(string.as_bytes(runtime)?) {
            Some(crate::value::Value::Integer(value)) => Ok(Value::Integer(value)),
            Some(crate::value::Value::Float(value)) => Ok(Value::Number(value)),
            _ => Err(conversion("number", value)),
        }
    } else {
        Ok(value.clone())
    }
}

fn string_coercion(value: Value, runtime: &mut Runtime) -> Result<Value> {
    value.check(runtime)?;
    match &value {
        Value::Integer(_) | Value::Number(_) => {
            let text = crate::concat::number_text(value.raw(runtime)?)
                .ok_or_else(|| conversion("string", &value))?;
            text.into_lua(runtime)
        }
        _ => Ok(value),
    }
}
macro_rules! coerced_strings {
    ($($ty:ty),+) => { $(
        impl FromLua for Coerce<$ty> {
            fn from_lua(value: Value, runtime: &mut Runtime) -> Result<Self> {
                let value = string_coercion(value, runtime)?;
                <$ty>::from_lua(value, runtime).map(Coerce)
            }
        }
    )+ };
}
coerced_strings!(String, Vec<u8>, LuaString);
impl FromLua for Coerce<bool> {
    fn from_lua(value: Value, runtime: &mut Runtime) -> Result<Self> {
        value.check(runtime)?;
        Ok(Coerce(!matches!(value, Value::Nil | Value::Boolean(false))))
    }
}

trait SingleInto: IntoLua {}
trait SingleFrom: FromLua {}
macro_rules! singles {
    ($($ty:ty),+) => { $(impl SingleInto for $ty {} impl SingleFrom for $ty {})+ };
}
singles!(
    bool,
    i8,
    i16,
    i32,
    i64,
    u8,
    u16,
    u32,
    u64,
    isize,
    usize,
    f32,
    f64,
    Vec<u8>,
    String,
    Value,
    LuaString,
    Table,
    Function,
    Thread,
    AnyUserData,
    LightUserdata
);
impl SingleInto for &str {}
impl SingleInto for &[u8] {}
impl<const N: usize> SingleInto for &[u8; N] {}
impl<T: IntoLua> SingleInto for Option<T> {}
impl<T: FromLua> SingleFrom for Option<T> {}
impl<T: IntoLua> SingleInto for Coerce<T> {}
impl<T> SingleFrom for Coerce<T> where Coerce<T>: FromLua {}
macro_rules! borrowed_singles {
    ($($ty:ty),+) => { $(impl SingleInto for &$ty {})+ };
}
borrowed_singles!(
    Value,
    LuaString,
    Table,
    Function,
    Thread,
    AnyUserData,
    LightUserdata
);

impl<T: SingleInto> IntoLuaMulti for T {
    #[inline]
    fn write_native(self, context: &mut super::NativeContext<'_>) -> Result<()> {
        let value = self
            .into_lua(context.runtime)
            .map_err(|error| at(error, 1))?;
        context.push_result(value)
    }
    #[inline]
    fn into_lua_multi(self, runtime: &mut Runtime) -> Result<MultiValue> {
        let value = self.into_lua(runtime).map_err(|error| at(error, 1))?;
        let mut values = runtime.take_owned_buffer();
        values.clear();
        values.push(value);
        Ok(MultiValue(values))
    }
}
impl<T: SingleFrom> FromLuaMulti for T {
    fn from_context(context: &mut super::NativeContext<'_>) -> Result<Self> {
        context.argument(0)
    }
    fn from_lua_multi(values: MultiValue, runtime: &mut Runtime) -> Result<Self> {
        for value in &values.0 {
            value.check(runtime)?;
        }
        let mut values = values.into_vec();
        let value = values.drain(..).next().unwrap_or(Value::Nil);
        runtime.recycle_owned_buffer(values);
        T::from_lua(value, runtime).map_err(|error| at(error, 1))
    }
}
impl IntoLuaMulti for MultiValue {
    fn into_lua_multi(self, runtime: &mut Runtime) -> Result<MultiValue> {
        for value in &self.0 {
            value.check(runtime)?;
        }
        Ok(self)
    }
}
impl FromLuaMulti for MultiValue {
    fn from_lua_multi(values: MultiValue, runtime: &mut Runtime) -> Result<Self> {
        values.into_lua_multi(runtime)
    }
}
impl<T: IntoLua> IntoLuaMulti for Variadic<T> {
    fn into_lua_multi(self, runtime: &mut Runtime) -> Result<MultiValue> {
        self.0
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                value
                    .into_lua(runtime)
                    .map_err(|error| at(error, index + 1))
            })
            .collect::<Result<Vec<_>>>()
            .map(MultiValue)
    }
}
impl<T: FromLua> FromLuaMulti for Variadic<T> {
    fn from_lua_multi(values: MultiValue, runtime: &mut Runtime) -> Result<Self> {
        values
            .into_iter()
            .enumerate()
            .map(|(index, value)| T::from_lua(value, runtime).map_err(|error| at(error, index + 1)))
            .collect::<Result<Vec<_>>>()
            .map(Variadic)
    }
}

macro_rules! tuples {
    ($($ty:ident:$index:tt),+) => {
        impl<$($ty: IntoLua),+> IntoLuaMulti for ($($ty,)+) {
            #[inline]
            fn write_native(self, context: &mut super::NativeContext<'_>) -> Result<()> {
                $(let value = self.$index.into_lua(context.runtime).map_err(|error| at(error, $index + 1))?;
                  context.push_result(value)?;)+
                Ok(())
            }
            fn into_lua_multi(self, runtime: &mut Runtime) -> Result<MultiValue> {
                let converted = [$(self.$index.into_lua(runtime).map_err(|error| at(error, $index + 1))?,)+];
                let mut values = runtime.take_owned_buffer();
                values.clear(); values.extend(converted);
                Ok(MultiValue(values))
            }
        }
        impl<$($ty: FromLua),+> FromLuaMulti for ($($ty,)+) {
            #[inline]
            fn from_context(context: &mut super::NativeContext<'_>) -> Result<Self> {
                Ok(($(context.argument::<$ty>($index)?,)+))
            }
            fn from_lua_multi(values: MultiValue, runtime: &mut Runtime) -> Result<Self> {
                for value in &values.0 { value.check(runtime)?; }
                let mut owned = values.into_vec();
                let result = {
                    let mut values = owned.drain(..);
                    (|| Ok(($($ty::from_lua(values.next().unwrap_or(Value::Nil), runtime).map_err(|error| at(error, $index + 1))?,)+)))()
                };
                runtime.recycle_owned_buffer(owned);
                result
            }
        }
    };
}
tuples!(A:0);
tuples!(A:0, B:1);
tuples!(A:0, B:1, C:2);
tuples!(A:0, B:1, C:2, D:3);
tuples!(A:0, B:1, C:2, D:3, E:4);
tuples!(A:0, B:1, C:2, D:3, E:4, F:5);
tuples!(A:0, B:1, C:2, D:3, E:4, F:5, G:6);
tuples!(A:0, B:1, C:2, D:3, E:4, F:5, G:6, H:7);
tuples!(A:0, B:1, C:2, D:3, E:4, F:5, G:6, H:7, I:8);
tuples!(A:0, B:1, C:2, D:3, E:4, F:5, G:6, H:7, I:8, J:9);
tuples!(A:0, B:1, C:2, D:3, E:4, F:5, G:6, H:7, I:8, J:9, K:10);
tuples!(A:0, B:1, C:2, D:3, E:4, F:5, G:6, H:7, I:8, J:9, K:10, L:11);
