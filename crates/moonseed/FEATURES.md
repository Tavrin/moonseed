# Cargo features

The supported features are:

| Feature | Compiles | Authority | Snapshot and Wasm impact |
| --- | --- | --- | --- |
| `default = ["native-host"]` | Portable VM and native adapter code | None until the host explicitly attaches capabilities | No schema change; builds on native and Wasm |
| `native-host` | Native filesystem, standard-stream, clock, environment and process adapters | Code only. Runtime construction grants no ambient authority; attaching adapters is the host's explicit decision | Live native resources can refuse checkpoints. Wasm compilation does not provide a native OS |
| `counters` | **Unstable** runtime diagnostic counters and JSON reporting through optional `serde_json` | No filesystem/process capability grant; independent of `native-host` | Counters are excluded from snapshots; compiles on Wasm |

`--no-default-features` keeps the portable VM, in-memory filesystem and mock
capabilities. Standard/debug libraries are selected by `Libraries` at runtime,
not Cargo features. Feature selection does not change snapshot 25 or chunk 2.

MSRV is Rust 1.88. Minor 0.x releases may increase it, with a changelog entry.
