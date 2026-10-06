//! OS symbol inventory and portable, checkpointed builtin work.
use crate::host::{Builtin, HostRegistry};
use crate::id::SnapshotError;
use crate::opcode::{read_i64, read_u8, read_u32};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum OsFn {
    Clock,
    Date,
    Difftime,
    Execute,
    Getenv,
    Remove,
    Rename,
    Setlocale,
    Time,
    Tmpname,
}
pub(crate) const FUNCTIONS: [(&str, &str, OsFn); 10] = [
    ("clock", "os.clock", OsFn::Clock),
    ("date", "os.date", OsFn::Date),
    ("difftime", "os.difftime", OsFn::Difftime),
    ("execute", "os.execute", OsFn::Execute),
    ("getenv", "os.getenv", OsFn::Getenv),
    ("remove", "os.remove", OsFn::Remove),
    ("rename", "os.rename", OsFn::Rename),
    ("setlocale", "os.setlocale", OsFn::Setlocale),
    ("time", "os.time", OsFn::Time),
    ("tmpname", "os.tmpname", OsFn::Tmpname),
];
/// Register the OS library. Installation grants no host authority.
pub fn register_os(registry: &mut HostRegistry) {
    for (_, symbol, function) in FUNCTIONS {
        registry.register_builtin(symbol, Builtin::Os(function));
    }
    registry.register_builtin("os.exit", Builtin::Exit);
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OsWork {
    pub function: OsFn,
    pub stage: u8,
    pub seconds: i64,
    pub pos: u32,
    pub out: Vec<u8>,
}
impl OsWork {
    pub fn new(function: OsFn) -> Self {
        Self {
            function,
            stage: 0,
            seconds: 0,
            pos: 0,
            out: Vec::new(),
        }
    }
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend([self.function as u8, self.stage]);
        out.extend(self.seconds.to_le_bytes());
        out.extend(self.pos.to_le_bytes());
        out.extend((self.out.len() as u32).to_le_bytes());
        out.extend(&self.out);
    }
    pub fn decode(input: &mut &[u8]) -> Result<Self, SnapshotError> {
        let tag = read_u8(input)?;
        let function = FUNCTIONS
            .get(tag as usize)
            .ok_or(SnapshotError::InvalidTag)?
            .2;
        let stage = read_u8(input)?;
        let seconds = read_i64(input)?;
        let pos = read_u32(input)?;
        let len = read_u32(input)? as usize;
        if len > crate::heap::STRING_CEILING || len > input.len() {
            return Err(SnapshotError::Truncated);
        }
        let (out, rest) = input.split_at(len);
        *input = rest;
        let work = Self {
            function,
            stage,
            seconds,
            pos,
            out: out.to_vec(),
        };
        if !work.fits() {
            return Err(SnapshotError::InvalidStructure);
        }
        Ok(work)
    }
    pub fn fits(&self) -> bool {
        let stages = match self.function {
            OsFn::Time => 20,
            OsFn::Date => 13,
            _ => 0,
        };
        self.stage <= stages
            && (self.function == OsFn::Date || (self.pos == 0 && self.out.is_empty()))
            && (self.function == OsFn::Date || self.seconds == 0 || self.function == OsFn::Time)
            && (self.function != OsFn::Date
                || self.stage == 3
                || (self.pos == 0 && self.out.is_empty()))
            && self.out.len() <= crate::heap::STRING_CEILING
            && self.out.len() <= (self.pos as usize).saturating_mul(250)
    }
}
