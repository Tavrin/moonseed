//! The host's access to full userdata between steps (ADR 0044). Each
//! accessor lends the payload to a closure while it borrows the runtime,
//! so no step, collection, or restore can run while the borrow lives, and
//! nothing the closure gets outlives it.

use super::*;
use crate::heap::UserdataObj;
use crate::userdata::HostUserdata;

impl Runtime {
    pub(super) fn userdata_by_id(&self, id: ObjectId) -> Option<&UserdataObj> {
        match self.heap.find_by_id(id)? {
            (Kind::Userdata, index, generation) => {
                self.heap.userdata.get(Handle::new(index, generation))
            }
            _ => None,
        }
    }

    /// A userdata's payload, to change it: a running young collection or
    /// atomic phase finishes first, as for any host call by id. A payload
    /// holds no Lua values (ADR 0044), so the change gives no reference.
    pub(super) fn userdata_by_id_mut(&mut self, id: ObjectId) -> Option<&mut UserdataObj> {
        self.settle_atomic();
        match self.heap.find_by_id(id)? {
            (Kind::Userdata, index, generation) => self
                .heap
                .userdata
                .get_mut_storing(Handle::new(index, generation), false),
            _ => None,
        }
    }

    /// Lend a byte userdata's bytes to `read`. `None` if `id` names no
    /// byte userdata. Unstable API.
    pub fn with_userdata_bytes<R>(&self, id: ObjectId, read: impl FnOnce(&[u8]) -> R) -> Option<R> {
        Some(read(self.userdata_by_id(id)?.payload.bytes()?))
    }

    /// Lend a byte userdata's bytes to `write`. Their length is fixed.
    /// Unstable API.
    pub fn with_userdata_bytes_mut<R>(
        &mut self,
        id: ObjectId,
        write: impl FnOnce(&mut [u8]) -> R,
    ) -> Option<R> {
        Some(write(self.userdata_by_id_mut(id)?.payload.bytes_mut()?))
    }

    /// Lend a host userdata's value to `read`, if it is a `T`. Unstable
    /// API.
    pub fn with_userdata<T: HostUserdata, R>(
        &self,
        id: ObjectId,
        read: impl FnOnce(&T) -> R,
    ) -> Option<R> {
        Some(read(self.userdata_by_id(id)?.payload.host::<T>()?))
    }

    /// Lend a host userdata's value to `write`, if it is a `T`. Its
    /// charge then becomes what it declares (`logical_size`): growth is
    /// charged even past the quota, since the host has already made it,
    /// and the next allocation sees it; a shrink leaves the logical heap
    /// at once. Unstable API.
    pub fn with_userdata_mut<T: HostUserdata, R>(
        &mut self,
        id: ObjectId,
        write: impl FnOnce(&mut T) -> R,
    ) -> Option<R> {
        let object = self.userdata_by_id_mut(id)?;
        let value = object.payload.host_mut::<T>()?;
        let result = write(value);
        let size = value.logical_size();
        let old = std::mem::replace(&mut object.charge, size);
        if size > old {
            self.heap.gc.charge(size - old);
        } else {
            self.heap.give_back(old - size);
        }
        Some(result)
    }
}
