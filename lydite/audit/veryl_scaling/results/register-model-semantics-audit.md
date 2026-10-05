# Register-count model semantics audit

Read-only audit at baseline a7204122c4226eeede44265504e52a01a0bea4b4.

## Outcome

No reference/DUT semantic mismatch found for 8/16/32 writable registers. The sole finding, fail-closed report validation hardening, has been fixed and independently retested. The baseline correct-design universal proofs remain UNKNOWN under the unchanged finite limits; finite checks below are not substitutes for those proofs.

## Independent checks

- Arithmetic/division instruction encoder/decoder independent of generator expressions; instruction widths fixed explicitly at 41/43/45 bits
- 10,752 reference steps exhaust all opcode, destination, and source combinations
- 4,032 X/W forwarding-match, youngest-writer-priority, and load-use-interlock pair checks
- Every register participates in reset and binding; individually perturbing any of 56 GPRs makes the relation false
- All 448 destination/opcode retirement-write tests preserve non-destinations, including writable r0 and nonwriting opcodes 4..7
- 516 reset-reachable trials, 36,480 cycles, 20,075 commits, 1,032 resets, and 5,472 stalled cycles; ROM/data live inputs vary independently between resets
- Source-to-DUT provenance: reset, next, wires, and outputs exactly equal their compiled SIR-lift sidecars at every size
- 33 semantic source mutants independently rejected by reset-reachable retirement/order mismatches with the binding checker disabled; 6 reset omissions correctly rejected with exact residual prestate

## Specific semantics reviewed

Opcode is the high 3 bits; rd/rs consume all 3/4/5 selector bits; immediate remains 32 bits. Load and branch target addresses remain low 2 bits. MOVI/ADD/XOR/LOAD write every selected GPR, including r0. BZ and reserved opcodes are nonwriters. Every architectural register and immutable ROM/data word is equated in the binding. Pending-W reads cover all registers and only writing instructions. Invalid-stage payloads and nonwriter W results are intentionally unconstrained; no input assumptions or expected observations enter the relation. D/X/W shape, fixed four-word ROM/data, arbitrary stall, reset capture, and pre-edge retirement semantics are retained.

## Coverage recommendations addressed

Requested a highest-register reset mutant in addition to r0 omission; implementation added it and high-selector truncation controls. Identified how forwarding can mask an unforwarded read fault in short programs; test owner added repeated self-ADD with three NOPs for every GPR and explicit false low/high X-match programs.

## Resolved gate hardening

Inherited validate_report trusted status=passed with missing or contradictory solver_result. The shared validate_report, used by the local wrapper, now requires SAT for binding_nonempty, progress_nonvacuity, commit_reachable_in_relation; UNSAT for other good obligations; and original-query validation for every SAT witness, including all existence obligations and mutant microstep. Independently tested 44 missing, unrecognized, UNKNOWN, contradictory verdict, and invalid existence-witness validation variants: all rejected. All 21 actual baseline validated SAT mutant reports still accepted. No actual inconsistent report or false positive was observed before the fix.

## Local evidence

- /workspace/shared/audit_register_semantics.py
- /workspace/shared/register-semantics-oracle-frozen.json
- /workspace/shared/audit_register_mutants.py
- /workspace/shared/register-semantics-mutants-frozen.json
- /workspace/shared/cpu-registers-baseline

audit/veryl_scaling/cpu_registers.py: `c96ea19961f68a87c1c682ffda7b4fb336499971171bc93a92027cb9a61e917c`

conformance/veryl-symbolic/pipeline_registers.veryl.in: `873d3d5a03d136e9423eef9227e9e30f3b3c18a82ea640c8642964d8a75d0b34`


## Final frozen test rerun

Final test file SHA256 d01e17827bf202498cfb0dcfaba05c05bed4eafea8c4f983442bd8b53e73b3d6. Independently reran all 24 tests against baseline 8/16/32 imported fixtures: passed. This suite adds 5,376 reference steps and 8,021 actual DUT ticks, 4,794 retirements, 345 resets, and 1,158 stalls. Shared gate unit tests: 5 passed. Log: /workspace/shared/register-semantics-final-suite-audit.log.

conformance/veryl-symbolic/run.py: `bdb1908e4d9b8b1c5bde9bf5d493eaae3f144c76956c51e4912e24d48ea63614`

audit/veryl_scaling/test_cpu_registers.py: `d01e17827bf202498cfb0dcfaba05c05bed4eafea8c4f983442bd8b53e73b3d6`
