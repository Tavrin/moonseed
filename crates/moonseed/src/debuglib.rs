//! The `debug` library's functions and the traceback's resumable state
//! (ADR 0040). The VM side is `runtime/debug.rs`.
//!
//! Lua hooks and introspection follow Lua 5.4.9. `debug.debug` and
//! `debug.setcstacklimit` are absent, not stubs. The library breaks every
//! sandbox boundary a program has (it reads and writes any local, upvalue,
//! and metatable), so no standard installer includes it.

use crate::heap::STRING_CEILING;
use crate::host::{Builtin, HostRegistry};
use crate::id::SnapshotError;
use crate::opcode::{read_i64, read_u8, read_u32};

/// A `debug` function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DbgFn {
    GetRegistry,
    GetMetatable,
    SetMetatable,
    GetInfo,
    GetLocal,
    SetLocal,
    GetUpvalue,
    SetUpvalue,
    UpvalueJoin,
    UpvalueId,
    GetUserValue,
    SetUserValue,
    Traceback,
    SetHook,
    GetHook,
}

/// The library's fields, their registry symbols, and their functions.
pub(crate) const DEBUG_FUNCTIONS: [(&str, &str, DbgFn); 15] = [
    ("getregistry", "debug.getregistry", DbgFn::GetRegistry),
    ("getmetatable", "debug.getmetatable", DbgFn::GetMetatable),
    ("setmetatable", "debug.setmetatable", DbgFn::SetMetatable),
    ("getinfo", "debug.getinfo", DbgFn::GetInfo),
    ("getlocal", "debug.getlocal", DbgFn::GetLocal),
    ("setlocal", "debug.setlocal", DbgFn::SetLocal),
    ("getupvalue", "debug.getupvalue", DbgFn::GetUpvalue),
    ("setupvalue", "debug.setupvalue", DbgFn::SetUpvalue),
    ("upvaluejoin", "debug.upvaluejoin", DbgFn::UpvalueJoin),
    ("upvalueid", "debug.upvalueid", DbgFn::UpvalueId),
    ("getuservalue", "debug.getuservalue", DbgFn::GetUserValue),
    ("setuservalue", "debug.setuservalue", DbgFn::SetUserValue),
    ("traceback", "debug.traceback", DbgFn::Traceback),
    ("sethook", "debug.sethook", DbgFn::SetHook),
    ("gethook", "debug.gethook", DbgFn::GetHook),
];

/// Register the `debug` functions.
pub fn register_debug(registry: &mut HostRegistry) {
    for (_, symbol, function) in DEBUG_FUNCTIONS {
        registry.register_builtin(symbol, Builtin::Debug(function));
    }
}

/// `luaL_traceback`'s first and last parts: a longer stack shows its first
/// 10 levels and its last 11.
pub(crate) const LEVELS1: i64 = 10;
pub(crate) const LEVELS2: i64 = 11;

/// Table entries a traceback step looks at while it searches the loaded
/// modules for a function's name.
pub(crate) const SEARCH_BATCH: u32 = 256;

/// Where the search for a level's function in `package.loaded` is: the
/// slot of `_LOADED` it looks at, and, inside the table found there, the
/// slot of that table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Search {
    pub(crate) outer: u32,
    pub(crate) inner: Option<u32>,
}

/// `debug.traceback` between steps.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Traceback {
    /// The thread is argument 1, so the message is argument 2.
    pub(crate) threaded: bool,
    /// The level shown next.
    pub(crate) level: i64,
    /// The deepest level, as when the traceback began.
    pub(crate) last: i64,
    /// Levels left to show before the skip, `limit2show`; negative when
    /// nothing is skipped.
    pub(crate) shown: i64,
    /// `Some` while the level's name is being searched for.
    pub(crate) search: Option<Search>,
    /// The text so far, charged to the heap.
    pub(crate) text: Vec<u8>,
}

/// A debug function's state between steps.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DebugWork {
    Traceback(Traceback),
}

impl DebugWork {
    pub(crate) fn scratch(&self) -> u32 {
        0
    }

    pub(crate) fn held_bytes(&self) -> usize {
        match self {
            Self::Traceback(traceback) => traceback.text.len(),
        }
    }

    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::Traceback(traceback) => {
                out.push(1);
                out.push(u8::from(traceback.threaded));
                out.extend(traceback.level.to_le_bytes());
                out.extend(traceback.last.to_le_bytes());
                out.extend(traceback.shown.to_le_bytes());
                match traceback.search {
                    None => out.push(0),
                    Some(Search { outer, inner }) => {
                        out.push(1);
                        out.extend(outer.to_le_bytes());
                        match inner {
                            None => out.push(0),
                            Some(inner) => {
                                out.push(1);
                                out.extend(inner.to_le_bytes());
                            }
                        }
                    }
                }
                out.extend((traceback.text.len() as u32).to_le_bytes());
                out.extend_from_slice(&traceback.text);
            }
        }
    }

    pub(crate) fn decode(input: &mut &[u8]) -> Result<Self, SnapshotError> {
        let flag = |input: &mut &[u8]| match read_u8(input)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SnapshotError::InvalidTag),
        };
        match read_u8(input)? {
            1 => {
                let threaded = flag(input)?;
                let level = read_i64(input)?;
                let last = read_i64(input)?;
                let shown = read_i64(input)?;
                let search = if flag(input)? {
                    let outer = read_u32(input)?;
                    let inner = if flag(input)? {
                        Some(read_u32(input)?)
                    } else {
                        None
                    };
                    Some(Search { outer, inner })
                } else {
                    None
                };
                let len = read_u32(input)? as usize;
                if len > STRING_CEILING || len > input.len() {
                    return Err(SnapshotError::Truncated);
                }
                let (text, rest) = input.split_at(len);
                *input = rest;
                Ok(Self::Traceback(Traceback {
                    threaded,
                    level,
                    last,
                    shown,
                    search,
                    text: text.to_vec(),
                }))
            }
            _ => Err(SnapshotError::InvalidTag),
        }
    }

    /// Whether restore may continue this work: counters a run makes, and
    /// a thread in argument 1 when the traceback names one.
    pub(crate) fn fits(&self, first_is_thread: bool) -> bool {
        let frames = crate::runtime::MAX_FRAMES as i64 + 1;
        match self {
            Self::Traceback(traceback) => {
                (!traceback.threaded || first_is_thread)
                    && (0..=frames).contains(&traceback.last)
                    && (0..=traceback.last + 1).contains(&traceback.level)
                    && (-frames - 1..=LEVELS1).contains(&traceback.shown)
                    && traceback.text.len() <= STRING_CEILING
            }
        }
    }
}
