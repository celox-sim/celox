# Timing-split candidate: emitted-SystemVerilog differential simulation

Run from repository root (absolute output/tools paths required):

```sh
python audit/veryl_scaling/split_candidate_tests/run_sv.py \
  --sv "$PWD/synthesis/rv32i-split-candidate/rtl/rv32i_pipeline.sv" \
  --out /workspace/shared/rv32i-split-sv-tests \
  --tools /workspace/scratch/07f10565586c/timing-prep/oss-cad-suite/bin
```

Boundary: the actual Veryl-emitted candidate SystemVerilog, compiled and run by
Verilator 5.053, compared at retirement and after every edge against the existing
handwritten independent integer oracle `rv32i_isa.execute`. The test memory is
synthetic Harvard memory obeying the documented aligned-word, atomic-W-store,
same-edge-M-load contract, with independently computed lane replacement.
This is finite simulation, not solver evidence or universal refinement/progress.

Verified candidate SV SHA256:
`fbf7ddb898859df4c600b91ce1fd960112dc69309e9eff8f10890068f00943df`.

Result: 171 programs, 4,803 retirements, all 40 RV32I names, 37 concurrent
same-edge W-store/M-load collisions, 1,842 stalled cycles. Checks include all
89 sequential variables frozen under stall and reset to zero after a midflight
reset while stalled; both operand paths, M-over-W youngest-value forwarding,
mixed hazards, x0 and aliases, signed and shift edges, byte/half/word lanes,
consecutive partial stores, exceptions and priorities, precise trap PC,
redirect/trap killing younger stores and faults, halt pulse suppression, and
repeated-PC dynamic instruction instances.

A dynamic FIFO gives each accepted fetch a globally unique serial number.
Own retirement consumes the oldest token; older W redirect/trap removes younger
tokens. PC is a payload sanity check, not token identity. Observed enabled-edge
acceptance-to-own-retirement histogram: 4 edges = 4,368; 5 edges = 350;
6 edges = 85. These observations do not establish a universal bound.

Sensitivity checks performed on disposable copies of emitted SV, never source:
- Replace M→D operand1 forwarding with W result: rejected at case 5, cycle 12,
  x2 actual 0 versus expected 0x80000000
- Disable hazard: rejected at case 1, cycle 11, x3 actual 0 versus expected 1

Full generated testbench, case encodings, compilation log, simulation trace and
mutant rejection logs remain under `/workspace/shared/rv32i-split-sv-tests/`.
`report.json` captures the passing run; `run_sv.py` regenerates the suite.

## RHS-selection candidate 2

Candidate 1 evidence above remains unchanged. Candidate 2 is separately tested
from `synthesis/rv32i-split-rhs-candidate/rtl/rv32i_pipeline.sv` with SHA256
`935e67d45f40482b41c3dbe88aace58623a3b0e4ad7976734de893eab812307c`.
It passes the identical 171 programs / 4,803 retirements / 37 store-load
collisions / 1,842 stall cycles / all 40 instruction names. All **88** actual
sequential variables pass full-state stall/reset checks (RHS is combinational
in this candidate). Dynamic-token latency histogram and first witnesses are
identical to candidate 1. Both mutations are again detected by simulation
assertions, at the same architectural mismatches as candidate 1.

```sh
python audit/veryl_scaling/split_candidate_tests/run_sv.py \
  --sv "$PWD/synthesis/rv32i-split-rhs-candidate/rtl/rv32i_pipeline.sv" \
  --out /workspace/shared/rv32i-split-rhs-sv-tests \
  --tools /workspace/scratch/07f10565586c/timing-prep/oss-cad-suite/bin
python audit/veryl_scaling/split_candidate_tests/run_mutations.py \
  --sv "$PWD/synthesis/rv32i-split-rhs-candidate/rtl/rv32i_pipeline.sv" \
  --out /workspace/shared/rv32i-split-rhs-sv-tests/mutants \
  --tools /workspace/scratch/07f10565586c/timing-prep/oss-cad-suite/bin
```

Passing report: `rhs-report.json`. Negative controls: `rhs-mutations-report.json`.
Candidate 2 full traces and generated artifacts: `/workspace/shared/rv32i-split-rhs-sv-tests/`.

## Predecode/masked-forwarding candidate 3

Prior variant evidence remains unchanged. Candidate 3 emitted SV SHA256:
`497b1f84697734f43159097ace7939e4a4237abd590dabe361e1eb9a8edc17bd`.
Actual Verilator simulation passes 1,227 programs / 44,963 retirements:
- All original 171 programs, unchanged
- 1,024 rs1 × rs2 selector-pair cases, each initialized architecturally with
  distinct values across all 32 registers, followed by operand/destination aliases
- 32 register-index-specific M-over-W both-source forwarding tests

The selector cases initialize registers 1–31 with `37*r - 500`, with x0 zero,
then drain setup dependencies before reading the tested pair. Thus testing
explicit r31 cases and both full 5-bit selector ranges does not rely on equal
register values. The forwarding cases use consecutive older value 1 and younger
value 2, detecting erroneous masked-OR accumulation as well as older priority.

Results: all 40 instruction names, 37 same-edge store/load collisions, 15,690
stalled cycles, full-state freeze/reset for all 89 actual sequential variables.
Observed dynamic-token enabled-edge latency histogram: 4 = 43,505;
5 = 1,373; 6 = 85. This remains finite evidence, not a universal bound.
Both disposable incorrect-M-forwarding and disabled-hazard mutants are rejected.
The mutation runner locates the serial or masked-OR forwarding form explicitly;
it requires exactly one mutation target and an actual simulation assertion failure.

```sh
python audit/veryl_scaling/split_candidate_tests/run_sv.py --all-selectors \
  --sv "$PWD/synthesis/rv32i-split-predecode-candidate/rtl/rv32i_pipeline.sv" \
  --out /workspace/shared/rv32i-split-predecode-sv-tests \
  --tools /workspace/scratch/07f10565586c/timing-prep/oss-cad-suite/bin
python audit/veryl_scaling/split_candidate_tests/run_mutations.py \
  --sv "$PWD/synthesis/rv32i-split-predecode-candidate/rtl/rv32i_pipeline.sv" \
  --out /workspace/shared/rv32i-split-predecode-sv-tests/mutants \
  --tools /workspace/scratch/07f10565586c/timing-prep/oss-cad-suite/bin
```

Reports: `predecode-report.json`, `predecode-mutations-report.json`.
Full traces: `/workspace/shared/rv32i-split-predecode-sv-tests/`.

## One-hot RF-selector candidate 4

Emitted SV SHA256:
`af51ab74d0bf793b51d4207070405054a190ff4612c3df5444afabf333c5fa12`.
Passes the same expanded 1,227 programs / 44,963 retirements / all 40 instruction
names / 1,024 selector pairs / 32 register-index M-over-W tests. Results remain
37 same-edge store/load collisions, 15,690 stalls, and observed latency histogram
4 = 43,505 / 5 = 1,373 / 6 = 85.

All 91 actual sequential variables pass stall freeze and midflight reset while
stalled. The test expects the two selector registers to reset to **1**, all other
state to zero. New direct assertions after every simulated edge establish
`d_sel1 == 1 << d_ir[19:15]` and `d_sel2 == 1 << d_ir[24:20]`, unconditionally on
D-valid, covering reset, normal advance, hazard holds, global stalls, redirects,
traps and halted invalid payloads. No testbench reset assumption was changed for
prior variants: selector-specific reset expectations apply only when these two
actual sequential variables exist.

Three disposable mutants are rejected: incorrect M forwarding, disabled hazard,
and rs2 selector capturing rs1's instruction field. The third fails the direct
selector-identity assertion during warmup at simulation time 14.

```sh
python audit/veryl_scaling/split_candidate_tests/run_sv.py --all-selectors \
  --sv "$PWD/synthesis/rv32i-split-onehot-candidate/rtl/rv32i_pipeline.sv" \
  --out /workspace/shared/rv32i-split-onehot-sv-tests \
  --tools /workspace/scratch/07f10565586c/timing-prep/oss-cad-suite/bin
python audit/veryl_scaling/split_candidate_tests/run_mutations.py \
  --sv "$PWD/synthesis/rv32i-split-onehot-candidate/rtl/rv32i_pipeline.sv" \
  --out /workspace/shared/rv32i-split-onehot-sv-tests/mutants \
  --tools /workspace/scratch/07f10565586c/timing-prep/oss-cad-suite/bin
```

Reports: `onehot-report.json`, `onehot-mutations-report.json`.
Full traces: `/workspace/shared/rv32i-split-onehot-sv-tests/`.
Earlier variant reports remain untouched. These are finite simulation checks,
not universal refinement or latency proofs.
