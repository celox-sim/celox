#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
source env.sh
"$FPGA_YOSYS" -V >logs/yosys-version.log
"$FPGA_NEXTPNR" --version >logs/nextpnr-version.log 2>&1
"$FPGA_YOSYS" -Q -T -p 'read_verilog -sv smoke/tool_smoke.sv; synth_ecp5 -top ToolSmoke -json smoke/tool_smoke.json' >logs/yosys-tool-smoke.log 2>&1
"$FPGA_NEXTPNR" --45k --package CABGA381 --speed 6 --freq 100 --seed 1 --out-of-context --json smoke/tool_smoke.json --report smoke/tool_smoke.report.json >logs/nextpnr-tool-smoke.log 2>&1
"$VERYL_TIMING_EMIT" smoke/rv32i_pipeline.snapshot.veryl smoke/rv32i_pipeline.sv >logs/emit-smoke.log 2>&1
"$FPGA_YOSYS" -Q -T -p 'read_verilog -sv smoke/rv32i_pipeline.sv; hierarchy -check -top RV32IPipeline' >logs/rv32i-syntax.log 2>&1
