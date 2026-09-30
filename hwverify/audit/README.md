# Independent audit

The concrete evaluator imports no Z3. `cpu_simulation.py` uses a separately
written integer ISA oracle, rather than the shared expression generator used
by the example specification and implementation. It checks 7,300 cycles and
1,161 commits across 23 trials. Coverage includes all opcodes, unused encoding
bits, wraparound, a branch-to-self program, arbitrary stall, mid-run reset,
and changes to the live program input that must have no effect without reset.

`replay_counterexamples.py` parses solver get-value S-expressions as data,
without eval or Z3. It confirms 12 counterexamples from eight faulty models.
It checks concrete next states, relation truth values, commit eligibility,
stall preservation, and rank decrease as relevant. A relation-induction
countermodel is not claimed to be reachable from reset.

`split_counter.json` is a second, non-CPU consumer with different state layouts:
a single 16-bit abstract count versus two 8-bit registers and a phase bit. It
passes; an incorrect carry update is rejected by the same Rust engine.

These tests and replays do not independently certify UNSAT results or prove
correctness of the Rust checker. Read SCHEMA.md and VERIFICATION-ja.md for the
actual trust boundary and the conditional nature of progress.
