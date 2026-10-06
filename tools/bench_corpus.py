#!/usr/bin/env python3
"""Run additive corpus revision 2; wall times are diagnostic by default."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import statistics
import subprocess
import sys
import time

import bench_env
from bench_manifest import REVISION, TAGS


ROOT = Path(__file__).resolve().parents[1]
CORPUS = ROOT / "bench" / "corpus"
WORKLOADS = (
    "empty", "fib", "table_fields", "alloc_churn", "strings", "sort",
    "numeric_loops", "generic_for", "method_calls", "closures", "coroutines",
    "metamethods", "patterns", "native_calls", "application",
    "branches", "array_access", "globals", "tail_recursion", "field_writes",
    "string_concat", "string_format",
)
DEFAULT_PUC = str(ROOT / "vendor/lua-5.4.9/src/lua")
DEFAULT_MOONSEED = str(Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")) / "bench-stable/moonseed-run")
TIMINGS = re.compile(rb"^timings compile_ns=(\d+) boot_ns=(\d+) run_ns=(\d+)$", re.M)
# Arguments are passed as argv, never interpolated into shell source.
CAPS = 'ulimit -v 2000000 || exit 125; exec timeout --kill-after=2s 300s "$@"'


def child(argv, core=None):
    """All children, including version/uptime probes, use the same caps."""
    if core is not None:
        argv = ["taskset", "-c", str(core), *argv]
    env = dict(os.environ, LC_ALL="C", TZ="UTC")
    start = time.perf_counter_ns()
    result = subprocess.run(
        ["bash", "--noprofile", "--norc", "-c", CAPS, "bench-corpus", *argv],
        cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        check=False,
    )
    elapsed = time.perf_counter_ns() - start
    matches = TIMINGS.findall(result.stderr)
    return {
        "wall_ns": elapsed,
        "load_average": os.getloadavg(),
        "returncode": result.returncode,
        "stdout": result.stdout.decode("utf-8", errors="replace"),
        "stderr": result.stderr.decode("utf-8", errors="replace"),
        "timings_ns": dict(zip(("compile", "boot", "run"), map(int, matches[0])))
        if len(matches) == 1 else None,
        # Compare bytes, not stripped or lossy decoded text.
        "stdout_hex": result.stdout.hex(),
    }


def executable(value):
    found = shutil.which(value)
    if found is None:
        raise ValueError(f"executable not found: {value}")
    return str(Path(found).resolve())


def read_text(path):
    try:
        return Path(path).read_text().strip()
    except OSError:
        return None


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def summary(samples, split=False):
    """No medians or ratios from incomplete/failed measurements."""
    if not samples or any(s["returncode"] != 0 or s.get("problem") for s in samples):
        return None
    values = sorted(s["wall_ns"] for s in samples)
    result = {
        "median_ns": statistics.median(values),
        "p95_ns": values[math.ceil(0.95 * len(values)) - 1],
    }
    if split:
        result["timings_median_ns"] = {
            key: statistics.median(s["timings_ns"][key] for s in samples)
            for key in ("compile", "boot", "run")
        }
        result["timings_p95_ns"] = {
            key: sorted(s["timings_ns"][key] for s in samples)[math.ceil(0.95 * len(samples)) - 1]
            for key in ("compile", "boot", "run")
        }
    return result


def failure(sample, engine):
    if sample["returncode"] == 124:
        return "timeout 300 s"
    if sample["returncode"] != 0:
        detail = (sample["stderr"] or sample["stdout"]).strip()
        return f"exit {sample['returncode']}: {detail or 'no diagnostic'}"
    if len(bytes.fromhex(sample["stdout_hex"]).splitlines()) != 1:
        return "expected one checksum line"
    if engine == "moonseed" and sample["timings_ns"] is None:
        return "missing or malformed timings line"
    return None


def benchmark(name, index, settings):
    script = CORPUS / f"{name}.lua"
    commands = {
        "puc": [settings["puc"], "-E", str(script)],
        "moonseed": [settings["moonseed"], "--timings", str(script)],
    }
    if settings["luac"]:
        commands["luac"] = [settings["luac"], "-p", str(script)]
    engines = ("puc", "moonseed") if index % 2 == 0 else ("moonseed", "puc")
    record = {
        "name": name,
        "categories": TAGS[name],
        "source_sha256": sha256(script),
        "first_engine": engines[0],
        "checksum_check": "unavailable",
        "checksum": None,
        "warmups": {engine: [] for engine in commands},
        "samples": {engine: [] for engine in commands},
        "failures": {},
    }
    checked = False
    for phase, count in (("warmups", settings["warmups"]), ("samples", settings["repetitions"])):
        for iteration in range(count):
            pair = {}
            # Alternate runtimes throughout each workload. Reverse the starting
            # runtime for successive workloads; luac is a separate compile probe.
            for engine in engines:
                if engine in record["failures"]:
                    continue
                sample = child(commands[engine], settings["core"])
                sample["problem"] = failure(sample, engine)
                record[phase][engine].append(sample)
                pair[engine] = sample
                if sample["problem"]:
                    record["failures"][engine] = sample["problem"]
            # Exactly one byte-for-byte PUC/Moonseed check: first pair, warm or
            # measured. Failed runs cannot supply a validated checksum.
            if not checked:
                checked = True
                if len(pair) == 2 and not any(s["problem"] for s in pair.values()):
                    record["checksum"] = pair["puc"]["stdout"]
                    if pair["puc"]["stdout_hex"] == pair["moonseed"]["stdout_hex"]:
                        record["checksum_check"] = "equal"
                    else:
                        record["checksum_check"] = "mismatch"
                        record["failures"]["moonseed"] = "checksum mismatch"
                        pair["moonseed"]["problem"] = "checksum mismatch"
                elif "puc" in pair and not pair["puc"]["problem"]:
                    record["checksum"] = pair["puc"]["stdout"]
            if record["checksum"] is not None:
                expected = record["checksum"].encode().hex()
                for engine, sample in pair.items():
                    if not sample["problem"] and sample["stdout_hex"] != expected:
                        sample["problem"] = "checksum changed or mismatched"
                        record["failures"][engine] = sample["problem"]
            if "luac" in commands and "luac" not in record["failures"]:
                sample = child(commands["luac"], settings["core"])
                sample["problem"] = None if sample["returncode"] == 0 else (
                    f"exit {sample['returncode']}: {sample['stderr'].strip()}"
                )
                record[phase]["luac"].append(sample)
                if sample["problem"]:
                    record["failures"]["luac"] = sample["problem"]
    record["summary"] = {}
    for engine in commands:
        samples = record["samples"][engine]
        record["summary"][engine] = (
            summary(samples, split=engine == "moonseed")
            if len(samples) == settings["repetitions"] and engine not in record["failures"]
            else None
        )
    puc, moonseed = record["summary"]["puc"], record["summary"]["moonseed"]
    record["ratio"] = (
        moonseed["median_ns"] / puc["median_ns"]
        if puc and moonseed and record["checksum_check"] == "equal" else None
    )
    record["paired_ratio"] = None
    if record["ratio"] is not None:
        ratios = sorted(m["wall_ns"] / p["wall_ns"] for p, m in zip(
            record["samples"]["puc"], record["samples"]["moonseed"], strict=True
        ))
        record["paired_ratio"] = {
            "median": statistics.median(ratios),
            "p10": ratios[math.ceil(0.10 * len(ratios)) - 1],
            "p90": ratios[math.ceil(0.90 * len(ratios)) - 1],
            "p95": ratios[math.ceil(0.95 * len(ratios)) - 1],
            "ratios": [m["wall_ns"] / p["wall_ns"] for p, m in zip(
                record["samples"]["puc"], record["samples"]["moonseed"], strict=True
            )],
        }
    return record


def cell_ns(value):
    return f"{value / 1_000_000:.3f}" if value is not None else "—"


def markdown(report):
    facts, settings = report["machine"], report["settings"]
    print("# PUC vs Moonseed corpus")
    print(f"\nUTC: {report['started_utc']}")
    print(f"\nCPU: {facts['cpu_model']}; core: {settings['core']}; governor: {facts['governor'] or 'unreadable'}.")
    print(f"\nUptime before: {facts['uptime_before']}")
    print(f"\nUptime after: {facts['uptime_after']}")
    print(f"\nW={settings['warmups']}, R={settings['repetitions']}; nearest-rank p95. Wall times include process startup and shutdown.")
    print(f"\nCorpus revision {report['corpus_revision']}; not_acceptance_evidence={report['not_acceptance_evidence']}. Today wall timings are diagnostic only.")
    print(f"\nPUC: `{settings['puc']}` ({facts['puc_version']})")
    print(f"\nMoonseed: `{settings['moonseed']}`; SHA-256 `{report['binaries']['moonseed_sha256']}`.")
    print(f"\nJSON: `{report['json_path']}`; contains source/binary hashes, diagnostics, and every sample.")
    print("\nAll time columns are milliseconds; ratio = Moonseed / PUC median.")
    print("\n| Workload | PUC median | PUC p95 | Moonseed median | Moonseed p95 | Ratio | MS compile | MS boot | MS run | PUC compile | PUC compile p95 | Check / status |")
    print("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|")
    for record in report["workloads"]:
        summaries = record["summary"]
        puc, moonseed, luac = (summaries.get(key) or {} for key in ("puc", "moonseed", "luac"))
        split = moonseed.get("timings_median_ns", {})
        ratio = f"{record['ratio']:.2f}x" if record["ratio"] is not None else "—"
        status = record["checksum_check"]
        for engine, problem in record["failures"].items():
            status += f"; {engine}: {problem}"
        status = status.replace("|", "\\|").replace("\n", " ").replace("\r", " ")
        cells = [record["name"], cell_ns(puc.get("median_ns")), cell_ns(puc.get("p95_ns")),
                 cell_ns(moonseed.get("median_ns")), cell_ns(moonseed.get("p95_ns")), ratio,
                 *(cell_ns(split.get(key)) for key in ("compile", "boot", "run")),
                 cell_ns(luac.get("median_ns")), cell_ns(luac.get("p95_ns")), status]
        print("| " + " | ".join(cells) + " |")
    print("\nAggregate ratios (empty/startup excluded): `" + json.dumps(report["aggregates"]) + "`")
    print("\nPaired ratios, Moonseed / PUC, under load, not a quiet-machine baseline. Nearest-rank percentiles:")
    print("\n| Workload | Median per-pair ratio | p10 | p90 | p95 |")
    print("|---|---:|---:|---:|---:|")
    for row in report["workloads"]:
        paired = row["paired_ratio"]
        values = [f"{paired[key]:.3f}" if paired else "—" for key in ("median", "p10", "p90", "p95")]
        print("| " + " | ".join([row["name"], *values]) + " |")
    print("\nInternal Moonseed split p95 (milliseconds):")
    print("\n| Workload | Compile p95 | Boot p95 | Execution p95 |")
    print("|---|---:|---:|---:|")
    for row in report["workloads"]:
        split = (row["summary"]["moonseed"] or {}).get("timings_p95_ns", {})
        print("| " + " | ".join([row["name"], *(cell_ns(split.get(k)) for k in ("compile", "boot", "run"))]) + " |")
    startup = report["workloads"][0]["summary"]
    print(f"\nStartup (empty.lua, including its checksum print): PUC median {cell_ns((startup.get('puc') or {}).get('median_ns'))} ms; Moonseed median {cell_ns((startup.get('moonseed') or {}).get('median_ns'))} ms.")
    if settings["luac"]:
        print("\nPUC compile is separate `luac -p` process wall time, including luac startup. It is not parse-only time and is not subtracted from total wall time.")
    else:
        print("\nPUC compile omitted: no luac executable found.")
    print("\nFailed engines stop after their first failure. No incomplete medians or ratios are reported; rerun the unchanged corpus after fixes.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", required=True, type=Path, help="write the full result to this path")
    args = parser.parse_args()
    try:
        warmups = int(os.environ.get("W", "1"))
        repetitions = int(os.environ.get("R", "9"))
        core = int(os.environ.get("CORE", "4"))
        if warmups < 0 or repetitions < 1:
            raise ValueError("W must be >= 0 and R must be >= 1")
        if core not in os.sched_getaffinity(0):
            raise ValueError(f"CORE={core} is not in the process's allowed CPU affinity")
        for command in ("bash", "timeout", "taskset"):
            executable(command)
        puc = executable(os.environ.get("PUC", DEFAULT_PUC))
        moonseed = executable(os.environ.get("MOONSEED_RUN", DEFAULT_MOONSEED))
        luac_value = os.environ.get("LUAC")
        luac_candidate = Path(puc).with_name("luac")
        luac = executable(luac_value) if luac_value else (
            str(luac_candidate) if os.access(luac_candidate, os.X_OK) else None
        )
        actual = {path.stem for path in CORPUS.glob("*.lua")}
        if actual != set(WORKLOADS):
            raise ValueError("corpus differs from the fixed WORKLOADS list")
    except (ValueError, OSError) as error:
        parser.error(str(error))
    version = child([puc, "-E", "-v"], core)
    if version["returncode"] != 0:
        parser.error(f"PUC version probe failed: {version['stderr']}")
    puc_version = (version["stdout"] + version["stderr"]).strip()
    if not puc_version.startswith("Lua 5.4.9"):
        parser.error(f"expected PUC Lua 5.4.9, got {puc_version!r}")
    cpuinfo = read_text("/proc/cpuinfo") or ""
    models = re.findall(r"^model name\s*:\s*(.+)$", cpuinfo, flags=re.M)
    settings = {"warmups": warmups, "repetitions": repetitions, "core": core,
                "puc": puc, "moonseed": moonseed, "luac": luac,
                "virtual_memory_kib": 2000000, "timeout_seconds": 300}
    report = {
        "schema_version": 2,
        "corpus_revision": REVISION,
        "environment": bench_env.capture(core, os.environ.get("BENCH_PROFILE", "bench-stable")),
        "started_utc": datetime.now(timezone.utc).isoformat(),
        "json_path": str(args.json.resolve()),
        "settings": settings,
        "machine": {
            "cpu_model": models[core] if core < len(models) else (models[0] if models else "unknown"),
            "governor": read_text(f"/sys/devices/system/cpu/cpu{core}/cpufreq/scaling_governor"),
            "uptime_before": child(["uptime"])["stdout"].strip(),
            "puc_version": puc_version,
        },
        "binaries": {"puc_sha256": sha256(puc), "moonseed_sha256": sha256(moonseed),
                     "luac_sha256": sha256(luac) if luac else None},
        "harness_sha256": sha256(__file__),
        "workloads": [],
    }
    for index, name in enumerate(WORKLOADS):
        print(f"[{index + 1}/{len(WORKLOADS)}] {name}", file=sys.stderr, flush=True)
        record = benchmark(name, index, settings)
        report["workloads"].append(record)
        print(f"  checksum={record['checksum_check']}; failures={record['failures']}", file=sys.stderr, flush=True)
    report["machine"]["uptime_after"] = child(["uptime"])["stdout"].strip()
    report["finished_utc"] = datetime.now(timezone.utc).isoformat()
    # Detect mid-run edits/rebuilds; such a report must not be a baseline.
    report["inputs_unchanged"] = (
        sha256(puc) == report["binaries"]["puc_sha256"]
        and sha256(moonseed) == report["binaries"]["moonseed_sha256"]
        and (not luac or sha256(luac) == report["binaries"]["luac_sha256"])
        and sha256(__file__) == report["harness_sha256"]
        and all(sha256(CORPUS / f"{r['name']}.lua") == r["source_sha256"] for r in report["workloads"])
    )
    ratios = [r["ratio"] for r in report["workloads"] if r["name"] != "empty" and r["ratio"] is not None]
    report["aggregates"] = {
        "valid_workloads": len(ratios), "expected_workloads": len(WORKLOADS) - 1,
        "geomean": statistics.geometric_mean(ratios) if ratios else None,
        "median": statistics.median(ratios) if ratios else None,
        "best": min(ratios) if ratios else None, "worst": max(ratios) if ratios else None,
        "categories": {},
    }
    for tag in sorted({tag for tags in TAGS.values() for tag in tags} - {"startup"}):
        values = [r["ratio"] for r in report["workloads"] if tag in r["categories"] and r["ratio"] is not None]
        report["aggregates"]["categories"][tag] = statistics.geometric_mean(values) if values else None
    loads = [s["load_average"][0] for r in report["workloads"] for phase in ("warmups", "samples") for samples in r[phase].values() for s in samples]
    report["not_acceptance_evidence"] = report["environment"]["not_acceptance_evidence"] or max(loads, default=0) > report["environment"]["load_threshold"]
    args.json.parent.mkdir(parents=True, exist_ok=True)
    args.json.write_text(json.dumps(report, indent=2) + "\n")
    markdown(report)
    if not report["inputs_unchanged"]:
        print("ERROR: inputs changed during the run; discard these measurements", file=sys.stderr)
    return int(not report["inputs_unchanged"] or any(
        r["failures"] or r["checksum_check"] != "equal" for r in report["workloads"]
    ))


if __name__ == "__main__":
    sys.exit(main())
