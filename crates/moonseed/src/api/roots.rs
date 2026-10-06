use std::cell::RefCell;
use std::rc::Rc;

use crate::id::{ObjectId, OwnerToken};
use crate::value::Value;

use super::{ApiError, Result};

struct Slot {
    generation: u64,
    count: usize,
    value: Value,
}

#[derive(Default)]
pub(crate) struct RootTable {
    slots: Vec<Slot>,
    free: Vec<usize>,
}

impl RootTable {
    pub(crate) fn values(&self) -> impl Iterator<Item = Value> + '_ {
        self.slots
            .iter()
            .filter(|slot| slot.count != 0)
            .map(|slot| slot.value)
    }

    fn insert(&mut self, value: Value) -> (usize, u64) {
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index];
            slot.count = 1;
            slot.value = value;
            (index, slot.generation)
        } else {
            let index = self.slots.len();
            self.slots.push(Slot {
                generation: 0,
                count: 1,
                value,
            });
            (index, 0)
        }
    }
}

pub(crate) struct Rooted {
    pub(crate) owner: OwnerToken,
    pub(crate) id: Option<ObjectId>,
    table: Rc<RefCell<RootTable>>,
    slot: usize,
    generation: u64,
}

impl Rooted {
    #[inline]
    pub(crate) fn new(
        table: &Rc<RefCell<RootTable>>,
        owner: OwnerToken,
        id: Option<ObjectId>,
        value: Value,
    ) -> Self {
        let (slot, generation) = table.borrow_mut().insert(value);
        Self {
            owner,
            id,
            table: Rc::clone(table),
            slot,
            generation,
        }
    }

    #[inline]
    pub(crate) fn value(&self, owner: OwnerToken) -> Result<Value> {
        if self.owner != owner {
            return Err(ApiError::WrongRuntime.into());
        }
        let roots = self.table.borrow();
        let slot = roots.slots.get(self.slot).ok_or(ApiError::Released)?;
        if slot.generation != self.generation || slot.count == 0 {
            return Err(ApiError::Released.into());
        }
        Ok(slot.value)
    }
}

impl Clone for Rooted {
    fn clone(&self) -> Self {
        self.table.borrow_mut().slots[self.slot].count += 1;
        Self {
            owner: self.owner,
            id: self.id,
            table: Rc::clone(&self.table),
            slot: self.slot,
            generation: self.generation,
        }
    }
}

impl Drop for Rooted {
    #[inline]
    fn drop(&mut self) {
        let mut roots = self.table.borrow_mut();
        let slot = &mut roots.slots[self.slot];
        if slot.generation != self.generation || slot.count == 0 {
            return;
        }
        slot.count -= 1;
        if slot.count == 0 {
            slot.value = Value::Nil;
            // Retire a slot whose generation cannot advance.
            if let Some(next) = slot.generation.checked_add(1) {
                slot.generation = next;
                roots.free.push(self.slot);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reused_root_slot_rejects_the_released_generation() {
        let table = Rc::new(RefCell::new(RootTable::default()));
        let owner = OwnerToken::mint();
        let root = Rooted::new(&table, owner, None, Value::Native(0));
        let slot = root.slot;
        let generation = root.generation;
        drop(root);
        let next = Rooted::new(&table, owner, None, Value::Native(1));
        assert_eq!(next.slot, slot);
        assert_ne!(next.generation, generation);
        let stale = Rooted {
            owner,
            id: None,
            table: Rc::clone(&table),
            slot,
            generation,
        };
        assert!(matches!(
            stale.value(owner),
            Err(super::super::Error::Api(ApiError::Released))
        ));
        drop(stale);
        assert_eq!(next.value(owner).unwrap(), Value::Native(1));
        drop(next);
        assert_eq!(table.borrow().values().count(), 0);
    }
}
