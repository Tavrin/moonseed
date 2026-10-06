#!/usr/bin/env python3
"""Capture PUC 5.4.9 diagnostics or compare Moonseed against frozen records."""

from __future__ import annotations

import argparse
import collections
import hashlib
import os
from pathlib import Path
import re
import resource
import subprocess
import sys

from diag_generate import ROOT, DIAG, driver, grouped_batches

PUC = Path(os.environ.get("MOONSEED_LUA54", ROOT / "vendor/lua-5.4.9/src/lua")).expanduser().resolve()
USERDATA = Path(os.environ.get("MOONSEED_LUA54_USERDATA", ROOT / "target/lua54-userdata")).expanduser().resolve()
TARGET = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")).expanduser().resolve()
# The Moonseed runner: $MOONSEED_RUN, else the debug build in $CARGO_TARGET_DIR.
MOONSEED = Path(os.environ.get("MOONSEED_RUN", TARGET / "debug" / "moonseed-run")).expanduser().resolve()
RESULTS = Path(os.environ.get("MOONSEED_DIAG_RESULTS", ROOT / "results/diag"))
EXPECTED = DIAG / "expected"
LIMIT_BYTES = 2_000_000 * 1024


def limit_memory():
    resource.setrlimit(resource.RLIMIT_AS, (LIMIT_BYTES, LIMIT_BYTES))


def run(executable: Path, source: bytes, *flags: str):
    # A one-line case table keeps every driver call site at a fixed line.
    path = RESULTS / "driver.lua"
    path.write_bytes(source)
    try:
        return subprocess.run(
            [str(executable), *flags, path.name], cwd=RESULTS, capture_output=True,
            timeout=60, preexec_fn=limit_memory, check=False,
        )
    except subprocess.TimeoutExpired as exc:
        return subprocess.CompletedProcess([str(executable), str(path)], 124,
                                           exc.stdout or b"", (exc.stderr or b"") + b"[timeout 60s]")


def line_map(data: bytes, ids: list[str]):
    """Return protocol records, preserving every output byte for the report."""
    lines = data.splitlines(keepends=True)
    output = {}
    foreign = []
    for line in lines:
        fields = line.rstrip(b"\r\n").split(b"\t")
        if len(fields) != 5:
            foreign.append(line)
            continue
        try:
            key = fields[0].decode("ascii")
        except UnicodeDecodeError:
            foreign.append(line)
            continue
        if key not in ids or key in output:
            foreign.append(line)
        else:
            output[key] = line
    return output, foreign


def meaning(record: bytes | None):
    if record is None:
        return "missing record"
    fields = record.rstrip(b"\r\n").split(b"\t")
    if len(fields) != 5:
        return "invalid record"
    status, typ, payload, identity = fields[1:]
    if status == b"ok":
        return "ok"
    if typ != b"string":
        return f"{status.decode('ascii', 'replace')} {typ.decode('ascii', 'replace')} identity={identity.decode('ascii', 'replace')}"
    try:
        message = bytes.fromhex(payload.decode("ascii")).decode("utf-8", "backslashreplace")
    except (ValueError, UnicodeDecodeError):
        return "bad hex payload"
    message = re.sub(r"(?:@?diag\.lua|\[string[^]]*\]):\d+", "SOURCE:LINE", message)
    message = re.sub(r"\b\d+\b", "N", message)
    message = re.sub(r"'[^']{1,80}'", "'NAME'", message)
    return f"{status.decode('ascii', 'replace')} {message[:150]}"


def shape(expected: bytes, actual: bytes | None):
    if actual is None:
        return "missing Moonseed record"
    a = actual.rstrip(b"\r\n").split(b"\t")
    e = expected.rstrip(b"\r\n").split(b"\t")
    if len(a) != 5:
        return "malformed Moonseed record"
    if a[1] != e[1] or a[2] != e[2]:
        return f"status/type: {e[1].decode()}/{e[2].decode()} -> {a[1].decode()}/{a[2].decode()}"
    if a[4] != e[4]:
        return "error object identity"
    if a[3] != e[3]:
        return f"message: {meaning(actual)}"
    return "protocol bytes"


def validate_expected(rows):
    for id_, record in rows.items():
        fields = record.rstrip(b"\r\n").split(b"\t")
        if fields[1] == b"ok" and "ok" not in id_:
            raise RuntimeError(f"successful case lacks ok in id: {id_}")
        if fields[3] == b"-":
            continue
        try:
            message = bytes.fromhex(fields[3].decode("ascii"))
        except (ValueError, UnicodeDecodeError) as exc:
            raise RuntimeError(f"invalid expected hex: {id_}") from exc
        if re.search(rb"(?:^|[\s'\"(])/(?:[\w.-]+/)+[\w.-]+", message):
            raise RuntimeError(f"absolute path in expected message: {id_}")


def table_shape(category):
    return category.replace("|", "&#124;").replace("\n", "\\n").replace("\t", "\\t")


def capture(batches, sources):
    EXPECTED.mkdir(parents=True, exist_ok=True)
    seen = set()
    for name, cases in batches:
        executable = USERDATA if cases[0]["group"] == "userdata" else PUC
        if not executable.is_file():
            raise RuntimeError(f"missing PUC executable: {executable}")
        result = run(executable, sources[name])
        ids = [c["id"] for c in cases]
        rows, foreign = line_map(result.stdout, ids)
        if result.returncode or foreign or set(rows) != set(ids):
            raise RuntimeError(f"{name}: PUC exit {result.returncode}, records {len(rows)}/{len(ids)}, "
                               f"foreign={foreign[:2]!r}, stderr={result.stderr[:400]!r}")
        validate_expected(rows)
        data = b"".join(rows[id_] for id_ in ids)
        (EXPECTED / f"{name}.txt").write_bytes(data)
        seen.add(f"{name}.txt")
        print(f"capture {name}: {len(cases)} cases sha256={hashlib.sha256(data).hexdigest()}")
    for stale in EXPECTED.glob("*.txt"):
        if stale.name not in seen:
            stale.unlink()


def check(batches, sources, report):
    if not MOONSEED.is_file():
        raise RuntimeError(f"build moonseed-run first: {MOONSEED}")
    RESULTS.mkdir(parents=True, exist_ok=True)
    counts = collections.defaultdict(lambda: collections.Counter())
    shapes = collections.Counter()
    examples = {}
    new_shapes = collections.Counter()
    new_examples = {}
    mismatches = []
    process_failures = []
    for name, cases in batches:
        group = cases[0]["group"]
        ids = [c["id"] for c in cases]
        frozen = (EXPECTED / f"{name}.txt").read_bytes()
        expected, foreign_expected = line_map(frozen, ids)
        if foreign_expected or set(expected) != set(ids):
            raise RuntimeError(f"{name}: invalid frozen expected file")
        validate_expected(expected)
        # Userdata cases use the reference harness's natives on both sides.
        flags = ("--userdata",) if group == "userdata" else ()
        result = run(MOONSEED, sources[name], *flags)
        actual, foreign = line_map(result.stdout, ids)
        if result.returncode or foreign:
            process_failures.append((name, result.returncode, result.stderr[:2000], foreign[:3]))
        for id_ in ids:
            counts[group]["cases"] += 1
            want = expected[id_]
            got = actual.get(id_)
            if got is None:
                counts[group]["moonseed crash"] += 1
            elif want == got:
                counts[group]["match"] += 1
            else:
                counts[group]["mismatch"] += 1
                if want.split(b"\t")[1] == b"ok" and got.split(b"\t")[1] != b"ok":
                    counts[group]["moonseed-only failure"] += 1
            if want != got:
                category = shape(want, got)
                shapes[category] += 1
                examples.setdefault(category, id_)
                if group in {"provenance_flow", "stripped"}:
                    new_shapes[category] += 1
                    new_examples.setdefault(category, id_)
                mismatches.append((id_, want, got, category))
        print(f"check {name}: {len(cases)} cases, exit={result.returncode}, matched={sum(expected[i] == actual.get(i) for i in ids)}")
    groups = sorted(counts)
    lines = ["# PUC 5.4.9 diagnostic corpus check", "",
             f"PUC: `{PUC}` (sha256 `{hashlib.sha256(PUC.read_bytes()).hexdigest()}`)",
             f"Moonseed: `{MOONSEED}` (sha256 `{hashlib.sha256(MOONSEED.read_bytes()).hexdigest()}`)",
             "", "Both engines ran the same generated Lua driver. Every record includes status, error type,",
             "hexadecimal error bytes, and any requested table identity marker. Userdata cases use the",
             "reference harness's natives (`newud`, `light`) on both sides (`moonseed-run --userdata`).",
             "Uncaught file mode is omitted: moonseed-run emits `ERROR: ...` and no PUC-style traceback.",
             "", "| Group | Cases | Match | Mismatch | Moonseed-only failure | Moonseed crash | Note |",
             "| --- | ---: | ---: | ---: | ---: | ---: | --- |"]
    for group in groups:
        count = counts[group]
        lines.append(f"| {group} | {count['cases']} | {count['match']} | "
                     f"{count['mismatch']} | {count['moonseed-only failure']} | {count['moonseed crash']} |  |")
    lines += ["", "## Top 30 mismatch shapes", "",
              "| Count | Shape | Example |", "| ---: | --- | --- |"]
    for category, count in shapes.most_common(30):
        lines.append(f"| {count} | {table_shape(category)} | `{examples[category]}` |")
    lines += ["", "## Top 20 mismatch shapes in new groups", "",
              f"{len(new_shapes)} distinct normalized shapes occurred across `provenance_flow` and `stripped`.", "",
              "| Count | Shape | Example |", "| ---: | --- | --- |"]
    for category, count in new_shapes.most_common(20):
        lines.append(f"| {count} | {table_shape(category)} | `{new_examples[category]}` |")
    lines += ["", "## Process failures", ""]
    if process_failures:
        for name, code, stderr, foreign in process_failures:
            lines.append(f"- `{name}`: exit {code}; stderr `{stderr!r}`; foreign stdout `{foreign!r}`")
    else:
        lines.append("None.")
    lines += ["", "## Exact mismatches", "",
              "Each value below is Python's byte representation of the full protocol line, including the newline.", ""]
    for id_, want, got, category in mismatches:
        lines += [f"### `{id_}` — {category}", "", f"- expected: `{want!r}`",
                  f"- actual: `{got!r}`", ""]
    report.parent.mkdir(parents=True, exist_ok=True)
    report.write_text("\n".join(lines) + "\n")
    print(f"report {report}: {len(mismatches)} mismatches")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--capture", action="store_true")
    mode.add_argument("--check", action="store_true")
    parser.add_argument("--report", type=Path, default=RESULTS / "baseline.md",
                        help="path for the --check report")
    args = parser.parse_args()
    batches = list(grouped_batches())
    RESULTS.mkdir(parents=True, exist_ok=True)
    sources = {}
    for name, cases in batches:
        sources[name] = driver(cases)
    if args.capture:
        capture(batches, sources)
    else:
        check(batches, sources, args.report)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError) as exc:
        print(f"diag_corpus: {exc}", file=sys.stderr)
        sys.exit(1)
