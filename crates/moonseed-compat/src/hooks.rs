//! The hook-related subset of Lua's reference T library, using public APIs.
use moonseed::{
    Coerce, HookAction, HookContext, HookEvent, HookMask, HostRegistry, NativeContext,
    NativePolicy, NativeReturn, Result, ResumeOutcome, Runtime, Table, Value,
};

const SYMBOL: &str = "compat.T.hook";
const STATES: &str = "__moonseed_T_hooks";

// runC's semicolon/whitespace tokens, quoted strings and # line comments.
fn tokens(code: &[u8]) -> Vec<Vec<u8>> {
    let mut result = Vec::new();
    let mut at = 0;
    while at < code.len() {
        if code[at].is_ascii_whitespace() || code[at] == b';' {
            at += 1;
            continue;
        }
        if code[at] == b'#' {
            while at < code.len() && code[at] != b'\n' {
                at += 1;
            }
            continue;
        }
        let quoted = matches!(code[at], b'\'' | b'"');
        let delimiter = code[at];
        if quoted {
            at += 1;
        }
        let start = at;
        while at < code.len()
            && if quoted {
                code[at] != delimiter
            } else {
                !code[at].is_ascii_whitespace() && code[at] != b';'
            }
        {
            at += 1;
        }
        result.push(code[start..at].to_vec());
        if quoted && at < code.len() {
            at += 1;
        }
    }
    result
}
fn integer(token: Option<&Vec<u8>>) -> Option<i64> {
    std::str::from_utf8(token?).ok()?.parse().ok()
}
fn mask(bytes: &[u8]) -> HookMask {
    let bytes = &bytes[..bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len())];
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
    mask
}
fn callback(cx: &mut HookContext<'_>) -> Result<HookAction> {
    let globals = cx.globals();
    let states: Table = cx.raw_get(&globals, STATES)?;
    let thread = cx.thread()?;
    let code: Option<Vec<u8>> = cx.raw_get(&states, &thread)?;
    let Some(code) = code else {
        return Ok(HookAction::Continue);
    };
    let event = match cx.event() {
        HookEvent::Return => "ret",
        HookEvent::TailCall => "tailcall",
        e => e.name(),
    };
    let event = Value::String(cx.create_string(event)?);
    let mut stack = vec![event, Value::Integer(cx.line().map_or(-1, i64::from))];
    let code = tokens(&code);
    let mut at = 0;
    while let Some(command) = code.get(at) {
        at += 1;
        match command.as_slice() {
            b"yield" if integer(code.get(at)) == Some(0) => return Ok(HookAction::Yield),
            b"pushint" | b"pushnum" => {
                let Some(n) = integer(code.get(at)) else {
                    break;
                };
                at += 1;
                stack.push(Value::Integer(n));
            }
            b"getglobal" => {
                let Some(key) = code.get(at) else { break };
                at += 1;
                stack.push(cx.raw_get(&globals, key.as_slice())?);
            }
            b"setglobal" => {
                let (Some(key), Some(value)) = (code.get(at), stack.pop()) else {
                    break;
                };
                at += 1;
                cx.raw_set(&globals, key.as_slice(), value)?;
            }
            b"pushvalue" => {
                let Some(index) = integer(code.get(at)) else {
                    break;
                };
                at += 1;
                let index = if index < 0 {
                    stack.len() as i64 + index
                } else {
                    index - 1
                };
                let Some(value) = usize::try_from(index)
                    .ok()
                    .and_then(|i| stack.get(i))
                    .cloned()
                else {
                    break;
                };
                stack.push(value);
            }
            b"append" => {
                let Some(index) = integer(code.get(at)) else {
                    break;
                };
                at += 1;
                let index = if index < 0 {
                    stack.len() as i64 + index
                } else {
                    index - 1
                };
                let Some(Value::Table(table)) = usize::try_from(index)
                    .ok()
                    .and_then(|i| stack.get(i))
                    .cloned()
                else {
                    break;
                };
                let Some(value) = stack.pop() else { break };
                let len = cx.raw_len(&table)?;
                cx.raw_set(&table, len + 1, value)?;
            }
            b"sethook" => {
                let (Some(bits), Some(count), Some(script)) = (
                    integer(code.get(at)),
                    integer(code.get(at + 1)),
                    code.get(at + 2),
                ) else {
                    break;
                };
                at += 3;
                cx.raw_set(&states, &thread, script.as_slice())?;
                let mut selection = HookMask::NONE;
                for (bit, flag) in [
                    (1, HookMask::CALL),
                    (2, HookMask::RETURN),
                    (4, HookMask::LINE),
                ] {
                    if bits & bit != 0 {
                        selection |= flag;
                    }
                }
                if script.is_empty() {
                    cx.clear_hook()?;
                } else {
                    cx.set_hook(SYMBOL, selection, count as i32)?;
                }
            }
            _ => break,
        }
        if at == code.len() {
            return Ok(HookAction::Continue);
        }
    }
    Err(cx.error("unsupported T.sethook script")?.into())
}
fn sethook(cx: &mut NativeContext<'_>) -> Result<NativeReturn> {
    sethook_inner(cx).map_err(|error| cx.argument_error(error))
}
fn sethook_inner(cx: &mut NativeContext<'_>) -> Result<NativeReturn> {
    if cx.arg(0).is_nil() {
        cx.clear_hook(None)?;
        return cx.return_values(());
    }
    let Coerce(code): Coerce<Vec<u8>> = cx.argument(0)?;
    let Coerce(selection): Coerce<Vec<u8>> = cx.argument(1)?;
    let count = if cx.arg(2).is_nil() {
        0
    } else {
        cx.argument::<Coerce<i64>>(2)?.0 as i32
    };
    let globals = cx.globals();
    let states: Table = cx.raw_get(&globals, STATES)?;
    let thread = cx.current_thread()?;
    cx.raw_set(&states, &thread, code.as_slice())?;
    if code.first().is_none_or(|byte| *byte == 0) {
        cx.clear_hook(None)?;
    } else {
        cx.set_hook(None, SYMBOL, mask(&selection), count)?;
    }
    cx.return_values(())
}
fn resume(cx: &mut NativeContext<'_>) -> Result<NativeReturn> {
    if let Some(resume) = cx.resumed() {
        return match resume.outcome {
            ResumeOutcome::Returned(values) => {
                if matches!(values.first(), Some(Value::Boolean(true))) {
                    cx.return_values(true)
                } else {
                    cx.return_values((false, values.get(1).cloned().unwrap_or(Value::Nil)))
                }
            }
            ResumeOutcome::Errored(error) => cx.return_values((false, error.value)),
        };
    }
    let thread: moonseed::Thread = cx.argument(0)?;
    let globals = cx.globals();
    let coroutine: Table = cx.raw_get(&globals, "coroutine")?;
    let function: Value = cx.raw_get(&coroutine, "resume")?;
    cx.call_lua(function, thread, 0, ())
}
pub fn register(registry: &mut HostRegistry) {
    registry.register_hook(SYMBOL, callback);
    registry.function("compat.T.sethook", NativePolicy::VmLocal, sethook);
    registry.function("compat.T.resume", NativePolicy::VmLocal, resume);
}
pub fn bind(runtime: &mut Runtime) -> Result<()> {
    let globals = runtime.globals();
    let states = runtime.create_table()?;
    let meta = runtime.create_table()?;
    meta.raw_set(runtime, "__mode", "k")?;
    states.set_metatable(runtime, Some(&meta))?;
    globals.raw_set(runtime, STATES, states)?;
    let table = runtime.create_table()?;
    for (name, symbol) in [
        ("sethook", "compat.T.sethook"),
        ("resume", "compat.T.resume"),
    ] {
        let function = runtime.make_closure(symbol, ())?;
        table.raw_set(runtime, name, function)?;
    }
    globals.raw_set(runtime, "T", table)
}
