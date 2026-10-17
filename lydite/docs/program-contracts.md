> The measurements on this page are records from 0.3. 0.4 added automatic selection that omits manual splits.
> New measurements and remaining limitations are in [proof search and partitioning](automatic-proofs.md).

# From an ISA array sum to a fetch/execute implementation

## Scope of the result

This adds an example that uses Rust+Z3 to check the initial condition, inductive invariant,
postcondition at halt, and a finite rank for the sum (mod 256) of an arbitrary 8-bit array of a fixed 16 elements.
The CPU state-correspondence check on the same JSON also passes, so the ISA-level result can be connected to the
fetch/execute implementation.

This is not a check that unrolls 33 instructions of bounded execution. It proves one-instruction preservation and
rank decrease for any state within the invariant. However, the specification's prefix sum over the fixed 16 elements is
expanded as a finite expression. It is not a proof for variable-length arrays or for unbounded-width integers in general.

## Program and ISA extensions

The old `cpu.json` is unchanged. Only the new `array_sum.json` adds an 8-bit index,
and gives opcodes 6/7, formerly reserved NOPs, the following meanings.

- ADDX: adds data[idx] to ACC mod256
- IXJ addr: increments idx by 1 mod256, and branches to addr if the new idx is nonzero

A data input and a start_index input are added to the reset interface. The precondition is
start_index=240, the data is an arbitrary array p.data, and the program is the following 3 instructions.

```
0: ADDX
1: IXJ 0
2: HALT
```

It reads the 16 elements at addresses 240..255 and HALTs after 33 ISA instructions. Data memory is not modified.
The existing structure in which the ISA and implementation share datapath expressions remains, so this is not
verification of independently written RTL. A concrete comparison against an independent integer ISA oracle is done separately.

## Contract

p.data is a ghost parameter fixed throughout the whole execution. It is not a live input.

- PC=0: 240≤idx≤255, ACC = prefix sum from the start up to just before idx
- PC=1: same index range, ACC = prefix sum + data[idx]
- PC=2: idx=0, ACC = sum of all 16 elements, not yet HALTed
- PC=3: idx=0, ACC = sum of all 16 elements, HALTed
- In every state the program is fixed and mem=p.data

The rank is a 10-bit unsigned value: 2*(256-idx)+1 at PC=0, 2*(256-idx) at PC=1,
1 at PC=2, and 0 once HALTed. The precondition gives an initial rank of 33, which decreases with each ISA instruction.
The implementation binding keeps the ISA state during stutter, and the noncommit rank checks progress
from fetch to execute. Without another reset after reset and with stall=False, it terminates in 66 microcycles.
Termination under an external stall that persists, or under repeated reset, is not claimed.

## Reusable obligations

`src/program.rs` reads the contract and generates: nonvacuity of pre, initialization with the reset input actually asserted, invariant preservation, step availability,
rank decrease, postcondition, and prohibition of steps after halt. Input-dependent invariants are rejected.
Even if the program is wrong, the refinement that "the implementation correctly executes that wrong program" still passes.
This is why separately requiring program-side obligations matters.

## Automation and remaining manual work

The first single preservation obligation was UNKNOWN under a 10-second limit. Failure is not turned into success.
The reproduction input is `examples/array_sum_baseline_unsplit.json`, and the raw evidence is
`results/array_sum_baseline_unsplit/`. The working version keeps the same specification expressions and uses two compact split hints,
pc0..2 / idx240..255. Rust builds the per-value splits, the out-of-range other case and their product,
and also checks coverage. No one needs to write a lemma/simp for each case.

However, choosing the invariant, rank, specification expressions and split hints is still manual. This does not demonstrate full automation,
a fundamental fix for SMT performance problems, or scalability to arbitrary programs.
See `proof/README.md` for the formalization of normalization. The trust gap with Rust/Z3 remains.

## Rerun

```
python examples/build_array_sum.py
cargo test --locked
cargo run --locked -- examples/array_sum.json --out results/array_sum --z3 /path/to/z3
python audit/array_sum_simulation.py
```

The negative examples are a wrong loop target, a wrong indexed load, a wrong post, a false invariant, a false pre,
a non-decreasing rank, an input-dependent invariant, and a missing case. Solver evidence is also
saved under each test-evidence directory. The existing tests that do not treat UNKNOWN/abnormal output as success are kept.

## Measured verification and additional trust boundaries

The final test list is `cargo test --locked`. The successful array-sum example has 84 obligations. One measured run took about 1.986 s wall time, with about 0.986 s total query time in the report.
Query time includes Z3 process startup and the like, but Rust expression construction and SMT output generation are not included in the total.
The unsplit version of the same contract times out at 10 seconds on the preservation obligation. The independent concrete ISA comparison
passed with 40 trials, 3310 microcycles and 1320 commits. The independent audit of the old CPU also
passed again with 23 trials, 7300 cycles and 1161 commits.

```
python audit/replay_program_counterexamples.py
```

A check that replays program counterexamples returned by Rust/Z3 in an independent IR evaluator was also added.
`audit/array_sum_verification_results.json` lists each example's status, time and failed obligations.

`proof/ProgramRules.lean` covers the abstract theorem that derives termination and post from an inductive invariant and
a decreasing natural-number rank, and a 1-step stuttering lift. Formal correspondence with Rust's obligation generation,
connection to this concrete CPU's bitvector rank, and checking proof evidence for SMT UNSAT have not
been completed. This boundary is also stated explicitly in `proof/README.md`.
