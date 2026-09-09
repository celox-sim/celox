#!/usr/bin/env python3
"""Build, validate, and time equivalent Celox/Verilator VCD workloads.

Requires Python 3, Verilator, a C++ toolchain, and Cargo. All generated files and
individual results live under --output. No builds run during timing samples.
"""

import argparse
import csv
import hashlib
import io
import json
import os
from pathlib import Path
import re
import resource
import shutil
import statistics
import subprocess
import tempfile
import time


ROOT = Path(__file__).resolve().parents[1]
HARNESS = ROOT / "benches/verilator/bench_vcd.cpp"
RUST_BENCH = ROOT / "crates/celox/benches/vcd.rs"
CASES = ("idle", "sparse", "dense")
MODES = ("off", "instrumented", "vcd")


def positive(value):
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("must be positive")
    return number


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def cache_celox_binaries(output, celox, baseline=None):
    sources = {"celox": (celox, "celox-vcd")}
    if baseline is not None:
        sources["baseline_celox"] = (baseline, "celox-before")
    # Snapshot every input before replacing a cached binary: the baseline can
    # be the previous celox-vcd in this same output directory.
    with tempfile.TemporaryDirectory(prefix="cache-", dir=output) as staging:
        for source, name in sources.values():
            shutil.copy2(source, Path(staging) / name)
        for _, name in sources.values():
            (Path(staging) / name).replace(output / name)
    return {engine: output / name for engine, (_, name) in sources.items()}


def generate_fixture(folder, signals, full_width, verify_only=False):
    high_bit = (1 << 63) if full_width else 0
    ports = ["input logic clk", "input logic rst"]
    body = []
    setters = []
    checks = []
    for i in range(signals):
        initial = high_bit | (i + 1)
        increment = i * 2 + 1
        ports.extend((f"input logic en{i}", f"output logic [63:0] q{i}"))
        body.append(
            f"always_ff @(posedge clk or negedge rst) begin\n"
            f"  if (!rst) q{i} <= 64'd{initial};\n"
            f"  else if (en{i}) q{i} <= q{i} + 64'd{increment};\nend\n"
        )
        setters.append(f"    top.en{i} = {i} < active;")
        checks.append(
            f"    if (top.q{i} != (UINT64_C({initial}) + "
            f"({i} < active ? ticks * UINT64_C({increment}) : 0)))\n"
            f'        throw std::runtime_error("unexpected q{i}");'
        )
    verilog = (
        "module Top (\n" + ",\n".join(ports) + ");\n"
        "timeunit 1ns; timeprecision 1ns;\n" + "".join(body) + "endmodule\n"
    )
    header = (
        "#pragma once\n#include <cstdint>\n#include <stdexcept>\n"
        f"constexpr unsigned kSignals = {signals};\n"
        "inline void set_enables(VTop& top, unsigned active) {\n"
        + "\n".join(setters) + "\n}\n"
        "inline void verify_counters(const VTop& top, unsigned active, uint64_t ticks) {\n"
        + "\n".join(checks) + "\n}\n"
    )
    for name, text in (("Top.sv", verilog), ("vcd_fixture.h", header)):
        path = folder / name
        if verify_only:
            if path.read_text() != text:
                raise ValueError(f"{path} differs from this fixture; omit --reuse-builds")
        else:
            path.write_text(text)


def read_waveform(path):
    """Canonicalize this fixture's VCD by names/widths and actual transitions.

    Identifier assignments, scopes, leading zeros, redundant records, and empty
    timestamps may differ. Missing/extra signals and wrong values must fail.
    """
    widths = {}
    identifiers = {}
    values = {}
    events = {}
    timestamp = None
    in_header = True
    header = []
    with path.open() as source:
        for raw in source:
            line = raw.strip()
            if in_header:
                header.append(line)
                if line.startswith("$var "):
                    fields = line.split()
                    width, identifier, name = int(fields[2]), fields[3], fields[4]
                    if name in widths or identifier in identifiers:
                        raise ValueError(f"{path}: duplicate signal/identifier: {line}")
                    widths[name] = width
                    identifiers[identifier] = name
                elif line.startswith("$enddefinitions"):
                    in_header = False
                continue
            if not line or line.startswith("$"):
                continue
            if line.startswith("#"):
                next_time = int(line[1:])
                if timestamp is not None and next_time < timestamp:
                    raise ValueError(f"{path}: timestamp went backwards")
                timestamp = next_time
                continue
            if timestamp is None:
                raise ValueError(f"{path}: value before first timestamp")
            if line[0] in "bB":
                bits, identifier = line[1:].split()
            elif line[0] in "01xXzZ":
                bits, identifier = line[0], line[1:]
            else:
                raise ValueError(f"{path}: unsupported value record: {line}")
            name = identifiers[identifier]
            if len(bits) > widths[name] or any(bit not in "01" for bit in bits):
                raise ValueError(f"{path}: invalid two-state value: {line}")
            value = int(bits, 2)
            if values.get(name) != value:
                key = (timestamp, name)
                if key in events:
                    raise ValueError(f"{path}: multiple transitions at {key}")
                events[key] = value
                values[name] = value
    if in_header:
        raise ValueError(f"{path}: missing VCD header")
    scale = re.search(r"\$timescale\s+1\s*ns\s+\$end", " ".join(header))
    if not scale:
        raise ValueError(f"{path}: expected a 1 ns timescale")
    return widths, events


def expected_waveform(signals, steps, case, full_width):
    active = 0 if case == "idle" else max(1, signals // 100) if case == "sparse" else signals
    high_bit = (1 << 63) if full_width else 0
    widths = {"clk": 1, "rst": 1}
    events = {(0, "clk"): 0, (0, "rst"): 1}
    for i in range(signals):
        widths[f"en{i}"] = 1
        widths[f"q{i}"] = 64
        events[0, f"en{i}"] = int(i < active)
        events[0, f"q{i}"] = (high_bit | (i + 1)) + (i * 2 + 1 if i < active else 0)
    for step in range(1, steps + 1):
        if step > 1:
            events[step * 2, "clk"] = 0
        events[step * 2 + 1, "clk"] = 1
        for i in range(active):
            events[step * 2 + 1, f"q{i}"] = (
                (high_bit | (i + 1)) + (step + 1) * (i * 2 + 1)
            ) & ((1 << 64) - 1)
    return widths, events


def validate_waveform(path, expected):
    actual = read_waveform(path)
    if actual[0] != expected[0]:
        raise ValueError(f"{path}: traced signal names or widths differ from the fixture")
    if actual[1] != expected[1]:
        keys = sorted(actual[1].keys() | expected[1].keys())
        for key in keys:
            if actual[1].get(key) != expected[1].get(key):
                raise ValueError(
                    f"{path}: at {key}, got {actual[1].get(key)}, "
                    f"expected {expected[1].get(key)}"
                )
    normalized = json.dumps(sorted(actual[1].items())).encode()
    return {"signals": len(actual[0]), "transitions": len(actual[1]),
            "normalized_sha256": hashlib.sha256(normalized).hexdigest(),
            "file_bytes": path.stat().st_size}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verilator", default=shutil.which("verilator"))
    parser.add_argument("--celox", type=Path, help="existing celox --bench vcd executable; otherwise build it")
    parser.add_argument("--baseline-celox", type=Path, help="also validate and time an earlier Celox benchmark")
    parser.add_argument("--output", type=Path, default=ROOT / "target/vcd-verilator-results")
    parser.add_argument("--signals", type=positive, default=256)
    parser.add_argument("--steps", type=positive, default=100_000)
    parser.add_argument("--repeats", type=positive, default=5)
    parser.add_argument("--check-steps", type=positive, default=20)
    parser.add_argument("--check-only", action="store_true", help="build and check waveforms without timing runs")
    parser.add_argument("--reuse-builds", action="store_true", help="reuse binaries verified against an earlier manifest")
    parser.add_argument("--stack-mib", type=positive, default=64, help="stack limit for this process and its children")
    parser.add_argument("--cpu", type=int, default=min(os.sched_getaffinity(0)))
    parser.add_argument("--jobs", type=positive, default=min(8, os.cpu_count() or 1))
    parser.add_argument("--patterns", nargs="+", choices=("counter", "full_width"), default=["counter", "full_width"])
    parser.add_argument("--cases", nargs="+", choices=CASES, default=list(CASES))
    parser.add_argument("--modes", nargs="+", choices=MODES, default=list(MODES))
    args = parser.parse_args()
    if not args.verilator:
        parser.error("Verilator is required; use --verilator /path/to/verilator")
    verilator = str(Path(args.verilator).resolve())
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    previous = json.loads((output / "manifest.json").read_text()) if args.reuse_builds else None
    _, hard_stack = resource.getrlimit(resource.RLIMIT_STACK)
    resource.setrlimit(resource.RLIMIT_STACK, (args.stack_mib * 1024 * 1024, hard_stack))
    environment = {key: value for key, value in os.environ.items() if not key.startswith("VCD_")}
    # Large generated port lists also need more stack in frontend worker threads.
    environment["RUST_MIN_STACK"] = str(args.stack_mib * 1024 * 1024)
    manifest = {
        "options": {key: str(value) if isinstance(value, Path) else value for key, value in vars(args).items()},
        "kernel": os.uname().release,
        "cpu": subprocess.check_output(["lscpu"], text=True),
        "verilator": subprocess.check_output([verilator, "--version"], text=True).strip(),
        "cxx": subprocess.check_output(["g++", "--version"], text=True).splitlines()[0],
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "git_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "git_status": subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True),
        "source_sha256": {str(path.relative_to(ROOT)): digest(path) for path in (HARNESS, RUST_BENCH, Path(__file__))},
        "timing_scope": "steady-state execution and final flush; excludes construction, reset and initial snapshot",
        "environment": {"RUST_MIN_STACK": environment["RUST_MIN_STACK"]},
        "builds": [], "validation": {}, "binaries": {}, "runs": [],
    }

    def build(command, label):
        start = time.monotonic_ns()
        with (output / f"{label}.log").open("w") as log:
            subprocess.run(command, cwd=ROOT, env=environment, stdout=log, stderr=subprocess.STDOUT, check=True)
        manifest["builds"].append({"command": command, "elapsed_ns": time.monotonic_ns() - start})
        write_json(output / "manifest.json", manifest)

    if previous:
        if previous["options"]["signals"] != args.signals or previous["verilator"] != manifest["verilator"]:
            raise ValueError("cached build settings differ; omit --reuse-builds")
        for source in (HARNESS, RUST_BENCH):
            name = str(source.relative_to(ROOT))
            if previous["source_sha256"][name] != digest(source):
                raise ValueError(f"{name} changed; omit --reuse-builds")
        for binary, expected_hash in previous["binaries"].items():
            if digest(Path(binary)) != expected_hash:
                raise ValueError(f"{binary} changed; omit --reuse-builds")
        manifest["builds"] = previous["builds"]
        manifest["reused_builds"] = True
        celox = args.celox.resolve() if args.celox else output / "celox-vcd"
    elif args.celox:
        celox = args.celox.resolve()
    else:
        print("Building the Celox benchmark", flush=True)
        build(["cargo", "bench", "--locked", "-p", "celox", "--bench", "vcd", "--no-run", "--message-format=json"], "build-celox")
        artifacts = []
        for line in (output / "build-celox.log").read_text().splitlines():
            if line.startswith("{"):
                record = json.loads(line)
                if record.get("reason") == "compiler-artifact" and record["target"]["name"] == "vcd" and record.get("executable"):
                    artifacts.append(record["executable"])
        if len(artifacts) != 1:
            raise ValueError("could not identify the Celox benchmark executable")
        celox = Path(artifacts[0])
    celox_engines = cache_celox_binaries(output, celox, args.baseline_celox)
    for binary in celox_engines.values():
        manifest["binaries"][str(binary)] = digest(binary)
    engines = (*celox_engines, "verilator")
    verilated = {}
    for pattern in args.patterns:
        folder = output / pattern
        folder.mkdir(exist_ok=True)
        generate_fixture(folder, args.signals, pattern == "full_width", verify_only=bool(previous))
        for traced in (False, True):
            label = f"{pattern}-{'traced' if traced else 'off'}"
            obj = folder / ("obj_traced" if traced else "obj_off")
            binary = obj / "VTop"
            if previous:
                if str(binary) not in previous["binaries"]:
                    raise ValueError(f"no verified cached build for {binary}; omit --reuse-builds")
                verilated[pattern, traced] = binary
                manifest["binaries"][str(binary)] = digest(binary)
                continue
            print(f"Building Verilator {label}", flush=True)
            command = [verilator, "--cc", "-O3", "--threads", "1", "--top-module", "Top",
                       "--Mdir", str(obj), "--exe", str(HARNESS), str(folder / "Top.sv"),
                       "-CFLAGS", f"-O3 -flto -I{folder}", "-LDFLAGS", "-flto"]
            if traced:
                command.extend(("--trace-vcd", "--no-trace-top", "--no-trace-params"))
            build(command, f"generate-{label}")
            build(["make", "-C", str(obj), "-f", "VTop.mk", f"-j{args.jobs}",
                   "OPT_FAST=-O3", "OPT_SLOW=-O3", "OPT_GLOBAL=-O3"], f"build-{label}")
            binary = obj / "VTop"
            verilated[pattern, traced] = binary
            manifest["binaries"][str(binary)] = digest(binary)
            write_json(output / "manifest.json", manifest)
    # Build first; only benchmark subprocesses inherit this affinity.
    os.sched_setaffinity(0, {args.cpu})

    def run(engine, mode, case, pattern, steps, destination, label):
        full_width = pattern == "full_width"
        options = {}
        if engine in celox_engines:
            options = {"VCD_BACKEND": "native", "VCD_SIGNALS": str(args.signals),
                       "VCD_STEPS": str(steps), "VCD_REPEATS": "1", "VCD_CASE": case,
                       "VCD_MODE": "dirty" if mode == "vcd" else mode,
                       "VCD_FULL_WIDTH": str(int(full_width)), "VCD_OUTPUT": str(destination)}
            command = [str(celox_engines[engine])]
        else:
            command = [str(verilated[pattern, mode != "off"]), mode, case, str(steps), str(destination)]
        result = subprocess.run(command, env={**environment, **options}, capture_output=True, text=True)
        (output / f"{label}.csv").write_text(result.stdout)
        (output / f"{label}.log").write_text(result.stderr)
        result.check_returncode()
        rows = list(csv.DictReader(io.StringIO(result.stdout)))
        if len(rows) != 1:
            raise ValueError(f"{label}: expected one CSV result")
        row = rows[0]
        if (int(row["signals"]), int(row["steps"]), row["case"]) != (args.signals, steps, case):
            raise ValueError(f"{label}: unexpected benchmark settings: {row}")
        # Celox reports value-line bytes separately; its writer emits every
        # requested timestamp, including the unchanged first low clock phase.
        total_bytes = int(row.get("bytes", 0))
        if engine in celox_engines and mode == "vcd":
            total_bytes = int(row["value_bytes"]) + sum(len(str(t)) + 2 for t in range(2, steps * 2 + 2))
        manifest["runs"].append({"label": label, "command": command, "environment": options})
        return {"engine": engine, "mode": mode, "case": case, "pattern": pattern,
                "elapsed_ns": int(row["elapsed_ns"]), "bytes": total_bytes}

    print("Validating both waveforms against the counter oracle", flush=True)
    for pattern in args.patterns:
        for case in args.cases:
            expected = expected_waveform(args.signals, args.check_steps, case, pattern == "full_width")
            for engine in engines:
                label = f"check-{pattern}-{case}-{engine}"
                path = output / f"{label}.vcd"
                run(engine, "vcd", case, pattern, args.check_steps, path, label)
                manifest["validation"][label] = validate_waveform(path, expected)
    write_json(output / "manifest.json", manifest)
    if args.check_only:
        print(f"All waveforms validated; results: {output}", flush=True)
        return

    samples = []
    for pattern in args.patterns:
        for case in args.cases:
            # Warm both binaries/modes before collecting independent processes.
            for mode in args.modes:
                for engine in engines:
                    run(engine, mode, case, pattern, args.steps, "/dev/null",
                        f"warm-{pattern}-{case}-{mode}-{engine}")
            for repeat in range(args.repeats):
                start = repeat % len(engines)
                engine_order = engines[start:] + engines[:start]
                modes = args.modes if repeat % 2 == 0 else tuple(reversed(args.modes))
                for mode in modes:
                    for engine in engine_order:
                        row = run(engine, mode, case, pattern, args.steps, "/dev/null",
                                  f"sample-{pattern}-{case}-{mode}-{engine}-{repeat}")
                        samples.append({**row, "repeat": repeat})
            print(f"Measured {pattern} {case}", flush=True)
            write_json(output / "samples.json", samples)
            write_json(output / "manifest.json", manifest)

    summary = []
    for pattern in args.patterns:
        for case in args.cases:
            for mode in args.modes:
                line = {"pattern": pattern, "case": case, "mode": mode}
                for engine in engines:
                    rows = [row for row in samples if (row["pattern"], row["case"], row["mode"], row["engine"]) == (pattern, case, mode, engine)]
                    times = [row["elapsed_ns"] for row in rows]
                    byte_counts = {row["bytes"] for row in rows}
                    if len(byte_counts) != 1:
                        raise ValueError(f"{engine} {pattern} {case}: output sizes changed between repetitions")
                    line[engine] = {"median_ns": statistics.median(times), "min_ns": min(times),
                                    "max_ns": max(times), "bytes": byte_counts.pop()}
                line["celox_over_verilator"] = line["celox"]["median_ns"] / line["verilator"]["median_ns"]
                baseline_text = ""
                if "baseline_celox" in line:
                    line["celox_over_baseline"] = line["celox"]["median_ns"] / line["baseline_celox"]["median_ns"]
                    baseline_text = f'Baseline {line["baseline_celox"]["median_ns"] / 1e6:9.3f} ms, '
                summary.append(line)
                print(f"{pattern:10} {case:6} {mode:12} "
                      + baseline_text +
                      f'Celox {line["celox"]["median_ns"] / 1e6:9.3f} ms, '
                      f'Verilator {line["verilator"]["median_ns"] / 1e6:9.3f} ms, '
                      f'Celox/Verilator {line["celox_over_verilator"]:.3f}x', flush=True)
    write_json(output / "summary.json", summary)
    print(f"All waveforms validated; results: {output}", flush=True)


if __name__ == "__main__":
    main()
