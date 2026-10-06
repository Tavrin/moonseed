#!/usr/bin/env bash
# Fetch the official Lua 5.4.9 test suite, pinned by SHA-256, and run it
# through Moonseed:
#
#     tools/lua_suite.sh            # fetch if needed, then run
#     tools/lua_suite.sh fetch      # fetch and verify only
#     tools/lua_suite.sh run OUT    # results to OUT instead
#
# The archive and its files stay in .cache/ (gitignored); nothing from it
# is committed. The results go to tests/compat/lua54-current.json;
# tests/compat/lua54-baseline.json (Phase 3.20) and lua54-phase321.json
# are earlier runs, kept to compare against (tools/lua_suite_diff.py). The
# suite matches one Lua release only: do not point this at another.
# See docs/LUA_LANGUAGE_AUDIT.md.
set -euo pipefail
cd "$(dirname "$0")/.."
URL=https://www.lua.org/tests/lua-5.4.9-tests.tar.gz
SHA256=7d971845f545ffc09fbb3128a86b2c6524161c70d0fdf0154a16e8c00c343fca
CACHE=.cache
DIR=$CACHE/lua-5.4.9-tests
ARCHIVE=$CACHE/lua-5.4.9-tests.tar.gz
mkdir -p "$CACHE"
[ -f "$ARCHIVE" ] || curl -sSfL --connect-timeout 15 --max-time 110 -o "$ARCHIVE" "$URL"
# Every run: check the archive, and extract it afresh, so the files run are
# exactly the pinned release.
echo "$SHA256  $ARCHIVE" | sha256sum -c --quiet -
rm -rf "$DIR"
tar -xzf "$ARCHIVE" -C "$CACHE"
[ "${1:-}" = fetch ] && exit 0
( ulimit -v 8000000; CARGO_BUILD_JOBS=8 timeout 1800 cargo build --release -p moonseed-compat )
target_dir=${CARGO_TARGET_DIR:-target}
target_dir=$(realpath "$target_dir")
OUT=tests/compat/lua54-current.json
[ "${1:-}" = run ] && OUT=${2:?usage: tools/lua_suite.sh run OUT}
mkdir -p "$(dirname "$OUT")"
OUT=$(realpath "$OUT")
# A fresh process, cwd and writable root for every file, including shell children.
# The process capability itself does not confine commands; bwrap supplies that
# outer boundary. Only the result/scratch directory and /tmp are writable.
parts=$(mktemp -d "$(dirname "$OUT")/suite-parts-XXXXXX")
trap 'rm -rf "$parts"' EXIT
for file in "$DIR"/*.lua; do
  name=$(basename "$file")
  ( ulimit -v 2000000; timeout 60 bwrap --die-with-parent --ro-bind / / \
      --dev-bind /dev/null /dev/null --dev-bind /dev/urandom /dev/urandom \
      --bind "$parts" "$parts" --bind "$parts" /tmp \
      "$target_dir/release/moonseed-compat" "$(realpath "$DIR")" "$parts/$name.json" "$SHA256" "$name" )
done
python3 - "$parts" "$OUT" <<'PYTHON'
import json, pathlib, sys
parts, out = map(pathlib.Path, sys.argv[1:])
rows = [json.loads(p.read_text()) for p in sorted(parts.glob('*.json'))]
combined = dict(rows[0], files=[f for row in rows for f in row['files']])
out.write_text(json.dumps(combined, indent=2) + '\n')
PYTHON
