#!/usr/bin/env python3
"""Compare two runs of the official Lua 5.4.9 suite (tools/lua_suite.sh).

    tools/lua_suite_diff.py tests/compat/lua54-baseline.json tests/compat/lua54-current.json

Prints each file's outcome and blocker before and after, the executed
instructions and printed lines after, and the totals by outcome and by
bucket. See docs/LUA_LANGUAGE_AUDIT.md.
"""
import collections
import json
import sys


def load(path):
    with open(path) as handle:
        data = json.load(handle)
    return {entry["file"]: entry for entry in data["files"]}, data


def main():
    before, before_data = load(sys.argv[1])
    after, after_data = load(sys.argv[2])
    if before_data.get("archive_sha256_verified") != after_data.get("archive_sha256_verified"):
        print("warning: the two runs checked different archives")
    print(f"{'file':<16} {'before':<30} {'after':<44} {'fuel':>12} {'lines':>5}")
    for name in sorted(after):
        old, new = before.get(name, {}), after[name]
        was = f"{old.get('outcome', '-')} {old.get('blocker') or ''}"
        now = f"{new['outcome']} {new['bucket']} {new.get('blocker') or ''}"
        print(f"{name:<16} {was:<30} {now:<44} {new['fuel']:>12} {new.get('output_lines', 0):>5}")
    for label, data in (("before", before), ("after", after)):
        outcomes = collections.Counter(entry["outcome"] for entry in data.values())
        buckets = collections.Counter(entry["bucket"] for entry in data.values())
        blockers = collections.Counter(entry.get("blocker") for entry in data.values() if entry.get("blocker"))
        compiled = sum(entry["compiled"] for entry in data.values())
        print(f"\n{label}: {compiled}/{len(data)} compile")
        print("  outcomes:", dict(sorted(outcomes.items())))
        print("  buckets: ", dict(sorted(buckets.items())))
        print("  blockers:", dict(blockers.most_common()))
    further = [
        name
        for name in after
        if after[name]["fuel"] > before.get(name, {}).get("fuel", 0)
        or after[name].get("blocker") != before.get(name, {}).get("blocker")
    ]
    print(f"\nfiles past their old blocker or running longer: {len(further)}")


if __name__ == "__main__":
    main()
