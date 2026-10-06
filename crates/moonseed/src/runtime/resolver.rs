//! The host package searcher (ADR 0053). Only loader creation happens here;
//! `require` retains the searcher protocol and calls the loader itself.

use super::library::{Ctx, Next};
use super::*;
use crate::{Resolved, ResolverPolicy};

fn encode(resolved: Resolved) -> Vec<u8> {
    let (tag, bytes) = match resolved {
        Resolved::Source(bytes) => (1, bytes),
        Resolved::Binary(bytes) => (2, bytes),
        Resolved::Native(symbol) => (3, symbol.into_bytes()),
        Resolved::NotFound(bytes) => (4, bytes),
    };
    let mut out = Vec::with_capacity(bytes.len().saturating_add(1));
    out.push(tag);
    out.extend(bytes);
    out
}

fn decode(bytes: &[u8]) -> Option<Resolved> {
    let (&tag, bytes) = bytes.split_first()?;
    Some(match tag {
        1 => Resolved::Source(bytes.to_vec()),
        2 => Resolved::Binary(bytes.to_vec()),
        3 => Resolved::Native(std::str::from_utf8(bytes).ok()?.to_owned()),
        4 => Resolved::NotFound(bytes.to_vec()),
        _ => return None,
    })
}

impl Runtime {
    pub(super) fn search_host(
        &mut self,
        ctx: &Ctx,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let name_value = self.lib_arg(ctx, 0);
        let Value::String(name_handle) = name_value else {
            return Err(VmError::Corrupt);
        };
        let name = self
            .heap
            .string_bytes(name_handle)
            .ok_or(VmError::Corrupt)?
            .to_vec();
        let resolved = if let Some((id, bytes)) = journal.module_result(self.effect_domain, &name) {
            // Replaying before the effect consumes its original sequence;
            // later uses consume none, even if the capability was omitted.
            if id.sequence > self.next_sequence {
                return Err(VmError::Corrupt);
            }
            if id.sequence == self.next_sequence {
                self.next_sequence = self.next_sequence.saturating_add(1);
            }
            let Some(resolved) = decode(bytes) else {
                return Ok(self.finish_next(
                    ctx.active,
                    Next::Error(
                        LuaFault::Require,
                        b"invalid journaled module resolution".to_vec(),
                    ),
                ));
            };
            resolved
        } else {
            let Some((policy, resolver)) = self.host_capabilities.module_resolver.clone() else {
                let diagnostic = self.new_string(b"no host module resolver".to_vec())?;
                return self.base_return(ctx.active, &[diagnostic]);
            };
            // A panicking resolver leaves the runtime poisoned, as a native's would.
            let outer = std::mem::replace(&mut self.in_callback, true);
            let resolved = match policy {
                ResolverPolicy::Pure => Ok(resolver.resolve(&name)),
                ResolverPolicy::External => {
                    let id = EffectId {
                        domain: self.effect_domain,
                        sequence: self.next_sequence,
                    };
                    self.next_sequence = self.next_sequence.saturating_add(1);
                    journal
                        .commit_module(id, &name, || encode(resolver.resolve(&name)))
                        .map_err(|_| VmError::Corrupt)
                        .and_then(|bytes| decode(&bytes).ok_or(VmError::Corrupt))
                }
            };
            self.in_callback = outer;
            resolved?
        };
        let loader = match resolved {
            Resolved::NotFound(diagnostic) => {
                let diagnostic = self.new_string(diagnostic)?;
                return self.base_return(ctx.active, &[diagnostic]);
            }
            Resolved::Native(symbol) => match self.native_value(&symbol) {
                Ok(loader) => loader,
                Err(VmError::UnknownNative) => {
                    return Ok(self.finish_next(
                        ctx.active,
                        Next::Error(
                            LuaFault::Require,
                            format!("unknown native module loader '{symbol}'").into_bytes(),
                        ),
                    ));
                }
                Err(error) => return Err(error),
            },
            Resolved::Source(source) => match self.module_source(&source, &name, false)? {
                Ok(loader) => loader,
                Err(message) => {
                    return Ok(
                        self.finish_next(ctx.active, Next::Error(LuaFault::Require, message))
                    );
                }
            },
            Resolved::Binary(source) => match self.module_source(&source, &name, true)? {
                Ok(loader) => loader,
                Err(message) => {
                    return Ok(
                        self.finish_next(ctx.active, Next::Error(LuaFault::Require, message))
                    );
                }
            },
        };
        self.base_return(ctx.active, &[loader, name_value])
    }

    fn module_source(
        &mut self,
        source: &[u8],
        name: &[u8],
        binary: bool,
    ) -> Result<Result<Value, Vec<u8>>, VmError> {
        if source.len() > self.heap.max_string {
            return Err(VmError::MemoryLimit);
        }
        let spec = if binary {
            // The same decoder and budget as `load`, including bytecode validation.
            let budget = self
                .heap
                .gc
                .headroom()
                .saturating_mul(2)
                .saturating_add(source.len() as u64);
            match crate::chunk::undump(source, budget) {
                Ok(spec) => spec,
                Err(error) => return Ok(Err(format!("module binary: {error}").into_bytes())),
            }
        } else {
            let defaults = crate::CompileLimits::default();
            let room = usize::try_from(self.heap.gc.headroom()).unwrap_or(usize::MAX);
            let limits = crate::CompileLimits {
                max_source_bytes: defaults.max_source_bytes.min(self.heap.max_string),
                max_instructions: defaults.max_instructions.min(room / 8),
                max_constants: defaults.max_constants.min(room / 16),
                max_functions: defaults.max_functions.min(room / 64),
            };
            match crate::compile::compile_for_guest(source, &limits, 0, self.heap.gc.headroom()) {
                Ok(chunk) => chunk.proto,
                Err(error) => {
                    return Ok(Err(format!("module source: {}", error.message).into_bytes()));
                }
            }
        };
        let env = Value::Table(self.heap.globals.ok_or(VmError::Corrupt)?);
        let chunk_name = if binary {
            ChunkName::Unnamed
        } else {
            let mut chunk_name = b"@".to_vec();
            chunk_name.extend(name);
            ChunkName::Bytes(chunk_name)
        };
        self.instantiate(&spec, env, chunk_name)
            .map(|loader| Ok(Value::Closure(loader)))
    }
}
