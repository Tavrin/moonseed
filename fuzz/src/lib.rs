//! Shared harness for Moonseed's libFuzzer targets.
//!
//! Every target runs under a capped [`Config`]: bounded fuel, logical heap,
//! objects, string length and stack. A target only fails on a panic, an
//! abort, a timeout, or real memory past libFuzzer's RSS limit; Lua errors,
//! compile errors and refused snapshots are expected outcomes.
//!
//! Library targets run a fixed Lua driver over `ARGS`, a table of byte-string
//! fields decoded from the input by [`fields`]: each field but the last is a
//! length byte and that many bytes; the last field is the rest of the input.
#![allow(clippy::arc_with_non_send_sync)] // Capability traits do not require Send/Sync.

use moonseed::{
    CompileLimits, Config, GcMode, Host, HostCapabilities, HostIoError, HostIoErrorKind, Journal,
    Libraries, MemoryFilesystem, MemoryOptions, Runtime, StepOutcome,
};
use std::sync::Arc;

/// Instructions per `run` call.
pub const QUANTUM: u64 = 20_000;

/// The capped configuration every target and every seed runtime uses.
pub fn config(fuel: u64, gc_mode: GcMode) -> Config {
    Config {
        fuel_limit: Some(fuel),
        max_objects: 200_000,
        max_logical_heap: 32 << 20,
        max_stack_slots: 20_000,
        max_string_bytes: 1 << 20,
        max_snapshot_bytes: 8 << 20,
        gc_mode,
        ..Config::default()
    }
}

/// Compiler limits for fuzzed source.
pub fn compile_limits() -> CompileLimits {
    CompileLimits {
        max_instructions: 1 << 16,
        max_constants: 1 << 14,
        max_functions: 1 << 12,
        max_source_bytes: 1 << 18,
    }
}

/// The in-memory filesystem every target sees: small fixed files.
pub fn filesystem() -> Arc<MemoryFilesystem> {
    Arc::new(
        MemoryFilesystem::new(
            [
                (b"data".to_vec(), b"one\ntwo\n0x10 3.5e2 -7\nlast".to_vec()),
                (b"lines".to_vec(), b"abc\ndef\r\n\nghi".to_vec()),
                (b"module.lua".to_vec(), b"return ...".to_vec()),
                (b"a/b/init.lua".to_vec(), b"return 'init'".to_vec()),
                (b"bad.lua".to_vec(), b"return +".to_vec()),
            ],
            MemoryOptions {
                max_open_files: 16,
                max_bytes_per_op: 64 * 1024,
                read_only: false,
                max_file_bytes: 1 << 20,
            },
        )
        .expect("filesystem"),
    )
}

/// Capabilities: the shared VFS and nothing else.
pub fn capabilities(fs: Arc<MemoryFilesystem>) -> HostCapabilities {
    HostCapabilities::sandbox().filesystem(fs)
}

/// A runtime with every library, the VFS, and the capped config.
pub fn runtime(fuel: u64, gc_mode: GcMode, fs: Arc<MemoryFilesystem>) -> Runtime {
    Runtime::builder()
        .config(config(fuel, gc_mode))
        .libraries(Libraries::ALL)
        .capabilities(capabilities(fs))
        .package_paths(b"?.lua;?/init.lua", b"")
        .build()
        .expect("runtime")
}

thread_local! {
    static REGISTRY: moonseed::HostRegistry =
        runtime(1, GcMode::Generational, filesystem()).registry().clone();
    static PENDING_REGISTRY: moonseed::HostRegistry = pending_runtime(1).registry().clone();
}

/// The restore host matching [`runtime`]'s registrations.
pub fn host(fs: Arc<MemoryFilesystem>) -> Host {
    Host::new(REGISTRY.with(Clone::clone))
        .limits(config(1, GcMode::Generational).limits())
        .capabilities(capabilities(fs))
}

/// Drive until the program stops or `steps` quanta pass. A capability wait
/// is completed with a host error (always type-correct), at most a few times.
pub fn drive(rt: &mut Runtime, steps: u32) -> Option<StepOutcome> {
    let mut journal = Journal::new();
    let mut last = None;
    let mut completions = 0;
    for _ in 0..steps {
        let outcome = rt.run(QUANTUM, &mut journal).ok()?;
        match &outcome {
            StepOutcome::Paused(_) => {}
            StepOutcome::Waiting(key) if completions < 4 => {
                completions += 1;
                let error = HostIoError::new(HostIoErrorKind::Other, b"fuzz".to_vec());
                if rt.complete_capability(*key, Err(error)).is_err() {
                    return Some(outcome);
                }
            }
            _ => return Some(outcome),
        }
        last = Some(outcome);
    }
    last
}

/// Split an input into at most `max` fields (see the crate docs).
pub fn fields(data: &[u8], max: usize) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut rest = data;
    while out.len() + 1 < max && !rest.is_empty() {
        let len = usize::from(rest[0]).min(rest.len() - 1);
        out.push(&rest[1..1 + len]);
        rest = &rest[1 + len..];
    }
    out.push(rest);
    out
}

/// Encode fields so that [`fields`] reads them back; earlier fields are
/// truncated to 255 bytes.
pub fn encode_fields(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        if i + 1 == parts.len() {
            out.extend_from_slice(part);
        } else {
            let part = &part[..part.len().min(255)];
            out.push(part.len() as u8);
            out.extend_from_slice(part);
        }
    }
    out
}

/// Compile and run `source` with the byte-string `args` as global `ARGS`
/// and the whole input as global `INPUT`.
pub fn run_driver(source: &[u8], args: &[&[u8]], input: &[u8], fuel: u64) {
    let mut rt = runtime(fuel, GcMode::Generational, filesystem());
    let Ok(table) = rt.create_table() else { return };
    for (i, arg) in args.iter().enumerate() {
        if table.raw_set(&mut rt, i as i64 + 1, *arg).is_err() {
            return;
        }
    }
    let globals = rt.globals();
    if globals.raw_set(&mut rt, "ARGS", table).is_err()
        || globals.raw_set(&mut rt, "INPUT", input).is_err()
    {
        return;
    }
    let chunk = moonseed::compile(source).expect("driver compiles");
    rt.load_main(&chunk).expect("driver loads");
    drive(&mut rt, (fuel / QUANTUM) as u32 + 2);
}

/// Restore `bytes`, run a little, checkpoint and restore again.
pub fn restore_round_trip(bytes: &[u8], collect: bool) {
    let fs = filesystem();
    let host = host(fs);
    let Ok(mut rt) = Runtime::restore(bytes, &host) else {
        return;
    };
    if collect {
        rt.collect();
    }
    drive(&mut rt, if collect { 3 } else { 10 });
    if let Ok(again) = rt.snapshot()
        && let Ok(mut rt) = Runtime::restore(&again, &host)
    {
        drive(&mut rt, 2);
    }
}

/// The pattern driver: `ARGS = {sel, pattern, subject, replacement}`.
pub const PATTERN_DRIVER: &[u8] = br#"
local A = ARGS
local sel = (A[1] or ""):byte(1) or 0
local p, s, r = A[2] or "", A[3] or "", A[4] or "%0"
local plain = (sel // 4) % 2 == 1
local init = (sel // 8) - 16
if init == 0 then init = nil end
pcall(string.find, s, p, init, plain)
pcall(string.match, s, p, init)
pcall(string.gsub, s, p, r)
pcall(string.gsub, s, p, r, sel % 4)
pcall(string.gsub, s, p, {a = 1, [1] = "x", x = false})
pcall(string.gsub, s, p, function(...) return select('#', ...) > 1 and "" or nil end)
pcall(function()
  local n = 0
  for a, b in string.gmatch(s, p, init) do n = n + 1; if n > 200 then break end end
end)
"#;

/// The utf8 driver: `ARGS = {sel, subject}`; `sel` bytes are small signed ints.
pub const UTF8_DRIVER: &[u8] = br#"
local A = ARGS
local sel, s = A[1] or "", A[2] or ""
local function sb(k) local b = sel:byte(k) or 0; if b >= 128 then b = b - 256 end; return b end
local i, j, n, lax = sb(1), sb(2), sb(3), (sel:byte(4) or 0) % 2 == 1
pcall(utf8.len, s, i == 0 and 1 or i, j == 0 and -1 or j, lax)
pcall(utf8.codepoint, s, i == 0 and 1 or i, j == 0 and nil or j, lax)
pcall(utf8.offset, s, n, i == 0 and nil or i)
pcall(function() local c = 0 for _, _ in utf8.codes(s, lax) do c = c + 1 end end)
pcall(function()
  local cps = {}
  for k = 1, #s - 3, 4 do
    local v = string.unpack("<i4", s, k)
    if sb(5) % 2 == 0 then v = v & 0x7FFFFFFF end
    cps[#cps + 1] = v
  end
  local u = utf8.char(table.unpack(cps))
  utf8.len(u, 1, -1, lax)
end)
pcall(function() local c = 0 for _ in s:gmatch(utf8.charpattern) do c = c + 1 end end)
"#;

/// The pack driver: `ARGS = {fmt, data, v1, ...}`.
pub const PACK_DRIVER: &[u8] = br#"
local A = ARGS
local fmt, data = A[1] or "", A[2] or ""
pcall(string.packsize, fmt)
pcall(string.unpack, fmt, data)
pcall(string.unpack, fmt, data, -#data)
local vals = {}
for k = 3, #A do
  local v = A[k]
  local num = tonumber(v)
  vals[#vals + 1] = (num and (math.tointeger(num) or num)) or v
end
local ok, packed = pcall(string.pack, fmt, table.unpack(vals))
if ok then pcall(string.unpack, fmt, packed) end
"#;

/// The io driver: `ARGS = {mode, ops, f1, ...}` over the VFS.
pub const IO_DRIVER: &[u8] = br#"
local A = ARGS
local mode, ops = A[1] or "r", A[2] or ""
local F = {}
for k = 3, #A do F[#F + 1] = A[k] end
if #F == 0 then F[1] = "l" end
local fi = 0
local function nextf() fi = fi % #F + 1; return F[fi] end
local function arg() local v = nextf(); return math.tointeger(tonumber(v)) or v end
local ok, f = pcall(io.open, "data", mode)
if not ok or not f then f = io.open("data", "r") end
for k = 1, math.min(#ops, 64) do
  local op = ops:byte(k) % 12
  if op == 0 then pcall(f.read, f, arg())
  elseif op == 1 then pcall(f.read, f, arg(), arg())
  elseif op == 2 then pcall(f.seek, f, nextf(), math.tointeger(tonumber(nextf())))
  elseif op == 3 then pcall(f.write, f, nextf())
  elseif op == 4 then pcall(f.setvbuf, f, nextf(), math.tointeger(tonumber(nextf())))
  elseif op == 5 then
    pcall(function() local it = f:lines(arg()); for _ = 1, 4 do if it() == nil then break end end end)
  elseif op == 6 then pcall(f.flush, f)
  elseif op == 7 then
    pcall(f.close, f)
    local ok, g = pcall(io.open, nextf(), mode)
    f = (ok and g) or io.open("data", "r+") or f
  elseif op == 8 then
    pcall(function() for l in io.lines("lines", arg()) do end end)
  elseif op == 9 then pcall(io.read, arg())
  elseif op == 10 then pcall(function() local g = io.open(nextf(), nextf()); if g then g:close() end end)
  else pcall(io.type, f); pcall(tostring, f); pcall(io.output, f); pcall(io.write, nextf())
  end
end
pcall(f.close, f)
"#;

/// The searchpath driver: `ARGS = {name, path, sep, rep, sel}`.
pub const SEARCHPATH_DRIVER: &[u8] = br#"
local A = ARGS
local name, path, sep, rep = A[1] or "", A[2] or "", A[3] or ".", A[4] or "/"
local b = (A[5] or ""):byte(1) or 0
pcall(package.searchpath, name, path, b % 2 == 0 and sep or nil, b % 4 < 2 and rep or nil)
package.path = path
pcall(require, name)
"#;

/// The diagnostic driver: `ARGS = {chunkname, id1, id2, sel, source}`.
pub const DIAG_DRIVER: &[u8] = br#"
local A = ARGS
local cn, a, b, sel, src = A[1] or "", A[2] or "", A[3] or "", (A[4] or ""):byte(1) or 0, A[5] or ""
local function ident(x)
  x = x:gsub("[^%w_]", "_")
  if x == "" or x:find("^%d") then x = "_" .. x end
  return x
end
local ia, ib = ident(a), ident(b)
local T = {
  "return %s.%s", "%s.%s()", "local %s = {} return %s:%s()", "return %s + %s",
  "return #%s .. %s", "%s.%s = 1", "local %s <const> = 1; %s = %s", "return %s[%s]()",
  "local t = {} return t.%s.%s", "goto %s", "return %s < %s", "for %s = 1, %s do end",
  "local %s <close> = %s", "return %s & %s", "return -%s", "return ('x'):%s(%s)",
}
local code = string.format(T[sel % #T + 1], ia, ib, ia, ib)
local function try(text)
  local f, err = load(text, cn)
  if f then
    pcall(f)
    xpcall(f, debug.traceback)
    pcall(string.dump, f, sel % 2 == 0)
  else
    pcall(string.format, "%q", err)
  end
end
try(code)
try(src)
pcall(error, a, sel % 4)
pcall(error, setmetatable({}, {__tostring = function() return b end}))
xpcall(error, debug.traceback, a)
pcall(debug.traceback, a, sel % 5)
"#;

/// A table-operation program from op bytes (the `table_ops` target).
pub fn table_program(data: &[u8]) -> Vec<u8> {
    const VALUES: [&str; 16] = [
        "nil",
        "0",
        "1",
        "-1",
        "2",
        "3",
        "7",
        "0.5",
        "-0.0",
        "1e300",
        "0/0",
        "math.maxinteger",
        "math.mininteger",
        "'k'",
        "true",
        "2^53",
    ];
    const TABLES: [&str; 3] = ["t", "u", "w"];
    let mut out = String::from(
        "local t, u = {1, 2, 3, nil, 5}, setmetatable({}, {__index = function(_, k) return k end,\n\
         __len = function() return 4 end})\n\
         local w = setmetatable({}, {__mode = 'k'})\n\
         local cmp = {function(a, b) return a < b end, function(a, b) return true end,\n\
         function(a, b) return tostring(a) > tostring(b) end}\n\
         local function v(x) return x end\n",
    );
    let mut bytes = data.iter().copied();
    let mut next = || bytes.next();
    while let Some(op) = next() {
        let a = VALUES[usize::from(next().unwrap_or(0) % 16)];
        let b = VALUES[usize::from(next().unwrap_or(1) % 16)];
        let t = TABLES[usize::from(op >> 5) % 3];
        let line = match op % 20 {
            0 => format!("{t}[{a}] = {b}"),
            1 => format!("table.insert({t}, {a})"),
            2 => format!("table.insert({t}, {a}, {b})"),
            3 => format!("table.remove({t}, {a})"),
            4 => format!("table.remove({t})"),
            5 => format!("local _ = #{t}"),
            6 => format!("table.concat({t}, ',', {a}, {b})"),
            7 => format!("table.unpack({t}, {a}, {b})"),
            8 => format!("table.move({t}, {a}, {b}, 1, u)"),
            9 => format!("table.move(t, 1, #{t}, {a})"),
            10 => format!("table.sort({t}, cmp[{}])", usize::from(op >> 5) % 3 + 1),
            11 => format!("table.sort({t})"),
            12 => format!("for k in pairs({t}) do {t}[k] = nil end"),
            13 => format!("for k, x in pairs({t}) do {t}[k] = {b} end"),
            14 => format!("for i, x in ipairs({t}) do if i > 50 then break end end"),
            15 => format!("{t} = table.pack(table.unpack({t}, 1, 8))"),
            16 => format!("setmetatable({t}, {{__newindex = rawset, __index = {t}}})"),
            17 => "collectgarbage('step', 1)".to_string(),
            18 => format!("next({t}, {a})"),
            _ => format!("for i = 1, 40 do {t}[i * {a}] = i end"),
        };
        out.push_str("pcall(function() ");
        out.push_str(&line);
        out.push_str(" end)\n");
    }
    out.into_bytes()
}

/// The host-capability setup program: opens every handle first and never
/// closes one, so a filesystem that ran it to the end holds every resource
/// a checkpoint of any of its steps names.
pub const HOSTCAP_SETUP: &[u8] = br#"
H1 = assert(io.open('data', 'r+'))
H2 = assert(io.open('lines', 'r'))
H3 = assert(io.open('out', 'w+'))
H4 = assert(io.open('app', 'a+'))
H3:setvbuf('full', 64); H3:write('pending bytes')
H4:setvbuf('line'); H4:write('x\ny')
IT = H2:lines('L')
for i = 1, 3 do H1:read('l'); H1:seek('cur', 1); H3:write(i, '\n'); H4:write(i) end
local s = 0
for i = 1, 3000 do
  s = s + i
  if i % 150 == 0 then
    H1:seek('set', i % 7); H1:read(3, 'n'); H3:write(s); H4:write(i, '\n'); IT()
    H3:seek('cur', -2); H3:read('a')
  end
end
io.output(H3); io.input(H2); io.write('tail'); io.read('l')
DONE = s
"#;

/// A filesystem that ran [`HOSTCAP_SETUP`] to the end, and its runtime's
/// final state (for the seed exporter).
pub fn hostcap_filesystem() -> Arc<MemoryFilesystem> {
    let fs = filesystem();
    let mut rt = runtime(10_000_000, GcMode::Incremental, fs.clone());
    let chunk = moonseed::compile(HOSTCAP_SETUP).expect("setup compiles");
    rt.load_main(&chunk).expect("setup loads");
    drive(&mut rt, 1000);
    fs
}

/// Capabilities that answer every operation with Pending.
pub fn pending_capabilities() -> HostCapabilities {
    let p = moonseed::hostcaps::testing::PendingHost::new();
    HostCapabilities::sandbox()
        .filesystem(Arc::new(p.clone()))
        .clock(Arc::new(p.clone()))
        .civil(Arc::new(p.clone()))
        .environment(Arc::new(p.clone()))
        .process(Arc::new(p.clone()))
        .stdio(Arc::new(p))
}

/// A runtime whose every capability is pending.
pub fn pending_runtime(fuel: u64) -> Runtime {
    Runtime::builder()
        .config(config(fuel, GcMode::Generational))
        .libraries(Libraries::ALL)
        .capabilities(pending_capabilities())
        .package_paths(b"?.lua", b"")
        .build()
        .expect("runtime")
}

/// The restore host matching [`pending_runtime`].
pub fn pending_host() -> Host {
    Host::new(PENDING_REGISTRY.with(Clone::clone))
        .limits(config(1, GcMode::Generational).limits())
        .capabilities(pending_capabilities())
}
