# Official Lua suite evidence

The JSON snapshots record file-by-file runs of the pinned Lua 5.4.9 archive.
`tools/lua_suite.sh run OUT` verifies and extracts that archive; compare an output
with `tools/lua_suite_diff.py tests/compat/lua54-current.json OUT`. A runner exit
of zero means the report was written, not that every Lua file passed.

Phase 3.35 H2 exposes the hook-related subset of `T.sethook` and `T.resume`
through the public host-hook API. `T` is present, so files which previously
skipped their entire C test library now reach individual unimplemented members.
This is a new frontier, not full C API compatibility. The script interpreter
supports the commands used by the suite's hook sections; unsupported scripts
raise an explicit Lua error. Original files and stored baseline JSON remain
unchanged.

Absolute hook positions count each VM's own instructions. Investigate count
assertions with pinned PUC and Moonseed listings before classifying them. Hook
line metadata, stack inspection, transfers and resumed results remain semantic
compatibility targets. PUC's private registry `_HOOKKEY` table is absent in the
side-table implementation; no synthetic registry mirror pretends otherwise.

For bounded lane runs, `LUA_SUITE_MEM_KB`, `LUA_SUITE_CPU` and `LUA_SUITE_WALL`
can lower the runner caps without changing the suite or its archive hash.
