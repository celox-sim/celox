#!/usr/bin/env python3
"""Run small SystemVerilog probes without the Veryl emitter or VPI adapters."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def execute(tool, source, output, public):
    name = source.stem + ("_public" if public else "")
    directory = output / tool / name
    directory.mkdir(parents=True, exist_ok=True)
    if tool == "icarus":
        build = ["iverilog", "-g2012", "-gstrict-expr-width", "-s", "Top", "-o", str(directory / "model.vvp"), str(source)]
        run = ["vvp", str(directory / "model.vvp")]
    else:
        build = ["verilator", "--binary", "--timing", "-Wno-fatal", "--top-module", "Top", "--Mdir", str(directory / "obj"), "-CFLAGS", "-O0", str(source)]
        if public:
            build += ["--public-flat-rw"]
        run = [str(directory / "obj/VTop")]
    if source.stem == "integer_edges":
        run += ["+a=8000000000000000", "+b=ffffffffffffffff", "+x=ff", "+z=01"]
    status, observed = "executed", []
    with (directory / "build.log").open("w") as log:
        result = subprocess.run(["timeout", "120s", *build], stdout=log, stderr=subprocess.STDOUT)
    if result.returncode:
        status = "compile_error"
    else:
        with (directory / "runtime.log").open("w") as log:
            result = subprocess.run(["timeout", "30s", *run], stdout=log, stderr=subprocess.STDOUT)
        if result.returncode:
            status = "runtime_error"
        observed = [line for line in (directory / "runtime.log").read_text().splitlines()
                    if line.startswith(("review:", "signed_division="))]
    detail = ""
    if status != "executed":
        log = "build.log" if status == "compile_error" else "runtime.log"
        detail = (directory / log).read_text().replace(str(ROOT), "<crate>").replace(str(directory), "<output>")[:6000]
    print(f"{tool}: {name}: {status}", flush=True)
    return {"tool": tool, "probe": name, "status": status, "observed": observed, "detail": detail}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=Path("/tmp/veryl-mismatch-probes"))
    parser.add_argument("--report", type=Path, default=ROOT / "verification/repros/mismatches/results.json")
    args = parser.parse_args()
    for tool in ("verilator", "iverilog", "vvp", "timeout"):
        if not shutil.which(tool):
            parser.error(f"{tool} must be on PATH")
    versions = {name: subprocess.check_output([command, flag], text=True, stderr=subprocess.DEVNULL).splitlines()[0]
                for name, command, flag in [("verilator", "verilator", "--version"), ("icarus", "iverilog", "-V")]}
    sources = sorted((ROOT / "verification/repros/mismatches").glob("*.sv")) + [ROOT / "verification/repros/integer_edges.sv"]
    jobs = [(tool, source, False) for source in sources for tool in ("verilator", "icarus")
            if not (source.stem == "shift_xz" and tool == "verilator")]
    jobs.append(("verilator", ROOT / "verification/repros/mismatches/read_before_write.sv", True))
    with ThreadPoolExecutor(max_workers=4) as pool:
        futures = [pool.submit(execute, tool, source, args.output.resolve(), public) for tool, source, public in jobs]
        rows = [future.result() for future in futures]
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps({"versions": versions, "probes": rows}, indent=2) + "\n")
    print(f"Observations (not a conformance verdict) saved to {args.report}")
    return int(any(row["status"] != "executed" for row in rows))


if __name__ == "__main__":
    raise SystemExit(main())
