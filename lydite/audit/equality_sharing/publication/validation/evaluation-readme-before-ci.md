# Compound equality-sharing evaluation

These deterministic refinement fixtures compare encoded state, rather than
placing restrictions on live inputs. Each machine retains a multiword encoding
of a logical value and computes a new output from it. The state relation equates
those compound expressions. Every live input remains arbitrary; reset separately
establishes the relation and the checker separately tests non-vacuity.

The cases contain no proof programs, saved results, source-name hints, numeric
term IDs, user-written proof recipes, or narrowed symbolic input domains.

## Families and evaluation roles

- `compound_sum_transport`: transport an asserted sum equality through a multiplier
- `compound_product_transport`: a compound product equality below a second multiplier
- `transitive_encoded_transport`: two equality edges connect sum, XOR, and a second sum encoding; the output needs their transitive consequence
- `reversed_transitive_mac_shift`: reversed edge orientations and a multiply/add/variable-shift wrapper
- `guarded_compound_transport`: the compound relation is available only while a retained state guard is true
- `nested_guard_compound_shift`: two nested implication guards with a variable shifter (no-regression control if the baseline passes)
- `nested_guard_compound_mac`: two nested implication guards with multiply/add/variable-shift transport
- `branched_compound_reuse`: the same compound root equality is needed in both sides of a retimed input/output mux, testing actual checked-handle reuse

The 32-bit shapes were developed alongside the mechanism. In particular, direct
sum/product and nested-guard MAC results informed implementation and cofactor
work; these are development cases, not independently unseen hold-outs. Transitive
paths, reversed orientation/multiple wrappers, and explicit branch reuse extend
the measured shapes, without an independence claim. The 24-bit and alpha-renamed
variants were frozen before the final candidate runs as additional generalization
controls. Width and alpha-renaming are not additional logical families.

The implementation is bounded, goal-directed, checked equality-path reuse. It is
not full congruence saturation or E-matching, and does not claim to reproduce Lean
grind or a commercial equivalence engine.

Each positive has an output-plus-one mutant and a mutant restricted to the
complement of an arbitrary live control. `guarded_fact_escape_wrong` additionally
uses a compound equality outside its active guard. The guarded resets deliberately
permit different inactive encodings, so every supplied negative has a concrete
counterexample reachable one step after reset.

## Reproduce

Run from the checkout root with an immutable checker and a fresh output path:

```sh
python -m unittest audit.equality_sharing.test_generate audit.equality_sharing.test_evidence
python -m audit.equality_sharing.run --checker PHASE1_CHECKER --out /fresh/phase1 --width 32 --automatic
python -m audit.equality_sharing.run --checker PHASE2_CHECKER --out /fresh/phase2 --width 32 --automatic
python -m audit.equality_sharing.run --checker PHASE1_CHECKER --out /fresh/phase1-heldout --width 24 --automatic --names compound_product_transport transitive_encoded_transport nested_guard_compound_mac
python -m audit.equality_sharing.run --checker PHASE2_CHECKER --out /fresh/phase2-heldout --width 24 --automatic --names compound_product_transport transitive_encoded_transport nested_guard_compound_mac
python -m audit.equality_sharing.run --checker PHASE2_CHECKER --out /fresh/phase2-alpha --width 24 --automatic --alpha-seed 9413 --names transitive_encoded_transport transitive_encoded_transport_wrong_result transitive_encoded_transport_complement_wrong
```

The runner strips ambient `LYDITE_*` settings, enables finite-only solving and
independent-lemma automatic proofs, leaves all per-primitive defaults unchanged,
and supplies an external-solver sentinel that must never run. Every exact input,
complete report, checker SHA-256, elapsed time, primitive-work total, and separate
(overlapping) proof-bundle-work total is retained. Only paired identical input
hashes with phase1 Unknown and complete phase2 verification count as new coverage.
A primitive auxiliary SAT is never itself a rejected original theorem; negative
acceptance requires the final report to reject the actual refinement and the
relevant original formula to have a replay-validated model.

The concrete Python tests independently check reset establishment, preservation
for sampled arbitrary related state encodings, both guard values, widths 4/24/32/64,
alpha-renaming, and reset-reachable negative witnesses. Those tests are sanity
checks, never authority for a universal proof. Successful solver results still
trust the live Rust finite solver and proof kernel, not serialized diagnostics.

## Canonical measured results

`evidence-summary.json` records the final paired runs. The phase1 executable hash
is `64b98b2ad38dc8860c44e330c1e53c7de85dbe1a67204e45249e5e55392c3cf9`;
the final phase2 hash is
`4d2d6f973f054117a1c9c0deacf85abdcb05c57d8b2fd03cc978d12cde86fd10`.
Both were built with the same release compiler/profile. Their original attempts
and every auxiliary query are retained, including exhausted attempts.

- 13 positive instances, with 8 phase1 Unknown → complete phase2 verification gains
- 5 gaining fixture shapes at 32 bits: sum transport, transitive transport, reversed transitive MAC/shift, guarded transport, and cross-branch reuse
- 2 additional gains for transitive/reversed-transitive shapes at 24 bits, plus 1 alpha-renamed transitive instance; these are generalization checks, not new logical shapes
- 19/19 negative instances rejected with replay-validated original-formula SAT models
- Every paired input is byte-identical; the existing 32-bit nested-guard shift pass is preserved
- Product transport and nested-guard MAC remain Unknown at both 24 and 32 bits; no complete coverage claim is made
- 8 concrete-generator and audit-accounting tests pass

The cross-branch case records one freshly proved root edge used in two paths,
with two exact guard discharges and a cached-handle reuse. Reversed/transitive
cases separately exercise composed equality paths. This is evidence for the
specific bounded mechanism, not a claim of general equivalence-class saturation.

The retained `pre-cofactor-evidence-summary.json` is historical screening data;
its exact input JSON and report paths remain available. Use `evidence-summary.json`
for the final source-bound result. The canonical result can be regenerated with
`compare.py --pair BASELINE_DIR CANDIDATE_DIR` (repeat `--pair` for each width or
alpha run) and `--out audit/equality_sharing/evidence-summary.json`.
