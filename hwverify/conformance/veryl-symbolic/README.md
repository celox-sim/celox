# Symbolic Veryl pipeline refinement

This is the first actual HDL implementation connected to the existing ISA
refinement checker. `pipeline.veryl.in` is a handwritten D/X/W pipeline; template
substitution changes only widths. The transition implementation is **not**
generated from the ISA. `run.py` reuses the separately written sequential ISA,
state binding and rank from `examples/build_branch_pipeline.py`, discards that
file's implementation expressions, then supplies expressions lifted from actual
Veryl → Celox ScheduledRtl/SIR by `hwverify-sir`.

## Model

* Two GPRs, 2-bit architectural/fetch PC, four arbitrary instruction words and
  four arbitrary read-only data words; data widths 4/8/16/32, instruction width W+5
* MOVI, ADD-immediate, XOR-immediate, LOAD, branch-if-zero, nonwriting opcodes
* X/W forwarding, load-use interlock, X-resolved branch and younger squash
* Arbitrary stall input; pre-edge commit observes the **old** W stage
* Active-low synchronous reset is an ordinary bit input, explicitly mapped to
  the checker's active-high reset. ROM/data are captured from unconstrained seed
  inputs on reset and retained afterward; there is no hidden stable-input premise
* Two-state synchronous single-clock semantics; not a four-state/event-simulator
  equivalence claim, arbitrary memory size, full ISA or synthesized hardware proof

The lifter starts from arbitrary selected state, executes comb symbolically,
observes commit, then executes the clock's FF/apply phase. Conditional branches
produce guarded expressions. No solver determines branch choices or a unique
concrete value. Reset is separately lifted with reset asserted from arbitrary
prestate. A reset expression still depending on prestate is conservatively
rejected, never zero-initialized to make it pass.

## Acceptance

Each correct width must satisfy all eight existing refinement/nonvacuity/hold/
conditional-progress obligations. Five separately compiled source mutations
(remove flush, wrong target, remove forwarding, remove interlock, wrong ADD)
must produce a SAT counterexample validated against the original query. A sixth
mutation removes r0 reset and must be rejected for state-dependent reset.
UNKNOWN, timeout, compile failure or absent validated SAT does not count as
successful mutant detection. Counterexamples satisfy the inductive relation;
that alone does **not** establish reset reachability.

The frontend, SIR lifter, lowering, rewrite kernel and custom finite solver remain
trusted. UNSAT logs are not independently checkable certificates. The core report's
legacy “no RTL import or synthesis claim” describes its own IR boundary; this
wrapper adds the explicitly trusted Veryl/SIR import, not a synthesis claim.

## Reproduce

First build the checksum-pinned frontend using `conformance/veryl-proof/run_ci.sh`
(or its documented prepare/build steps). Then run:

```sh
./conformance/veryl-symbolic/run_ci.sh /tmp/fresh-symbolic-evidence
```

The entry point builds the Rust lifter/checker, runs lifter tests and the full
4-width × 7-case matrix, and writes a terminal run-status file even on failure.
It installs a Z3 tripwire; no Z3 fallback is allowed. The whole-corpus CI job runs
this step after the original conformance suite and uploads its own evidence.
For iteration, `run.py --out FRESH_DIR --widths 4 32` accepts explicit frontend,
lifter and checker paths. `--faults` with no arguments runs only correct designs.

The CI entry point also executes the scaling regression gates: 18 correct/mutant
component cases at16/32/64 sizes and28 CPU-capacity cases at8/16/32/64 ROM/data
words, retaining two GPRs. The separate register axis holds ROM/data at four words
and exercises8/16/32 writable GPRs, with42 normal/mutant/reset cases and an
independent concrete integer-ISA regression over the imported DUT.

See `audit/veryl_scaling/README.md` and `audit/veryl_scaling/CPU-REGISTERS.md` for
actual measurements, fixed budgets, semantic scope and reproduction. Neither
axis implies combined maximum capacity/register coverage or RV32I support.
