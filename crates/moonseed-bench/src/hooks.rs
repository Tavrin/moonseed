//! The frozen C-hook harness contract, implemented through the embedding API.
use moonseed::{
    Coerce, HookAction, HookContext, HookEvent, HookMask, HostRegistry, NativeContext,
    NativePolicy, NativeReturn, Result, ResumeOutcome, Runtime, Table, Value,
};

const SYMBOL: &str = "harness.chook";
const STATES: &str = "__moonseed_hook_states";

fn record(cx: &mut HookContext<'_>) -> Result<HookAction> {
    let globals = cx.globals();
    let states: Table = cx.raw_get(&globals, STATES)?;
    let thread = cx.thread()?;
    let state: Option<Table> = cx.raw_get(&states, thread)?;
    let Some(state) = state else {
        return Ok(HookAction::Continue);
    };
    let rows: Table = cx.raw_get(&state, "records")?;
    let mode: i64 = cx.raw_get(&state, "yieldmode")?;
    let length = cx.raw_len(&rows)?;
    if length >= 20000 {
        return Err(cx.error("C hook record limit")?.into());
    }
    let info = cx.info(0)?.ok_or(moonseed::ApiError::InvalidCallState)?;
    let depth = cx.depth()?;
    let event = cx.event();
    let line = cx.line();
    let mut transfers = Vec::new();
    if matches!(event, HookEvent::Call | HookEvent::Return) {
        for i in 0..info.ntransfer {
            if let Some((name, value)) = cx.local(0, (info.ftransfer + i) as i32)? {
                transfers.push((name, value.to_owned_value()?));
            }
        }
    }
    let row = cx.create_table()?;
    cx.raw_set(&row, "event", event.name())?;
    cx.raw_set(&row, "line", line)?;
    cx.raw_set(&row, "name", info.name)?;
    cx.raw_set(&row, "namewhat", info.namewhat)?;
    cx.raw_set(&row, "what", info.what)?;
    cx.raw_set(&row, "short_src", info.short_src)?;
    cx.raw_set(&row, "currentline", info.currentline)?;
    cx.raw_set(&row, "linedefined", info.linedefined)?;
    cx.raw_set(&row, "istailcall", info.istailcall)?;
    cx.raw_set(&row, "ftransfer", info.ftransfer)?;
    cx.raw_set(&row, "ntransfer", info.ntransfer)?;
    cx.raw_set(&row, "nparams", info.nparams)?;
    cx.raw_set(&row, "isvararg", info.isvararg)?;
    cx.raw_set(&row, "depth", depth)?;
    let values = cx.create_table()?;
    for (i, (name, value)) in transfers.into_iter().enumerate() {
        let entry = cx.create_table()?;
        cx.raw_set(&entry, "name", name)?;
        cx.raw_set(&entry, "value", value)?;
        cx.raw_set(&values, i + 1, entry)?;
    }
    cx.raw_set(&row, "transfers", values)?;
    cx.raw_set(&rows, length + 1, row)?;
    let bit = match event {
        HookEvent::Call => 1,
        HookEvent::Return => 2,
        HookEvent::Line => 4,
        HookEvent::Count => 8,
        HookEvent::TailCall => 16,
    };
    Ok(if mode & bit != 0 {
        HookAction::Yield
    } else {
        HookAction::Continue
    })
}
fn selected(cx: &mut NativeContext<'_>) -> Result<(Value, usize)> {
    let value = cx.arg(0).to_owned_value()?;
    Ok(if matches!(value, Value::Thread(_)) {
        (value, 1)
    } else {
        (cx.current_thread()?, 0)
    })
}
fn install(cx: &mut NativeContext<'_>) -> Result<NativeReturn> {
    install_inner(cx).map_err(|error| cx.argument_error(error))
}
fn install_inner(cx: &mut NativeContext<'_>) -> Result<NativeReturn> {
    let (thread, arg) = selected(cx)?;
    let Coerce(mask): Coerce<Vec<u8>> = cx.argument(arg)?;
    let Coerce(count): Coerce<i64> = cx.argument(arg + 1)?;
    let Coerce(mode): Coerce<Vec<u8>> = cx.argument(arg + 2)?;
    let mode = &mode[..mode.iter().position(|b| *b == 0).unwrap_or(mode.len())];
    let mode = match mode {
        b"none" => 0,
        b"line" => 4,
        b"count" => 8,
        b"both" => 12,
        b"call" => 1,
        b"return" => 2,
        _ => {
            return Err(cx.bad_argument(arg + 2, "invalid yieldmode")?.into());
        }
    };
    let bytes = &mask[..mask.iter().position(|b| *b == 0).unwrap_or(mask.len())];
    let mut mask = HookMask::NONE;
    for (byte, bit) in [
        (b'c', HookMask::CALL),
        (b'r', HookMask::RETURN),
        (b'l', HookMask::LINE),
    ] {
        if bytes.contains(&byte) {
            mask |= bit;
        }
    }
    let globals = cx.globals();
    let states: Table = cx.raw_get(&globals, STATES)?;
    let state = cx.create_table()?;
    let rows = cx.create_table()?;
    cx.raw_set(&state, "yieldmode", mode)?;
    cx.raw_set(&state, "records", &rows)?;
    cx.raw_set(&states, &thread, state)?;
    cx.set_hook(Some(&thread), SYMBOL, mask, count as i32)?;
    cx.return_values(rows)
}
fn get(cx: &mut NativeContext<'_>) -> Result<NativeReturn> {
    if let Some(resume) = cx.resumed() {
        return Ok(match resume.outcome {
            ResumeOutcome::Returned(values) => NativeReturn::Return(values),
            ResumeOutcome::Errored(error) => NativeReturn::Error(error.value),
        });
    }
    let (thread, _) = selected(cx)?;
    let globals = cx.globals();
    let debug: Table = cx.raw_get(&globals, "debug")?;
    let function: Value = cx.raw_get(&debug, "gethook")?;
    cx.call_lua(function, thread, 0, ())
}
pub fn register(registry: &mut HostRegistry) {
    registry.register_hook(SYMBOL, record);
    registry.function("harness.chook.install", NativePolicy::VmLocal, install);
    registry.function("harness.chook.get", NativePolicy::VmLocal, get);
    registry.function("harness.chook.off", NativePolicy::VmLocal, |cx| {
        let (thread, _) = selected(cx)?;
        cx.clear_hook(Some(&thread))?;
        cx.return_values(())
    });
}
pub fn bind(runtime: &mut Runtime) -> Result<()> {
    let globals = runtime.globals();
    let states = runtime.create_table()?;
    let meta = runtime.create_table()?;
    meta.raw_set(runtime, "__mode", "k")?;
    states.set_metatable(runtime, Some(&meta))?;
    globals.raw_set(runtime, STATES, states)?;
    for (name, symbol) in [
        ("chook", "harness.chook.install"),
        ("chookget", "harness.chook.get"),
        ("chookoff", "harness.chook.off"),
    ] {
        let function = runtime.make_closure(symbol, ())?;
        globals.raw_set(runtime, name, function)?;
    }
    Ok(())
}
