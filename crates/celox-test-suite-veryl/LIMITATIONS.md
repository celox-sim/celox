# External verification limitations

These exclusions keep normal external verification usable while preserving the shared assertions and every observed failure. They are **exact tool/case allowlists**, not rules that turn new compiler errors, panics, timeouts, or mismatches into skips. Unlisted failures still fail the command.

The manifest is [`src/verification/limitations.json`](src/verification/limitations.json). The runner reports `ignored`, never `passed`, before compiling an excluded fixture. `--include-ignored` reruns the original checks and returns their actual result, including a nonzero exit on failure. Exclusions are not promises about later tool versions; recheck them when upgrading.

The retained pre-exclusion reports contain [73 Verilator-path failures](verification/limitations/verilator.json) and [184 Icarus-path failures](verification/limitations/icarus.json). These include failures in Veryl before any simulator starts. The original [ten conformance exclusions](MISMATCH_REVIEW.md) remain separate. Verilator's 109 four-state cases, including the new zero-divisor case, report `unsupported`.

## State modes

Two-state zero division retains the Verilator-compatible zero expectation. Icarus executes the emitted `logic` ports with four-state arithmetic and returns X; this is a model mismatch, not an Icarus specification violation. The adapter continues to reject unknown reads in a two-state fixture. The four-state companion explicitly checks that division and remainder by known zero return all X (IEEE 1800-2023 11.4.3), including positive/negative dividends and 0 / 0.

Celox now produces all X for known-zero divisors in four-state mode in the
native, Cranelift, Wasm, and interpreter backends. The four affected ignores
have been removed. The [original failure observations](verification/repros/four_state_zero_divisor.json)
are retained as pre-fix evidence. The shared case also checks signed/unsigned
8-, 64-, 65-, and 128-bit operations, constant zero divisors, nonzero high words,
and recovery from zero divisors. Celox's backend matrix additionally covers
clocked assignments and both state modes. [Post-fix observations](verification/repros/four_state_zero_divisor_fixed.json)
retain the commands, coverage, and the resolved code-generation failure below.

The expanded mixed-width circuit exposed an x86 SIMD lifetime error at the
register allocator's late block splits. SIMD selection and scheduling now
respect those boundaries; the original mixed-width circuit remains enabled
as regression coverage.

## Reviewed groups

Categories describe the observed blocker. A compilation rejection is not automatically proof that the source is legal or that the external simulator is wrong. Multiple blockers can exist in a fixture; a group records the first retained failure. Fixing Veryl emission may expose a different simulator limitation.

| Group | Stage | Verilator | Icarus |
| --- | --- | ---: | ---: |
| [icarus_array_codegen](#icarus-array-codegen) | compile | 0 | 2 |
| [icarus_array_input](#icarus-array-input) | compile | 0 | 1 |
| [icarus_array_slices](#icarus-array-slices) | compile | 0 | 2 |
| [icarus_array_types](#icarus-array-types) | compile | 0 | 1 |
| [icarus_assignment_patterns](#icarus-assignment-patterns) | compile | 0 | 27 |
| [icarus_case_break_crash](#icarus-case-break-crash) | execute | 0 | 1 |
| [icarus_function_outputs](#icarus-function-outputs) | compile | 0 | 47 |
| [icarus_interfaces](#icarus-interfaces) | compile | 0 | 5 |
| [icarus_package_types](#icarus-package-types) | compile | 0 | 6 |
| [icarus_sparse_memory_timeout](#icarus-sparse-memory-timeout) | compile | 0 | 1 |
| [icarus_type_queries](#icarus-type-queries) | compile | 0 | 2 |
| [icarus_unpacked_subroutines](#icarus-unpacked-subroutines) | compile | 0 | 20 |
| [sv_array_argument_conversion](#sv-array-argument-conversion) | compile | 6 | 0 |
| [sv_enum_conversion](#sv-enum-conversion) | compile | 1 | 1 |
| [sv_mixed_assignment_patterns](#sv-mixed-assignment-patterns) | compile | 6 | 6 |
| [sv_mux_port_shape](#sv-mux-port-shape) | compile | 2 | 0 |
| [sv_scalar_select](#sv-scalar-select) | compile | 1 | 1 |
| [sv_type_cast_syntax](#sv-type-cast-syntax) | compile | 1 | 0 |
| [verilator_variable_wildcard](#verilator-variable-wildcard) | compile | 1 | 0 |
| [veryl_ff_effects](#veryl-ff-effects) | emission | 47 | 52 |
| [veryl_inout_panic](#veryl-inout-panic) | emission | 2 | 2 |
| [veryl_negative_for_bounds](#veryl-negative-for-bounds) | emission | 2 | 2 |
| [veryl_runtime_system_calls](#veryl-runtime-system-calls) | emission | 4 | 4 |
| [icarus_two_state_zero_division](#icarus-two-state-zero-division) | execute | 0 | 1 |

## Maintenance

Keep the manifest limited to reviewed case IDs with a reason and retained evidence. Do not generate a new allowlist from every failing result. For an excluded case, run a focused check, inspect the full compiler/runtime log and emitted SV, then remove only the exclusion that has been resolved. The shared case does not acquire an ignore.

```sh
cargo run -p celox-test-suite-veryl --features verilator --bin verify-verilator -- \
  --include-ignored --filter array_literal::test_array_literal_default_comb_assignment --jobs 1
cargo run -p celox-test-suite-veryl --features icarus --bin verify-icarus -- \
  --include-ignored --filter case_switch::test_case_break_inside_comb_function_for --jobs 1
```

The known sparse-memory compilation timeout is excluded only for that Icarus case. Missing executables, timeouts on other cases, and new diagnostics on enabled cases remain failures. Default ignored rows have no simulation verdict; the before-exclusion reports preserve their actual emission/compile/runtime statuses.

## icarus-array-codegen

Icarus 13.0 fails code generation with cannot evaluate VEC4 expression (26) for these one-element array literal assignments.

Category: `simulator_internal_error`. Observed stage: `compile`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: icarus 2. See the manifest for exact IDs.

## icarus-array-input

Icarus 13.0 requires an array index on data_arr and fails elaboration of the mux input port. Verilator also reports a port-shape problem on this fixture; no conformance verdict is assigned.

Category: `simulator_compile_limitation`. Observed stage: `compile`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: icarus 1. See the manifest for exact IDs.

## icarus-array-slices

Icarus 13.0 explicitly reports that array slices are not supported for continuous assignment at these instance ports.

Category: `simulator_unsupported`. Observed stage: `compile`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: icarus 2. See the manifest for exact IDs.

## icarus-array-types

Icarus 13.0 rejects the emitted unpacked-array context types for rob_addr/rob_data/rob_f3/rob_fwd. The corresponding Verilator run passes.

Category: `simulator_compile_limitation`. Observed stage: `compile`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: icarus 1. See the manifest for exact IDs.

## icarus-assignment-patterns

Icarus 13.0 rejects the emitted keyed default/struct assignment-pattern syntax. These same fixtures pass Verilator; that agreement is evidence of a tool limitation, not by itself a conformance verdict.

Category: `simulator_compile_limitation`. Observed stage: `compile`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: icarus 27. See the manifest for exact IDs.

## icarus-case-break-crash

Icarus 13.0 vvp aborts in vthread_s::cleanup with stack_vec4_.empty() while running a function for-loop/case-break fixture.

Category: `simulator_internal_error`. Observed stage: `execute`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: icarus 1. See the manifest for exact IDs.

## icarus-function-outputs

Icarus 13.0 rejects output arguments on these functions (function port is not an input port). These fixtures cannot reach simulation.

Category: `simulator_unsupported`. Observed stage: `compile`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: icarus 47. See the manifest for exact IDs.

## icarus-interfaces

Icarus 13.0 rejects the interface/modport port declaration syntax in these designs. Some fixtures may have additional emission issues; this records the first observed blocker.

Category: `simulator_compile_limitation`. Observed stage: `compile`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: icarus 5. See the manifest for exact IDs.

## icarus-package-types

Icarus 13.0 rejects package-qualified types in these emitted module port declarations. The corresponding Verilator runs pass.

Category: `simulator_compile_limitation`. Observed stage: `compile`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: icarus 6. See the manifest for exact IDs.

## icarus-sparse-memory-timeout

The million-element sparse-memory fixture exceeds the retained 120-second Icarus compilation budget (exit 124). This is a case-specific resource exclusion, not a blanket allowance for timeouts.

Category: `resource_limit`. Observed stage: `compile`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: icarus 1. See the manifest for exact IDs.

## icarus-type-queries

Icarus 13.0 rejects type-name operands of $size and reports an elaboration internal error. The corresponding Verilator runs pass.

Category: `simulator_compile_limitation`. Observed stage: `compile`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: icarus 2. See the manifest for exact IDs.

## icarus-unpacked-subroutines

Icarus 13.0 explicitly reports that subroutine ports with unpacked dimensions are not supported.

Category: `simulator_unsupported`. Observed stage: `compile`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: icarus 20. See the manifest for exact IDs.

## sv-array-argument-conversion

Veryl forwards unpacked arrays with different element widths to function formals without elementwise conversion. Verilator rejects EXTEND/EXTENDS on these unpacked-array arguments.

Category: `sv_emission_issue`. Observed stage: `compile`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: verilator 6. See the manifest for exact IDs.

## sv-enum-conversion

Veryl emits assignment of a logic vector to an enum without an explicit enum cast. Both SV compilers reject this connection; their rejection is not treated as a simulator conformance bug.

Category: `sv_emission_issue`. Observed stage: `compile`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: verilator 1, icarus 1. See the manifest for exact IDs.

## sv-mixed-assignment-patterns

The emitted assignment pattern mixes positional/replicated entries with keyed default entries, or leaves an unkeyed entry after default. Both parsers reject this emitted syntax; the fixture expectations have not been validated.

Category: `sv_emission_issue`. Observed stage: `compile`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: verilator 6, icarus 6. See the manifest for exact IDs.

## sv-mux-port-shape

Verilator rejects the emitted standard-library mux/demux connection because its port and actual disagree on packed versus unpacked array shape. The smoke fixture remains unvalidated.

Category: `sv_emission_issue`. Observed stage: `compile`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: verilator 2. See the manifest for exact IDs.

## sv-scalar-select

Veryl emits an un-ranged scalar logic formal and then selects bit 0. Both compilers reject selecting this scalar; the Veryl-to-SV representation needs review.

Category: `sv_emission_issue`. Observed stage: `compile`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: verilator 1, icarus 1. See the manifest for exact IDs.

## sv-type-cast-syntax

Veryl emits shortint unsigned'(1) for the generic u16 cast; Verilator rejects the emitted type-cast syntax. This is not a numerical mismatch.

Category: `sv_emission_issue`. Observed stage: `compile`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: verilator 1. See the manifest for exact IDs.

## verilator-variable-wildcard

Verilator 5.052 explicitly reports Unsupported for a nonconstant four-state RHS of ==? or !=?. The two-state suite mode does not change the emitted logic port declarations.

Category: `simulator_unsupported`. Observed stage: `compile`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: verilator 1. See the manifest for exact IDs.

## veryl-ff-effects

Veryl 0.21.0 rejects function output or non-local effects in always_ff. These fixtures use the Celox extension; strict SV emission stops before either simulator runs.

Category: `veryl_extension`. Observed stage: `emission`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: verilator 47, icarus 52. See the manifest for exact IDs.

## veryl-inout-panic

Veryl 0.21.0 panics with entered unreachable code while emitting these function inout cases. No SV simulator is reached.

Category: `veryl_emitter_bug`. Observed stage: `emission`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: verilator 2, icarus 2. See the manifest for exact IDs.

## veryl-negative-for-bounds

Veryl 0.21.0 rejects negative bounds in elaborated for loops (InvalidForRange::NegativeBound). This is a frontend restriction, not an external simulator mismatch.

Category: `veryl_restriction`. Observed stage: `emission`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: verilator 2, icarus 2. See the manifest for exact IDs.

## veryl-runtime-system-calls

Veryl 0.21.0 requires an evaluable argument for these direct $clog2/$onehot calls (UnevaluableValue). Runtime operands prevent emission.

Category: `veryl_restriction`. Observed stage: `emission`. Versions: Veryl 0.21.0; Verilator 5.052 / Icarus 13.0.

Affected cases: verilator 4, icarus 4. See the manifest for exact IDs.

## icarus-two-state-zero-division

This two-state fixture follows Verilator and expects zero on division by zero. Icarus evaluates the emitted logic ports in four-state SV and correctly produces X, which the two-state adapter refuses to discard. This is a state-model mismatch, not an Icarus SV conformance bug. The four-state companion requires X.

Category: `state_model_difference`. Observed stage: `execute`. Versions: Veryl 0.21.0; Icarus 13.0.

Affected cases: icarus 1. See the manifest for exact IDs.
