//! Checkpointed, handle-free source loading and Lua filesystem search.
use super::builtins::{LoadSource, text_arg};
use super::library::Ctx;
use super::*;
use crate::SnapshotError;
use crate::heap::Task;
use crate::hostcaps::{CapabilityPoll, CapabilityRequest, CapabilityValue};
use crate::opcode::{read_u8, read_u32};

const UNIT: usize = 1024;
const READ: usize = 64 * 1024;
const FILE: u8 = 0;
const DOFILE: u8 = 1;
const SEARCH: u8 = 2;
const MODULE: u8 = 3;
const PATH_RETURNED: u8 = 0;
const NORMALIZE: u8 = 1;
const EXPAND: u8 = 2;
const PROBE: u8 = 3;
const READ_SOURCE: u8 = 4;
const COMPILE: u8 = 5;
const ERROR_LIST: u8 = 6;

/// Only bytes and cursors live outside the traced stack. Environments and the
/// captured package table stay in the original argument/callee window.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct HostLoad {
    kind: u8,
    phase: u8,
    stdin: bool,
    name: Vec<u8>,
    path: Vec<u8>,
    sep: Vec<u8>,
    rep: Vec<u8>,
    replaced: Vec<u8>,
    expanded: Vec<u8>,
    candidate: Vec<u8>,
    source: Vec<u8>,
    message: Vec<u8>,
    cursor: usize,
    insertion: usize,
    inserting: bool,
}

fn cbytes(bytes: &[u8]) -> Vec<u8> {
    bytes[..bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len())].to_vec()
}
impl HostLoad {
    pub(crate) fn held_bytes(&self) -> usize {
        [
            &self.name,
            &self.path,
            &self.sep,
            &self.rep,
            &self.replaced,
            &self.expanded,
            &self.candidate,
            &self.source,
            &self.message,
        ]
        .iter()
        .map(|v| v.len())
        .sum()
    }
    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        out.extend([
            self.kind,
            self.phase,
            u8::from(self.stdin),
            u8::from(self.inserting),
        ]);
        out.extend((self.cursor as u32).to_le_bytes());
        out.extend((self.insertion as u32).to_le_bytes());
        for bytes in [
            &self.name,
            &self.path,
            &self.sep,
            &self.rep,
            &self.replaced,
            &self.expanded,
            &self.candidate,
            &self.source,
            &self.message,
        ] {
            out.extend((bytes.len() as u32).to_le_bytes());
            out.extend(bytes);
        }
    }
    pub(crate) fn decode(input: &mut &[u8]) -> Result<Self, SnapshotError> {
        let kind = read_u8(input)?;
        let phase = read_u8(input)?;
        let flag = |input: &mut &[u8]| match read_u8(input)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SnapshotError::InvalidTag),
        };
        let stdin = flag(input)?;
        let inserting = flag(input)?;
        let cursor = read_u32(input)? as usize;
        let insertion = read_u32(input)? as usize;
        let bytes = |input: &mut &[u8]| {
            let len = read_u32(input)? as usize;
            if len > crate::heap::STRING_CEILING || len > input.len() {
                return Err(SnapshotError::Truncated);
            }
            let (bytes, rest) = input.split_at(len);
            *input = rest;
            Ok(bytes.to_vec())
        };
        let work = Self {
            kind,
            phase,
            stdin,
            inserting,
            cursor,
            insertion,
            name: bytes(input)?,
            path: bytes(input)?,
            sep: bytes(input)?,
            rep: bytes(input)?,
            replaced: bytes(input)?,
            expanded: bytes(input)?,
            candidate: bytes(input)?,
            source: bytes(input)?,
            message: bytes(input)?,
        };
        if !work.fits() {
            return Err(SnapshotError::InvalidStructure);
        }
        Ok(work)
    }
    pub(crate) fn fits(&self) -> bool {
        self.kind <= MODULE
            && self.phase <= ERROR_LIST
            && self.source.len() <= crate::limits::DEFAULT_SOURCE_BYTES
            && (!self.stdin || self.kind <= DOFILE)
            && match self.phase {
                PATH_RETURNED => self.kind == MODULE,
                NORMALIZE => {
                    self.kind >= SEARCH
                        && self.cursor <= self.name.len()
                        && self.insertion <= self.rep.len()
                }
                EXPAND => {
                    self.kind >= SEARCH
                        && self.cursor <= self.path.len()
                        && self.insertion <= self.replaced.len()
                }
                PROBE => {
                    self.kind >= SEARCH && self.cursor <= self.expanded.len().saturating_add(1)
                }
                ERROR_LIST => self.kind >= SEARCH && self.cursor <= self.expanded.len(),
                READ_SOURCE | COMPILE => self.kind != SEARCH,
                _ => false,
            }
    }
    /// At most UNIT input/output bytes per step, including replacement expansion.
    fn transform(&mut self, limit: usize) -> Result<(), VmError> {
        let normalize = self.phase == NORMALIZE;
        let (input, needle, replacement, output) = if normalize {
            (
                &self.name,
                self.sep.as_slice(),
                &self.rep,
                &mut self.replaced,
            )
        } else {
            (
                &self.path,
                b"?".as_slice(),
                &self.replaced,
                &mut self.expanded,
            )
        };
        let mut budget = UNIT;
        while budget > 0 {
            if self.inserting {
                let n = (replacement.len() - self.insertion).min(budget);
                if output.len().saturating_add(n) > limit {
                    return Err(VmError::MemoryLimit);
                }
                output.extend_from_slice(&replacement[self.insertion..self.insertion + n]);
                self.insertion += n;
                budget -= n;
                if self.insertion == replacement.len() {
                    self.inserting = false;
                    self.insertion = 0;
                } else {
                    break;
                }
            } else if self.cursor == input.len() {
                break;
            } else if !needle.is_empty() && input[self.cursor..].starts_with(needle) {
                // PUC tests strchr(name, *sep) before gsub; starts_with implies that test.
                self.cursor += needle.len();
                self.inserting = true;
                budget -= 1;
            } else {
                if output.len() == limit {
                    return Err(VmError::MemoryLimit);
                }
                output.push(input[self.cursor]);
                self.cursor += 1;
                budget -= 1;
            }
        }
        if self.cursor == input.len() && !self.inserting {
            self.cursor = 0;
            self.phase = if normalize { EXPAND } else { PROBE };
        }
        Ok(())
    }
}

impl Runtime {
    fn hostload_arg(&self, active: Handle<ThreadObj>, func: u32, passed: u32, index: u32) -> Value {
        if index >= passed {
            Value::Nil
        } else {
            self.heap
                .threads
                .get(active)
                .and_then(|t| t.stack.get((func + 1 + index) as usize))
                .copied()
                .unwrap_or(Value::Nil)
        }
    }
    pub(super) fn start_file(
        &mut self,
        active: Handle<ThreadObj>,
        dofile: bool,
        _journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let (func, _, passed, _) = self.call_site(active)?;
        for index in 0..if dofile { 1 } else { 2 } {
            let value = self.hostload_arg(active, func, passed, index);
            if value != Value::Nil && text_arg(&self.heap, value).is_none() {
                let ctx = Ctx {
                    active,
                    func,
                    passed,
                    framed: false,
                };
                return Ok(self.finish_next(active, self.bad_type(&ctx, index, "string")));
            }
        }
        let name =
            text_arg(&self.heap, self.hostload_arg(active, func, passed, 0)).map(|s| cbytes(&s));
        let work = HostLoad {
            kind: if dofile { DOFILE } else { FILE },
            phase: READ_SOURCE,
            stdin: name.is_none(),
            name: name.unwrap_or_default(),
            ..HostLoad::default()
        };
        self.start_hostload(active, work)
    }
    pub(super) fn start_search(
        &mut self,
        active: Handle<ThreadObj>,
        module: bool,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let (func, _, passed, _) = self.call_site(active)?;
        let ctx = Ctx {
            active,
            func,
            passed,
            framed: false,
        };
        // PUC's ll_searchpath evaluates checkstring(path) before checkstring(name).
        if !module && text_arg(&self.heap, self.hostload_arg(active, func, passed, 1)).is_none() {
            return Ok(self.finish_next(active, self.bad_type(&ctx, 1, "string")));
        }
        let Some(name) = text_arg(&self.heap, self.hostload_arg(active, func, passed, 0)) else {
            return Ok(self.finish_next(active, self.bad_type(&ctx, 0, "string")));
        };
        let mut work = HostLoad {
            kind: if module { MODULE } else { SEARCH },
            phase: NORMALIZE,
            name: cbytes(&name),
            sep: b".".to_vec(),
            rep: b"/".to_vec(),
            ..HostLoad::default()
        };
        if module {
            let callee = self
                .heap
                .threads
                .get(active)
                .and_then(|t| t.stack.get(func as usize))
                .copied()
                .ok_or(VmError::Corrupt)?;
            let Value::NativeClosure(closure) = callee else {
                return Err(VmError::Corrupt);
            };
            let package = self
                .heap
                .native_closures
                .get(closure)
                .and_then(|c| c.values.first())
                .copied()
                .ok_or(VmError::Corrupt)?;
            let key = self.new_string(b"path".to_vec())?;
            match index::get(&self.heap, package, key) {
                Ok(Resolved::Done(value)) => {
                    let Some(path) = text_arg(&self.heap, value) else {
                        return Ok(self.library_error(
                            active,
                            LuaFault::Require,
                            b"'package.path' must be a string".to_vec(),
                        ));
                    };
                    work.path = cbytes(&path);
                    self.start_hostload(active, work)
                }
                Ok(Resolved::Call { function, target }) => {
                    work.phase = PATH_RETURNED;
                    self.start_hostload(active, work)?;
                    self.builtin_call(function, &[target, key], journal)
                }
                Err(fault) => Ok(self.fault(fault)),
            }
        } else {
            for index in 1..4 {
                let value = self.hostload_arg(active, func, passed, index);
                if index >= 2 && value == Value::Nil {
                    continue;
                }
                let Some(bytes) = text_arg(&self.heap, value) else {
                    return Ok(self.finish_next(active, self.bad_type(&ctx, index, "string")));
                };
                let bytes = cbytes(&bytes);
                match index {
                    1 => work.path = bytes,
                    2 => work.sep = bytes,
                    _ => work.rep = bytes,
                }
            }
            self.start_hostload(active, work)
        }
    }
    fn start_hostload(
        &mut self,
        active: Handle<ThreadObj>,
        work: HostLoad,
    ) -> Result<Poll, VmError> {
        let held = work.held_bytes() as u64;
        self.make_room(0, held);
        if !self.heap.gc.fits(held) {
            return Err(VmError::MemoryLimit);
        }
        self.heap.charge_held(held);
        self.push_builtin(active, Task::HostLoad(Box::new(work)))?;
        Ok(Poll::Continue)
    }
    fn hostload_work(&self, active: Handle<ThreadObj>) -> Result<&HostLoad, VmError> {
        match self
            .heap
            .threads
            .get(active)
            .and_then(|t| t.frames.last())
            .and_then(|f| f.boundary())
        {
            Some(Boundary::Builtin {
                task: Task::HostLoad(work),
                ..
            }) => Ok(work),
            _ => Err(VmError::Corrupt),
        }
    }
    fn hostload_work_mut(&mut self, active: Handle<ThreadObj>) -> Result<&mut HostLoad, VmError> {
        match self
            .heap
            .threads
            .get_mut(active)
            .and_then(|t| t.frames.last_mut())
            .and_then(|f| f.boundary_mut())
        {
            Some(Boundary::Builtin {
                task: Task::HostLoad(work),
                ..
            }) => Ok(work),
            _ => Err(VmError::Corrupt),
        }
    }
    /// Reserve and charge before growing any retained buffer. No private Values.
    fn hostload_room(&mut self, bytes: usize) -> Result<(), VmError> {
        self.make_room(0, bytes as u64);
        if !self.heap.gc.fits(bytes as u64) {
            return Err(VmError::MemoryLimit);
        }
        self.heap.charge_held(bytes as u64);
        Ok(())
    }
    pub(super) fn finish_hostload(
        &mut self,
        active: Handle<ThreadObj>,
        journal: &mut Journal,
    ) -> Result<Poll, VmError> {
        let (func, _, passed, _) = self.call_site_below(active)?;
        let phase = self.hostload_work(active)?.phase;
        match phase {
            PATH_RETURNED => {
                let (slot, _) = self.builtin_slot(active)?;
                let value = self
                    .heap
                    .threads
                    .get(active)
                    .and_then(|t| t.stack.get(slot as usize))
                    .copied()
                    .unwrap_or(Value::Nil);
                let Some(path) = text_arg(&self.heap, value) else {
                    return Ok(self.library_error(
                        active,
                        LuaFault::Require,
                        b"'package.path' must be a string".to_vec(),
                    ));
                };
                let path = cbytes(&path);
                self.hostload_room(path.len())?;
                let work = self.hostload_work_mut(active)?;
                work.path = path;
                work.phase = NORMALIZE;
            }
            NORMALIZE | EXPAND => {
                self.make_room(0, UNIT as u64);
                if !self.heap.gc.fits(UNIT as u64) {
                    return Err(VmError::MemoryLimit);
                }
                let before = self.hostload_work(active)?.held_bytes();
                let limit = self.heap.max_string;
                let result = self.hostload_work_mut(active)?.transform(limit);
                let growth = self.hostload_work(active)?.held_bytes() - before;
                self.heap.charge_held(growth as u64);
                result?;
            }
            PROBE => {
                let work = self.hostload_work(active)?;
                // Split after substitution: a semicolon in name/rep is a separator too.
                if !work.inserting {
                    if work.cursor > work.expanded.len() || work.expanded.is_empty() {
                        self.hostload_room(9)?;
                        let work = self.hostload_work_mut(active)?;
                        work.phase = ERROR_LIST;
                        work.cursor = 0;
                        work.message = b"no file '".to_vec();
                        return Ok(Poll::Continue);
                    }
                    let n = work.expanded[work.cursor..]
                        .iter()
                        .take(UNIT)
                        .position(|b| *b == b';');
                    let consumed = n.unwrap_or((work.expanded.len() - work.cursor).min(UNIT));
                    self.hostload_room(consumed)?;
                    let work = self.hostload_work_mut(active)?;
                    work.candidate
                        .extend_from_slice(&work.expanded[work.cursor..work.cursor + consumed]);
                    work.cursor += consumed;
                    // candidate may be empty: PUC probes leading/doubled empty templates.
                    if n.is_none() && work.cursor < work.expanded.len() {
                        return Ok(Poll::Continue);
                    }
                    work.cursor += 1;
                    work.inserting = true;
                }
                // A completed candidate is probed; while Pending it remains canonical.
                let path = self.hostload_work(active)?.candidate.clone();
                let result = self.capability(
                    &CapabilityRequest::FilesystemProbeReadable { path },
                    journal,
                )?;
                let CapabilityPoll::Ready(result) = result else {
                    return Ok(Poll::Continue);
                };
                if matches!(result, Ok(CapabilityValue::Boolean(true))) {
                    let work = self.hostload_work(active)?;
                    if work.kind == SEARCH {
                        let value = self.new_string(work.candidate.clone())?;
                        return self.builtin_done(active, &[value]);
                    }
                    let work = self.hostload_work_mut(active)?;
                    work.phase = READ_SOURCE;
                } else {
                    let work = self.hostload_work_mut(active)?;
                    let released = work.candidate.len() as u64;
                    work.candidate.clear();
                    work.inserting = false;
                    self.heap
                        .threads
                        .get_mut(active)
                        .ok_or(VmError::Corrupt)?
                        .charged_held -= released;
                    self.heap.give_back(released);
                }
            }
            ERROR_LIST => {
                let work = self.hostload_work(active)?;
                if work.cursor < work.expanded.len() {
                    let end = (work.cursor + UNIT).min(work.expanded.len());
                    let growth = work.expanded[work.cursor..end]
                        .iter()
                        .map(|b| if *b == b';' { 12 } else { 1 })
                        .sum::<usize>();
                    if work.message.len().saturating_add(growth + 1) > self.heap.max_string {
                        return Err(VmError::MemoryLimit);
                    }
                    self.hostload_room(growth)?;
                    let work = self.hostload_work_mut(active)?;
                    for b in &work.expanded[work.cursor..end] {
                        if *b == b';' {
                            work.message.extend_from_slice(b"'\n\tno file '");
                        } else {
                            work.message.push(*b);
                        }
                    }
                    work.cursor = end;
                } else {
                    let work = self.hostload_work(active)?;
                    let module = work.kind == MODULE;
                    let mut bytes = work.message.clone();
                    bytes.push(b'\'');
                    let message = self.new_string(bytes)?;
                    return if module {
                        self.builtin_done(active, &[message])
                    } else {
                        self.builtin_done(active, &[Value::Nil, message])
                    };
                }
            }
            READ_SOURCE => {
                let work = self.hostload_work(active)?;
                let limit = crate::limits::DEFAULT_SOURCE_BYTES.min(self.heap.max_string);
                let max = READ.min(limit.saturating_add(1).saturating_sub(work.source.len()));
                if max == 0 {
                    return self.hostload_failure(active, b"source byte limit exceeded".to_vec());
                }
                let request = if work.stdin {
                    CapabilityRequest::StdioReadStdin { max }
                } else {
                    CapabilityRequest::FilesystemReadFileRange {
                        path: if work.kind == MODULE {
                            work.candidate.clone()
                        } else {
                            work.name.clone()
                        },
                        offset: work.source.len() as u64,
                        max,
                    }
                };
                let result = self.capability(&request, journal)?;
                let CapabilityPoll::Ready(result) = result else {
                    return Ok(Poll::Continue);
                };
                match result {
                    Ok(CapabilityValue::Bytes(bytes)) => {
                        if self
                            .hostload_work(active)?
                            .source
                            .len()
                            .saturating_add(bytes.len())
                            > limit
                        {
                            return self
                                .hostload_failure(active, b"source byte limit exceeded".to_vec());
                        }
                        self.hostload_room(bytes.len())?;
                        let work = self.hostload_work_mut(active)?;
                        let eof = if work.stdin {
                            bytes.is_empty()
                        } else {
                            bytes.len() < max
                        };
                        work.source.extend(bytes);
                        if eof {
                            work.phase = COMPILE;
                        }
                    }
                    Err(error) => {
                        let work = self.hostload_work(active)?;
                        let mut message = if work.source.is_empty() {
                            b"cannot open ".to_vec()
                        } else {
                            b"cannot read ".to_vec()
                        };
                        message.extend_from_slice(if work.stdin {
                            b"stdin"
                        } else if work.kind == MODULE {
                            &work.candidate
                        } else {
                            &work.name
                        });
                        message.extend_from_slice(b": ");
                        message.extend(error.message);
                        return self.hostload_failure(active, message);
                    }
                    _ => return Err(VmError::Corrupt),
                }
            }
            COMPILE => {
                let work = self.hostload_work(active)?;
                let kind = work.kind;
                let filename = if kind == MODULE {
                    work.candidate.clone()
                } else {
                    work.name.clone()
                };
                let mut name = if work.stdin {
                    b"=stdin".to_vec()
                } else {
                    b"@".to_vec()
                };
                if !work.stdin {
                    name.extend_from_slice(&filename);
                }
                let mut source = std::mem::take(&mut self.hostload_work_mut(active)?.source);
                // PUC discards BOM and a leading # line, preserving text line numbers.
                let mut start = usize::from(source.starts_with(b"\xef\xbb\xbf")) * 3;
                let comment = source.get(start) == Some(&b'#');
                if comment {
                    start += source[start..]
                        .iter()
                        .position(|b| *b == b'\n')
                        .map_or(source.len() - start, |n| n + 1);
                }
                if comment && source.get(start) != Some(&0x1b) {
                    start = start.saturating_sub(1);
                    source[start] = b'\n';
                }
                let mode = if kind == FILE {
                    self.hostload_arg(active, func, passed, 1)
                } else {
                    Value::Nil
                };
                let env = if kind == FILE && passed >= 3 {
                    self.hostload_arg(active, func, passed, 2)
                } else {
                    Value::Table(self.heap.globals.ok_or(VmError::Corrupt)?)
                };
                let LoadSource::Values(values) = self.load_bytes(
                    active,
                    &source[start..],
                    &name,
                    mode,
                    env,
                    ChunkName::Bytes(name.clone()),
                )?;
                if values.first() == Some(&Value::Nil) {
                    let Value::String(error) = values[1] else {
                        return Err(VmError::Corrupt);
                    };
                    let message = self
                        .heap
                        .string_bytes(error)
                        .ok_or(VmError::Corrupt)?
                        .to_vec();
                    return self.hostload_failure(active, message);
                }
                let loader = values[0];
                if kind == DOFILE {
                    if let Some(Boundary::Builtin { task, .. }) = self
                        .heap
                        .threads
                        .get_mut(active)
                        .and_then(|t| t.frames.last_mut())
                        .and_then(|f| f.boundary_mut())
                    {
                        *task = Task::DoFile;
                    }
                    return self.builtin_call(loader, &[], journal);
                }
                if kind == MODULE {
                    // Root the new function while allocating filename data.
                    let root = self.api_owned(loader).map_err(execution::api_vm)?;
                    let data = self.new_string(filename)?;
                    let result = self.builtin_done(active, &[loader, data]);
                    drop(root);
                    return result;
                }
                return self.builtin_done(active, &values);
            }
            _ => return Err(VmError::Corrupt),
        }
        Ok(Poll::Continue)
    }
    fn hostload_failure(
        &mut self,
        active: Handle<ThreadObj>,
        message: Vec<u8>,
    ) -> Result<Poll, VmError> {
        let work = self.hostload_work(active)?;
        if work.kind == MODULE {
            let mut text = b"error loading module '".to_vec();
            text.extend_from_slice(&work.name);
            text.extend_from_slice(b"' from file '");
            text.extend_from_slice(&work.candidate);
            text.extend_from_slice(b"':\n\t");
            text.extend(message);
            return Ok(self.library_error(active, LuaFault::Require, text));
        }
        let dofile = work.kind == DOFILE;
        let message = self.new_string(message)?;
        if dofile {
            Ok(self.throw_on(active, LuaFault::Native, message))
        } else {
            self.builtin_done(active, &[Value::Nil, message])
        }
    }
}

#[cfg(test)]
#[allow(clippy::arc_with_non_send_sync)] // Public capability objects use Arc without Send/Sync.
mod tests {
    use super::*;
    use crate::hostcaps::testing::PendingHost;
    use crate::hostcaps::{Completion, Filesystem, MemoryFilesystem, MemoryOptions, OpenMode};
    use crate::{Host, HostCapabilities, HostRegistry, Libraries};
    use std::sync::Arc;

    fn registry() -> HostRegistry {
        let mut registry = HostRegistry::new();
        crate::register_standard(&mut registry);
        crate::register_debug(&mut registry);
        registry
    }
    fn runtime(source: &[u8], caps: HostCapabilities) -> Runtime {
        let mut r = Runtime::builder()
            .registry(registry())
            .libraries(
                Libraries::BASE
                    | Libraries::PACKAGE
                    | Libraries::COROUTINE
                    | Libraries::STRING
                    | Libraries::DEBUG
                    | Libraries::TABLE,
            )
            .capabilities(caps)
            .build()
            .unwrap();
        r.load_main(&crate::compile(source).unwrap()).unwrap();
        r
    }
    fn restore(r: &Runtime, caps: &HostCapabilities) -> Runtime {
        Runtime::restore(
            &r.snapshot().unwrap(),
            &Host::new(registry()).capabilities(caps.clone()),
        )
        .unwrap()
    }
    fn vfs(files: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>) -> Arc<MemoryFilesystem> {
        Arc::new(MemoryFilesystem::new(files, MemoryOptions::default()).unwrap())
    }
    fn drive(r: &mut Runtime, journal: &mut Journal, caps: &HostCapabilities) {
        for _ in 0..10000 {
            match r.run(1, journal).unwrap() {
                StepOutcome::Completed => return,
                StepOutcome::Paused(_) => *r = restore(r, caps),
                other => panic!("{other:?}: {:?}", r.lua_error()),
            }
        }
        panic!("did not finish");
    }
    #[test]
    fn review_discarded_search_candidates_release_their_charge() {
        let caps = HostCapabilities::sandbox().filesystem(vfs([]));
        let mut r = runtime(
            b"package.searchpath('x',string.rep('missing;',100))",
            caps.clone(),
        );
        let mut j = Journal::new();
        let mut checked = 0;
        let mut overhead = None;
        loop {
            let outcome = r.run(1, &mut j).unwrap();
            let active = r.heap.active.unwrap();
            if let Ok(work) = r.hostload_work(active)
                && work.phase == PROBE
                && !work.inserting
                && work.candidate.is_empty()
            {
                let thread = r.heap.threads.get(active).unwrap();
                let extra = thread.charged_held - thread.held_bytes();
                assert_eq!(*overhead.get_or_insert(extra), extra);
                checked += 1;
            }
            if outcome == StepOutcome::Completed {
                break;
            }
            assert!(matches!(outcome, StepOutcome::Paused(_)));
            r = restore(&r, &caps);
        }
        assert!(checked >= 100);
    }
    #[test]
    fn file_and_package_work_checkpoint_at_every_unit_without_live_handles() {
        let mut large = vec![b' '; 130000];
        large.extend_from_slice(b"return 41,nil,43,nil");
        let fs = vfs([
            (b"large".to_vec(), large),
            (b"m.lua".to_vec(), b"return 17".to_vec()),
        ]);
        let caps = HostCapabilities::sandbox().filesystem(fs.clone());
        let source = b"local f=assert(loadfile('large')); local a,b,c,d=f(); package.path='missing/?.lua;?.lua'; local m,p=require('m'); local n=select('#',dofile('large')); return a,b,c,d,m,p,n";
        let mut r = runtime(source, caps.clone());
        let mut j = Journal::new();
        drive(&mut r, &mut j, &caps);
        let got = r.entry_results().unwrap();
        assert_eq!(
            got[..5],
            [
                Value::Integer(41),
                Value::Nil,
                Value::Integer(43),
                Value::Nil,
                Value::Integer(17)
            ]
        );
        assert_eq!(got[6], Value::Integer(4));
        assert_eq!(fs.open_count(), 0);
        let mut straight = runtime(source, caps.clone());
        let mut fresh = Journal::new();
        assert_eq!(
            straight.run(10000, &mut fresh).unwrap(),
            StepOutcome::Completed
        );
        assert_eq!(r.fuel_consumed(), straight.fuel_consumed());
        assert_eq!(j.entries(), fresh.entries());
    }
    #[test]
    fn loadfile_committed_source_replays_after_external_mutation() {
        let fs = vfs([(b"f".to_vec(), b"return 11".to_vec())]);
        let caps = HostCapabilities::sandbox().filesystem(fs.clone());
        let mut r = runtime(b"return assert(loadfile('f'))()", caps.clone());
        let before = r.snapshot().unwrap();
        let mut j = Journal::new();
        loop {
            assert!(matches!(r.run(1, &mut j).unwrap(), StepOutcome::Paused(_)));
            if !j.entries().is_empty() {
                break;
            }
        }
        let committed = r.snapshot().unwrap();
        let Completion::Ready(Ok(id)) = fs.open(b"f", OpenMode::parse(b"w").unwrap()) else {
            panic!()
        };
        assert!(matches!(
            fs.write_at(id, 0, b"return 22"),
            Completion::Ready(Ok(9))
        ));
        assert!(matches!(fs.close(id), Completion::Ready(Ok(()))));
        for image in [&before, &committed] {
            let mut replay =
                Runtime::restore(image, &Host::new(registry()).capabilities(caps.clone())).unwrap();
            drive(&mut replay, &mut j, &caps);
            assert_eq!(replay.entry_results().unwrap(), [Value::Integer(11)]);
        }
        let mut new = runtime(b"return dofile('f')", caps);
        assert_eq!(
            new.run(1000, &mut Journal::new()).unwrap(),
            StepOutcome::Completed
        );
        assert_eq!(new.entry_results().unwrap(), [Value::Integer(22)]);
    }
    #[test]
    fn pending_loadfile_search_and_module_restore_before_and_after_completion() {
        for source in [
            b"return assert(loadfile('f'))()".as_slice(),
            b"package.path='?.lua'; return require('f')",
            b"return package.searchpath('f','missing;?')",
        ] {
            let pending = Arc::new(PendingHost::new());
            let caps = HostCapabilities::sandbox().filesystem(pending.clone());
            let mut r = runtime(source, caps.clone());
            let mut j = Journal::new();
            for _ in 0..20 {
                match r.run(1000, &mut j).unwrap() {
                    StepOutcome::Completed => break,
                    StepOutcome::Waiting(key) => {
                        let info = r.wait(key).unwrap();
                        let operation = info.operation;
                        let Value::String(bytes) = r
                            .heap
                            .threads
                            .get(r.heap.active.unwrap())
                            .unwrap()
                            .frames
                            .last()
                            .unwrap()
                            .wait_request()
                            .unwrap()
                            .payload[0]
                        else {
                            panic!()
                        };
                        let request =
                            CapabilityRequest::from_bytes(r.heap.string_bytes(bytes).unwrap())
                                .unwrap();
                        let count = pending.calls().len();
                        drop(info.payload);
                        r = restore(&r, &caps);
                        assert_eq!(r.run(1000, &mut j).unwrap(), StepOutcome::Waiting(key));
                        let value = match request {
                            CapabilityRequest::FilesystemProbeReadable { path } => {
                                CapabilityValue::Boolean(path != b"missing")
                            }
                            CapabilityRequest::FilesystemReadFileRange { .. } => {
                                CapabilityValue::Bytes(b"return 29".to_vec())
                            }
                            _ => panic!("{operation}"),
                        };
                        r.complete_capability(key, Ok(value)).unwrap();
                        r = restore(&r, &caps);
                        assert_eq!(pending.calls().len(), count);
                    }
                    other => panic!("{other:?}: {:?}", r.lua_error()),
                }
            }
            assert_eq!(r.run(1000, &mut j).unwrap(), StepOutcome::Completed);
            assert_eq!(pending.calls().len(), j.entries().len());
            if source.starts_with(b"return assert") {
                assert_eq!(r.entry_results().unwrap(), [Value::Integer(29)]);
            }
        }
    }
    #[test]
    fn dofile_yield_and_native_wait_preserve_all_results_and_hooks() {
        let fs = vfs([(
            b"f".to_vec(),
            b"local x=coroutine.yield('pause'); return x,nil,3,nil".to_vec(),
        )]);
        let caps = HostCapabilities::sandbox().filesystem(fs);
        let source = b"local calls,returns=0,0; local hook=function(e) local i=debug.getinfo(2,'f'); if i.func==dofile then if e=='call' then calls=calls+1 elseif e=='return' then returns=returns+1 end end end; local co=coroutine.create(function() return dofile('f') end); debug.sethook(co,hook,'cr'); local a,b=coroutine.resume(co); local c,d,e,f,g=coroutine.resume(co,8); debug.sethook(); return a,b,c,d,e,f,g,calls,returns";
        let mut r = runtime(source, caps.clone());
        let mut j = Journal::new();
        drive(&mut r, &mut j, &caps);
        let got = r.entry_results().unwrap();
        assert_eq!(got[0], Value::Bool(true));
        assert_eq!(
            got[2..],
            [
                Value::Bool(true),
                Value::Integer(8),
                Value::Nil,
                Value::Integer(3),
                Value::Nil,
                Value::Integer(1),
                Value::Integer(1)
            ]
        );
        let fs = vfs([(b"f".to_vec(), b"return wait(),nil,7,nil".to_vec())]);
        let caps = HostCapabilities::sandbox().filesystem(fs);
        let mut reg = registry();
        reg.function("wait", crate::NativePolicy::VmLocal, |_| {
            Ok(crate::NativeReturn::Wait(crate::WaitRequest {
                operation: "chunk".into(),
                payload: crate::MultiValue::default(),
            }))
        });
        let mut r = Runtime::builder()
            .registry(reg.clone())
            .libraries(Libraries::BASE)
            .capabilities(caps.clone())
            .build()
            .unwrap();
        r.set_global_native("wait", "wait").unwrap();
        r.load_main(&crate::compile(b"return dofile('f')").unwrap())
            .unwrap();
        let mut j = Journal::new();
        let StepOutcome::Waiting(key) = r.run(1000, &mut j).unwrap() else {
            panic!()
        };
        r = Runtime::restore(
            &r.snapshot().unwrap(),
            &Host::new(reg.clone()).capabilities(caps.clone()),
        )
        .unwrap();
        r.complete(
            key,
            crate::Completion::Return(vec![crate::api::Value::Integer(5)]),
        )
        .unwrap();
        r = Runtime::restore(&r.snapshot().unwrap(), &Host::new(reg).capabilities(caps)).unwrap();
        assert_eq!(r.run(1000, &mut j).unwrap(), StepOutcome::Completed);
        assert_eq!(
            r.entry_results().unwrap(),
            [Value::Integer(5), Value::Nil, Value::Integer(7), Value::Nil]
        );
    }
    #[test]
    fn stdin_short_reads_continue_until_empty_and_checkpoint_between_parts() {
        struct ShortInput(std::cell::Cell<usize>);
        impl crate::Stdio for ShortInput {
            fn read_stdin(&self, _max: usize) -> Completion<Vec<u8>> {
                let parts = [b"return ".as_slice(), b"31,nil", b",33,nil", b""];
                let n = self.0.get();
                self.0.set(n + 1);
                Completion::Ready(Ok(parts[n.min(3)].to_vec()))
            }
        }
        let caps = HostCapabilities::sandbox().stdio(Arc::new(ShortInput(std::cell::Cell::new(0))));
        let mut r = runtime(b"return dofile()", caps.clone());
        let mut j = Journal::new();
        drive(&mut r, &mut j, &caps);
        assert_eq!(
            r.entry_results().unwrap(),
            [
                Value::Integer(31),
                Value::Nil,
                Value::Integer(33),
                Value::Nil
            ]
        );
        assert_eq!(j.entries().len(), 4);
    }
    #[test]
    fn binary_files_use_the_shared_loader_and_explicit_nil_environment() {
        let chunk = crate::compile(b"return 23,'binary',nil").unwrap();
        let binary = crate::chunk::dump(&chunk.proto, false, 1 << 20).unwrap();
        let fs = vfs([(b"f".to_vec(), binary)]);
        let caps = HostCapabilities::sandbox().filesystem(fs);
        let source = b"local f=assert(loadfile('f','b',nil)); local a,e=loadfile('f','t'); assert(a==nil and e:find('binary',1,true)); return select('#',dofile('f')),f()";
        let mut r = runtime(source, caps.clone());
        drive(&mut r, &mut Journal::new(), &caps);
        let got = r.entry_results().unwrap();
        assert_eq!(got[0..2], [Value::Integer(3), Value::Integer(23)]);
        assert_eq!(got[3], Value::Nil);
    }
    #[test]
    fn search_substitution_metatable_path_and_restore_validation() {
        let fs = vfs([
            (b"long".repeat(800), b"return 37".to_vec()),
            (b"f.lua".to_vec(), b"return 19".to_vec()),
        ]);
        let caps = HostCapabilities::sandbox().filesystem(fs);
        let source = b"package.path=nil; setmetatable(package,{__index=function(_,k) if k=='path' then return '?.lua' end end}); local n,p=require('f'); return n,p,package.searchpath(string.rep('long',800),'?')";
        let mut r = runtime(source, caps.clone());
        drive(&mut r, &mut Journal::new(), &caps);
        assert_eq!(r.entry_results().unwrap()[0], Value::Integer(19));
        assert!(matches!(r.entry_results().unwrap()[2], Value::String(_)));
        let mut work = HostLoad {
            kind: MODULE,
            phase: PROBE,
            ..HostLoad::default()
        };
        work.cursor = 2;
        let mut bytes = Vec::new();
        work.encode(&mut bytes);
        assert_eq!(
            HostLoad::decode(&mut bytes.as_slice()),
            Err(SnapshotError::InvalidStructure)
        );
    }
}
