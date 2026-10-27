# Correctness of memory simplification

The target is `memory_read` / `memory_write` in `crates/aplite/src/term.rs`. Only Lean 4.34.1 + Std are used.

## What is proved

`MemoryRules.lean` defines a mutually recursive AST of values and memories, its semantics, and executable smart constructors.

- `memoryRead_sound`: for any memory expression, address and natural-number budget, the evaluated value of the simplified result equals that of the original read
- `memoryWrite_sound`: for any memory expression, address and written value, the entire memory of the simplified result equals that of the original write

Correspondence with the four Rust rules:

| Rust rule | Lean model |
|---|---|
| read_after_same_write | If the store and read addresses are syntactically identical, return the latest value |
| read_after_write_alias_split | Even if the syntax differs the actual addresses may coincide, so branch on equality of the interpreted values |
| constant_memory_read | Return the constant memory's value, including when budget=0 |
| last_write_wins | Remove exactly one immediately preceding store to a syntactically identical address |

Address syntax A and actual addresses D are separated, and an arbitrary interpretation function A → D is used. Injectivity of the interpretation function is not assumed. No particular restriction is placed on address width, memory capacity or value type. Rust's BitVec/SMT arrays can be read as instances of this model.

A read that exhausts its budget is left as the original read. The constant-memory rule does not depend on the budget. This is not a proof of a compiler that rewrites the whole tree at once; it proves meaning preservation of the two smart constructors that Rust actually uses.

## Extent of the guarantee

What the Lean kernel checks is this typed model. It is not automatically extracted from the Rust program, and the correspondence with the Rust implementation is not yet formally proved.

Rust has string ops, Vec arguments and runtime Sorts. That inputs are well-formed so as to correspond to the Lean typed AST, soundness of the syntactic equality check, frontend type checking, the Rc/Vec implementation, SMT output, proof obligation generation, and Z3 are outside the scope of these theorems. In particular, Rust's `starts_with("(as const ")` check assumes correct nodes produced by the frontend. This proof alone does not mean the whole system is verified.

`#print axioms` reports `propext, Quot.sound` for both soundness theorems. No sorry, new axioms or native_decide are used.

## Cross-checking the implementation and negative examples

`check_rust_rules.rs` directly imports the real `../../crates/aplite/src/term.rs` and compares it against an independent evaluator that does not use Z3.

- All 256 memories with 2-bit addresses and 2-bit words
- All 64 assignments of 3 address variables, including actual-address collisions under identical and different syntax
- All 16 pairs of values for two writes, 2 kinds of write-address syntax, 3 kinds of read addresses, budget 0..3
- 6,291,456 read comparisons and 524,288 write comparisons
- Constant-memory reads and execution of all four rules are also confirmed

This is differential testing over a finite range, not a proof over all Rust inputs.

`recheck.py` also applies faulty changes to temporary copies of both Lean and Rust, "assume non-alias whenever the syntax differs" and "keep the first write", and confirms they are rejected by proof failure and by concrete value mismatches. The original sources are not modified. It confirms not just a failing exit but the Lean diagnostic / Rust semantic mismatch.

## Rerun

With Lean 4.34.1 and Rust on PATH:

```
python3 proof/recheck.py --lean lean --rustc rustc
```

Each command has a 120-second limit. `proof/results.json` records execution times, diagnostics and the SHA256 of the target sources. There is no need to rewrite machine-dependent paths for distribution to make the proof go through.

The proofs go through with only Lean's basic case analysis and budget induction. A single Lean check took about 0.5 seconds (exact measurements are in results.json). The justification for this small set of memory rules can be maintained without relying on general automated search. On the other hand, the correspondence from Rust to this model remains the next open issue, and the parts covered by tests are not to be confused with proved ones.


## Inference rules for program contracts

`ProgramRules.lean` adds `total_correctness`. Assuming an initial invariant, preservation while active, strict decrease of a natural-number rank, and post at terminal, it reaches terminal with post within rank(initial) steps. Unsigned BitVec values can be interpreted as natural numbers, so this provides the mathematical justification for the obligations the checker generates. `lift_invariant` carries an invariant to the implementation step corresponding to one specification step or a stutter.

These theorems have not been formally instantiated for a concrete CPU or Rust's obligation generator. Nor is this a mechanism by which Lean reads and checks Z3 results. Composition with the implementation's progress conditions and the environment conditions of external stall/reset remain as the trust boundary described in [program contracts](../docs/program-contracts.md).

`total_correctness` depends on the standard propext/Classical.choice/Quot.sound, and `lift_invariant` only on propext. `recheck.py` reruns the checks of both files.
