#![allow(clippy::arc_with_non_send_sync)] // Public capability profiles use Arc without Send/Sync.
#![allow(deprecated)] // Legacy proof and compatibility call sites.
//! `moonseed-run FILE [--timings] [--compile-only]`: runs one Lua file with the
//! standard libraries and debug, unlimited fuel, `print` to stdout.
//! With `--features measure`, `--listing` prints bytecode without executing it.
use moonseed::{
    Config, HostRegistry, HostValue, Journal, LuaFault, Runtime, StepOutcome, VmError, compile,
    register_base, register_coroutine, register_debug, register_math, register_os,
    register_package, register_string, register_table, register_userdata_proof, register_utf8,
};
mod hooks;
mod host_civil;
mod host_fixture;
mod host_fs;
use std::io::Write;
use std::time::Instant;

struct Output {
    writer: std::io::BufWriter<std::io::Stdout>,
    error: Option<std::io::Error>,
}

fn outcome_error(runtime: &Runtime, outcome: &Result<StepOutcome, VmError>) -> Option<String> {
    match outcome {
        Ok(StepOutcome::Completed | StepOutcome::ExitRequested { .. }) => None,
        Ok(StepOutcome::LuaError(fault)) => {
            let message = match runtime.lua_error() {
                Some((_, HostValue::String(bytes))) => String::from_utf8_lossy(&bytes).into_owned(),
                Some((fault, value)) => format!("{fault:?}: {value:?}"),
                None => format!("{fault:?}"),
            };
            if *fault == LuaFault::Memory {
                let memory = runtime.memory();
                Some(format!(
                    "{message} (objects={} max_objects={} logical_bytes={})",
                    memory.objects, memory.max_objects, memory.logical_bytes
                ))
            } else {
                Some(message)
            }
        }
        Ok(other) => Some(format!("stopped {other:?}")),
        Err(error) => Some(format!("host error {error:?}")),
    }
}

#[allow(clippy::arc_with_non_send_sync)] // Capability objects need not be Send/Sync.
fn run() -> Result<i32, String> {
    let mut path = None;
    let mut timings = false;
    let mut compile_only = false;
    let mut userdata = false;
    let mut hooks = false;
    #[cfg(feature = "measure")]
    let mut listing = false;
    let mut positional = false;
    let mut host_profile = None;
    let mut host_env = false;
    let mut host_process = false;
    let mut host_civil = false;
    let mut fixture_tmp = false;
    let mut lua_args_enabled = false;
    let mut lua_args = Vec::new();
    for arg in std::env::args().skip(1) {
        match (positional, arg.as_str()) {
            (false, "--") => positional = true,
            (false, "--timings") => timings = true,
            (false, "--compile-only") => compile_only = true,
            (false, "--userdata") => userdata = true,
            (false, "--hooks") => hooks = true,
            (false, "--host-environment=process") => host_env = true,
            (false, "--host-process") => host_process = true,
            (false, "--host-civil=fixture-tz") => host_civil = true,
            (false, "--host-fixture-tmp") => fixture_tmp = true,
            (false, "--lua-args") => lua_args_enabled = true,
            (false, option) if option.starts_with("--host=") => {
                host_profile = Some(option[7..].to_owned())
            }
            #[cfg(feature = "measure")]
            (false, "--listing") => listing = true,
            (false, option) if option.starts_with('-') => {
                return Err(format!("unknown option: {option}"));
            }
            _ if path.is_none() => path = Some(arg),
            _ if lua_args_enabled => lua_args.push(arg),
            _ => return Err("expected exactly one file".into()),
        }
    }
    let path = path.ok_or("usage: moonseed-run FILE [--timings] [--compile-only] [--userdata]")?;
    let source = std::fs::read(&path).map_err(|error| format!("cannot read {path}: {error}"))?;
    let start = Instant::now();
    let mut chunk =
        compile(&source).map_err(|error| format!("{:?} {}", error.kind, error.message))?;
    chunk.set_chunk_name(format!("@{path}").as_bytes());
    let compile_ns = start.elapsed().as_nanos();
    #[cfg(feature = "measure")]
    if listing {
        moonseed::write_listing(&chunk, &mut std::io::stdout().lock())
            .map_err(|error| format!("listing: {error}"))?;
        compile_only = true;
    }
    if compile_only {
        if timings {
            eprintln!("timings compile_ns={compile_ns} boot_ns=0 run_ns=0");
        }
        return Ok(0);
    }

    let start = Instant::now();
    let config = Config {
        fuel_limit: None,
        ..Config::default()
    };
    let mut registry = HostRegistry::new();
    // Preserve the historical profile's registry as well as its installed tables.
    // The capability profile's builder registers its additional selected libraries.
    register_base(&mut registry);
    register_math(&mut registry);
    register_table(&mut registry);
    register_string(&mut registry);
    register_utf8(&mut registry);
    register_package(&mut registry);
    register_coroutine(&mut registry);
    register_debug(&mut registry);
    register_os(&mut registry);
    if userdata {
        register_userdata_proof(&mut registry);
    }
    if hooks {
        hooks::register(&mut registry);
    }
    let libraries = moonseed::Libraries::ALL;
    let hosted = host_profile.is_some() || lua_args_enabled;
    let mut runtime = if let Some(profile) = host_profile {
        let capabilities = if let Some(root) = profile.strip_prefix("native:") {
            let options = moonseed::NativeOptions {
                env: host_env,
                process: host_process,
                stdio: true,
                ..Default::default()
            };
            let mut capabilities =
                moonseed::native_host(root, options.clone()).map_err(|error| error.to_string())?;
            if fixture_tmp {
                let filesystem = moonseed::NativeFilesystem::new(root, options)
                    .map_err(|error| error.to_string())?;
                capabilities = capabilities.filesystem(std::sync::Arc::new(
                    host_fs::FixtureTemporaryNames(filesystem),
                ));
            }
            if host_civil && std::env::var("TZ").as_deref() == Ok("America/New_York") {
                capabilities = capabilities.civil(std::sync::Arc::new(host_civil::FixtureNewYork));
            }
            capabilities
        } else if let Some(root) = profile.strip_prefix("vfs:") {
            let mut files = Vec::new();
            fn relative_bytes(
                root: &std::path::Path,
                path: &std::path::Path,
            ) -> Result<Vec<u8>, String> {
                let relative = path.strip_prefix(root).map_err(|e| e.to_string())?;
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStrExt;
                    Ok(relative.as_os_str().as_bytes().to_vec())
                }
                #[cfg(not(unix))]
                {
                    relative
                        .to_str()
                        .map(|p| p.as_bytes().to_vec())
                        .ok_or_else(|| "fixture path is not UTF-8".to_owned())
                }
            }
            fn copy(
                files: &mut Vec<(Vec<u8>, Vec<u8>)>,
                directories: &mut std::collections::BTreeSet<Vec<u8>>,
                root: &std::path::Path,
                dir: &std::path::Path,
            ) -> Result<(), String> {
                for e in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
                    let e = e.map_err(|e| e.to_string())?;
                    let kind = e.file_type().map_err(|e| e.to_string())?;
                    let p = e.path();
                    if kind.is_dir() {
                        directories.insert(relative_bytes(root, &p)?);
                        copy(files, directories, root, &p)?;
                    } else if kind.is_file() {
                        files.push((
                            relative_bytes(root, &p)?,
                            std::fs::read(&p).map_err(|e| e.to_string())?,
                        ));
                    } else {
                        return Err(format!(
                            "fixture import rejects symlinks and special files: {}",
                            p.display()
                        ));
                    }
                }
                Ok(())
            }
            let root = std::path::Path::new(root);
            let mut directories = std::collections::BTreeSet::new();
            copy(&mut files, &mut directories, root, root)?;
            let fs = std::sync::Arc::new(host_fixture::FixtureFilesystem {
                directories,
                fs: moonseed::hostcaps::MemoryFilesystem::new(files, Default::default())
                    .map_err(|e| e.to_string())?,
            });
            let mut input = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut input)
                .map_err(|e| e.to_string())?;
            moonseed::HostCapabilities::sandbox()
                .filesystem(fs)
                .stdio(std::sync::Arc::new(
                    moonseed::hostcaps::testing::MemoryStdio::new(input),
                ))
        } else {
            return Err("host profile must be native:ROOT or vfs:ROOT".into());
        };
        Runtime::builder()
            .config(config)
            .registry(registry)
            .libraries(libraries)
            .capabilities(capabilities)
            .build()
            .map_err(|error| error.to_string())?
    } else if lua_args_enabled {
        Runtime::builder()
            .config(config)
            .registry(registry)
            .libraries(libraries)
            .build()
            .map_err(|error| error.to_string())?
    } else {
        let mut runtime =
            Runtime::load_chunk(config, registry, &chunk).map_err(|error| format!("{error:?}"))?;
        runtime
            .install_standard()
            .map_err(|error| format!("{error:?}"))?;
        runtime
            .install_debug()
            .map_err(|error| format!("{error:?}"))?;
        runtime
    };
    if lua_args_enabled {
        runtime
            .install_arg(path.as_bytes(), &lua_args, &[b"hostlib-engine".as_slice()])
            .map_err(|error| error.to_string())?;
    }
    if hosted {
        let function_id = runtime
            .load_function(&chunk)
            .map_err(|error| format!("{error:?}"))?;
        let Some(moonseed::Value::Function(function)) = runtime.object(function_id) else {
            return Err("missing chunk function".into());
        };
        let args = lua_args
            .iter()
            .map(|arg| runtime.create_string(arg).map(moonseed::Value::String))
            .collect::<moonseed::Result<Vec<_>>>()
            .map_err(|error| error.to_string())?;
        runtime
            .start_call(&function, moonseed::MultiValue::from(args))
            .map_err(|error| error.to_string())?;
    }
    if hooks {
        hooks::bind(&mut runtime).map_err(|error| error.to_string())?;
    }
    // The reference harness's userdata natives (tools/lua54_userdata_harness.c).
    if userdata {
        for name in ["newud", "light", "udpeek", "udpoke"] {
            runtime
                .set_global_native(name, name)
                .map_err(|error| format!("{error:?}"))?;
        }
    }
    let sink = std::rc::Rc::new(std::cell::RefCell::new(Output {
        writer: std::io::BufWriter::new(std::io::stdout()),
        error: None,
    }));
    let writer = sink.clone();
    runtime.set_output(Box::new(move |bytes| {
        let mut output = writer.borrow_mut();
        if output.error.is_none() {
            output.error = output.writer.write_all(bytes).err();
        }
    }));
    let boot_ns = start.elapsed().as_nanos();

    #[cfg(feature = "counters")]
    let _scope = runtime.counter_scope();
    let start = Instant::now();
    let mut journal = Journal::new();
    let outcome = runtime.run_until_terminal(u64::MAX, &mut journal);
    let mut failure = outcome_error(&runtime, &outcome);
    let mut exit_status = requested_status(&outcome);
    // Match lua.c: run registered finalizers at exit, before freeing the state.
    if matches!(
        outcome,
        Ok(StepOutcome::Completed | StepOutcome::LuaError(_))
    ) {
        runtime
            .begin_close()
            .map_err(|error| format!("close: {error:?}"))?;
        let closed = runtime.run_until_terminal(u64::MAX, &mut journal);
        failure = failure.or_else(|| outcome_error(&runtime, &closed));
        exit_status = requested_status(&closed);
    }
    let mut output = sink.borrow_mut();
    if let Err(error) = output.writer.flush() {
        output.error = Some(error);
    }
    if let Some(error) = output.error.take() {
        failure = Some(format!("stdout: {error}"));
    }
    let run_ns = start.elapsed().as_nanos();
    if timings {
        eprintln!("timings compile_ns={compile_ns} boot_ns={boot_ns} run_ns={run_ns}");
    }
    #[cfg(feature = "counters")]
    if let Some(path) = std::env::var_os("MOONSEED_COUNTERS") {
        std::fs::write(path, runtime.counters().to_json()).map_err(|e| format!("counters: {e}"))?;
    }
    if let Some(message) = failure {
        return Err(message);
    }
    Ok(exit_status)
}

fn requested_status(outcome: &Result<StepOutcome, VmError>) -> i32 {
    match outcome {
        Ok(StepOutcome::ExitRequested { status, .. }) => match status {
            moonseed::ExitStatus::Success => 0,
            moonseed::ExitStatus::Failure => 1,
            moonseed::ExitStatus::Code(code) => *code,
        },
        _ => 0,
    }
}

fn main() {
    let status = match run() {
        Ok(status) => status,
        Err(message) => {
            eprintln!("ERROR: {message}");
            1
        }
    };
    std::process::exit(status);
}
