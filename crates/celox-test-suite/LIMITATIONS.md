# External verification limitations

These exclusions keep normal external verification usable while preserving the shared assertions and every observed failure. They are **exact tool/case allowlists**, not rules that turn new compiler errors, panics, timeouts, or mismatches into skips. Unlisted failures still fail the command.

The manifest is [`src/veryl/verification/limitations.json`](src/veryl/verification/limitations.json). The runner reports `ignored`, never `passed`, before compiling an excluded fixture. `--include-ignored` reruns the original checks and returns their actual result, including a nonzero exit on failure. Exclusions are not promises about later tool versions; recheck them when upgrading.

The retained pre-exclusion reports contain [73 Verilator-path failures](verification/limitations/verilator.json) and [186 Icarus-path failures](verification/limitations/icarus.json); a [focused modport-import recheck](verification/repros/modport_import_icarus.json) retains 3 more Icarus failures, and a [focused interface recheck](verification/repros/interface_coverage_icarus.json) 20 more. These include failures in Veryl before any simulator starts. The original [ten conformance exclusions](MISMATCH_REVIEW.md) remain separate. Verilator's 110 four-state cases, including the zero-divisor and missing-return cases, report `unsupported`.

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
| [icarus_function_outputs](#icarus-function-outputs) | compile | 0 | 48 |
| [icarus_interfaces](#icarus-interfaces) | compile | 0 | 25 |
| [icarus_modport_functions](#icarus-modport-functions) | compile | 0 | 3 |
| [icarus_instance_array_unpacked_port](#icarus-instance-array-unpacked-port) | compile | 0 | 1 |
| [icarus_package_types](#icarus-package-types) | compile | 0 | 6 |
| [icarus_sparse_memory_timeout](#icarus-sparse-memory-timeout) | compile | 0 | 1 |
| [icarus_type_queries](#icarus-type-queries) | compile | 0 | 2 |
| [icarus_unpacked_subroutines](#icarus-unpacked-subroutines) | compile | 0 | 20 |
| [sv_array_argument_conversion](#sv-array-argument-conversion) | compile | 6 | 0 |
| [sv_enum_conversion](#sv-enum-conversion) | compile | 1 | 1 |
| [sv_mixed_assignment_patterns](#sv-mixed-assignment-patterns) | compile | 6 | 6 |
| [sv_mux_port_shape](#sv-mux-port-shape) | compile | 2 | 0 |
| [sv_type_cast_syntax](#sv-type-cast-syntax) | compile | 1 | 0 |
| [verilator_variable_wildcard](#verilator-variable-wildcard) | compile | 1 | 0 |
| [veryl_ff_effects](#veryl-ff-effects) | emission | 47 | 52 |
| [verilator_inout_dfg_crash](#verilator-inout-dfg-crash) | compile | 2 | 0 |
| [icarus_function_inout](#icarus-function-inout) | compile | 0 | 2 |
| [veryl_negative_for_bounds](#veryl-negative-for-bounds) | emission | 2 | 2 |
| [veryl_runtime_system_calls](#veryl-runtime-system-calls) | emission | 4 | 4 |
| [veryl_native_clock_components](#veryl-native-clock-components) | emission | 10 | 10 |
| [icarus_two_state_zero_division](#icarus-two-state-zero-division) | execute | 0 | 1 |

## Maintenance

Keep the manifest limited to reviewed case IDs with a reason and retained evidence. Do not generate a new allowlist from every failing result. For an excluded case, run a focused check, inspect the full compiler/runtime log and emitted SV, then remove only the exclusion that has been resolved. The shared case does not acquire an ignore.

```sh
cargo run -p celox-test-suite --features verilator --bin verify-verilator -- \
  --include-ignored --filter array_literal::test_array_literal_default_comb_assignment --jobs 1
cargo run -p celox-test-suite --features icarus --bin verify-icarus -- \
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

Affected cases: icarus 48. See the manifest for exact IDs.

## icarus-interfaces

Icarus 13.0 rejects the interface/modport port declaration syntax in these designs. Some fixtures may have additional emission issues; this records the first observed blocker.

Category: `simulator_compile_limitation`. Observed stage: `compile`. Versions: Veryl 0.21.0 and 0.22.0; Verilator 5.052 / Icarus 13.0.

Affected cases: icarus 25. See the manifest for exact IDs. The 20 added `interface` cases were observed in a [focused recheck](verification/repros/interface_coverage_icarus.json); the corresponding Verilator runs pass, except the four-state case, which Verilator reports as unsupported. The two added interface cases without modport ports (`test_interface_member_access_in_comb_and_ff`, `test_proto_interface_generic_module`) pass on Icarus.

## icarus-modport-functions

Icarus 13.0 reports `sorry: modport task/function ports are not yet supported` for the emitted modport `import` declarations; the interface/modport port declarations are also rejected. The corresponding Verilator runs pass.

Category: `simulator_unsupported`. Observed stage: `compile`. Versions: Veryl 0.22.0; Verilator 5.052 / Icarus 13.0.

Affected cases: icarus 3. See the manifest and the [focused recheck](verification/repros/modport_import_icarus.json).

## icarus-instance-array-unpacked-port

Icarus 13.0 does not distribute an unpacked array connection over the elements of an instance array (IEEE 1800-2023 23.3.3.5) and reports that uwire y has two drivers. The corresponding Verilator run passes.

Category: `simulator_compile_limitation`. Observed stage: `compile`. Versions: Veryl 0.22.0; Verilator 5.052 / Icarus 13.0.

Affected cases: icarus 1. See the manifest for exact IDs.

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

## Function inout recheck on develop

Veryl `d1f7025898b90dc5a8200e0b619b4b8f7bda357a` supports function
`inout logic` arguments. The old `inout tri logic` fixtures now receive
`InvalidModifier::NetInFunction`, rather than the historical unreachable-code
panic. The old observations remain in `verification/limitations/*.json` as
historical evidence; `veryl_inout_panic` is no longer an active exclusion.

Both shared fixtures now omit `tri`. The clocked fixture computes its next
state with the inout function in `always_comb`, then explicitly commits that
state in `always_ff`. This retains pre-clock state and sampled/returned-value
assertions without requiring an effectful function call inside `always_ff`.
The case ID is retained for report continuity. A separate Celox test requires
`SideEffectFunctionCallInAlwaysFf` for direct register copyout in `always_ff`.

IEEE 1800-2023 13.5.2 distinguishes inout copy-in/copy-out from reference
arguments: the actual is copied in on entry and updated on return. The comb
fixture checks aliased input sampling before that copyout.

The two positive cases run on Celox's native, Cranelift, Wasm, and interpreter
backends, and on the Veryl reference simulator, which passes both on Veryl
0.22.0; the clocked case's earlier failure there (`state=0`, expected `7`) no
longer reproduces. Celox's SV frontend still rejects function inout arguments.
[Backend observations](verification/repros/inout_backends.json) record the
earlier observations separately from the resolved analyzer panic.

### verilator-inout-dfg-crash

Verilator 5.052 fails compiling both emitted functions with an internal
`V3DfgSynthesize.cpp: Non-ReadOnly reference` error. Veryl analysis and SV
emission succeed. Category: `simulator_crash`; stage: `compile`; affected cases:
Verilator 2, Icarus 0. [Forced recheck](verification/repros/inout_verilator.json).

### icarus-function-inout

Icarus 13.0 rejects both emitted functions because their arguments include
inout ports (`Function arguments must be input ports`). Veryl analysis and SV
emission succeed. Category: `simulator_unsupported`; stage: `compile`; affected
cases: Verilator 0, Icarus 2. [Forced recheck](verification/repros/inout_icarus.json).

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

## veryl-native-clock-components

The ten `concurrent_initial` cases exercise native `$tb::clock_gen` and
`$tb::reset_gen` methods. The shared SV adapter does not translate those
components. Both external verification commands stop in Veryl 0.22.0 emission
with `internal error: entered unreachable code`, before producing SV or running
Verilator/Icarus. This is an adapter/emitter limitation, not a simulation verdict.
The [Verilator-path report](verification/repros/concurrent_initial_verilator.json)
and [Icarus-path report](verification/repros/concurrent_initial_icarus.json)
retain all nine observations each. Reproduce with `--include-ignored --filter
concurrent_initial` on either verification binary. The added `gated_drive` case
was separately attempted through [Verilator](verification/repros/concurrent_initial_gated_drive_verilator.json)
and [Icarus](verification/repros/concurrent_initial_gated_drive_icarus.json), with the same emission failure.

Six of the Veryl fixtures are also run by
`cargo test -p celox-bench --test veryl_heliodor
shared_concurrent_initial_fixtures_match_veryl`, using Veryl 0.22.0's
AOT-C runner with synchronous and asynchronous compilation. Celox's four execution
backends run all ten fixtures without exclusions. The legacy Veryl/SV differential harness exclusions remain
because those adapters do not expose native testbench execution.

The `mixed_edges`, `reset_between_edges`, and `reset_only_clock` cases are not included in the
six-fixture native-runner parity test. Their assertions remain enabled on all
four Celox backends. The [timing audit](verification/repros/concurrent_initial_timing.md)
records the Veryl 0.22.0 scheduler's intended concurrency model, the conflicting
published multi-clock description, and the two observed timing discrepancies.
In particular, an observation before the inverted clock's falling edge rules
out simultaneous-edge ordering as the explanation for `mixed_edges`.

Both [minimal SV timing fixtures](verification/repros/concurrent_initial_timing.json)
pass Icarus and Verilator. They are hand-written corroboration, not a successful
translation of the full Veryl fixtures. IEEE NBA rules alone do not establish
the Veryl native testbench's contract.

The `reset_only_clock` regression covers a configured period with no `next()`
call for that generator, including differently parameterized child instances.
Veryl 0.22.0 also derives reset periods from `ClockNext` statements and falls back
to period 2 for this case. The [focused observation](verification/repros/reset_only_clock.json)
records the native-runner result; the case stays enabled on all Celox backends.

## SystemVerilog suite

The SystemVerilog suite's exclusions are listed in [`verification/sv/limitations.json`](verification/sv/limitations.json), with the failures observed before them in [`verification/sv/limitations/`](verification/sv/limitations). A case is excluded for a tool only when the expected value follows IEEE 1800 and the tool either cannot run the design or disagrees with the standard; where the two tools disagree with each other, the entry says so.

| Group | Stage | Tool | Cases | Reason |
| --- | --- | --- | ---: | --- |
| `sv_icarus_constant_array_queries` | compile | icarus | 1 | Icarus 13.0 rejects array queries of variables in constant expressions and typedef operands; IEEE 1800-2023 20.7 permits them. Verilator passes. |
| `sv_icarus_runtime_array_queries` | execute | icarus | 1 | Icarus 13.0 aborts on a runtime dimension argument. A standalone reproducer also rejects an integer variable as the dimension. IEEE 1800-2023 20.7 permits querying fixed-size dimensions with runtime expressions. |
| `sv_icarus_immediate_cover_actions` | execute | icarus | 1 | Icarus 13.0 drops immediate cover pass statements even with `-gassertions`. IEEE 1800-2023 16.3 requires the action for a true condition. Verilator passes with the adapter's `--coverage-user` flag. |
| `sv_icarus_generate_binding` | compile | icarus | 4 | Icarus 13.0 cannot bind generate-scope parameters and signals that the block declares after their use ("Unable to bind"). Verilator passes these cases. |
| `sv_icarus_generate_forward_reference` | execute | icarus | 1 | Icarus 13.0 resolves a name used before its declaration in the same generate block to the enclosing scope. Verilator (which warns VARHIDDEN) and Celox resolve it to the block's own declaration. |
| `sv_icarus_generate_case_context` | execute | icarus | 1 | Icarus 13.0 compares a case-generate selector without the common width and signedness of the selector and all labels; IEEE 1800-2023 12.5 makes the comparison unsigned when any operand is unsigned. Verilator agrees with the expected value. |
| `sv_icarus_input_port_width` | execute | icarus | 1 | Icarus 13.0 evaluates an input port connection at its self-determined width before padding, losing the carry; IEEE 1800-2023 10.8 and 11.8.2 widen the operands to the port first. Verilator agrees with the expected value. |
| `sv_icarus_syntax` | compile | icarus | 8 | Icarus 13.0 rejects syntax used by these designs: selects of concatenations and replications (IEEE 1800-2023 A.8.4, primary), keyed assignment patterns, and parameter type declarations in generate blocks. Verilator passes these cases. |
| `sv_icarus_packed_array_parameters` | compile | icarus | 5 | Icarus 13.0 reports that packed array parameters are not supported, or fails a cast size that depends on a type alias dimension. Verilator passes or reports these four-state cases as unsupported. |
| `sv_icarus_unpacked_array_parameters` | compile | icarus | 3 | Icarus 13.0 reports that unpacked array parameters are not supported (IEEE 1800-2023 6.20.2) and rejects the typed assignment pattern of a struct parameter. Verilator passes the struct-parameter case; its internal error on the $isunbounded array query is recorded separately. |
| `sv_icarus_internal_error` | compile | icarus | 3 | Icarus 13.0 crashes on these designs: a segmentation fault while compiling, or a vvp assertion (vthread.cc of_DISABLE_FLOW) while running. Verilator passes these cases. |
| `sv_icarus_generate_function_scope` | execute | icarus | 1 | Icarus 13.0 evaluates a size query in a generate block with a function from another scope and reaches a $fatal in an active branch. Verilator agrees with the expected value. |
| `sv_icarus_unknown_generate_condition` | compile | icarus | 1 | Icarus 13.0 rejects a loop-generate condition with unknown bits; Verilator accepts it, and Celox treats an unknown condition as false, as for if-generate (IEEE 1800-2023 12.4, 27.5). |
| `sv_icarus_function_outputs` | compile | icarus | 7 | Icarus 13.0 rejects output and inout function arguments (function port is not an input port). |
| `sv_icarus_instance_array_unpacked_port` | compile | icarus | 4 | Icarus 13.0 does not distribute an unpacked array connection over the elements of an instance array (IEEE 1800-2023 23.3.3.5) and reports multiple drivers. Verilator passes these cases. |
| `sv_icarus_instance_array_fill_literal` | compile | icarus | 1 | Icarus 13.0 accepts a one-bit '0 connected to a wider port of an instance array, which IEEE 1800-2023 23.3.3.5 makes an error. Verilator rejects it. |
| `sv_icarus_inside` | compile | icarus | 1 | Icarus 13.0 does not support inside expressions ("sorry: inside expressions not supported yet"). Verilator passes the case. |
| `sv_icarus_packed_element_size` | execute | icarus | 1 | Icarus 13.0 returns 2 for $size of an element of a multidimensional packed struct member (value.m[0] of logic [1:0][3:0]); IEEE 1800-2023 20.7 makes it the element's size, 4. Verilator agrees with the expected value. |
| `sv_icarus_type_size_queries` | compile | icarus | 1 | Icarus 13.0 rejects a data type as the argument of $size ("Type names are not valid expressions here"), which IEEE 1800-2023 20.7 permits. Verilator passes the case. |
| `sv_icarus_mixed_state_struct_members` | execute | icarus | 1 | Icarus 13.0 does not convert a 2-state member of a packed struct with 4-state members from four state when reading it, so a compound assignment to the member keeps X; IEEE 1800-2023 7.2.1 converts it. Verilator cannot check four-state values. |
| `sv_icarus_unary_plus_unknown` | execute | icarus | 1 | Icarus 13.0 returns the operand of a unary plus unchanged when it has X or Z bits; IEEE 1800-2023 11.4.3 makes the result of an arithmetic operator all X. Verilator cannot check four-state values. |
| `sv_icarus_runtime_packed_subselect` | compile | icarus | 3 | Icarus 13.0 rejects a further select after a runtime index into a multidimensional packed array, or after runtime indices into an unpacked array of them (t[k][j], t[k][2:1], u[j][k][i]: "Array index expressions must be constant here"). Verilator passes the two-state case; the four-state one is outside what it checks. |
| `sv_icarus_interfaces` | compile | icarus | 11 | Icarus 13.0 rejects interface port declarations (syntax error in port declarations), modport function imports ("sorry: modport task/function ports are not yet supported") and multidimensional interface instance arrays ("sorry: Multi-dimensional arrays of instances are not yet supported"). Verilator passes these cases. |
| `sv_icarus_packed_inner_index_range` | execute | icarus | 1 | Icarus 13.0 flattens an out-of-range inner index of a multidimensional packed array into the neighbouring element: a read gives a known value instead of X, and a write lands in that element; IEEE 1800-2023 7.4.6 and 11.5.1 make the select out of range. Verilator cannot check four-state values, and it truncates out-of-range packed indices. |
| `sv_icarus_unpacked_subroutine_ports` | compile | icarus | 2 | Icarus 13.0 reports that subroutine ports with unpacked dimensions are not yet supported. Verilator passes the valid case. |
| `sv_icarus_unpacked_array_patterns` | compile | icarus | 1 | Icarus 13.0 reports that an assignment pattern with unpacked array items assigned to a multidimensional unpacked array is not yet supported ("Procedural assignment of array or array slice"), so the run cannot count it as a language rejection. The design is invalid (IEEE 1800-2023 7.6, 10.8). |
| `sv_icarus_display_field_width` | execute | icarus | 1 | Icarus 13.0 zero-pads a value wider than an explicit field width of %h or %o to the argument's full width (%3h of 32'h1234 prints 00001234); IEEE 1800-2023 21.2.1.2 expands the field to the value without leading zeros (1234). Verilator agrees with the expected value. |
| `sv_verilator_constant_unknown_bits` | execute | verilator | 1 | Verilator 5.052 is two-state: unknown bits in constant system function arguments ($onehot(2'bx1)) are not kept unknown, so a generate condition differs from IEEE 1800-2023 20.9. Icarus agrees with the expected values. |
| `sv_verilator_generate_case_unknown_label` | compile | verilator | 1 | Verilator 5.052 rejects an x or ? digit in a case-generate label ("no such thing as generate casez"). Icarus passes the case. |
| `sv_verilator_self_determined_index` | execute | verilator | 1 | Verilator 5.052 does not wrap a self-determined index expression (2'd3 + 2'd1) to its width before selecting; IEEE 1800-2023 11.5.1 and 11.6.1 make the index 0. Icarus agrees with the expected value. |
| `sv_verilator_short_circuit_calls` | execute | verilator | 1 | Verilator 5.052 runs a function call in the right operand of && in a select index when the left operand is false, so its output argument is written; IEEE 1800-2023 11.4.7 does not evaluate that operand. Icarus cannot compile the design (sv_icarus_function_outputs). |
| `sv_verilator_function_outputs` | compile | verilator | 1 | Verilator 5.052 fails with an internal error (V3DfgSynthesize: Non-ReadOnly reference) on output and inout function arguments. |
| `sv_verilator_generate_name_collision` | compile | verilator | 1 | Verilator 5.052 rejects a generate block named like a port with an "Unsupported" diagnostic instead of a duplicate-name error, so the run cannot count it as a language rejection. The design is invalid (IEEE 1800-2023 5.6.1, 23.9) and Icarus rejects it as a duplicate declaration. |
| `sv_verilator_part_select_write_overhang` | execute | verilator | 1 | Verilator 5.052 writes the out-of-range bit of a partially out-of-range indexed part-select (minus_write[i -: 2] with i = 0) into bit 7 instead of discarding it; IEEE 1800-2023 11.5.1 ignores writes to out-of-range bits. Icarus agrees with the expected value. |
| `sv_verilator_unpacked_array_extension` | compile | verilator | 2 | Verilator 5.052 rejects an unpacked array argument or assignment pattern item with a narrower element type through "EXTEND unexpected in assignment to unpacked array" (EXTENDS for a signed element), a diagnostic about its own width extension rather than the element types, so the run cannot count it as a language rejection. The designs are invalid (IEEE 1800-2023 7.6, 10.8). |
| `sv_icarus_dimensions_type_queries` | compile | icarus | 2 | Icarus 13.0 rejects data type arguments to `$dimensions`, with syntax/type-name errors and an assertion for a multidimensional cast. IEEE 1800-2023 20.7 permits these queries. Verilator passes the integral/string/real type case. |
| `sv_verilator_chandle_dimensions` | execute | verilator | 1 | Verilator 5.052 returns 1 for `$dimensions(chandle)`; IEEE 1800-2023 6.14 and 20.7 require 0 for this nonarray pointer type. |
| `sv_icarus_subroutine_unpacked_ports` | compile | icarus | 2 | Icarus 13.0 rejects subroutine arguments with unpacked dimensions and passing an array to those arguments. IEEE 1800-2023 13.5 permits unpacked subroutine arguments. Verilator passes the case. |
| `sv_icarus_function_return_dimensions` | execute | icarus | 1 | Icarus 13.0 returns 0 for $dimensions of a call to a package function returning a multidimensional packed value. IEEE 1800-2023 20.7 requires the declared return rank. Verilator passes the case. |
| `sv_icarus_one_bit_expression_dimensions` | execute | icarus | 3 | Icarus 13.0 reports 0 for one-bit vector operands of $dimensions, including sized literals, one-bit vector parameters and signing conversions. IEEE 1800-2023 6.20.2, 6.24.1 and 20.7 require vector rank 1; Slang 12.0.0 corroborates these type shapes. |
| `sv_verilator_one_bit_expression_dimensions` | execute | verilator | 2 | Verilator 5.052 reports 0 for one-bit vector operands of $dimensions, including sized literals and signing conversions. IEEE 1800-2023 6.20.2, 6.24.1 and 20.7 require vector rank 1; Slang 12.0.0 corroborates these type shapes. |
| `sv_verilator_timescale_constant` | compile | verilator | 1 | Verilator 5.052 cannot evaluate $timeprecision in a parameter initializer. IEEE 1800-2023 11.2.1 lists the timescale system functions in 20.4 among constant system function calls with constant arguments; this case uses no arguments. Slang 12.0.0 also rejects both timescale functions in constant expressions. |
| `sv_verilator_timescale_unit_argument` | compile | verilator | 2 | Verilator 5.052 parses $unit only as the start of a qualified name and rejects $timeunit($unit). IEEE 1800-2023 20.4.1 explicitly accepts $unit and $root as standalone query arguments; Slang in SV 2023 mode accepts this source. |
| `sv_verilator_timescale_scope` | execute | verilator | 3 | Verilator 5.052 ignores the design-element argument of $timeunit and returns the caller module unit, while $timeprecision returns the design-wide minimum even in a package function. IEEE 1800-2023 20.4.1 requires the selected/current design element scale. A standalone probe with own, child, leaf and package scales reproduces the disagreement without the suite testbench; Slang accepts the sources in SV 2023 mode. |
| `sv_icarus_timescale_queries_compile` | compile | icarus | 5 | Icarus 13.0 does not implement the SV 2023 $timeunit/$timeprecision functions: it rejects their syntax, cannot synthesize calls, or cannot evaluate parameter initializers. IEEE 1800-2023 20.4.1 defines the queries and 11.2.1 permits constant timescale calls with constant arguments. |
| `sv_icarus_timescale_queries_runtime` | execute | icarus | 1 | Icarus 13.0 compiles procedural and package calls of the SV 2023 timescale functions but vvp exits with errors that $timeunit/$timeprecision are not defined. IEEE 1800-2023 20.4.1 defines both functions; Slang accepts this source in SV 2023 mode. |
| `sv_icarus_isunbounded` | compile | icarus | 9 | Icarus 13.0 rejects parameters initialized to the symbolic value $ and cannot evaluate $isunbounded on bounded parameters in constant expressions. IEEE 1800-2023 6.20.7 permits $ parameter values and 20.6.3 defines the one-bit query result. Slang accepts the cases; Verilator passes integer-parameter declarations, aliases and bounded queries, while scalar unbounded declarations have a separate recorded limitation. |
| `sv_verilator_unbounded_parameter_overrides` | compile | verilator | 1 | Verilator 5.052 rejects $ and a parameter whose value is $ as an instance parameter override (UNSUPPORTED / cannot convert defparam value to constant). IEEE 1800-2023 6.20.7 permits assigning the symbolic value to another parameter. Slang accepts all four instances and preserves the unbounded value. |
| `sv_verilator_isunbounded_array_parameter` | compile | verilator | 1 | Verilator 5.052 raises an internal error in V3EmitCConstInit.h for $isunbounded on an unpacked array parameter. IEEE 1800-2023 6.20.2 permits aggregate value parameters, and 20.6.3 returns false when the parameter is not $. Slang accepts the case and evaluates QUERY to 1'b0. |
| `sv_verilator_unbounded_scalar_parameter` | compile | verilator | 2 | Verilator 5.052 rejects $ assigned to scalar logic parameters in procedural blocks (UNSUPPORTED: Unbounded outside of queue or string operations). IEEE 1800-2023 6.20.7 permits $ for simple bit vector parameter types, including logic (6.11.1). Slang accepts both cases. |
| `sv_definition_package_namespaces` | compile | verilator, icarus | 2 | Verilator 5.052 merges module and package identifiers, warns MODDUP and then cannot find P for a P:: reference. Icarus 13.0 rejects a module declaration that follows a same-named package. IEEE 1800-2023 3.13 gives definitions and packages separate global namespaces; Slang 12.0.0 accepts both declaration orders in this fixture. |
| `sv_icarus_separate_time_declarations` | compile | icarus | 1 | Icarus 13.0 rejects repeated initial timeunit/timeprecision declarations before the other precision/unit is first declared, reporting a missing precision or missing initial declaration. IEEE 1800-2023 3.14.2.2 permits matching repeats and separate unit/precision declarations before non-time items. Slang 12.0.0 accepts the time declarations (it separately rejects the constant timescale queries). |

The [system-function edge-case evidence](verification/repros/system_function_edge_cases.json)
retains the failed Icarus runs and minimal sources. Cover comparisons enable
Verilator user coverage so pass actions execute; this adds no coverage-report
contract to Celox. The readmem corroboration passes Icarus, which warns for
both short and long files. Verilator 5.052 aborts on the negative-index memory
in that reproducer; it is not counted as a successful comparison.
