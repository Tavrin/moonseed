#!/usr/bin/env bash
# Run one command under resource caps, for reviews, fuzzing, and any program
# whose input is adversarial or generated:
#
#     tools/capped.sh "$test_binary" review_case --ignored
#
# Build first, outside the caps: the compiler and linker need more address
# space than the default allows.
#
# Caps: CAP_MEM_KB of address space (default 2,000,000, about 2 GB), CAP_CPU
# seconds of CPU (default 60), and CAP_WALL seconds of wall clock (default
# 30, then SIGKILL 5 s later). The limits apply to the command and every
# process it starts. Scratch files are the caller's to remove; keep them
# under one directory so a single rm cleans up. See SECURITY.md.
set -euo pipefail
ulimit -v "${CAP_MEM_KB:-2000000}"
ulimit -t "${CAP_CPU:-60}"
exec timeout -k 5 "${CAP_WALL:-30}" "$@"
