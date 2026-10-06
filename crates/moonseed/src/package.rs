//! The `package` library's functions and their resumable state (ADR 0039).
//!
//! The VM side is `runtime/package.rs`. `require` and the preload searcher
//! read and write tables the program can give metamethods, and call Lua
//! (the searchers and the loader), so both are machines on the library
//! engine (ADR 0033), whose values sit in scratch slots.

use crate::heap::STRING_CEILING;
use crate::host::{Builtin, HostRegistry};
use crate::id::SnapshotError;
use crate::opcode::{read_i64, read_u8, read_u32};

/// A `package` function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PkgFn {
    /// `require`: a native closure over the `package` table, which it
    /// reads `searchers` from, as Lua's is a C closure over it.
    Require,
    /// `package.searchers[1]`: looks in `package.preload`.
    SearchPreload,
    /// `package.searchers[2]`: the optional host resolver.
    SearchHost,
    SearchPath,
    SearchLua,
}

/// The symbol of `require`.
pub(crate) const REQUIRE: &str = "package.require";
/// The symbol of the preload searcher.
pub(crate) const SEARCH_PRELOAD: &str = "package.searchpreload";
pub(crate) const SEARCH_LUA: &str = "package.searchlua";
pub(crate) const SEARCH_PATH: &str = "package.searchpath";
pub(crate) const SEARCH_HOST: &str = "package.searchhost";

/// Register `require`, the preload searcher, and the optional host searcher.
pub fn register_package(registry: &mut HostRegistry) {
    registry.register_builtin(REQUIRE, Builtin::Package(PkgFn::Require));
    registry.register_builtin(SEARCH_PRELOAD, Builtin::Package(PkgFn::SearchPreload));
    registry.register_builtin(SEARCH_HOST, Builtin::Package(PkgFn::SearchHost));
    registry.register_builtin(SEARCH_LUA, Builtin::Package(PkgFn::SearchLua));
    registry.register_builtin(SEARCH_PATH, Builtin::Package(PkgFn::SearchPath));
}

/// `package.config`: Lua 5.4's directory separator, path separator,
/// template mark, executable-directory mark, and ignore mark.
pub(crate) const CONFIG: &[u8] = b"/\n;\n?\n!\n-\n";

/// Where `require` is. Scratch slots: 0 the loaded table, 1 `LOADED[name]`,
/// 2 `package.searchers`, 3 and 4 a searcher's two results (the loader and
/// its data), 5 the loader's result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequireStep {
    /// The registry's `_LOADED` is being read into scratch 0.
    Loaded = 1,
    /// `LOADED[name]` is being read into scratch 1.
    Check = 2,
    /// `package.searchers` is being read into scratch 2.
    Searchers = 3,
    /// The next searcher is to be called.
    Search = 4,
    /// A searcher returned into scratch 3 and 4.
    Searched = 5,
    /// The loader returned into scratch 5.
    Loaded2 = 6,
    /// `LOADED[name] = result` was stored.
    Stored = 7,
    /// `LOADED[name]` is being read again into scratch 1.
    Recheck = 8,
    /// `LOADED[name] = true` was stored.
    StoredTrue = 9,
}

/// Where the preload searcher is. Scratch 0: the registry's `_PRELOAD`;
/// 1: `PRELOAD[name]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreloadStep {
    Table = 1,
    Field = 2,
}

/// A package function's state between steps.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PackageWork {
    /// `require(name)`: `index` is the searcher to call next, `message`
    /// what the searchers said so far, charged to the heap.
    Require {
        step: RequireStep,
        index: i64,
        message: Vec<u8>,
    },
    Preload {
        step: PreloadStep,
    },
}

impl PackageWork {
    pub(crate) fn scratch(&self) -> u32 {
        match self {
            Self::Require { .. } => 6,
            Self::Preload { .. } => 2,
        }
    }

    pub(crate) fn held_bytes(&self) -> usize {
        match self {
            Self::Require { message, .. } => message.len(),
            Self::Preload { .. } => 0,
        }
    }

    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::Require {
                step,
                index,
                message,
            } => {
                out.push(1);
                out.push(*step as u8);
                out.extend(index.to_le_bytes());
                out.extend((message.len() as u32).to_le_bytes());
                out.extend_from_slice(message);
            }
            Self::Preload { step } => {
                out.push(2);
                out.push(*step as u8);
            }
        }
    }

    pub(crate) fn decode(input: &mut &[u8]) -> Result<Self, SnapshotError> {
        Ok(match read_u8(input)? {
            1 => {
                let step = match read_u8(input)? {
                    1 => RequireStep::Loaded,
                    2 => RequireStep::Check,
                    3 => RequireStep::Searchers,
                    4 => RequireStep::Search,
                    5 => RequireStep::Searched,
                    6 => RequireStep::Loaded2,
                    7 => RequireStep::Stored,
                    8 => RequireStep::Recheck,
                    9 => RequireStep::StoredTrue,
                    _ => return Err(SnapshotError::InvalidTag),
                };
                let index = read_i64(input)?;
                let len = read_u32(input)? as usize;
                if len > STRING_CEILING || len > input.len() {
                    return Err(SnapshotError::Truncated);
                }
                let (message, rest) = input.split_at(len);
                *input = rest;
                Self::Require {
                    step,
                    index,
                    message: message.to_vec(),
                }
            }
            2 => Self::Preload {
                step: match read_u8(input)? {
                    1 => PreloadStep::Table,
                    2 => PreloadStep::Field,
                    _ => return Err(SnapshotError::InvalidTag),
                },
            },
            _ => return Err(SnapshotError::InvalidTag),
        })
    }

    /// Whether restore may continue this work: the module name is a string
    /// and the counters are ones a run makes. `string(0)` is the name's
    /// length when argument 0 is a string.
    pub(crate) fn fits(&self, string: &dyn Fn(u32) -> Option<usize>) -> bool {
        string(0).is_some()
            && match self {
                Self::Require { index, message, .. } => {
                    (1..=i64::from(i32::MAX)).contains(index) && message.len() <= STRING_CEILING
                }
                Self::Preload { .. } => true,
            }
    }
}
