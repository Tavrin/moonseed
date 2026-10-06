//! Lua's registry, the `package` library, and `require` (ADR 0039).
//!
//! The registry is an ordinary table, made when the first library is
//! installed and a root from then on: the main thread at 1, the globals at
//! 2, and `_LOADED` and `_PRELOAD`, the tables `package.loaded` and
//! `package.preload` are. Every library installer puts its table in
//! `_LOADED`, whatever order they run in, so `require "string"` returns the
//! `string` global itself.
//!
//! `require` follows Lua 5.4.9's `ll_require`: `LOADED[name]` if it is
//! true; otherwise each `package.searchers` entry in turn, until one gives
//! a function, then that loader with the name and the loader data, its
//! result stored in `LOADED[name]` (or `true`), and the result and the
//! loader data returned. Reads and writes go through metamethods, and the
//! calls are made from the function's frame, so no coroutine may yield
//! across them, as in Lua.

use super::library::{AuxWork, Ctx, Next, Op};
use super::*;
use crate::library::Work;
use crate::package::{
    CONFIG, PackageWork, PkgFn, PreloadStep, REQUIRE, RequireStep, SEARCH_HOST, SEARCH_LUA,
    SEARCH_PATH, SEARCH_PRELOAD,
};

impl AuxWork for PackageWork {
    fn next(runtime: &mut Runtime, ctx: &Ctx, work: &mut Self) -> Result<Next, VmError> {
        runtime.package_next(ctx, work)
    }
    fn wrap(self) -> Work {
        Work::Package(Box::new(self))
    }
    fn wrap_boxed(self: Box<Self>) -> Work {
        Work::Package(self)
    }
}

impl Runtime {
    /// Lua's registry, made on first use with the main thread at 1 and the
    /// globals at 2.
    pub(crate) fn lua_registry(&mut self) -> Result<Handle<crate::heap::TableObj>, VmError> {
        if let Some(registry) = self.heap.registry {
            return Ok(registry);
        }
        let registry = self.alloc_table()?;
        // A root before it is filled.
        self.heap.registry = Some(registry);
        if let Some(main) = self.heap.entry {
            self.raw_set_field(registry, Value::Integer(1), Value::Thread(main))?;
        }
        let globals = self.heap.globals.ok_or(VmError::Corrupt)?;
        self.raw_set_field(registry, Value::Integer(2), Value::Table(globals))?;
        Ok(registry)
    }

    /// `registry[name]`, made a new table when it is not one: Lua's
    /// `luaL_getsubtable` on the registry.
    pub(crate) fn registry_subtable(
        &mut self,
        name: &str,
    ) -> Result<Handle<crate::heap::TableObj>, VmError> {
        let registry = self.lua_registry()?;
        let key = Value::String(self.alloc_string(name.as_bytes().to_vec())?);
        let found = self
            .heap
            .key_view(key)
            .ok()
            .and_then(|key| self.heap.table_get_view(registry, key));
        if let Some(Value::Table(table)) = found {
            return Ok(table);
        }
        let table = self.alloc_table()?;
        self.raw_set_field(registry, key, Value::Table(table))?;
        Ok(table)
    }

    pub(super) fn raw_set_field(
        &mut self,
        table: Handle<crate::heap::TableObj>,
        key: Value,
        value: Value,
    ) -> Result<(), VmError> {
        if self.heap.update_string_key(table, key, value) {
            return Ok(());
        }
        let normalized = self
            .heap
            .normalize_value(key)
            .map_err(|_| VmError::Corrupt)?;
        self.heap
            .table_insert(table, normalized, key, value)
            .map_err(|_| VmError::MemoryLimit)
    }

    /// Record an installed library in the registry's `_LOADED`, where
    /// `require` and `package.loaded` find it.
    pub(super) fn register_module(&mut self, name: &str, module: Value) -> Result<(), VmError> {
        let loaded = self.registry_subtable("_LOADED")?;
        let key = Value::String(self.alloc_string(name.as_bytes().to_vec())?);
        self.raw_set_field(loaded, key, module)
    }

    /// Install `package` and `require` (ADR 0039): `package.loaded` and
    /// `package.preload` are the registry's `_LOADED` and `_PRELOAD`,
    /// `package.searchers` holds preload, the optional filesystem Lua searcher,
    /// then the optional host resolver. Paths initially default to empty. The
    /// registry must have the functions (see [`crate::register_package`]).
    pub fn install_package(&mut self) -> Result<(), VmError> {
        let package = self.new_library_table("package")?;
        let Value::Table(package_table) = package else {
            return Err(VmError::Corrupt);
        };
        self.register_module("package", package)?;
        let loaded = self.registry_subtable("_LOADED")?;
        let preload = self.registry_subtable("_PRELOAD")?;
        self.set_field(package, "loaded", Value::Table(loaded))?;
        self.set_field(package, "preload", Value::Table(preload))?;
        let searchers = Value::Table(self.alloc_table()?);
        self.set_field(package, "searchers", searchers)?;
        let preload_searcher = self.native_value(SEARCH_PRELOAD)?;
        if let Value::Table(searchers) = searchers {
            self.raw_set_field(searchers, Value::Integer(1), preload_searcher)?;
            let mut index = 2;
            if self.host_capabilities.filesystem.is_some() {
                let Value::Native(native) = self.native_value(SEARCH_LUA)? else {
                    return Err(VmError::Corrupt);
                };
                let searcher = self.alloc_native_closure(native, vec![package], Vec::new())?;
                self.raw_set_field(
                    searchers,
                    Value::Integer(index),
                    Value::NativeClosure(searcher),
                )?;
                index += 1;
            }
            if self.host_capabilities.module_resolver.is_some() {
                let host_searcher = self.native_value(SEARCH_HOST)?;
                self.raw_set_field(searchers, Value::Integer(index), host_searcher)?;
            }
        }
        let searchpath = self.native_value(SEARCH_PATH)?;
        self.set_field(package, "searchpath", searchpath)?;
        let config = Value::String(self.alloc_string(CONFIG.to_vec())?);
        self.set_field(package, "config", config)?;
        for field in ["path", "cpath"] {
            let empty = Value::String(self.alloc_string(Vec::new())?);
            self.set_field(package, field, empty)?;
        }
        // `require` keeps the `package` table, which it reads
        // `searchers` from.
        let Value::Native(native) = self.native_value(REQUIRE)? else {
            return Err(VmError::Corrupt);
        };
        let require =
            self.alloc_native_closure(native, vec![Value::Table(package_table)], Vec::new())?;
        let globals = self.heap.globals.ok_or(VmError::Corrupt)?;
        self.set_field(
            Value::Table(globals),
            "require",
            Value::NativeClosure(require),
        )
    }

    pub(super) fn set_package_paths(&mut self, path: &[u8], cpath: &[u8]) -> Result<(), VmError> {
        let globals = self.heap.globals.ok_or(VmError::Corrupt)?;
        let package = self
            .heap
            .tables
            .get(globals)
            .ok_or(VmError::Corrupt)?
            .table
            .get_view(crate::table::KeyView::string(b"package"))
            .ok_or(VmError::Corrupt)?;
        for (field, bytes) in [("path", path), ("cpath", cpath)] {
            let value = self.new_string(bytes.to_vec())?;
            self.set_field(package, field, value)?;
        }
        Ok(())
    }

    /// A `package` function, called from the active frame's call site.
    pub(super) fn call_package(
        &mut self,
        active: Handle<ThreadObj>,
        function: PkgFn,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let (func, _, passed, _) = self.call_site(active)?;
        let ctx = Ctx {
            active,
            func,
            passed,
            framed: false,
        };
        if matches!(function, PkgFn::SearchPath | PkgFn::SearchLua) {
            return self.start_search(active, function == PkgFn::SearchLua, journal);
        }
        // Both take a string name (`luaL_checkstring`).
        if self.string_arg(&ctx, 0)?.is_none() {
            let next = self.bad_type(&ctx, 0, "string");
            return Ok(self.finish_next(active, next));
        }
        if function == PkgFn::SearchHost {
            return self.search_host(&ctx, journal);
        }
        let work = match function {
            PkgFn::Require => {
                if self.package_table(&ctx).is_none() {
                    return Ok(self.fault(LuaFault::Argument));
                }
                // Decline on absent raw fields so the full machine still applies
                // registry/loaded metamethods. A truthy cached value needs no key
                // allocation, continuation, loader data, or filesystem effects.
                if let Some(registry) = self.heap.registry
                    && let Some(Value::Table(loaded)) = self
                        .heap
                        .table_get_view(registry, crate::table::KeyView::string(b"_LOADED"))
                    && let Ok(key) = self.heap.key_view(self.lib_arg(&ctx, 0))
                    && let Some(value) = self.heap.table_get_view(loaded, key)
                    && value.truthy()
                {
                    return self.base_return(active, &[value]);
                }
                PackageWork::Require {
                    step: RequireStep::Loaded,
                    index: 1,
                    message: Vec::new(),
                }
            }
            PkgFn::SearchPreload => PackageWork::Preload {
                step: PreloadStep::Table,
            },
            PkgFn::SearchHost | PkgFn::SearchPath | PkgFn::SearchLua => {
                return Err(VmError::Corrupt);
            }
        };
        // The first operation reads the registry: it exists once `package`
        // is installed, and a sandbox may have called a searcher without it.
        let registry = self.lua_registry()?;
        let key = Value::String(self.alloc_string(match function {
            PkgFn::Require => b"_LOADED".to_vec(),
            PkgFn::SearchPreload => b"_PRELOAD".to_vec(),
            PkgFn::SearchHost | PkgFn::SearchPath | PkgFn::SearchLua => {
                return Err(VmError::Corrupt);
            }
        })?);
        self.lib_call_args.clear();
        self.run_first_op(
            ctx,
            work,
            Op::Get {
                obj: Value::Table(registry),
                key,
                into: 0,
            },
            journal,
        )
    }

    /// Start a machine whose first operation is `op`: the operation is
    /// made, then the machine goes on from its result.
    fn run_first_op(
        &mut self,
        ctx: Ctx,
        work: PackageWork,
        op: Op,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        match self.try_op(&ctx, op)? {
            super::library::Tried::Got(_) => self.run_aux(ctx, work, None, journal),
            super::library::Tried::Fault(fault) => Ok(self.fault(fault)),
            super::library::Tried::Call {
                function,
                args,
                wait,
            } => {
                self.save_lib(&ctx, work.wrap(), wait)?;
                self.builtin_call(function, args.as_slice(), journal)
            }
        }
    }

    /// The `package` table `require` keeps.
    fn package_table(&self, ctx: &Ctx) -> Option<Value> {
        let callee = self
            .heap
            .threads
            .get(ctx.active)?
            .stack
            .get(ctx.func as usize)
            .copied()?;
        let Value::NativeClosure(closure) = callee else {
            return None;
        };
        match self.heap.native_closures.get(closure)?.values.as_slice() {
            [table @ Value::Table(_)] => Some(*table),
            _ => None,
        }
    }

    /// A package machine's next part. Each read of a table and each call
    /// is an operation; the results are in scratch slots.
    fn package_next(&mut self, ctx: &Ctx, work: &mut PackageWork) -> Result<Next, VmError> {
        let name = self.lib_arg(ctx, 0);
        let Value::String(name_handle) = name else {
            return Err(VmError::Corrupt);
        };
        let scratch = |runtime: &Self, index| runtime.scratch(ctx, index);
        match work {
            PackageWork::Preload { step } => match step {
                PreloadStep::Table => {
                    *step = PreloadStep::Field;
                    Ok(Next::Op(Op::Get {
                        obj: scratch(self, 0),
                        key: name,
                        into: 1,
                    }))
                }
                PreloadStep::Field => {
                    let value = scratch(self, 1);
                    if matches!(value, Value::Nil) {
                        let mut text = b"no field package.preload['".to_vec();
                        text.extend_from_slice(
                            self.heap.string_bytes(name_handle).unwrap_or_default(),
                        );
                        text.extend_from_slice(b"']");
                        return Ok(Next::Done(vec![self.new_string(text)?]));
                    }
                    Ok(Next::Done(vec![
                        value,
                        self.new_string(b":preload:".to_vec())?,
                    ]))
                }
            },
            PackageWork::Require {
                step,
                index,
                message,
            } => match step {
                RequireStep::Loaded => {
                    *step = RequireStep::Check;
                    Ok(Next::Op(Op::Get {
                        obj: scratch(self, 0),
                        key: name,
                        into: 1,
                    }))
                }
                RequireStep::Check => {
                    let found = scratch(self, 1);
                    if found.truthy() {
                        return Ok(Next::Done(vec![found]));
                    }
                    let package = self.package_table(ctx).ok_or(VmError::Corrupt)?;
                    let key = Value::String(self.alloc_string(b"searchers".to_vec())?);
                    *step = RequireStep::Searchers;
                    Ok(Next::Op(Op::Get {
                        obj: package,
                        key,
                        into: 2,
                    }))
                }
                RequireStep::Searchers => {
                    if !matches!(scratch(self, 2), Value::Table(_)) {
                        return Ok(Next::Error(
                            LuaFault::Require,
                            b"'package.searchers' must be a table".to_vec(),
                        ));
                    }
                    *step = RequireStep::Search;
                    self.package_next(ctx, work)
                }
                RequireStep::Search => {
                    let Value::Table(searchers) = scratch(self, 2) else {
                        return Err(VmError::Corrupt);
                    };
                    let searcher = self
                        .heap
                        .table_get(searchers, &crate::table::TableKey::Integer(*index))
                        .unwrap_or(Value::Nil);
                    if matches!(searcher, Value::Nil) {
                        let mut text = b"module '".to_vec();
                        text.extend_from_slice(
                            self.heap.string_bytes(name_handle).unwrap_or_default(),
                        );
                        text.extend_from_slice(b"' not found:");
                        text.extend_from_slice(message);
                        return Ok(Next::Error(LuaFault::Require, text));
                    }
                    *step = RequireStep::Searched;
                    self.lib_call_args.clear();
                    self.lib_call_args.push(name);
                    Ok(Next::Op(Op::CallPair {
                        f: searcher,
                        into: 3,
                    }))
                }
                RequireStep::Searched => {
                    let found = scratch(self, 3);
                    if found.is_function() {
                        // The loader, with the name and the loader data.
                        *step = RequireStep::Loaded2;
                        let data = scratch(self, 4);
                        self.lib_call_args.clear();
                        self.lib_call_args.extend_from_slice(&[name, data]);
                        return Ok(Next::Op(Op::Call { f: found, into: 5 }));
                    }
                    if let Some(text) =
                        super::builtins::text_arg(&self.heap, found).map(|text| text.into_owned())
                    {
                        let grow = 2 + text.len();
                        if message.len() + grow > self.heap.max_string
                            || !self.heap.gc.fits(grow as u64)
                        {
                            return Ok(Next::Fault(LuaFault::Memory));
                        }
                        self.heap.charge_held(grow as u64);
                        message.extend_from_slice(b"\n\t");
                        message.extend_from_slice(&text);
                    }
                    *index += 1;
                    *step = RequireStep::Search;
                    self.package_next(ctx, work)
                }
                RequireStep::Loaded2 => {
                    let result = scratch(self, 5);
                    if matches!(result, Value::Nil) {
                        *step = RequireStep::Stored;
                        return self.package_next(ctx, work);
                    }
                    *step = RequireStep::Stored;
                    Ok(Next::Op(Op::Set {
                        obj: scratch(self, 0),
                        key: name,
                        value: result,
                    }))
                }
                RequireStep::Stored => {
                    *step = RequireStep::Recheck;
                    Ok(Next::Op(Op::Get {
                        obj: scratch(self, 0),
                        key: name,
                        into: 1,
                    }))
                }
                RequireStep::Recheck => {
                    let found = scratch(self, 1);
                    if !matches!(found, Value::Nil) {
                        return Ok(Next::Done(vec![found, scratch(self, 4)]));
                    }
                    // No value: `true`.
                    *step = RequireStep::StoredTrue;
                    Ok(Next::Op(Op::Set {
                        obj: scratch(self, 0),
                        key: name,
                        value: Value::Bool(true),
                    }))
                }
                RequireStep::StoredTrue => {
                    Ok(Next::Done(vec![Value::Bool(true), scratch(self, 4)]))
                }
            },
        }
    }
}
