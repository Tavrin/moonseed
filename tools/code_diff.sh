#!/usr/bin/env bash
# Compare the machine code of named functions in two builds, ignoring addresses.
#
#     tools/code_diff.sh <binary-a> <binary-b> <symbol-suffix>...
#
# For example, with two bench binaries built from an unpatched copy and a copy
# patched by tools/cold_opcode_growth.py:
#
#     tools/code_diff.sh a/kernel b/kernel 'Runtime>::exec' 'Runtime>::op_get_field'
#
# Prints the number of differing instruction lines per function; 0 means the
# code is identical apart from where it is placed. See docs/PERFORMANCE.md.
set -euo pipefail
a=$1; b=$2; shift 2
dis() {
  local bin=$1 name=$2 line addr size
  line=$(nm -S -C "$bin" | grep -E " [tT] .*${name}\$" | head -1)
  [ -n "$line" ] || { echo "missing $name in $bin" >&2; return 1; }
  addr=$(echo "$line" | cut -d' ' -f1); size=$(echo "$line" | cut -d' ' -f2)
  objdump -d --no-show-raw-insn --start-address=0x$addr \
    --stop-address=$(printf '0x%x' $((0x$addr + 0x$size))) "$bin" | tail -n +8 |
    sed -E 's/^ *[0-9a-f]+:\s*//; s/[0-9a-f]{4,} <[^>]*>/ADDR/g; s/0x[0-9a-f]+\(%rip\)/RIP/g; s/#.*$//'
}
for name in "$@"; do
  echo "$name: $(diff <(dis "$a" "$name") <(dis "$b" "$name") | grep -c '^[<>]' || true) differing lines"
done
