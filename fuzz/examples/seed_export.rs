//! Seed exporter for the fuzz corpora.
//!
//! `cargo run --example seed_export -- <repo root> <suite dir or -> <out dir> [extra dir]`
//! writes `<out>/<target>/<hash>` seeds from the repository's fixtures, the
//! official Lua 5.4.9 suite when present, binary chunks dumped from those
//! sources, real snapshots (mid-run, mid-collection, holding VFS handles, and
//! waiting on pending capabilities), and library calls recorded while the
//! fixtures and suite files run under an instrumented prelude.
#![allow(clippy::arc_with_non_send_sync)]

use moonseed::{Config, GcMode, Journal, Libraries, Runtime, StepOutcome, Table};
use moonseed_fuzz::{encode_fields, filesystem, runtime};
use std::collections::{BTreeMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

#[derive(Default)]
struct Corpus {
    seeds: BTreeMap<&'static str, Vec<Vec<u8>>>,
    seen: HashSet<(&'static str, Vec<u8>)>,
}

impl Corpus {
    fn add(&mut self, target: &'static str, bytes: Vec<u8>) {
        if self.seen.insert((target, bytes.clone())) {
            self.seeds.entry(target).or_default().push(bytes);
        }
    }
    fn write(&self, out: &Path) {
        for (target, seeds) in &self.seeds {
            let dir = out.join(target);
            std::fs::create_dir_all(&dir).unwrap();
            for seed in seeds {
                let mut h = DefaultHasher::new();
                seed.hash(&mut h);
                std::fs::write(dir.join(format!("{:016x}", h.finish())), seed).unwrap();
            }
            println!("{target}: {} seeds", seeds.len());
        }
    }
}

fn lua_files(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            if recursive {
                lua_files(&path, true, out);
            }
        } else if path.extension().is_some_and(|e| e == "lua") {
            out.push(path);
        }
    }
}

fn set_global(rt: &mut Runtime, name: &str, bytes: &[u8]) {
    let globals = rt.globals();
    globals.raw_set(rt, name, bytes).unwrap();
}

fn strings(rt: &mut Runtime, table: &Table) -> Vec<Vec<u8>> {
    let n = table.raw_len(rt).unwrap_or(0);
    (1..=n)
        .filter_map(|i| table.raw_get::<_, Option<Vec<u8>>>(rt, i).ok().flatten())
        .collect()
}

const DUMPER: &[u8] = br#"
local f = load(SRC, "=seed")
if f then DUMPS = {string.dump(f), string.dump(f, true)} end
"#;

/// Wraps library entry points to record their arguments as target inputs.
const RECORDER: &[u8] = br#"
__REC = {pattern = {}, utf8 = {}, pack = {}, io = {}, searchpath = {}}
local tostring, select, pcall, type, mtype = tostring, select, pcall, type, math.type
local char, sub, concat = string.char, string.sub, table.concat
local function enc(...)
  local n, out = select('#', ...), {}
  for k = 1, n do
    local v = select(k, ...)
    v = v == nil and "" or tostring(v)
    if k < n then v = sub(v, 1, 255); out[#out + 1] = char(#v) .. v else out[#out + 1] = v end
  end
  return concat(out)
end
local function rec(tag, f)
  local t = __REC[tag]
  if #t < 4000 then local ok, s = pcall(f); if ok then t[#t + 1] = s end end
end
local function small(i) return mtype(i) == 'integer' and i >= -16 and i < 16 and i ~= 0 end
local function ibyte(i) if mtype(i) ~= 'integer' then return 0 end; return i & 0xFF end
for _, name in ipairs{'find', 'match', 'gsub', 'gmatch'} do
  local f = string[name]
  string[name] = function(s, p, a3, a4, ...)
    rec('pattern', function()
      local sel = small(a3) and (a3 + 16) * 8 or 0
      if name == 'find' and a4 then sel = sel + 4 end
      local repl = (name == 'gsub' and type(a3) == 'string') and a3 or "%0"
      return enc(char(sel), p, s, repl)
    end)
    return f(s, p, a3, a4, ...)
  end
end
for _, name in ipairs{'len', 'codepoint', 'offset', 'codes', 'char'} do
  local f = utf8[name]
  utf8[name] = function(...)
    local a, b, c, d = ...
    local all = {...}
    rec('utf8', function()
      if name == 'char' then
        local t = all
        for k = 1, #t do t[k] = string.pack('<i4', t[k]) end
        return enc(char(0, 0, 0, 0, 1), concat(t))
      elseif name == 'offset' then
        return enc(char(ibyte(c), 0, ibyte(b), 0), a)
      end
      return enc(char(ibyte(b), ibyte(c), 0, (c == true or d == true) and 1 or 0), a)
    end)
    return f(...)
  end
end
do
  local pack, unpack, packsize = string.pack, string.unpack, string.packsize
  string.pack = function(fmt, ...)
    local args, n = {...}, select('#', ...)
    rec('pack', function() return enc(fmt, "", table.unpack(args, 1, n)) end)
    return pack(fmt, ...)
  end
  string.unpack = function(fmt, s, ...) rec('pack', function() return enc(fmt, s) end); return unpack(fmt, s, ...) end
  string.packsize = function(fmt) rec('pack', function() return enc(fmt, "") end); return packsize(fmt) end
end
do
  local open, wrapped, mode = io.open, false, "r"
  local function wrap(m, name, op)
    local f = m[name]
    m[name] = function(self, ...)
      local args = {...}
      local n = select('#', ...)
      rec('io', function() return enc(mode, op, table.unpack(args, 1, n)) end)
      return f(self, ...)
    end
  end
  io.open = function(name, m, ...)
    mode = m or "r"
    rec('io', function() return enc(mode, "\0\1\2\3\4\5\6\7\8\9\10\11", "l", "set", "2", "full", "8") end)
    local f, e, c = open(name, m, ...)
    if f and not wrapped then
      wrapped = true
      local index = getmetatable(f).__index
      wrap(index, 'read', "\0\1")
      wrap(index, 'seek', "\2")
      wrap(index, 'write', "\3")
      wrap(index, 'setvbuf', "\4")
      wrap(index, 'lines', "\5")
    end
    return f, e, c
  end
end
do
  local sp = package.searchpath
  package.searchpath = function(name, path, sep, rep)
    rec('searchpath', function()
      local sel = (sep == nil and 1 or 0) + (rep == nil and 2 or 0)
      return enc(name, path, sep or ".", rep or "/", char(sel))
    end)
    return sp(name, path, sep, rep)
  end
end
"#;

fn run_to_end(rt: &mut Runtime, steps: u32) {
    let mut journal = Journal::new();
    for _ in 0..steps {
        match rt.run(moonseed_fuzz::QUANTUM, &mut journal) {
            Ok(StepOutcome::Paused(_)) => {}
            other => {
                if std::env::var_os("SEED_DEBUG").is_some() {
                    eprintln!("{other:?}");
                }
                break;
            }
        }
    }
}

/// Snapshots of `source` at chosen steps of `quantum` instructions.
fn snapshots(config: Config, source: &[u8], quantum: u64, picks: &[u32]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let Ok(chunk) = moonseed::compile_with_limits(source, &moonseed_fuzz::compile_limits()) else {
        return out;
    };
    let mut rt = Runtime::builder()
        .config(config)
        .libraries(Libraries::ALL)
        .capabilities(moonseed_fuzz::capabilities(filesystem()))
        .package_paths(b"?.lua;?/init.lua", b"")
        .build()
        .unwrap();
    if rt.load_main(&chunk).is_err() {
        return out;
    }
    let mut journal = Journal::new();
    let last = picks.iter().copied().max().unwrap_or(0);
    for step in 0..=last {
        let outcome = rt.run(quantum, &mut journal);
        if (picks.contains(&step) || !matches!(outcome, Ok(StepOutcome::Paused(_))))
            && let Ok(bytes) = rt.snapshot()
        {
            out.push(bytes);
        }
        if !matches!(outcome, Ok(StepOutcome::Paused(_))) {
            break;
        }
    }
    out
}

const GC_SOURCE: &[u8] = br#"
local weak = setmetatable({}, {__mode = 'k'})
local vals = setmetatable({}, {__mode = 'v'})
local keep, finalized = {}, 0
for i = 1, 4000 do
  local t = {i, tostring(i), {i}}
  weak[t] = i; vals[i] = t
  if i % 3 == 0 then keep[#keep + 1] = t end
  if i % 50 == 0 then setmetatable({}, {__gc = function() finalized = finalized + 1 end}) end
  if i % 400 == 0 then keep = {}; collectgarbage(i % 800 == 0 and 'incremental' or 'generational') end
  local co = coroutine.wrap(function(x) coroutine.yield(x .. "y") end); co(tostring(i))
end
collectgarbage()
return finalized
"#;

const PENDING_PROGRAMS: [&[u8]; 8] = [
    b"local f = io.open('x', 'r'); return f",
    b"return os.time()",
    b"return os.getenv('HOME')",
    b"return os.execute('cmd')",
    b"return os.remove('x'), os.rename('a', 'b')",
    b"return io.read('l')",
    b"return os.clock(), os.date('%c')",
    b"for l in io.lines('x') do end",
];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(&args[1]);
    let suite = PathBuf::from(&args[2]);
    let out = PathBuf::from(&args[3]);
    let mut corpus = Corpus::default();

    let mut fixtures = Vec::new();
    lua_files(&root.join("tests/fixtures"), false, &mut fixtures);
    lua_files(&root.join("tests/hostlib/fixtures"), true, &mut fixtures);
    lua_files(&root.join("crates/moonseed/examples"), true, &mut fixtures);
    let mut suite_files = Vec::new();
    lua_files(&suite, false, &mut suite_files);
    // Optional: generated case sources (e.g. tools/hostlib_generate.py cases).
    if let Some(extra) = args.get(4) {
        lua_files(Path::new(extra), false, &mut suite_files);
    }
    let read = |p: &PathBuf| std::fs::read(p).unwrap();
    let fixture_sources: Vec<Vec<u8>> = fixtures.iter().map(read).collect();
    let suite_sources: Vec<Vec<u8>> = suite_files.iter().map(read).collect();

    // Source targets.
    for src in fixture_sources.iter().chain(&suite_sources) {
        corpus.add("compile", src.clone());
        corpus.add("compile_run", src.clone());
    }
    let names: [&[u8]; 5] = [b"@fixture.lua", b"=stdin", b"chunk\nname", b"", b"=[C]"];
    for (i, src) in fixture_sources.iter().enumerate() {
        let sel = [i as u8];
        let name = names[i % names.len()];
        corpus.add("diag", encode_fields(&[name, b"field", b"x1", &sel, src]));
    }

    // Binary chunks.
    for src in fixture_sources.iter().chain(&suite_sources) {
        let mut rt = runtime(5_000_000, GcMode::Generational, filesystem());
        set_global(&mut rt, "SRC", src);
        rt.load_main(&moonseed::compile(DUMPER).unwrap()).unwrap();
        run_to_end(&mut rt, 300);
        let globals = rt.globals();
        if let Ok(Some(dumps)) = globals.raw_get::<_, Option<Table>>(&mut rt, "DUMPS") {
            for dump in strings(&mut rt, &dumps) {
                if dump.len() <= 64 * 1024 {
                    corpus.add("chunk_load", dump);
                }
            }
        }
    }

    // Snapshots: mid-run states of every fixture.
    for src in &fixture_sources {
        let config = moonseed_fuzz::config(5_000_000, GcMode::Generational);
        for snap in snapshots(config, src, 200, &[0, 2, 9, 40]) {
            corpus.add("snapshot_restore", snap);
        }
    }
    // Mid-collection states: small debt, both collector modes.
    for (mode, quantum) in [(GcMode::Incremental, 97), (GcMode::Generational, 131)] {
        let config = Config {
            gc_min_debt: 256,
            ..moonseed_fuzz::config(50_000_000, mode)
        };
        let picks: Vec<u32> = (0..2000).step_by(37).collect();
        for snap in snapshots(config.clone(), GC_SOURCE, quantum, &picks) {
            corpus.add("gc_restore", snap);
        }
        for src in fixture_sources
            .iter()
            .filter(|s| s.windows(14).any(|w| w == b"collectgarbage"))
        {
            for snap in snapshots(config.clone(), src, quantum, &[3, 17, 60]) {
                corpus.add("gc_restore", snap);
            }
        }
    }

    // Host capability state: VFS handles at every step of the setup program.
    {
        let fs = filesystem();
        let mut rt = runtime(10_000_000, GcMode::Incremental, fs);
        let chunk = moonseed::compile(moonseed_fuzz::HOSTCAP_SETUP).unwrap();
        rt.load_main(&chunk).unwrap();
        let mut journal = Journal::new();
        for _ in 0..400 {
            let outcome = rt.run(300, &mut journal);
            if let Ok(mut snap) = rt.snapshot() {
                snap.push(0);
                corpus.add("hostcap_restore", snap);
            }
            if !matches!(outcome, Ok(StepOutcome::Paused(_))) {
                break;
            }
        }
        for program in PENDING_PROGRAMS {
            let mut rt = moonseed_fuzz::pending_runtime(1_000_000);
            rt.load_main(&moonseed::compile(program).unwrap()).unwrap();
            if let Ok(StepOutcome::Waiting(_)) = rt.run(100_000, &mut journal)
                && let Ok(mut snap) = rt.snapshot()
            {
                snap.push(0x80);
                corpus.add("hostcap_restore", snap);
            }
        }
    }

    // Library calls recorded while fixtures and suite files run.
    for src in fixture_sources.iter().chain(&suite_sources) {
        let mut rt = runtime(30_000_000, GcMode::Generational, filesystem());
        let mut program = RECORDER.to_vec();
        program.extend_from_slice(b"\nlocal f = load(SRC, '@seed.lua') if f then pcall(f) end\n");
        set_global(&mut rt, "SRC", src);
        let Ok(chunk) = moonseed::compile(&program) else {
            continue;
        };
        rt.load_main(&chunk).unwrap();
        run_to_end(&mut rt, 2000);
        let globals = rt.globals();
        let Ok(Some(rec)) = globals.raw_get::<_, Option<Table>>(&mut rt, "__REC") else {
            continue;
        };
        for target in ["pattern", "utf8", "pack", "io", "searchpath"] {
            if let Ok(Some(list)) = rec.raw_get::<_, Option<Table>>(&mut rt, target) {
                let name = if target == "io" { "io_vfs" } else { target };
                for seed in strings(&mut rt, &list) {
                    corpus.add(name, seed);
                }
            }
        }
    }

    // Table operation sequences: every op once, then fixture bytes.
    corpus.add("table_ops", (0u8..=255).collect());
    corpus.add(
        "table_ops",
        (0u8..20).flat_map(|op| [op, op * 3, op * 7 + 1]).collect(),
    );
    for src in fixture_sources.iter().take(40) {
        corpus.add("table_ops", src.iter().take(256).copied().collect());
    }
    corpus.write(&out);
}
