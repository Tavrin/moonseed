#![allow(deprecated)] // Legacy proof and compatibility call sites.
#![allow(clippy::arc_with_non_send_sync)] // Public capability traits do not require Send/Sync.
//! Runs the official Lua 5.4.9 test suite, file by file, through Moonseed,
//! and writes what happened as JSON. See `docs/LUA_LANGUAGE_AUDIT.md`.
//!
//! Usage: `moonseed-compat <suite dir> <output.json> [sha256]`.
//! `tools/lua_suite.sh` fetches the pinned suite, checks the archive's
//! SHA-256, extracts it afresh, and runs this under process caps with the
//! hash it checked; without one, the JSON says the files are unverified.
//!
//! The public builder installs all Lua libraries and explicit native authority.
//! Suite files are read-only; generated files use a fresh scratch directory.
//! The runner's shell capability relies on the outer bwrap sandbox in
//! `tools/lua_suite.sh`. Each initial chunk is read by the harness; nested Lua
//! loads use loadfile/dofile and the filesystem searcher. A line-preserving
//! missing-global observer records skipped library requirements. The optional
//! MOONSEED_COMPAT_TRACE wrapper adds xpcall/debug.traceback for blocker tracing.

mod filesystem;
mod hooks;

use moonseed::{
    CompileErrorKind, Config, HostRegistry, Journal, Libraries, NativeCall, NativeFilesystem,
    NativeOptions, NativeOutcome, NativePolicy, Runtime, StepOutcome, compile, line_col,
};
use std::cell::RefCell;
use std::fmt::Write as _;

const SUITE: &str = "lua-5.4.9-tests";
const URL: &str = "https://www.lua.org/tests/lua-5.4.9-tests.tar.gz";
/// Instructions one file may run.
const FUEL: u64 = 200_000_000;

/// The standard globals a Lua 5.4 program can name, and the bucket a
/// failure after reading a missing one belongs to.
const GLOBALS: &[(&str, &str)] = &[
    ("assert", "BASE LIBRARY"),
    ("print", "BASE LIBRARY"),
    ("type", "BASE LIBRARY"),
    ("tostring", "BASE LIBRARY"),
    ("tonumber", "BASE LIBRARY"),
    ("next", "BASE LIBRARY"),
    ("pairs", "BASE LIBRARY"),
    ("ipairs", "BASE LIBRARY"),
    ("collectgarbage", "BASE LIBRARY"),
    ("load", "BASE LIBRARY"),
    ("_VERSION", "BASE LIBRARY"),
    ("_G", "BASE LIBRARY"),
    ("warn", "BASE LIBRARY"),
    ("string", "STRING LIBRARY"),
    ("table", "TABLE LIBRARY"),
    ("math", "MATH LIBRARY"),
    ("utf8", "UTF8 LIBRARY"),
    ("coroutine", "COROUTINE LIBRARY"),
    ("debug", "DEBUG LIBRARY"),
    ("io", "IO / HOST CAPABILITY"),
    ("os", "IO / HOST CAPABILITY"),
    ("arg", "IO / HOST CAPABILITY"),
    ("loadfile", "IO / HOST CAPABILITY"),
    ("dofile", "IO / HOST CAPABILITY"),
    ("require", "PACKAGE LIBRARY"),
    ("package", "PACKAGE LIBRARY"),
    ("T", "USERDATA/C API"),
    // Defined by `all.lua` for the files it runs; read alone, a file
    // falls back to `print`.
    ("Message", "SUITE DRIVER"),
];

thread_local! {
    static MISSING: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// `__index` of `_ENV`: record the missing name, return nothing.
fn note(call: &mut NativeCall<'_>) -> NativeOutcome {
    let key = call.arg(1);
    let name = call
        .string_bytes(key)
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_else(|| "?".into());
    MISSING.with(|missing| missing.borrow_mut().push(name));
    NativeOutcome::Ready
}

/// `luaL_loadfile`'s rule: a first line starting with `#` is skipped, its
/// newline kept.
fn loader_text(source: &[u8]) -> Vec<u8> {
    if source.first() == Some(&b'#') {
        let end = source
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap_or(source.len());
        source[end..].to_vec()
    } else {
        source.to_vec()
    }
}

/// Identifiers that name a standard global, in source order of first use,
/// outside comments and strings and not after `.` or `:`. Textual: a local
/// of the same name counts too.
fn referenced(source: &[u8]) -> Vec<&'static str> {
    let mut found: Vec<&'static str> = Vec::new();
    let mut at = 0;
    let long_open = |at: usize| -> Option<usize> {
        // `[` `=`* `[`: the level.
        if source.get(at) != Some(&b'[') {
            return None;
        }
        let mut level = 0;
        while source.get(at + 1 + level) == Some(&b'=') {
            level += 1;
        }
        (source.get(at + 1 + level) == Some(&b'[')).then_some(level)
    };
    let skip_long = |at: usize, level: usize| -> usize {
        let close: Vec<u8> = std::iter::once(b']')
            .chain(std::iter::repeat_n(b'=', level))
            .chain(std::iter::once(b']'))
            .collect();
        let start = at + level + 2;
        source[start.min(source.len())..]
            .windows(close.len())
            .position(|window| window == close.as_slice())
            .map_or(source.len(), |offset| start + offset + close.len())
    };
    let mut previous = b' ';
    while at < source.len() {
        let byte = source[at];
        if source[at..].starts_with(b"--") {
            at += 2;
            match long_open(at) {
                Some(level) => at = skip_long(at, level),
                None => {
                    while at < source.len() && source[at] != b'\n' {
                        at += 1;
                    }
                }
            }
            continue;
        }
        if let Some(level) = long_open(at) {
            at = skip_long(at, level);
            previous = b'"';
            continue;
        }
        if byte == b'"' || byte == b'\'' {
            at += 1;
            while at < source.len() && source[at] != byte && source[at] != b'\n' {
                at += if source[at] == b'\\' { 2 } else { 1 };
            }
            at += 1;
            previous = b'"';
            continue;
        }
        if byte.is_ascii_alphabetic() || byte == b'_' {
            let start = at;
            while at < source.len() && (source[at].is_ascii_alphanumeric() || source[at] == b'_') {
                at += 1;
            }
            let word = &source[start..at];
            if previous != b'.'
                && previous != b':'
                && let Some((name, _)) = GLOBALS.iter().find(|(name, _)| name.as_bytes() == word)
                && !found.contains(name)
            {
                found.push(name);
            }
            previous = b'a';
            continue;
        }
        if !byte.is_ascii_whitespace() {
            previous = byte;
        }
        at += 1;
    }
    found
}

/// Whether `name` is a global of Lua's standard libraries or of the suite.
fn standard(name: &str) -> bool {
    GLOBALS.iter().any(|(global, _)| *global == name)
}

fn bucket(name: &str) -> &'static str {
    GLOBALS
        .iter()
        .find(|(global, _)| *global == name)
        .map_or("UNKNOWN", |(_, bucket)| bucket)
}

fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_list(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|item| json_string(item)).collect();
    format!("[{}]", quoted.join(", "))
}

struct FileResult {
    file: String,
    compiled: bool,
    outcome: String,
    bucket: String,
    detail: String,
    blocker: Option<String>,
    missing: Vec<String>,
    referenced: Vec<String>,
    fuel: u64,
    /// Lines `print` wrote, and the last one: how far the file got.
    output_lines: usize,
    last_output: String,
}

fn run_file(
    name: &str,
    source: &[u8],
    suite: &std::path::Path,
    scratch: &std::path::Path,
) -> FileResult {
    let text = loader_text(source);
    let referenced: Vec<String> = referenced(&text).into_iter().map(String::from).collect();
    let mut result = FileResult {
        file: name.to_string(),
        compiled: false,
        outcome: String::new(),
        bucket: String::new(),
        detail: String::new(),
        blocker: None,
        missing: Vec::new(),
        referenced,
        fuel: 0,
        output_lines: 0,
        last_output: String::new(),
    };
    // Compiled alone first: whether Moonseed accepts the file as written.
    if let Err(error) = compile(&text) {
        let (line, _) = line_col(&text, error.span.start);
        result.outcome = "COMPILE_ERROR".into();
        result.bucket = if error.kind == CompileErrorKind::Limit {
            "RESOURCE LIMIT".into()
        } else {
            "LANGUAGE CORE".into()
        };
        result.detail = format!("line {line}: {:?}: {}", error.kind, error.message);
        return result;
    }
    result.compiled = true;
    let mut instrumented =
        b"do local note = __moonseed_harness_note __moonseed_harness_note = nil \
          setmetatable(_ENV, { __index = note }) end "
            .to_vec();
    let trace = std::env::var_os("MOONSEED_COMPAT_TRACE").is_some();
    if trace {
        instrumented.extend_from_slice(b"local __ok,__err=xpcall(function(...) ");
    }
    instrumented.extend_from_slice(&text);
    if trace {
        instrumented
            .extend_from_slice(b"\nend,debug.traceback); if not __ok then error(__err,0) end");
    }
    let Ok(mut chunk) = compile(&instrumented) else {
        result.outcome = "HARNESS_ERROR".into();
        result.bucket = "HARNESS LIMITATION".into();
        result.detail = "the instrumented file does not compile".into();
        return result;
    };
    chunk.set_chunk_name(format!("@{name}").as_bytes());
    let mut registry = HostRegistry::new();
    hooks::register(&mut registry);
    registry.register_native("harness.note", NativePolicy::VmLocal, note);
    let config = Config {
        fuel_limit: Some(FUEL),
        ..Config::default()
    };
    MISSING.with(|missing| missing.borrow_mut().clear());
    let written = std::rc::Rc::new(RefCell::new(Vec::new()));
    let lines = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let sink = written.clone();
    let counter = lines.clone();
    let caps = moonseed::native_host(
        scratch,
        NativeOptions {
            process: true,
            env: true,
            ..NativeOptions::default()
        },
    )
    .expect("native suite profile")
    .filesystem(std::sync::Arc::new(filesystem::SuiteFilesystem {
        suite: NativeFilesystem::new(
            suite,
            NativeOptions {
                read_only: true,
                ..NativeOptions::default()
            },
        )
        .expect("suite root"),
        scratch: NativeFilesystem::new(scratch, NativeOptions::default()).expect("scratch root"),
    }));
    let outcome = Runtime::builder()
        .config(config)
        .registry(registry)
        .libraries(Libraries::ALL)
        .capabilities(caps)
        .package_paths(b"./?.lua", b"")
        .build()
        .map_err(|_| moonseed::VmError::Corrupt)
        .and_then(|mut runtime| {
            runtime
                .install_arg(
                    name.as_bytes(),
                    &[] as &[&[u8]],
                    &[std::env::current_exe()
                        .unwrap()
                        .to_string_lossy()
                        .as_bytes()],
                )
                .map_err(|_| moonseed::VmError::Corrupt)?;
            runtime
                .load_main(&chunk)
                .map_err(|_| moonseed::VmError::Corrupt)?;
            hooks::bind(&mut runtime).map_err(|_| moonseed::VmError::Corrupt)?;
            runtime.set_output(Box::new(move |bytes| {
                let mut buffer = sink.borrow_mut();
                // The last megabyte is enough to say how far a file got.
                if buffer.len() > 1 << 20 {
                    let cut = buffer.len() - (1 << 19);
                    buffer.drain(..cut);
                }
                buffer.extend_from_slice(bytes);
                counter.set(counter.get() + bytes.iter().filter(|byte| **byte == b'\n').count());
            }));
            runtime.set_global_native("__moonseed_harness_note", "harness.note")?;
            let mut journal = Journal::new();
            let outcome = runtime.run_until_terminal(u64::MAX, &mut journal);
            let error = runtime.lua_error();
            if matches!(
                outcome,
                Ok(StepOutcome::Completed | StepOutcome::LuaError(_))
            ) {
                runtime.begin_close()?;
                runtime.run_until_terminal(u64::MAX, &mut journal)?;
            }
            result.fuel = runtime.fuel_consumed();
            outcome.map(|outcome| (outcome, error))
        });
    result.missing = MISSING.with(|missing| {
        let mut unique: Vec<String> = Vec::new();
        for name in missing.borrow().iter() {
            if !unique.contains(name) {
                unique.push(name.clone());
            }
        }
        unique
    });
    let last_missing = MISSING.with(|missing| missing.borrow().last().cloned());
    {
        let output = written.borrow();
        if let Some(dir) = std::env::var_os("MOONSEED_COMPAT_OUTPUT") {
            let dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&dir).expect("compat output dir");
            std::fs::write(dir.join(format!("{name}.stdout")), &*output).expect("compat output");
        }
        let text = String::from_utf8_lossy(&output);
        // Every line written, and a last one without its newline.
        result.output_lines =
            lines.get() + usize::from(!output.is_empty() && !output.ends_with(b"\n"));
        result.last_output = text
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .chars()
            .take(120)
            .collect();
    }
    match outcome {
        // A file that ends having read a missing global did not run what it
        // tests: `api.lua` and `code.lua` skip themselves without the C
        // test library `T`, as they do in PUC Lua without it, and a
        // `pcall` can catch the error a missing library raised.
        // Only a standard global counts as missing. The suite reads others
        // on purpose as nil (`undef`), as PUC Lua does; `Message` is the
        // suite driver's, and a file read alone uses `print`.
        Ok((StepOutcome::Completed, _))
            if result
                .missing
                .iter()
                .all(|name| !standard(name) || name == "Message") =>
        {
            result.outcome = "PASS".into();
            result.bucket = "PASS".into();
        }
        Ok((StepOutcome::Completed, _))
            if result
                .missing
                .iter()
                .all(|name| !standard(name) || name == "Message" || name == "T") =>
        {
            result.outcome = "COMPLETED_WITHOUT_T".into();
            result.bucket = "USERDATA/C API".into();
            result.blocker = Some("T".into());
            result.detail = "ran to its end without the C test library `T`, skipping what needs it, as in PUC Lua built without it".into();
        }
        Ok((StepOutcome::Completed, _)) => {
            let name = result
                .missing
                .iter()
                .rev()
                .find(|name| standard(name) && *name != "Message" && *name != "T")
                .cloned()
                .unwrap_or_default();
            result.outcome = "COMPLETED_WITHOUT".into();
            result.bucket = bucket(&name).into();
            result.detail = format!("ran to its end, but read the missing global `{name}`");
            result.blocker = Some(name);
        }
        Ok((StepOutcome::ExitRequested { status, close }, _)) => {
            result.outcome = "EXIT_REQUESTED".into();
            result.bucket = "STANDALONE INTERPRETER".into();
            result.detail = format!("{status:?}, close={close}");
        }
        Ok((StepOutcome::Terminated(reason), _)) => {
            result.outcome = "TERMINATED".into();
            result.bucket = "RESOURCE LIMIT".into();
            result.detail = format!("{reason:?}");
        }
        Ok((StepOutcome::LuaError(fault), error)) => {
            result.outcome = "LUA_ERROR".into();
            result.detail = match error {
                Some((_, moonseed::HostValue::String(bytes))) => {
                    format!("{fault:?}: {}", String::from_utf8_lossy(&bytes))
                }
                Some((_, value)) => format!("{fault:?}: {value:?}"),
                None => format!("{fault:?}"),
            };
            match last_missing {
                Some(name) => {
                    result.bucket = bucket(&name).into();
                    result.blocker = Some(name);
                }
                None => result.bucket = "UNKNOWN".into(),
            }
        }
        Ok((other, _)) => {
            result.outcome = "STOPPED".into();
            result.bucket = "HARNESS LIMITATION".into();
            result.detail = format!("{other:?}: the file waits or yields to the host");
        }
        Err(error) => {
            result.outcome = "HOST_ERROR".into();
            result.bucket = "UNKNOWN".into();
            result.detail = format!("{error:?}");
        }
    }
    result
}

// Ledger free text must not retain the operator's executable, home or temp roots.
fn report_text(text: &str) -> String {
    text.split_inclusive(char::is_whitespace)
        .map(|word| {
            let start = ["/home/", "/mnt/", "/tmp/", "/Users/", "\\Users\\"]
                .iter()
                .filter_map(|prefix| word.find(prefix))
                .min();
            match start {
                Some(start) => {
                    let tail = word[start..].rsplit(['/', '\\']).next().unwrap_or("");
                    format!("{}<redacted>/{tail}", &word[..start])
                }
                None => word.to_owned(),
            }
        })
        .collect()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(dir), Some(out)) = (args.next(), args.next()) else {
        eprintln!("usage: moonseed-compat <suite dir> <output.json> [sha256]");
        std::process::exit(2);
    };
    // The hash of the archive the caller checked and extracted `dir` from.
    let verified = args.next();
    let selected = args.next();
    let dir = std::fs::canonicalize(dir).expect("suite root");
    let out = std::path::PathBuf::from(out);
    let out = if out.is_absolute() {
        out
    } else {
        std::env::current_dir().unwrap().join(out)
    };
    let scratch = out
        .parent()
        .unwrap()
        .join(format!("suite-scratch-{}", std::process::id()));
    std::fs::create_dir(&scratch).expect("fresh suite scratch");
    // attrib.lua writes both flat modules and the P1 sub-package fixture.
    std::fs::create_dir_all(scratch.join("libs/P1")).expect("suite module scratch");
    std::env::set_current_dir(&scratch).expect("suite process cwd");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("suite dir")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "lua"))
        .collect();
    files.sort();
    let mut results = Vec::new();
    for path in &files {
        if selected
            .as_ref()
            .is_some_and(|file| path.file_name().unwrap() != file.as_str())
        {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let source = std::fs::read(path).expect("suite file");
        let result = run_file(&name, &source, &dir, &scratch);
        println!(
            "{:<16} {:<14} {:<22} {:<16} {:>6} {}",
            result.file,
            result.outcome,
            result.bucket,
            result.blocker.as_deref().unwrap_or(""),
            result.output_lines,
            result.detail.chars().take(70).collect::<String>()
        );
        results.push(result);
    }
    let mut json = String::from("{\n");
    let _ = writeln!(json, "  \"suite\": {},", json_string(SUITE));
    let _ = writeln!(json, "  \"url\": {},", json_string(URL));
    let _ = writeln!(
        json,
        "  \"archive_sha256_verified\": {},",
        verified.as_deref().map_or("null".into(), json_string)
    );
    let _ = writeln!(json, "  \"fuel_limit\": {FUEL},");
    json.push_str("  \"files\": [\n");
    for (index, result) in results.iter().enumerate() {
        let _ = write!(
            json,
            "    {{\"file\": {}, \"compiled\": {}, \"outcome\": {}, \"bucket\": {}, \"blocker\": {}, \"detail\": {}, \"fuel\": {}, \"output_lines\": {}, \"last_output\": {}, \"missing_globals\": {}, \"referenced_globals\": {}}}",
            json_string(&result.file),
            result.compiled,
            json_string(&result.outcome),
            json_string(&result.bucket),
            result.blocker.as_deref().map_or("null".into(), json_string),
            json_string(&report_text(&result.detail)),
            result.fuel,
            result.output_lines,
            json_string(&report_text(&result.last_output)),
            json_list(&result.missing),
            json_list(&result.referenced),
        );
        json.push_str(if index + 1 < results.len() {
            ",\n"
        } else {
            "\n"
        });
    }
    json.push_str("  ]\n}\n");
    std::fs::write(&out, json).expect("write results");
    std::env::set_current_dir(out.parent().unwrap()).unwrap();
    std::fs::remove_dir_all(scratch).expect("remove suite scratch");
}
