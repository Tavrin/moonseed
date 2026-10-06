//! Tables: insertion-ordered slots plus a lookup index.
//!
//! Live entries and dead traversal anchors share one vector. The index finds
//! either. Enumeration order is that vector, not hash-bucket order. String
//! keys compare by bytes. Table, closure, and thread keys compare by
//! [`ObjectId`].
//!
//! A dead anchor keeps a deleted key's position so `next` can continue. It is
//! not a value and it does not own a collectable object. Structural insertion
//! appends the new key; once the dead anchors outnumber half the live
//! entries, it first drops every anchor and keeps the remaining live order.
//! A key may then have dead anchors before its live slot; the index names
//! its last slot.
//! Up to five slots use a reverse scan; the sixth builds the derived indexes.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hash, Hasher};

use crate::hashutil::{StableBuildHasher, TableBuildHasher, string_hash};
use crate::id::{LuaFault, ObjectId};
use crate::value::{LightDomain, Value};

/// The cached hash follows the key bytes in one shared allocation. Its
/// fat pointer replaces the old thin owner plus hash without widening keys.
/// Key bytes remain independent of the collectable string, including anchors.
#[derive(Clone, Debug)]
pub(crate) struct KeyString(std::rc::Rc<[u8]>);

impl KeyString {
    fn copied(bytes: &[u8], hash: u64) -> Self {
        if bytes.len() <= 31 {
            let mut stored = [0; 39];
            stored[..bytes.len()].copy_from_slice(bytes);
            stored[bytes.len()..8 + bytes.len()].copy_from_slice(&hash.to_le_bytes());
            Self(std::rc::Rc::from(&stored[..8 + bytes.len()]))
        } else {
            let mut stored = Vec::with_capacity(8 + bytes.len());
            stored.extend_from_slice(bytes);
            stored.extend_from_slice(&hash.to_le_bytes());
            Self(std::rc::Rc::from(stored))
        }
    }

    #[inline(always)]
    fn parts(&self) -> (&[u8], u64) {
        let len = self.0.len().checked_sub(8).expect("key includes hash");
        let (bytes, hash) = self.0.split_at(len);
        (bytes, u64::from_le_bytes(hash.try_into().unwrap()))
    }

    #[inline]
    pub(crate) fn bytes(&self) -> &[u8] {
        let len = self.0.len().checked_sub(8).expect("key includes hash");
        &self.0[..len]
    }

    #[inline(always)]
    fn matches_name(&self, name: &[u8], hash: u64) -> bool {
        // A valid slice fits isize::MAX, so adding the hash cannot wrap.
        if self.0.len() != name.len() + 8 {
            return false;
        }
        let (bytes, stored_hash) = self.0.split_at(name.len());
        u64::from_le_bytes(stored_hash.try_into().unwrap()) == hash && name_bytes_eq(bytes, name)
    }
}

/// Field hints usually compare short identifiers from distinct buffers. Cover
/// every byte with overlapping fixed-size loads instead of calling memcmp.
/// The length guard also bounds every slice below; long keys keep slice equality.
#[inline(always)]
fn name_bytes_eq(a: &[u8], b: &[u8]) -> bool {
    let len = a.len();
    if len != b.len() {
        return false;
    }
    match len {
        0 => true,
        1 => a[0] == b[0],
        2..=3 => {
            u16::from_ne_bytes(a[..2].try_into().unwrap())
                == u16::from_ne_bytes(b[..2].try_into().unwrap())
                && a[len - 1] == b[len - 1]
        }
        4..=7 => {
            u32::from_ne_bytes(a[..4].try_into().unwrap())
                == u32::from_ne_bytes(b[..4].try_into().unwrap())
                && u32::from_ne_bytes(a[len - 4..].try_into().unwrap())
                    == u32::from_ne_bytes(b[len - 4..].try_into().unwrap())
        }
        8..=16 => {
            u64::from_ne_bytes(a[..8].try_into().unwrap())
                == u64::from_ne_bytes(b[..8].try_into().unwrap())
                && u64::from_ne_bytes(a[len - 8..].try_into().unwrap())
                    == u64::from_ne_bytes(b[len - 8..].try_into().unwrap())
        }
        _ => a == b,
    }
}

/// Hash and equality go through [`KeyView`], so a table can be probed with a
/// borrowed key (string bytes in a register or a constant) without building
/// an owned `TableKey`.
#[derive(Clone, Debug)]
pub(crate) enum TableKey {
    Bool(bool),
    Integer(i64),
    /// Canonical bits of a non-integral, non-NaN float. Never `-0.0`.
    Float(u64),
    String(KeyString),
    Object(ObjectId),
    /// A native function, by its index in `Heap::natives`.
    Native(u32),
    /// A light userdata, by its token (ADR 0043).
    Light(LightDomain, u64),
}

/// A table key that borrows its string bytes.
#[derive(Clone, Copy, Eq, Debug)]
pub(crate) enum KeyView<'a> {
    Bool(bool),
    Integer(i64),
    Float(u64),
    String(&'a [u8], u64),
    Object(ObjectId),
    Native(u32),
    Light(LightDomain, u64),
}

// Every probe of a table's index compares keys: string and integer keys
// in line, every other kind out of line. With the light userdata arm, a
// derived comparison made table lookups measurably slower (Phase 3.26).
impl PartialEq for KeyView<'_> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (KeyView::String(a, _), KeyView::String(b, _)) => a == b,
            (KeyView::Integer(a), KeyView::Integer(b)) => a == b,
            _ => self.other_eq(other),
        }
    }
}

impl<'a> KeyView<'a> {
    pub(crate) fn string(bytes: &'a [u8]) -> Self {
        Self::String(bytes, string_hash(bytes))
    }

    #[inline]
    pub(crate) fn cached_string(bytes: &'a [u8], hash: u64) -> Self {
        debug_assert_eq!(hash, string_hash(bytes));
        Self::String(bytes, hash)
    }

    #[inline(never)]
    fn other_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (KeyView::Bool(a), KeyView::Bool(b)) => a == b,
            (KeyView::Float(a), KeyView::Float(b)) => a == b,
            (KeyView::Object(a), KeyView::Object(b)) => a == b,
            (KeyView::Native(a), KeyView::Native(b)) => a == b,
            (KeyView::Light(d, a), KeyView::Light(e, b)) => d == e && a == b,
            _ => false,
        }
    }
}

impl Hash for KeyView<'_> {
    #[inline]
    fn hash<H: Hasher>(&self, output: &mut H) {
        if let Self::String(bytes, hash) = self {
            debug_assert_eq!(*hash, string_hash(bytes));
            output.write_u64(*hash);
            return;
        }
        let mut hash = StableBuildHasher.build_hasher();
        let state = &mut hash;
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Bool(v) => v.hash(state),
            Self::Integer(v) => v.hash(state),
            Self::Float(v) => v.hash(state),
            Self::Object(v) => v.hash(state),
            Self::Native(v) => v.hash(state),
            Self::Light(d, v) => {
                d.hash(state);
                v.hash(state);
            }
            Self::String(..) => unreachable!(),
        }
        let hash = hash.finish();
        output.write_u64(hash);
    }
}

impl TableKey {
    pub(crate) fn string(bytes: Vec<u8>) -> Self {
        let hash = string_hash(&bytes);
        Self::String(KeyString::copied(&bytes, hash))
    }

    pub(crate) fn view(&self) -> KeyView<'_> {
        match self {
            Self::Bool(bit) => KeyView::Bool(*bit),
            Self::Integer(integer) => KeyView::Integer(*integer),
            Self::Float(bits) => KeyView::Float(*bits),
            Self::String(key) => {
                let (bytes, hash) = key.parts();
                KeyView::cached_string(bytes, hash)
            }
            Self::Object(id) => KeyView::Object(*id),
            Self::Native(index) => KeyView::Native(*index),
            Self::Light(domain, bits) => KeyView::Light(*domain, *bits),
        }
    }

    /// The value `next` returns for this key. Numbers and booleans are
    /// rebuilt from the key, so a float key that normalized to an integer
    /// is the integer. Strings and objects keep the object that was stored.
    fn canonical_value(&self, key_value: Value) -> Value {
        match self {
            Self::Bool(bit) => Value::Bool(*bit),
            Self::Integer(integer) => Value::Integer(*integer),
            Self::Float(bits) => Value::Float(f64::from_bits(*bits)),
            Self::Native(index) => Value::Native(*index),
            Self::Light(domain, bits) => Value::LightUserdata(*domain, *bits),
            Self::String(..) | Self::Object(_) => key_value,
        }
    }
}

impl PartialEq for TableKey {
    fn eq(&self, other: &Self) -> bool {
        self.view() == other.view()
    }
}

impl Eq for TableKey {}

impl Hash for TableKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.view().hash(state);
    }
}

/// The borrowed form a `HashMap<TableKey, _>` can be probed with.
pub(crate) trait AsKeyView {
    fn as_view(&self) -> KeyView<'_>;
}

impl AsKeyView for TableKey {
    fn as_view(&self) -> KeyView<'_> {
        self.view()
    }
}

impl AsKeyView for KeyView<'_> {
    fn as_view(&self) -> KeyView<'_> {
        *self
    }
}

impl<'a> Borrow<dyn AsKeyView + 'a> for TableKey {
    fn borrow(&self) -> &(dyn AsKeyView + 'a) {
        self
    }
}

impl PartialEq for dyn AsKeyView + '_ {
    fn eq(&self, other: &Self) -> bool {
        self.as_view() == other.as_view()
    }
}

impl Eq for dyn AsKeyView + '_ {}

impl Hash for dyn AsKeyView + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_view().hash(state);
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Slot {
    Live {
        key: TableKey,
        /// Traced by the collector. String keys keep the string object alive
        /// while the entry is live. Equality still uses bytes.
        key_value: Value,
        value: Value,
        next_live: Option<u32>,
        prev_live: Option<u32>,
    },
    /// Position of a deleted key. `key` is not traced: object keys are an
    /// [`ObjectId`], string keys are owned bytes.
    Dead {
        key: TableKey,
        /// May name a slot that was deleted later. [`Table::resolve`] follows
        /// until a live slot. Rebuilt from slot order on restore; not itself
        /// snapshot state.
        next_live: Option<u32>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct Table {
    slots: Vec<Slot>,
    index: HashMap<TableKey, u32, TableBuildHasher>,
    /// Derived positive-key -> latest slot map, including dead anchors.
    /// Zero is absent; other entries are slot + 1. Never snapshot state.
    /// Growth is bounded by slot count, not the largest integer key.
    array: Vec<u32>,
    first_live: Option<u32>,
    last_live: Option<u32>,
    live_count: u32,
    dead_count: u32,
    /// Bytes of the string keys of live entries, and of dead anchors. An
    /// anchor owns a copy of its key's bytes that no string object backs
    /// any more, so anchors also go once their bytes pass the live keys'
    /// (Phase 3.30): their memory stays within what the heap charges.
    live_key_bytes: u64,
    dead_key_bytes: u64,
    /// Live entries with a positive integer key: the bound `raw_border`
    /// searches below (Phase 3.22).
    positive_count: u32,
    /// Keys `1..=prefix` are all live: where `raw_border`'s search starts.
    /// A delete at or below it shortens it; `raw_border` extends it. So
    /// appending and taking `#` is not a scan from 1 each time, and the
    /// result is still the smallest border (Phase 3.22).
    prefix: std::cell::Cell<u32>,
}

/// Dead anchors may hold this many key bytes beyond the live keys' before
/// an insert drops them.
const ANCHOR_BYTES: u64 = 4096;

/// Count slots, including anchors: scans stay bounded after deletion too.
const SMALL_TABLE_SLOTS: usize = 5;

/// The bytes a key owns: a string key's copy of its bytes.
fn key_bytes(key: &TableKey) -> u64 {
    match key {
        TableKey::String(key) => key.bytes().len() as u64,
        _ => 0,
    }
}

/// Whether `key` counts toward `Table::positive_count`.
fn is_positive(key: &TableKey) -> bool {
    matches!(key, TableKey::Integer(key) if *key > 0)
}

impl Table {
    /// A known library field count avoids intermediate hash allocations.
    /// Capacity is physical storage; logical slots are charged on insertion.
    pub(crate) fn reserve_hash(&mut self, entries: usize) {
        self.index.reserve(entries);
    }

    pub(crate) fn new() -> Self {
        Self {
            slots: Vec::new(),
            index: HashMap::with_hasher(TableBuildHasher),
            array: Vec::new(),
            first_live: None,
            last_live: None,
            live_count: 0,
            dead_count: 0,
            live_key_bytes: 0,
            dead_key_bytes: 0,
            positive_count: 0,
            prefix: std::cell::Cell::new(0),
        }
    }

    #[cfg(any(test, feature = "__measure"))]
    pub(crate) fn live_len(&self) -> usize {
        self.live_count as usize
    }

    #[cfg(any(test, feature = "__measure"))]
    pub(crate) fn dead_len(&self) -> usize {
        self.dead_count as usize
    }

    /// Live entries plus dead anchors.
    pub(crate) fn slot_len(&self) -> usize {
        self.slots.len()
    }

    pub(crate) fn slots(&self) -> &[Slot] {
        &self.slots
    }

    /// Find the latest slot, preserving dead anchors just like the index.
    #[inline(always)]
    fn lookup(&self, key: KeyView<'_>) -> Option<u32> {
        if let KeyView::Integer(integer) = key
            && integer > 0
            && let Ok(offset) = usize::try_from(integer - 1)
            && let Some(&entry) = self.array.get(offset)
        {
            let found = entry.checked_sub(1);
            debug_assert_eq!(found, self.index.get(&key as &dyn AsKeyView).copied());
            return found;
        }
        // Only the bounded small mode has an empty index. Reuse the hash
        // lookup's empty test so indexed probes need no extra mode branch.
        if self.index.is_empty() {
            return self.lookup_small(key).checked_sub(1);
        }
        self.index.get(&key as &dyn AsKeyView).copied()
    }

    // Keep the bounded scan out of accelerated lookup callers, while lookup
    // itself stays inlined so dense reads and updates retain their fast path.
    #[inline(never)]
    fn lookup_small(&self, key: KeyView<'_>) -> u32 {
        let slot = match key {
            KeyView::String(bytes, hash) => self.slots.iter().rposition(|slot| {
                let (Slot::Live { key, .. } | Slot::Dead { key, .. }) = slot;
                matches!(key, TableKey::String(stored) if stored.parts().1 == hash && stored.bytes() == bytes)
            }),
            KeyView::Integer(integer) => self.slots.iter().rposition(|slot| {
                let (Slot::Live { key, .. } | Slot::Dead { key, .. }) = slot;
                matches!(key, TableKey::Integer(stored) if *stored == integer)
            }),
            _ => self.slots.iter().rposition(|slot| {
                let (Slot::Live { key: stored, .. } | Slot::Dead { key: stored, .. }) = slot;
                stored.view() == key
            }),
        };
        // Same absence/slot+1 encoding as the integer accelerator. Keeping
        // the return in one word preserves its branch-only absence check.
        slot.map_or(0, |slot| slot as u32 + 1)
    }

    pub(crate) fn get(&self, key: &TableKey) -> Value {
        self.get_view(key.view()).unwrap_or(Value::Nil)
    }

    /// The value of a live key, looked up without allocating. `None` when
    /// the key is absent or a dead anchor.
    pub(crate) fn get_view(&self, key: KeyView<'_>) -> Option<Value> {
        let value = self
            .lookup(key)
            .and_then(|index| match &self.slots[index as usize] {
                Slot::Live { value, .. } => Some(*value),
                Slot::Dead { .. } => None,
            });
        #[cfg(feature = "counters")]
        crate::counters::table_probe(value.is_some(), matches!(key, KeyView::String(..)));
        value
    }

    /// The same uncached read, exposing its slot for one-probe refresh.
    /// Keep get_view separate so ordinary probes retain their code generation.
    #[inline]
    fn lookup_live(&self, key: KeyView<'_>) -> Option<(u32, Value)> {
        self.lookup(key)
            .and_then(|index| match &self.slots[index as usize] {
                Slot::Live { value, .. } => Some((index, *value)),
                Slot::Dead { .. } => None,
            })
    }

    /// A hint is only a candidate slot, never a table identity or a cached
    /// value. Validate bounds, liveness and string bytes on every use. This
    /// also rejects dead anchors, moved/reused slots and different layouts;
    /// a different table with the same live key at this slot is a safe hit.
    #[inline(always)]
    pub(crate) fn get_name_hint(
        &self,
        name: &[u8],
        hash: u64,
        hint: &std::cell::Cell<u32>,
    ) -> Option<Value> {
        let slot = hint.get();
        if let Some(Slot::Live {
            key: TableKey::String(key),
            value,
            ..
        }) = self.slots.get(slot as usize)
            && key.matches_name(name, hash)
        {
            debug_assert_eq!(Some(slot), self.lookup(KeyView::cached_string(name, hash)));
            // Both reads select this exact slot, including NaN payload bits;
            // Value's language-style PartialEq cannot compare NaNs here.
            #[cfg(feature = "counters")]
            crate::counters::table_probe(true, true);
            return Some(*value);
        }
        let key = KeyView::cached_string(name, hash);
        let entry = self.lookup_live(key);
        #[cfg(feature = "counters")]
        crate::counters::table_probe(entry.is_some(), true);
        if let Some((slot, _)) = entry {
            hint.set(slot);
        }
        // Keep a failed candidate for a later table in an __index chain.
        // Each probe validates it afresh; it never caches an absent value.
        entry.map(|(_, value)| value)
    }

    /// Event lookup uses a disposable slot candidate shared across lookup
    /// sites. Validate the key on every hit; a value or metatable is never
    /// cached, and a deletion, compaction or changed handler takes effect now.
    #[inline]
    pub(crate) fn get_event_hint(
        &self,
        event: &[u8],
        hint: &std::cell::Cell<u32>,
    ) -> Option<Value> {
        let slot = hint.get();
        if let Some(Slot::Live {
            key: TableKey::String(key),
            value,
            ..
        }) = self.slots.get(slot as usize)
            && key.bytes() == event
        {
            #[cfg(feature = "counters")]
            crate::counters::table_probe(true, true);
            return Some(*value);
        }
        let entry = self.lookup_live(KeyView::string(event));
        #[cfg(feature = "counters")]
        crate::counters::table_probe(entry.is_some(), true);
        if let Some((slot, _)) = entry {
            hint.set(slot);
        }
        entry.map(|(_, value)| value)
    }

    /// As with update_view, nil is declined so deletion and __newindex keep
    /// their existing cold path. The caller retains the GC write barrier.
    #[inline]
    pub(crate) fn update_name_hint(
        &mut self,
        name: &[u8],
        hash: u64,
        hint: &std::cell::Cell<u32>,
        value: Value,
    ) -> bool {
        if matches!(value, Value::Nil) {
            return false;
        }
        #[cfg(debug_assertions)]
        let expected = self.lookup(KeyView::cached_string(name, hash));
        let slot = hint.get();
        if let Some(Slot::Live {
            key: TableKey::String(key),
            value: slot_value,
            ..
        }) = self.slots.get_mut(slot as usize)
            && key.matches_name(name, hash)
        {
            #[cfg(debug_assertions)]
            debug_assert_eq!(expected, Some(slot));
            #[cfg(feature = "counters")]
            crate::counters::table_probe(true, true);
            *slot_value = value;
            return true;
        }
        let key = KeyView::cached_string(name, hash);
        let slot = self.update_view_slot(key, value);
        hint.set(slot.unwrap_or(u32::MAX));
        slot.is_some()
    }

    /// Replace the value of a live key with a non-nil value, keeping its
    /// key object. `false`, with nothing changed, if the key is not live or
    /// `value` is nil (a delete goes through `insert`).
    pub(crate) fn update_view(&mut self, key: KeyView<'_>, value: Value) -> bool {
        if matches!(value, Value::Nil) {
            return false;
        }
        let Some(index) = self.lookup(key) else {
            #[cfg(feature = "counters")]
            crate::counters::table_probe(false, matches!(key, KeyView::String(..)));
            return false;
        };
        match &mut self.slots[index as usize] {
            Slot::Live {
                value: slot_value, ..
            } => {
                #[cfg(feature = "counters")]
                crate::counters::table_probe(true, matches!(key, KeyView::String(..)));
                *slot_value = value;
                true
            }
            Slot::Dead { .. } => {
                #[cfg(feature = "counters")]
                crate::counters::table_probe(false, matches!(key, KeyView::String(..)));
                false
            }
        }
    }

    #[inline]
    fn update_view_slot(&mut self, key: KeyView<'_>, value: Value) -> Option<u32> {
        if matches!(value, Value::Nil) {
            return None;
        }
        let Some(index) = self.lookup(key) else {
            #[cfg(feature = "counters")]
            crate::counters::table_probe(false, matches!(key, KeyView::String(..)));
            return None;
        };
        match &mut self.slots[index as usize] {
            Slot::Live {
                value: slot_value, ..
            } => {
                #[cfg(feature = "counters")]
                crate::counters::table_probe(true, matches!(key, KeyView::String(..)));
                *slot_value = value;
                Some(index)
            }
            Slot::Dead { .. } => {
                #[cfg(feature = "counters")]
                crate::counters::table_probe(false, matches!(key, KeyView::String(..)));
                None
            }
        }
    }

    pub(crate) fn insert(&mut self, key: TableKey, key_value: Value, value: Value) {
        if matches!(value, Value::Nil) {
            self.delete_live(&key);
            return;
        }
        if let Some(index) = self.lookup(key.view())
            && matches!(self.slots[index as usize], Slot::Live { .. })
        {
            // An update keeps the key object the entry was created with.
            if let Slot::Live {
                value: slot_value, ..
            } = &mut self.slots[index as usize]
            {
                *slot_value = value;
            }
            return;
        }
        // Anchors go once they outnumber half the live entries: each
        // compaction rebuilds the table, so it waits until as many deletes
        // as it costs have paid for it (Phase 3.30).
        if u64::from(self.dead_count) * 2 > u64::from(self.live_count)
            || self.dead_key_bytes > self.live_key_bytes.saturating_add(ANCHOR_BYTES)
        {
            self.compact();
        }
        let key_value = key.canonical_value(key_value);
        self.append_live(key, key_value, value);
    }

    /// Replace the slot vector and rebuild the index and live links.
    /// The caller supplies insertion order, including dead anchors.
    pub(crate) fn restore(&mut self, slots: Vec<Slot>) {
        self.slots = slots;
        self.reindex_and_relink();
    }

    /// `None` starts at the first live entry. A key that is neither live nor
    /// a dead anchor is an invalid traversal key.
    #[cfg(any(test, feature = "__measure"))]
    pub(crate) fn next(&self, key: Option<&TableKey>) -> Result<Option<(Value, Value)>, ()> {
        self.next_view(key.map(TableKey::view))
    }

    pub(crate) fn next_view(&self, key: Option<KeyView<'_>>) -> Result<Option<(Value, Value)>, ()> {
        let cursor = match key {
            None => return Ok(self.first_live.and_then(|index| self.pair_at(index))),
            Some(key) => self.lookup(key).ok_or(())?,
        };
        let next = match &self.slots[cursor as usize] {
            Slot::Live { next_live, .. } | Slot::Dead { next_live, .. } => *next_live,
        };
        Ok(self.resolve(next).and_then(|index| self.pair_at(index)))
    }

    /// Smallest Lua border. `O(B)` presence probes, `B` at most the number of
    /// live positive integer keys. Not a binary search over an array part.
    pub(crate) fn raw_border(&self) -> i64 {
        let border = smallest_border_from(
            i64::from(self.prefix.get()),
            i64::from(self.positive_count),
            |key| self.get(&TableKey::Integer(key)) != Value::Nil,
        );
        // Keys 1..=border are live; a border fits the positive-key count.
        self.prefix.set(u32::try_from(border).unwrap_or(0));
        border
    }

    #[cfg(test)]
    pub(crate) fn live_keys(&self) -> Vec<TableKey> {
        self.slots
            .iter()
            .filter_map(|slot| match slot {
                Slot::Live { key, .. } => Some(key.clone()),
                Slot::Dead { .. } => None,
            })
            .collect()
    }

    fn pair_at(&self, index: u32) -> Option<(Value, Value)> {
        match &self.slots[index as usize] {
            Slot::Live {
                key_value, value, ..
            } => Some((*key_value, *value)),
            Slot::Dead { .. } => None,
        }
    }

    fn resolve(&self, mut cursor: Option<u32>) -> Option<u32> {
        let mut steps = 0usize;
        while let Some(index) = cursor {
            if steps > self.slots.len() {
                return None;
            }
            steps += 1;
            match &self.slots[index as usize] {
                Slot::Live { .. } => return Some(index),
                Slot::Dead { next_live, .. } => cursor = *next_live,
            }
        }
        None
    }

    /// Delete the live entry in slot `slot`, as a nil store to its key
    /// would: it becomes a dead anchor in place, so no other slot moves.
    pub(crate) fn delete_slot(&mut self, slot: usize) {
        if let Some(Slot::Live { key, .. }) = self.slots.get(slot) {
            let key = key.clone();
            self.delete_live(&key);
        }
    }

    fn delete_live(&mut self, key: &TableKey) {
        self.delete_view(key.view());
    }

    pub(crate) fn delete_view(&mut self, key: KeyView<'_>) {
        let Some(index) = self.lookup(key) else {
            return;
        };
        let (owned, next_live, prev_live) = match &self.slots[index as usize] {
            Slot::Dead { .. } => return,
            Slot::Live {
                key,
                next_live,
                prev_live,
                ..
            } => (key.clone(), *next_live, *prev_live),
        };
        if let Some(prev) = prev_live {
            if let Slot::Live {
                next_live: slot, ..
            } = &mut self.slots[prev as usize]
            {
                *slot = next_live;
            }
        } else {
            self.first_live = next_live;
        }
        if let Some(next) = next_live {
            if let Slot::Live {
                prev_live: slot, ..
            } = &mut self.slots[next as usize]
            {
                *slot = prev_live;
            }
        } else {
            self.last_live = prev_live;
        }
        let bytes = key_bytes(&owned);
        self.slots[index as usize] = Slot::Dead {
            key: owned,
            next_live,
        };
        self.live_count -= 1;
        self.dead_count += 1;
        self.live_key_bytes -= bytes;
        self.dead_key_bytes += bytes;
        if let Slot::Dead { key, .. } = &self.slots[index as usize]
            && is_positive(key)
        {
            self.positive_count -= 1;
            if let TableKey::Integer(key) = key
                && *key <= i64::from(self.prefix.get())
            {
                self.prefix.set((*key - 1) as u32);
            }
        }
    }

    fn compact(&mut self) {
        let live = self
            .slots
            .drain(..)
            .filter_map(|slot| match slot {
                Slot::Live {
                    key,
                    key_value,
                    value,
                    ..
                } => Some(Slot::Live {
                    key,
                    key_value,
                    value,
                    next_live: None,
                    prev_live: None,
                }),
                Slot::Dead { .. } => None,
            })
            .collect();
        self.slots = live;
        self.reindex_and_relink();
    }

    fn append_live(&mut self, key: TableKey, key_value: Value, value: Value) {
        count!("table_inserts");
        let index = u32::try_from(self.slots.len()).expect("table entry count fits u32");
        let prev = self.last_live;
        if let Some(prev) = prev {
            if let Slot::Live { next_live, .. } = &mut self.slots[prev as usize] {
                *next_live = Some(index);
            }
        } else {
            self.first_live = Some(index);
        }
        #[cfg(feature = "counters")]
        let capacity = self.index.capacity();
        if self.slots.len() == SMALL_TABLE_SLOTS {
            // Replay the original growth policy in slot order. The last
            // occurrence wins, including a deleted traversal anchor.
            if matches!(
                self.slots.first(),
                Some(
                    Slot::Live {
                        key: TableKey::Integer(1),
                        ..
                    } | Slot::Dead {
                        key: TableKey::Integer(1),
                        ..
                    }
                )
            ) {
                self.index.reserve(4);
            }
            for (slot, entry) in self.slots.iter().enumerate() {
                let (Slot::Live { key, .. } | Slot::Dead { key, .. }) = entry;
                self.index.insert(key.clone(), slot as u32);
            }
            self.rebuild_array();
        }
        if self.slots.len() >= SMALL_TABLE_SLOTS {
            self.index.insert(key.clone(), index);
            self.index_array(&key, index);
        }
        #[cfg(feature = "counters")]
        if capacity != self.index.capacity() {
            count!("table_resizes");
        }
        if is_positive(&key) {
            self.positive_count += 1;
        }
        self.live_key_bytes += key_bytes(&key);
        self.slots.push(Slot::Live {
            key,
            key_value,
            value,
            next_live: None,
            prev_live: prev,
        });
        self.last_live = Some(index);
        self.live_count += 1;
    }

    fn index_array(&mut self, key: &TableKey, slot: u32) {
        let TableKey::Integer(integer) = key else {
            return;
        };
        let Ok(key) = usize::try_from(*integer) else {
            return;
        };
        if key == 0 {
            return;
        }
        if key > self.array.len() {
            // At most four words per slot (rounding to a power of two).
            // Sparse keys stay in the hash map. Repeated growth is geometric.
            if key > self.slots.len().saturating_add(1).saturating_mul(2) {
                return;
            }
            let Some(len) = key.checked_next_power_of_two() else {
                return;
            };
            let old = self.array.len();
            self.array.resize(len, 0);
            // Older sparse keys may now be covered, including dead anchors.
            // A slot scan costs the whole table; probing the index for each
            // newly covered key costs the added range at a hash probe each.
            // Take the cheaper one, so one growth never costs more than the
            // scan and the total stays linear, because ranges grow
            // geometrically (Phase 3.32 review: scanning alone was
            // O(n log n) for string-heavy tables gaining integer keys).
            if (len - old).saturating_mul(16) < self.slots.len() {
                for covered in old + 1..=len {
                    if let Some(&at) = self.index.get(&TableKey::Integer(covered as i64)) {
                        self.array[covered - 1] = at + 1;
                    }
                }
            } else {
                // Slot order makes the last duplicate win, just like the index.
                for (i, entry) in self.slots.iter().enumerate() {
                    let (Slot::Live { key, .. } | Slot::Dead { key, .. }) = entry;
                    if let TableKey::Integer(integer) = key
                        && *integer > old as i64
                        && *integer <= len as i64
                    {
                        self.array[*integer as usize - 1] = i as u32 + 1;
                    }
                }
            }
        }
        self.array[key - 1] = slot
            .checked_add(1)
            .expect("table entry count below u32::MAX");
    }

    fn reindex_and_relink(&mut self) {
        count!("table_rehashes");
        self.index.clear();
        if self.slots.len() <= SMALL_TABLE_SLOTS {
            self.index = HashMap::with_hasher(TableBuildHasher);
        }
        self.live_count = 0;
        self.dead_count = 0;
        self.live_key_bytes = 0;
        self.dead_key_bytes = 0;
        self.positive_count = 0;
        self.prefix.set(0);
        self.first_live = None;
        self.last_live = None;
        let mut next_at = vec![None; self.slots.len()];
        let mut next_live: Option<u32> = None;
        for index in (0..self.slots.len()).rev() {
            next_at[index] = next_live;
            if matches!(self.slots[index], Slot::Live { .. }) {
                next_live = Some(index as u32);
            }
        }
        let mut prev_live: Option<u32> = None;
        let indexed = self.slots.len() > SMALL_TABLE_SLOTS;
        for (index, slot) in self.slots.iter_mut().enumerate() {
            let key = match &*slot {
                Slot::Live { key, .. } | Slot::Dead { key, .. } => key.clone(),
            };
            let positive = is_positive(&key);
            let bytes = key_bytes(&key);
            if indexed {
                self.index.insert(key, index as u32);
            }
            match slot {
                Slot::Live {
                    next_live,
                    prev_live: prev_slot,
                    ..
                } => {
                    *next_live = next_at[index];
                    *prev_slot = prev_live;
                    if prev_live.is_none() {
                        self.first_live = Some(index as u32);
                    }
                    prev_live = Some(index as u32);
                    self.last_live = Some(index as u32);
                    self.live_count += 1;
                    self.live_key_bytes += bytes;
                    if positive {
                        self.positive_count += 1;
                    }
                }
                Slot::Dead { next_live, .. } => {
                    *next_live = next_at[index];
                    self.dead_count += 1;
                    self.dead_key_bytes += bytes;
                }
            }
        }
        if indexed {
            self.rebuild_array();
        } else {
            self.array = Vec::new();
        }
    }

    fn rebuild_array(&mut self) {
        self.array = Vec::new();
        let limit = self.slots.len().saturating_mul(2);
        let max = self
            .slots
            .iter()
            .filter_map(|slot| {
                let (Slot::Live { key, .. } | Slot::Dead { key, .. }) = slot;
                match key {
                    TableKey::Integer(key) if *key > 0 => {
                        usize::try_from(*key).ok().filter(|key| *key <= limit)
                    }
                    _ => None,
                }
            })
            .max()
            .unwrap_or(0);
        if let Some(len) = max.checked_next_power_of_two().filter(|_| max > 0) {
            self.array.resize(len, 0);
            for (i, slot) in self.slots.iter().enumerate() {
                let (Slot::Live { key, .. } | Slot::Dead { key, .. }) = slot;
                if let TableKey::Integer(key) = key
                    && *key > 0
                    && *key <= len as i64
                {
                    self.array[*key as usize - 1] = i as u32 + 1;
                }
            }
        }
    }
}

/// `limit` is the number of distinct live positive integer keys, or a
/// synthetic cap. The search never probes past `limit` or `i64::MAX`, and
/// it does not compute `maxinteger + 1`.
#[cfg(test)]
pub(crate) fn smallest_border(limit: i64, present: impl Fn(i64) -> bool) -> i64 {
    smallest_border_from(0, limit, present)
}

/// [`smallest_border`], given that keys `1..=known` are present.
pub(crate) fn smallest_border_from(known: i64, limit: i64, present: impl Fn(i64) -> bool) -> i64 {
    if known > 0 && known <= limit {
        return extend_border(known, limit, present);
    }
    if limit <= 0 || !present(1) {
        return 0;
    }
    extend_border(1, limit, present)
}

/// The smallest border at or above `border`, which is present.
fn extend_border(mut border: i64, limit: i64, present: impl Fn(i64) -> bool) -> i64 {
    loop {
        if border == limit || border == i64::MAX {
            return border;
        }
        let next = border.checked_add(1).expect("border is below i64::MAX");
        if !present(next) {
            return border;
        }
        border = next;
    }
}

/// Normalize a value into a table key.
///
/// `string_bytes` borrows the source bytes and cached hash. The key's owner
/// keeps its own bytes, including after string collection. `object_id` is the identity of a
/// table, closure, or thread.
pub(crate) fn normalize_key(
    value: Value,
    string_bytes: Option<(&[u8], u64)>,
    object_id: Option<ObjectId>,
) -> Result<TableKey, LuaFault> {
    match value {
        Value::Nil => Err(LuaFault::NilKey),
        Value::Bool(bit) => Ok(TableKey::Bool(bit)),
        Value::Integer(integer) => Ok(TableKey::Integer(integer)),
        Value::Float(number) => normalize_float(number),
        Value::String(_) => {
            let (bytes, hash) = string_bytes.ok_or(LuaFault::Type)?;
            Ok(TableKey::String(KeyString::copied(bytes, hash)))
        }
        Value::Table(_)
        | Value::Closure(_)
        | Value::Thread(_)
        | Value::NativeClosure(_)
        | Value::Userdata(_) => Ok(TableKey::Object(object_id.ok_or(LuaFault::Type)?)),
        Value::Native(index) => Ok(TableKey::Native(index)),
        Value::LightUserdata(domain, bits) => Ok(TableKey::Light(domain, bits)),
    }
}

/// 2^63 as a float: the first float above every `i64`.
const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;

/// The key a value would be stored under, borrowing string bytes from
/// `string_bytes`. `None` for nil, NaN, and object keys, which need the
/// owned path (`normalize_key`) or fault.
pub(crate) fn value_view(value: Value, string_bytes: Option<(&[u8], u64)>) -> Option<KeyView<'_>> {
    Some(match value {
        Value::Bool(bit) => KeyView::Bool(bit),
        Value::Integer(integer) => KeyView::Integer(integer),
        Value::Float(number) => match normalize_float(number).ok()? {
            TableKey::Integer(integer) => KeyView::Integer(integer),
            TableKey::Float(bits) => KeyView::Float(bits),
            _ => return None,
        },
        Value::String(_) => {
            let (bytes, hash) = string_bytes?;
            KeyView::cached_string(bytes, hash)
        }
        Value::Native(index) => KeyView::Native(index),
        Value::LightUserdata(domain, bits) => KeyView::Light(domain, bits),
        Value::Nil
        | Value::Table(_)
        | Value::Closure(_)
        | Value::Thread(_)
        | Value::NativeClosure(_)
        | Value::Userdata(_) => return None,
    })
}

fn normalize_float(number: f64) -> Result<TableKey, LuaFault> {
    if number.is_nan() {
        return Err(LuaFault::NanKey);
    }
    // `-0.0` and `0.0` are the integer key 0.
    if number == 0.0 {
        return Ok(TableKey::Integer(0));
    }
    // An integral float in the `i64` range is that integer key. 2^63 is
    // integral but outside the range; a saturating cast would alias it with
    // `math.maxinteger`.
    if number.fract() == 0.0 && (-TWO_POW_63..TWO_POW_63).contains(&number) {
        return Ok(TableKey::Integer(number as i64));
    }
    Ok(TableKey::Float(number.to_bits()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_tables_match_slot_index_through_mutation_and_restore() {
        assert!(std::mem::size_of::<TableKey>() <= 24);
        let keys = [
            TableKey::Integer(1),
            TableKey::Integer(2),
            TableKey::Integer(i64::MAX),
            TableKey::string(b"x".to_vec()),
            TableKey::string(b"x\0y".to_vec()),
            TableKey::string(vec![7; 128]),
            TableKey::Bool(true),
            TableKey::Float(1.5f64.to_bits()),
            TableKey::Object(ObjectId(9)),
            TableKey::Native(4),
        ];
        let mut table = Table::new();
        let mut seed = 0x2e14_497c_83b9_d061u64;
        let mut small = 0;
        let mut large = 0;
        let mut promoted = 0;
        let mut shrunk = 0;
        for round in 0..4000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let was_small = table.slots.len() <= SMALL_TABLE_SLOTS;
            let key = &keys[(seed >> 32) as usize % keys.len()];
            match seed % 9 {
                0..=3 => table.insert(key.clone(), Value::Nil, Value::Integer(round)),
                4 => table.delete_view(key.view()),
                5 => {
                    table.update_view(key.view(), Value::Integer(round));
                }
                6 => {
                    if !table.slots.is_empty() {
                        table.delete_slot(seed as usize % table.slots.len());
                    }
                }
                7 => table.restore(table.slots.clone()),
                _ => table.compact(),
            }
            // Periodically drain through the collector boundary and shrink
            // back across the threshold, retaining dead anchors until compact.
            if round % 97 == 96 {
                for slot in 0..table.slots.len() {
                    table.delete_slot(slot);
                }
                table.compact();
            }
            let is_small = table.slots.len() <= SMALL_TABLE_SLOTS;
            promoted += usize::from(was_small && !is_small);
            shrunk += usize::from(!was_small && is_small);
            if is_small {
                small += 1;
                assert_eq!(table.index.capacity(), 0);
                assert_eq!(table.array.capacity(), 0);
            } else {
                large += 1;
            }
            // Independent original hash lookup, built from the actual ordered
            // slots, including dead keys and last-occurrence semantics.
            let mut reference = HashMap::with_hasher(TableBuildHasher);
            for (index, slot) in table.slots.iter().enumerate() {
                let (Slot::Live { key, .. } | Slot::Dead { key, .. }) = slot;
                reference.insert(key.clone(), index as u32);
            }
            for key in &keys {
                let expected = reference.get(key).copied();
                assert_eq!(table.lookup(key.view()), expected);
                let value = expected.and_then(|index| match table.slots[index as usize] {
                    Slot::Live { value, .. } => Some(value),
                    Slot::Dead { .. } => None,
                });
                assert_eq!(table.get_view(key.view()), value);
                let next = expected
                    .map(|index| {
                        // A dead tail retains its old end-of-traversal link
                        // even if an insert subsequently appends a live key.
                        let (Slot::Live { next_live, .. } | Slot::Dead { next_live, .. }) =
                            table.slots[index as usize];
                        let mut cursor = next_live;
                        while let Some(index) = cursor {
                            match table.slots[index as usize] {
                                Slot::Live {
                                    key_value, value, ..
                                } => return Some((key_value, value)),
                                Slot::Dead { next_live, .. } => cursor = next_live,
                            }
                        }
                        None
                    })
                    .ok_or(());
                assert_eq!(table.next(Some(key)), next);
            }
            let border = smallest_border(i64::from(table.positive_count), |integer| {
                reference
                    .get(&TableKey::Integer(integer))
                    .is_some_and(|&index| matches!(table.slots[index as usize], Slot::Live { .. }))
            });
            assert_eq!(table.raw_border(), border);
        }
        assert!(small > 0 && large > 0 && promoted > 0 && shrunk > 0);
    }

    #[test]
    fn dense_index_matches_hash_through_mutation_and_restore() {
        let mut fast = Table::new();
        let mut hash = Table::new();
        let mut state = 0x67e2_19b5_08ac_d431u64;
        let keys: Vec<_> = (1..=96)
            .map(TableKey::Integer)
            .chain([
                TableKey::Integer(0),
                TableKey::Integer(-1),
                TableKey::Integer(i64::MAX),
                TableKey::Integer(1 << 32),
                TableKey::Bool(true),
                TableKey::string(b"field".to_vec()),
                TableKey::Object(ObjectId(9)),
                normalize_float(1.5).unwrap(),
                normalize_float(1.0).unwrap(),
            ])
            .collect();
        // Force growth and cover a sparse key when the dense range catches up.
        for k in (1..=96).rev() {
            put(&mut fast, k, k);
            put(&mut hash, k, k);
        }
        for step in 0..4_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let key = &keys[state as usize % keys.len()];
            let value = Value::Integer(step);
            hash.array.clear(); // Reference lookups always use the original hash index.
            match (state >> 32) % 8 {
                0..=2 => {
                    fast.insert(key.clone(), value, value);
                    hash.insert(key.clone(), value, value);
                }
                3 => {
                    fast.delete_view(key.view());
                    hash.delete_view(key.view());
                }
                4 => assert_eq!(
                    fast.update_view(key.view(), value),
                    hash.update_view(key.view(), value)
                ),
                5 => {
                    // The collector's weak-value / ephemeron removal boundary.
                    if !fast.slots.is_empty() {
                        let slot = state as usize % fast.slots.len();
                        fast.delete_slot(slot);
                        hash.delete_slot(slot);
                    }
                }
                6 => {
                    fast.restore(fast.slots.clone());
                    hash.restore(hash.slots.clone());
                }
                _ => {
                    fast.compact();
                    hash.compact();
                }
            }
            hash.array.clear();
            assert_eq!(fast.live_keys(), hash.live_keys());
            assert_eq!(fast.dead_len(), hash.dead_len());
            assert_eq!(fast.raw_border(), hash.raw_border());
            assert_eq!(fast.next(None), hash.next(None));
            for key in &keys {
                assert_eq!(fast.get(key), hash.get(key));
                assert_eq!(fast.get_view(key.view()), hash.get_view(key.view()));
                assert_eq!(fast.next(Some(key)), hash.next(Some(key)));
            }
            for (offset, &slot) in fast.array.iter().enumerate() {
                assert_eq!(
                    slot.checked_sub(1),
                    fast.index
                        .get(&TableKey::Integer(offset as i64 + 1))
                        .copied()
                );
            }
            assert!(fast.array.len() <= fast.slots.len().saturating_mul(4));
        }
    }

    /// Growth into a string-heavy table probes the index for the new range
    /// (small growth) or scans the slots (large growth); both must agree
    /// with the index, including keys that were sparse and dead anchors.
    #[test]
    fn dense_growth_refill_matches_hash_in_string_heavy_tables() {
        let mut table = Table::new();
        // Sparse at insertion time, then one of them deleted to an anchor.
        for key in [9_000, 9_001, 9_500] {
            put(&mut table, key, key);
        }
        table.delete_view(TableKey::Integer(9_001).view());
        for i in 0..8_192 {
            let key = TableKey::string(format!("k{i}").into_bytes());
            table.insert(key, Value::Integer(i), Value::Integer(i));
        }
        let mut key = 1;
        while key <= 16_383 {
            put(&mut table, key, key);
            for (offset, &slot) in table.array.iter().enumerate() {
                assert_eq!(
                    slot.checked_sub(1),
                    table
                        .index
                        .get(&TableKey::Integer(offset as i64 + 1))
                        .copied()
                );
            }
            key = key * 2 + 1;
        }
        assert!(table.array.len() >= 9_501);
    }

    #[test]
    fn cached_string_lookup_matches_uncached_random_tables() {
        let mut seed = 0xa593_261f_7810_4d27u64;
        let mut random = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            seed
        };
        let keys: Vec<Vec<u8>> = (0..128)
            .map(|i| (0..i * 3).map(|_| (random() >> 32) as u8).collect())
            .collect();
        let hashes: Vec<u64> = keys.iter().map(|key| string_hash(key)).collect();
        let mut table = Table::new();
        let mut oracle = std::collections::BTreeMap::new();
        for round in 0..4000 {
            let index = (random() >> 32) as usize % keys.len();
            let key = &keys[index];
            let value = if random() & 3 == 0 {
                Value::Nil
            } else {
                Value::Integer(round)
            };
            table.insert(
                TableKey::String(KeyString::copied(key, hashes[index])),
                Value::Nil,
                value,
            );
            if value == Value::Nil {
                oracle.remove(key);
            } else {
                oracle.insert(key.clone(), value);
            }
            if round % 31 == 0 {
                table.restore(table.slots.clone());
            }
            for (key, &hash) in keys.iter().zip(&hashes) {
                let expected = oracle.get(key).copied();
                assert_eq!(table.get_view(KeyView::cached_string(key, hash)), expected);
                assert_eq!(table.get_view(KeyView::string(key)), expected);
            }
        }
    }

    #[test]
    fn slot_hints_match_uncached_random_tables() {
        let mut seed = 0x459d_18ae_7493_b261u64;
        let mut random = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            seed
        };
        // Include empty, embedded-NUL and long keys, with distinct buffers.
        let keys: Vec<Vec<u8>> = (0..32)
            .map(|i| {
                let len = if i <= 17 { i } else { i * 7 };
                (0..len).map(|_| (random() >> 32) as u8).collect()
            })
            .collect();
        let hashes: Vec<_> = keys.iter().map(|key| string_hash(key)).collect();
        let mut cached = std::array::from_fn::<_, 3, _>(|_| Table::new());
        let mut plain = cached.clone();
        let hints = std::array::from_fn::<_, 8, _>(|_| std::cell::Cell::new(u32::MAX));
        for round in 0..4000 {
            let bits = random();
            let table_index = (bits >> 32) as usize % cached.len();
            let key_index = (bits >> 16) as usize % keys.len();
            let name = &keys[key_index];
            let hash = hashes[key_index];
            let hint = &hints[bits as usize % hints.len()];
            let table = &mut cached[table_index];
            let oracle = &mut plain[table_index];
            let value = if bits & 7 == 0 {
                Value::Nil
            } else {
                Value::Integer(round)
            };
            // Inject arbitrary/out-of-range hints, independently of layout.
            if round % 17 == 0 {
                hint.set((bits >> 32) as u32);
            }
            match (bits >> 8) % 7 {
                0..=1 => {
                    let key = TableKey::string(name.clone());
                    table.insert(key.clone(), Value::Nil, value);
                    oracle.insert(key, Value::Nil, value);
                }
                2 => assert_eq!(
                    table.update_name_hint(name, hash, hint, value),
                    oracle.update_view(KeyView::cached_string(name, hash), value)
                ),
                3 => {
                    table.delete_view(KeyView::cached_string(name, hash));
                    oracle.delete_view(KeyView::cached_string(name, hash));
                }
                4 => {
                    if !table.slots.is_empty() {
                        let slot = bits as usize % table.slots.len();
                        table.delete_slot(slot); // Weak/ephemeron GC removal.
                        oracle.delete_slot(slot);
                    }
                }
                5 => {
                    table.compact();
                    oracle.compact();
                }
                _ => {
                    table.restore(table.slots.clone());
                    oracle.restore(oracle.slots.clone());
                }
            }
            for (name, &hash) in keys.iter().zip(&hashes) {
                assert_eq!(
                    table.get_name_hint(name, hash, hint),
                    oracle.get_view(KeyView::cached_string(name, hash))
                );
            }
            // Keep a real cached candidate between rounds, often used next
            // on another table/key. Earlier probes deliberately share it.
            table.get_name_hint(name, hash, hint);
            assert_eq!(table.live_keys(), oracle.live_keys());
            assert_eq!(table.dead_len(), oracle.dead_len());
            let ordered = |table: &Table| {
                table
                    .slots
                    .iter()
                    .filter_map(|slot| match slot {
                        Slot::Live { key, value, .. } => Some((key.clone(), *value)),
                        Slot::Dead { .. } => None,
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(ordered(table), ordered(oracle));
        }
    }

    fn put(table: &mut Table, key: i64, value: i64) {
        table.insert(
            TableKey::Integer(key),
            Value::Integer(key),
            Value::Integer(value),
        );
    }

    #[test]
    fn float_keys_normalize_like_lua() {
        assert_eq!(
            normalize_key(Value::Float(9_223_372_036_854_775_808.0), None, None),
            Ok(TableKey::Float(9_223_372_036_854_775_808.0f64.to_bits()))
        );
        assert_eq!(
            normalize_key(Value::Float(-9_223_372_036_854_775_808.0), None, None),
            Ok(TableKey::Integer(i64::MIN))
        );
        let mut table = Table::new();
        table.insert(TableKey::Integer(2), Value::Float(2.0), Value::Integer(1));
        let (key, _) = table.next(None).unwrap().unwrap();
        assert_eq!(key, Value::Integer(2), "next returns the integer key");
        table.insert(TableKey::Integer(2), Value::Integer(2), Value::Integer(9));
        assert_eq!(table.get_view(KeyView::Integer(2)), Some(Value::Integer(9)));
        assert!(!table.update_view(KeyView::Integer(3), Value::Integer(1)));
        assert!(!table.update_view(KeyView::Integer(2), Value::Nil));
        assert_eq!(value_view(Value::Float(f64::NAN), None), None);
        assert_eq!(
            value_view(Value::Float(3.0), None),
            Some(KeyView::Integer(3))
        );
    }

    #[test]
    fn the_border_cache_follows_inserts_deletes_and_compaction() {
        let mut table = Table::new();
        let recount = |table: &Table| {
            table
                .slots
                .iter()
                .filter(|slot| matches!(slot, Slot::Live { key, .. } if is_positive(key)))
                .count() as u32
        };
        let mut state = 7u64;
        for _ in 0..4000 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let key = (state >> 40) as i64 % 64 - 8;
            let value = if state & 3 == 0 {
                Value::Nil
            } else {
                Value::Integer(1)
            };
            table.insert(TableKey::Integer(key), Value::Integer(key), value);
            assert_eq!(table.positive_count, recount(&table));
            // The cached prefix gives the smallest border a scan from 1 does.
            let scanned = smallest_border(i64::from(recount(&table)), |key| {
                table.get(&TableKey::Integer(key)) != Value::Nil
            });
            assert_eq!(table.raw_border(), scanned);
        }
        let slots = table.slots.clone();
        table.restore(slots);
        assert_eq!(table.positive_count, recount(&table));
    }

    #[test]
    fn insertion_order_survives_update_and_delete() {
        let mut table = Table::new();
        put(&mut table, 1, 10);
        put(&mut table, 2, 20);
        put(&mut table, 1, 11);
        table.insert(TableKey::Integer(1), Value::Integer(1), Value::Nil);
        put(&mut table, 3, 30);
        assert_eq!(
            table.live_keys(),
            vec![TableKey::Integer(2), TableKey::Integer(3)]
        );
        assert!(matches!(
            table.get(&TableKey::Integer(2)),
            Value::Integer(20)
        ));
        assert_eq!(table.dead_len(), 0);
    }

    #[test]
    fn nil_assignment_leaves_a_dead_anchor_until_the_next_insert() {
        let mut table = Table::new();
        table.insert(TableKey::Bool(true), Value::Bool(true), Value::Integer(1));
        table.insert(TableKey::Bool(true), Value::Bool(true), Value::Nil);
        assert_eq!(table.live_len(), 0);
        assert_eq!(table.dead_len(), 1);
        assert!(matches!(table.get(&TableKey::Bool(true)), Value::Nil));
        put(&mut table, 1, 1);
        assert_eq!(table.dead_len(), 0);
        assert_eq!(table.live_len(), 1);
    }

    #[test]
    fn float_keys_collapse_onto_integers() {
        assert_eq!(normalize_float(1.0).unwrap(), TableKey::Integer(1));
        assert_eq!(normalize_float(-0.0).unwrap(), TableKey::Integer(0));
        assert_eq!(normalize_float(0.0).unwrap(), TableKey::Integer(0));
        assert!(matches!(normalize_float(f64::NAN), Err(LuaFault::NanKey)));
        assert_eq!(
            normalize_float(1.5).unwrap(),
            TableKey::Float(1.5f64.to_bits())
        );
    }

    #[test]
    fn next_walks_live_entries_and_survives_deleting_the_current_key() {
        let mut table = Table::new();
        put(&mut table, 1, 10);
        put(&mut table, 2, 20);
        put(&mut table, 3, 30);
        let (key, value) = table.next(None).unwrap().unwrap();
        assert_eq!((key, value), (Value::Integer(1), Value::Integer(10)));
        table.insert(TableKey::Integer(1), Value::Integer(1), Value::Nil);
        let (key, value) = table.next(Some(&TableKey::Integer(1))).unwrap().unwrap();
        assert_eq!((key, value), (Value::Integer(2), Value::Integer(20)));
        table.insert(TableKey::Integer(2), Value::Integer(2), Value::Integer(99));
        let (key, _) = table.next(Some(&TableKey::Integer(2))).unwrap().unwrap();
        assert_eq!(key, Value::Integer(3));
        assert!(table.next(Some(&TableKey::Integer(3))).unwrap().is_none());
        assert!(table.next(Some(&TableKey::Integer(9))).is_err());
        assert!(matches!(
            table.get(&TableKey::Integer(2)),
            Value::Integer(99)
        ));
        assert_eq!(table.dead_len(), 1);
    }

    #[test]
    fn deleting_another_existing_key_is_skipped() {
        let mut table = Table::new();
        put(&mut table, 1, 10);
        put(&mut table, 2, 20);
        put(&mut table, 3, 30);
        table.insert(TableKey::Integer(2), Value::Integer(2), Value::Nil);
        let (key, _) = table.next(Some(&TableKey::Integer(1))).unwrap().unwrap();
        assert_eq!(key, Value::Integer(3));
        assert!(table.next(Some(&TableKey::Integer(2))).unwrap().unwrap().0 == Value::Integer(3));
    }

    #[test]
    fn reinsertion_appends_and_drops_dead_anchors() {
        let mut table = Table::new();
        put(&mut table, 1, 10);
        put(&mut table, 2, 20);
        table.insert(TableKey::Integer(1), Value::Integer(1), Value::Nil);
        put(&mut table, 1, 11);
        assert_eq!(
            table.live_keys(),
            vec![TableKey::Integer(2), TableKey::Integer(1)]
        );
        assert_eq!(table.dead_len(), 0);
        assert!(matches!(
            table.get(&TableKey::Integer(1)),
            Value::Integer(11)
        ));
        let (key, _) = table.next(None).unwrap().unwrap();
        assert_eq!(key, Value::Integer(2));
    }

    #[test]
    fn repeated_insert_delete_does_not_accumulate_anchors() {
        let mut table = Table::new();
        for key in 0..1_000 {
            put(&mut table, key, 1);
            table.insert(TableKey::Integer(key), Value::Integer(key), Value::Nil);
            assert!(table.dead_len() <= 1, "dead {}", table.dead_len());
            assert_eq!(table.live_len(), 0);
        }
        for key in 0..1_000 {
            put(&mut table, key, 1);
        }
        assert_eq!(table.dead_len(), 0);
        assert_eq!(table.live_len(), 1_000);
        for key in 0..1_000 {
            table.insert(TableKey::Integer(key), Value::Integer(key), Value::Nil);
        }
        assert_eq!(table.dead_len(), 1_000);
        assert_eq!(table.slot_len(), 1_000);
        put(&mut table, 5, 1);
        assert_eq!(table.dead_len(), 0);
        assert_eq!(table.live_len(), 1);
    }

    #[test]
    fn growing_the_index_does_not_change_live_order() {
        let mut table = Table::new();
        for key in 0..64 {
            put(&mut table, key, key);
        }
        assert_eq!(
            table.live_keys(),
            (0..64).map(TableKey::Integer).collect::<Vec<_>>()
        );
    }

    #[test]
    fn borders_follow_the_smallest_legal_border() {
        let empty = Table::new();
        assert_eq!(empty.raw_border(), 0);

        let mut sequence = Table::new();
        put(&mut sequence, 1, 10);
        put(&mut sequence, 2, 20);
        put(&mut sequence, 3, 30);
        assert_eq!(sequence.raw_border(), 3);

        let mut hole = Table::new();
        put(&mut hole, 1, 10);
        put(&mut hole, 3, 30);
        assert_eq!(hole.raw_border(), 1);

        let mut missing = Table::new();
        put(&mut missing, 2, 20);
        put(&mut missing, 3, 30);
        assert_eq!(missing.raw_border(), 0);

        let mut irrelevant = Table::new();
        put(&mut irrelevant, 0, 1);
        put(&mut irrelevant, -5, 1);
        irrelevant.insert(
            TableKey::string(b"x".to_vec()),
            Value::Nil,
            Value::Integer(1),
        );
        irrelevant.insert(TableKey::Object(ObjectId(9)), Value::Nil, Value::Integer(1));
        irrelevant.insert(
            TableKey::Float(1.5f64.to_bits()),
            Value::Nil,
            Value::Integer(1),
        );
        put(&mut irrelevant, 1, 7);
        put(&mut irrelevant, 2, 8);
        assert_eq!(irrelevant.raw_border(), 2);

        let mut sparse = Table::new();
        put(&mut sparse, 1_000_000_000_000, 1);
        assert_eq!(sparse.slot_len(), 1);
        assert_eq!(sparse.raw_border(), 0);

        let mut high = Table::new();
        put(&mut high, 1, 1);
        put(&mut high, 2, 1);
        put(&mut high, i64::MAX, 1);
        assert_eq!(high.slot_len(), 3);
        assert_eq!(high.raw_border(), 2);
    }

    #[test]
    fn float_one_counts_as_the_integer_border_key() {
        let mut table = Table::new();
        let key = normalize_float(1.0).unwrap();
        table.insert(key, Value::Integer(1), Value::Integer(10));
        put(&mut table, 2, 20);
        assert_eq!(table.raw_border(), 2);
    }

    #[test]
    fn border_search_does_not_wrap_maxinteger() {
        assert_eq!(smallest_border(0, |_| true), 0);
        assert_eq!(smallest_border(i64::MAX, |_| false), 0);
        assert_eq!(smallest_border(4, |key| (1..=4).contains(&key)), 4);
        assert_eq!(smallest_border(5, |key| key != 3), 2);
        assert_eq!(
            smallest_border(i64::MAX, |key| key == 1 || key == i64::MAX),
            1
        );
        assert_eq!(smallest_border(i64::MAX, |key| key == 1), 1);
    }

    #[test]
    fn restore_keeps_a_dead_anchor_between_live_keys() {
        let mut table = Table::new();
        put(&mut table, 1, 10);
        put(&mut table, 2, 20);
        table.insert(TableKey::Integer(1), Value::Integer(1), Value::Nil);
        let slots = table.slots().to_vec();
        let mut restored = Table::new();
        restored.restore(slots);
        let (key, _) = restored.next(Some(&TableKey::Integer(1))).unwrap().unwrap();
        assert_eq!(key, Value::Integer(2));
        assert_eq!(restored.dead_len(), 1);
        assert_eq!(restored.raw_border(), 0);
    }

    #[test]
    fn anchors_of_long_string_keys_go_before_their_bytes_pass_the_live_keys() {
        // Many live keys, so the count rule alone would keep the anchors.
        let mut table = Table::new();
        for key in 0..100 {
            put(&mut table, key, key);
        }
        for round in 0..50u8 {
            let key = TableKey::string(vec![round; 1 << 20]);
            table.insert(key.clone(), Value::Integer(1), Value::Integer(1));
            table.insert(key, Value::Integer(1), Value::Nil);
            assert!(table.dead_key_bytes <= table.live_key_bytes + ANCHOR_BYTES + (1 << 20));
        }
        assert!(table.dead_len() <= 1);
    }
}
