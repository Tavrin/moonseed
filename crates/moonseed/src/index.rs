//! Language-level indexing and length: what source `obj[key]`,
//! `obj[key] = value`, and `#obj` mean. `GetTable` / `SetTable` stay raw.
//!
//! A table has its own metatable; a value of another type has its type's,
//! if any (ADR 0034), so a string indexes through the string metatable's
//! `__index`. Metamethod lookup is always raw: the
//! event name is read from the metatable with a raw lookup, never through
//! `__index`. A non-function `__index` / `__newindex` value is indexed or
//! assigned again, up to [`MAX_META_CHAIN`] steps; a function is returned to
//! the caller as [`Resolved::Call`], which the VM runs as an ordinary call
//! and then finishes the instruction with its first result.

use crate::heap::Heap;
use crate::id::LuaFault;
use crate::table::KeyView;
use crate::value::Value;

/// Lua 5.4's bound on `__index` / `__newindex` chains (`MAXTAGLOOP`).
pub(crate) const MAX_META_CHAIN: usize = 2000;

/// What a language-level operation resolved to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Resolved {
    /// The result. A completed store resolves to nil.
    Done(Value),
    /// Call `function` with `target` and the operation's other operands;
    /// the call's first result is the operation's result.
    Call { function: Value, target: Value },
}

/// A value's metamethod for `event`, read raw from the metatable
/// [`Heap::metatable_of`] finds. `None` without a metatable or the field.
#[inline(always)]
pub(crate) fn metamethod(heap: &Heap, value: Value, event: &[u8]) -> Option<Value> {
    let metatable = heap.metatable_of(value)?;
    // A small direct-mapped set suffices: even a collision cannot return a
    // wrong event because Table validates the key bytes on every hit.
    let last = event.last().copied().unwrap_or(0) as usize;
    let hint = &heap.event_hints[(event.len() + last) & 31];
    heap.tables
        .get(metatable)?
        .table
        .get_event_hint(event, hint)
}

/// A refused insert as a Lua fault: the quota is a memory error.
fn insert_fault(error: crate::heap::InsertError) -> LuaFault {
    match error {
        crate::heap::InsertError::Memory => LuaFault::Memory,
        crate::heap::InsertError::NoTable => LuaFault::Index,
    }
}

fn is_function(value: Value) -> bool {
    value.is_function()
}

/// Raw read of `key`; a nil or NaN key reads nil.
fn raw_get(heap: &Heap, table: Value, key: Value) -> Result<Value, LuaFault> {
    let Value::Table(handle) = table else {
        return Err(LuaFault::Index);
    };
    match heap.key_view(key) {
        Ok(key) => heap.table_get_view(handle, key).ok_or(LuaFault::Index),
        Err(LuaFault::NilKey | LuaFault::NanKey) => Ok(Value::Nil),
        Err(fault) => Err(fault),
    }
}

/// `obj[key]`.
pub(crate) fn get(heap: &Heap, obj: Value, key: Value) -> Result<Resolved, LuaFault> {
    index_chain(heap, obj, |heap, table| raw_get(heap, table, key))
}

/// `obj.name`, a constant string key, looked up without allocating.
/// Reuse the instruction's disposable hint on each raw table probe in the
/// existing chain. __index is still resolved on every miss, so changes to a
/// metatable, target table or userdata methods cannot leave a cached result.
pub(crate) fn get_name(
    heap: &Heap,
    obj: Value,
    name: KeyView<'_>,
    hint: Option<&crate::heap::FieldHints>,
) -> Result<Resolved, LuaFault> {
    index_chain_with_hint(heap, obj, hint.map(|hint| &hint.index), |heap, table| {
        let Value::Table(handle) = table else {
            return Err(LuaFault::Index);
        };
        let table = &heap.tables.get(handle).ok_or(LuaFault::Index)?.table;
        let value = match (name, hint) {
            (KeyView::String(bytes, hash), Some(hint)) => {
                table.get_name_hint(bytes, hash, &hint.name)
            }
            _ => table.get_view(name),
        };
        Ok(value.unwrap_or(Value::Nil))
    })
}

fn index_chain(
    heap: &Heap,
    obj: Value,
    raw: impl Fn(&Heap, Value) -> Result<Value, LuaFault>,
) -> Result<Resolved, LuaFault> {
    index_chain_with_hint(heap, obj, None, raw)
}

fn index_chain_with_hint(
    heap: &Heap,
    obj: Value,
    hint: Option<&std::cell::Cell<u32>>,
    raw: impl Fn(&Heap, Value) -> Result<Value, LuaFault>,
) -> Result<Resolved, LuaFault> {
    let mut current = obj;
    for _ in 0..MAX_META_CHAIN {
        let table = matches!(current, Value::Table(_));
        if table {
            let value = raw(heap, current)?;
            if !matches!(value, Value::Nil) {
                return Ok(Resolved::Done(value));
            }
        }
        count!("table_metamethod_fallbacks");
        let method = if let Some(hint) = hint {
            // A constant-field instruction already owns a dedicated hint and
            // the constant event hash. Keep its established one-probe path.
            heap.metatable_of(current).and_then(|metatable| {
                heap.tables.get(metatable)?.table.get_name_hint(
                    b"__index",
                    crate::hashutil::string_hash(b"__index"),
                    hint,
                )
            })
        } else {
            metamethod(heap, current, b"__index")
        };
        match method {
            // A table without `__index` reads nil; any other value
            // cannot be indexed.
            None if table => return Ok(Resolved::Done(Value::Nil)),
            None => return Err(LuaFault::Index),
            Some(function) if is_function(function) => {
                return Ok(Resolved::Call {
                    function,
                    target: current,
                });
            }
            Some(next) => current = next,
        }
    }
    Err(LuaFault::MetaChain)
}

/// `obj[key] = value`. A live key is updated in place without consulting
/// `__newindex`. Otherwise `__newindex` decides; with none, the key is
/// inserted (nil deletes nothing, a nil or NaN key faults). When a
/// metamethod is called the raw store does not happen.
pub(crate) fn set(
    heap: &mut Heap,
    obj: Value,
    key: Value,
    value: Value,
) -> Result<Resolved, LuaFault> {
    let mut current = obj;
    for _ in 0..MAX_META_CHAIN {
        let Value::Table(handle) = current else {
            // Another type's value only has its type's `__newindex`.
            count!("table_metamethod_fallbacks");
            match metamethod(heap, current, b"__newindex") {
                None => return Err(LuaFault::Index),
                Some(function) if is_function(function) => {
                    return Ok(Resolved::Call {
                        function,
                        target: current,
                    });
                }
                Some(next) => {
                    current = next;
                    continue;
                }
            }
        };
        let normalized = heap.key_view(key);
        if let Ok(normalized) = &normalized {
            let live = !matches!(
                heap.table_get_view(handle, *normalized)
                    .ok_or(LuaFault::Index)?,
                Value::Nil
            );
            if live {
                if !heap.update_string_key(handle, key, value) {
                    let normalized = heap.normalize_value(key)?;
                    heap.table_insert(handle, normalized, key, value)
                        .map_err(insert_fault)?;
                }
                return Ok(Resolved::Done(Value::Nil));
            }
        }
        count!("table_metamethod_fallbacks");
        match metamethod(heap, current, b"__newindex") {
            None => {
                normalized?;
                if !heap.update_string_key(handle, key, value) {
                    let normalized = heap.normalize_value(key)?;
                    heap.table_insert(handle, normalized, key, value)
                        .map_err(insert_fault)?;
                }
                return Ok(Resolved::Done(Value::Nil));
            }
            Some(function) if is_function(function) => {
                return Ok(Resolved::Call {
                    function,
                    target: current,
                });
            }
            Some(next) => current = next,
        }
    }
    Err(LuaFault::MetaChain)
}

/// `#value`: a string's byte length, never a metamethod; a table's
/// `__len`, else its raw border; another value's type's `__len`. Any `__len` value is called: a table with `__call` works, and
/// anything else faults as a bad call.
pub(crate) fn len(heap: &Heap, value: Value) -> Result<Resolved, LuaFault> {
    match value {
        Value::String(handle) => {
            let bytes = heap.string_bytes(handle).ok_or(LuaFault::Length)?;
            let length = i64::try_from(bytes.len()).map_err(|_| LuaFault::Length)?;
            Ok(Resolved::Done(Value::Integer(length)))
        }
        Value::Table(handle) => match metamethod(heap, value, b"__len") {
            Some(function) => Ok(Resolved::Call {
                function,
                target: value,
            }),
            None => Ok(Resolved::Done(Value::Integer(
                heap.tables
                    .get(handle)
                    .ok_or(LuaFault::Length)?
                    .table
                    .raw_border(),
            ))),
        },
        // Another type's `__len`, from its type's metatable.
        _ => match metamethod(heap, value, b"__len") {
            Some(function) => Ok(Resolved::Call {
                function,
                target: value,
            }),
            None => Err(LuaFault::Length),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::TableKey;

    fn table(heap: &mut Heap) -> Value {
        Value::Table(heap.alloc_table().unwrap())
    }

    fn put(heap: &mut Heap, table: Value, name: &[u8], value: Value) {
        let Value::Table(handle) = table else {
            panic!("not a table");
        };
        let key = Value::String(heap.alloc_string(name.to_vec()).unwrap());
        heap.table_insert(handle, TableKey::string(name.to_vec()), key, value)
            .unwrap();
    }

    fn set_meta(heap: &mut Heap, table: Value, metatable: Value) {
        let (Value::Table(table), Value::Table(metatable)) = (table, metatable) else {
            panic!("not tables");
        };
        heap.tables.get_mut(table).unwrap().metatable = Some(metatable);
    }

    #[test]
    fn metamethod_lookup_is_raw() {
        let mut heap = Heap::new();
        let t = table(&mut heap);
        let mt = table(&mut heap);
        let mt_mt = table(&mut heap);
        let fallback = table(&mut heap);
        put(&mut heap, fallback, b"__index", Value::Integer(1));
        put(&mut heap, mt_mt, b"__index", fallback);
        set_meta(&mut heap, t, mt);
        set_meta(&mut heap, mt, mt_mt);
        // `mt` has no raw `__index`; its own metatable must not supply one.
        assert_eq!(metamethod(&heap, t, b"__index"), None);
        assert_eq!(
            get_name(&heap, t, KeyView::string(b"x"), None),
            Ok(Resolved::Done(Value::Nil))
        );
    }

    #[test]
    fn event_hints_follow_metatable_edits_and_slot_reuse() {
        let mut heap = Heap::new();
        let t = table(&mut heap);
        let mt = table(&mut heap);
        let other = table(&mut heap);
        set_meta(&mut heap, t, mt);
        put(&mut heap, mt, b"__add", Value::Integer(1));
        assert_eq!(metamethod(&heap, t, b"__add"), Some(Value::Integer(1)));
        put(&mut heap, mt, b"__add", Value::Integer(2));
        assert_eq!(metamethod(&heap, t, b"__add"), Some(Value::Integer(2)));
        put(&mut heap, mt, b"__add", Value::Nil);
        put(&mut heap, mt, b"__sub", Value::Integer(3));
        assert_eq!(metamethod(&heap, t, b"__add"), None);
        assert_eq!(metamethod(&heap, t, b"__sub"), Some(Value::Integer(3)));
        put(&mut heap, other, b"__add", Value::Integer(4));
        set_meta(&mut heap, t, other);
        assert_eq!(metamethod(&heap, t, b"__add"), Some(Value::Integer(4)));
        assert_eq!(metamethod(&heap, t, b"__sub"), None);
    }

    #[test]
    fn chains_stop_at_the_bound() {
        let mut heap = Heap::new();
        let t = table(&mut heap);
        let mt = table(&mut heap);
        put(&mut heap, mt, b"__index", t);
        put(&mut heap, mt, b"__newindex", t);
        set_meta(&mut heap, t, mt);
        assert_eq!(
            get_name(&heap, t, KeyView::string(b"x"), None),
            Err(LuaFault::MetaChain)
        );
        let key = Value::String(heap.alloc_string(b"x".to_vec()).unwrap());
        assert_eq!(
            set(&mut heap, t, key, Value::Integer(1)),
            Err(LuaFault::MetaChain)
        );
        // A live key is updated without consulting `__newindex`.
        put(&mut heap, t, b"y", Value::Integer(1));
        let y = Value::String(heap.alloc_string(b"y".to_vec()).unwrap());
        assert_eq!(
            set(&mut heap, t, y, Value::Integer(2)),
            Ok(Resolved::Done(Value::Nil))
        );
        assert_eq!(
            get_name(&heap, t, KeyView::string(b"y"), None),
            Ok(Resolved::Done(Value::Integer(2)))
        );
    }

    #[test]
    fn length_rules() {
        let mut heap = Heap::new();
        let text = Value::String(heap.alloc_string(b"a\0c".to_vec()).unwrap());
        assert_eq!(len(&heap, text), Ok(Resolved::Done(Value::Integer(3))));
        assert_eq!(len(&heap, Value::Integer(1)), Err(LuaFault::Length));
        let t = table(&mut heap);
        assert_eq!(len(&heap, t), Ok(Resolved::Done(Value::Integer(0))));
    }
}
