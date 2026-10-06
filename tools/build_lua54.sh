#!/usr/bin/env bash
# Linux reference build. The output path is printed last; no compatibility flags.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:?usage: tools/build_lua54.sh OUTPUT_DIRECTORY}
mkdir -p "$out"
out=$(cd "$out" && pwd)
archive=$out/lua-5.4.9.tar.gz
python3 "$root/tools/ci_run.py" --seconds 120 -- curl --fail --location --silent --show-error \
  --connect-timeout 15 --max-time 110 https://www.lua.org/ftp/lua-5.4.9.tar.gz -o "$archive"
python3 "$root/tools/ci_run.py" --seconds 60 -- python3 - "$archive" <<'PY'
import hashlib, pathlib, sys
expected = '2335b6c582a52654f94612bf10d2f4672805d05329aa6568b1d8cd9e5c6fb8e6'
actual = hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest()
if actual != expected:
    sys.exit(f'Lua source hash mismatch: {actual}')
print('Lua source SHA-256 verified:', actual)
PY
# Always extract a fresh source tree, regardless of previous build output.
rm -rf "$out/lua-5.4.9"
python3 "$root/tools/ci_run.py" --seconds 60 -- tar -xzf "$archive" -C "$out"
# lua.org's stock Makefile enables LUA_COMPAT_5_3; remove it explicitly.
sed -i 's/-DLUA_COMPAT_5_3//g' "$out/lua-5.4.9/src/Makefile"
python3 "$root/tools/ci_run.py" -- make -C "$out/lua-5.4.9" -j8 linux
python3 "$root/tools/ci_run.py" -- cc -O2 -I"$out/lua-5.4.9/src" \
  "$root/tools/lua54_userdata_harness.c" "$out/lua-5.4.9/src/liblua.a" -lm -ldl -o "$out/lua54-userdata"
# Oracle tests spawn these wrappers, retaining per-program memory/wall limits.
for name in lua54 lua54-userdata; do
  binary=$out/lua54-userdata
  [[ $name != lua54 ]] || binary=$out/lua-5.4.9/src/lua
  printf '#!/usr/bin/env bash\nexec python3 %q --runtime -- %q "$@"\n' \
    "$root/tools/ci_run.py" "$binary" > "$out/$name-capped"
  chmod +x "$out/$name-capped"
done
python3 "$root/tools/ci_run.py" --runtime -- "$out/lua-5.4.9/src/lua" -v
printf '%s\n' "$out/lua-5.4.9/src/lua"
