//! Canonical observation of the proof program.
//!
//! Integer results and non-string aliasing use [`ObjectId`](crate::id::ObjectId).
//! Pause counts are intentionally absent.

use crate::heap::{Status, UpvalueState};
use crate::host::{EffectRecord, Journal};
use crate::runtime::Runtime;
use crate::table::KeyView;
use crate::value::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
#[doc(hidden)] // Kernel observations; outside the embedding API.
pub struct Observation {
    pub tag: i64,
    pub mark: i64,
    pub inc_result: i64,
    pub get_result: i64,
    pub yielded: i64,
    pub upvalue: i64,
    pub a_id: u64,
    pub b_id: u64,
    pub a_b: u64,
    pub b_a: u64,
    pub inc_id: u64,
    pub get_id: u64,
    pub inc_upvalue: u64,
    pub get_upvalue: u64,
    pub yielder_id: u64,
    pub yielder_status: u8,
    pub fuel_consumed: u64,
    pub journal: Vec<EffectRecord>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[doc(hidden)] // Kernel observations; outside the embedding API.
pub struct ObserveError;

impl Runtime {
    #[doc(hidden)] // Kernel observations; outside the embedding API.
    pub fn observe(&self, journal: &Journal) -> Result<Observation, ObserveError> {
        let globals = self.heap().globals.ok_or(ObserveError)?;
        let table_a = self.field(globals, b"A")?;
        let Value::Table(a_handle) = table_a else {
            return Err(ObserveError);
        };
        let a_id = self.heap().tables.get(a_handle).ok_or(ObserveError)?.id;
        let b = self.field(a_handle, b"b")?;
        let Value::Table(b_handle) = b else {
            return Err(ObserveError);
        };
        let b_id = self.heap().tables.get(b_handle).ok_or(ObserveError)?.id;
        let back = self.field(b_handle, b"a")?;
        let Value::Table(back_handle) = back else {
            return Err(ObserveError);
        };
        let b_a = self.heap().tables.get(back_handle).ok_or(ObserveError)?.id;
        let a_b = b_id.raw();

        let inc = self.field(a_handle, b"inc")?;
        let get = self.field(a_handle, b"get")?;
        let yielder = self.field(a_handle, b"yielder")?;
        let Value::Closure(inc_handle) = inc else {
            return Err(ObserveError);
        };
        let Value::Closure(get_handle) = get else {
            return Err(ObserveError);
        };
        let Value::Thread(yielder_handle) = yielder else {
            return Err(ObserveError);
        };
        let inc_obj = self.heap().closures.get(inc_handle).ok_or(ObserveError)?;
        let get_obj = self.heap().closures.get(get_handle).ok_or(ObserveError)?;
        let inc_uv = *inc_obj.upvalues.first().ok_or(ObserveError)?;
        let get_uv = *get_obj.upvalues.first().ok_or(ObserveError)?;
        let upvalue = match self.heap().upvalues.get(inc_uv).ok_or(ObserveError)?.state {
            UpvalueState::Closed(Value::Integer(value)) => value,
            _ => return Err(ObserveError),
        };
        let yielder_obj = self
            .heap()
            .threads
            .get(yielder_handle)
            .ok_or(ObserveError)?;
        Ok(Observation {
            tag: self.int_field(a_handle, b"tag")?,
            mark: self.int_field(a_handle, b"mark")?,
            inc_result: self.int_field(a_handle, b"inc_result")?,
            get_result: self.int_field(a_handle, b"get_result")?,
            yielded: self.int_field(a_handle, b"yielded")?,
            upvalue,
            a_id: a_id.raw(),
            b_id: b_id.raw(),
            a_b,
            b_a: b_a.raw(),
            inc_id: inc_obj.id.raw(),
            get_id: get_obj.id.raw(),
            inc_upvalue: self
                .heap()
                .upvalues
                .get(inc_uv)
                .ok_or(ObserveError)?
                .id
                .raw(),
            get_upvalue: self
                .heap()
                .upvalues
                .get(get_uv)
                .ok_or(ObserveError)?
                .id
                .raw(),
            yielder_id: yielder_obj.id.raw(),
            yielder_status: yielder_obj.status.tag(),
            fuel_consumed: self.fuel_consumed(),
            journal: journal.entries().to_vec(),
        })
    }

    fn field(
        &self,
        table: crate::id::Handle<crate::heap::TableObj>,
        key: &[u8],
    ) -> Result<Value, ObserveError> {
        let object = self.heap().tables.get(table).ok_or(ObserveError)?;
        Ok(object
            .table
            .get_view(KeyView::string(key))
            .unwrap_or(Value::Nil))
    }

    fn int_field(
        &self,
        table: crate::id::Handle<crate::heap::TableObj>,
        key: &[u8],
    ) -> Result<i64, ObserveError> {
        match self.field(table, key)? {
            Value::Integer(value) => Ok(value),
            _ => Err(ObserveError),
        }
    }
}

impl Observation {
    #[doc(hidden)] // Kernel observations; outside the embedding API.
    pub fn assert_canonical_shape(&self) -> Result<(), String> {
        if self.tag != 7 {
            return Err(format!("tag {}", self.tag));
        }
        if self.inc_result != 1 || self.get_result != 1 || self.upvalue != 1 {
            return Err(format!(
                "closure results inc {} get {} n {}",
                self.inc_result, self.get_result, self.upvalue
            ));
        }
        if self.yielded != 42 || self.yielder_status != Status::LuaSuspended.tag() {
            return Err(format!(
                "yielder status {} value {}",
                self.yielder_status, self.yielded
            ));
        }
        if self.a_b != self.b_id || self.b_a != self.a_id || self.a_id == self.b_id {
            return Err("cycle aliasing broken".to_string());
        }
        if self.inc_upvalue != self.get_upvalue {
            return Err("closures do not share an upvalue".to_string());
        }
        if self.journal.len() != 1 {
            return Err(format!("journal len {}", self.journal.len()));
        }
        let effect = &self.journal[0];
        if effect.arg != 1 || effect.outcome != effect.id.sequence as i64 {
            return Err("mark outcome mismatch".to_string());
        }
        if self.mark != effect.outcome {
            return Err("table mark does not match journal".to_string());
        }
        Ok(())
    }
}
