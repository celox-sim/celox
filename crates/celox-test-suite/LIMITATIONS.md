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

Verilator 5.052 rejects a string variable or string method expression as the
`$sscanf` format, requiring a literal instead. IEEE 1800-2023 21.3.4.3 permits
string expressions here. Only `scan::sscanf_string_format_expression` is
excluded for Verilator; string sources with literal formats remain enabled.
The [direct lint observation](verification/sv/limitations/scan-string-format-verilator.json)
retains the diagnostic against the unchanged shared source. Icarus 13.0
compiles the dynamic-format case but crashes in `vvp`; a reduced source using
only a format variable also crashes. This same case is excluded for Icarus,
with the [runtime observation and reduced source](verification/sv/limitations/scan-string-format-icarus.json)
retained separately. The literal-format case remains enabled for both tools.

Named unpacked array parameter forwarding is checked against IEEE 1800-2023
6.20.2 and 7.6. Verilator 5.052 passes the whole-array forwarding case, but
reverses elements across opposite declaration directions and rejects a constant
subarray argument. Slang 12.0 accepts all four cases and confirms their parameter
values. Icarus 13.0 explicitly reports unpacked array parameters as unsupported.
Only these exact tool/case combinations are excluded; Celox runs every assertion.
The focused [Verilator](verification/repros/named_array_parameters_verilator.json),
[Icarus](verification/repros/named_array_parameters_icarus.json), and
[Slang](verification/repros/named_array_parameters_slang.json) observations retain
the failures and independent elaborated values.

Static function lifetime cases expose resets of the implicit result in Verilator
5.052 and Icarus 13.0, and of an output-only formal in Verilator. The expected
retention follows IEEE 1800-2023 6.21, 13.4.1, 13.4.2 and 13.5. The
[adapter-free source](verification/repros/sv_static_function_lifetimes.sv) and
[focused observations](verification/repros/sv_static_function_lifetimes.json)
record these differences. Icarus also rejects explicit static locals in automatic
functions and output function arguments. Only the affected tool/case pairs are
excluded; their original assertions remain available through `--include-ignored`.

The new `$dist_uniform` seed-width case is excluded for Verilator 5.052 and
Icarus 13.0. Verilator generates an invalid C++ reference binding for converted
seed arguments; Icarus rejects the `shortint` seed at startup. IEEE 1800-2023
20.14.2 permits an integral seed variable. The unchanged case passes Celox's
backends; the source and observed failures are retained in
[`sv_dist_uniform_seed_width.json`](verification/repros/sv_dist_uniform_seed_width.json).

Verilator 5.052 executes `always @*` once at time zero in
`latch::classic_star_declaration_startup`, before any sensitivity input changes.
Static declarations initialize before procedures start (IEEE 1800-2023 6.8),
while `always @*` waits for an input change (9.2.2.2.2).
The [adapter-free reproduction](verification/repros/wildcard_declaration_startup.sv)
fails the same assertion in Verilator and passes in Icarus 13.0.
The [retained observation](verification/repros/wildcard_declaration_startup_verilator.json)
records this failure; `--include-ignored` retains its assertions.
The same startup disagreement affects the complete body in
`latch::star_complete_declaration_startup`; its [exact source](verification/repros/star_complete_declaration_startup.sv)
and [standalone comparison](verification/repros/wildcard_task_sensitivity_verilator.json)
show Verilator failing the startup assertion and Icarus completing it.

Verilator 5.052 also executes an `always` with an edgeless variable event list
at time zero, in `timing::explicit_list_of_every_read_waits_for_a_change` and
`timing::list_with_an_extra_signal_wakes_on_it`. Its event control waits for a
value change (IEEE 1800-2023 9.4.2), and declaration initializers precede it
(6.8). The [adapter-free reproduction](verification/repros/event_list_declaration_startup.sv)
fails its startup assertion in Verilator and passes in Icarus 13.0, which also
passes the unchanged shared assertions. The [retained observation](verification/repros/event_list_declaration_startup_verilator.json)
records both failures; `--include-ignored` retains their assertions.

Verilator 5.052 keeps only the last of the nonblocking updates that an inner
loop issues to overlapping part-selects at run-time positions when an outer loop
delays between iterations, in
`timing::process_nonblocking_assignments_at_run_time_positions`, and writes a
nonblocking update of `q[8]` of `logic [7:0] q` into bit 0, in
`timing::process_nonblocking_assignments_outside_their_target`. Each
nonblocking assignment is its own update in issue order (IEEE 1800-2023 10.4.2),
and bits outside the target are not written (11.5.1, 7.4.6). The
[adapter-free reproduction](verification/repros/process_nba_positions.sv) fails
both checks in Verilator and passes in Icarus 13.0, which also passes the
unchanged shared assertions. The [retained observation](verification/repros/process_nba_positions_verilator.json)
records both failures; `--include-ignored` retains their assertions.

Verilator 5.052 schedules an `always @*` that calls a timed task differently
from its event control in `timing::always_star_timed_task_calls_run_as_processes`:
the call for `a = 5` at 3 ns writes `y` at 6 ns instead of 5 ns, and in
`timing::always_star_timed_task_calls_assign_nonblocking` a nonblocking update
after the task's 1 ns delay lands at 2 ns instead of 3 ns. A standalone
probe also shows it rerunning the call for a change made while the task was
suspended and for variables only the task body reads. IEEE 1800-2023 9.4.2.2
makes the implicit event list the variables the statement reads, and 9.4.2
waits only for changes after the event control is reached. Icarus 13.0 passes
the unchanged shared assertions.

IEEE 1800-2023 9.4.2.2 defines the event list of an `always @*` by the
identifiers that appear in its statement, so a branch that constant folding
removes still contributes, and Celox's combinational and process `always @*`
count it, with the reads of the defaults of omitted arguments. Icarus 13.0
leaves the folded branch out in
`timing::always_star_timed_task_calls_wait_on_folded_branch_reads`. Verilator
5.052 leaves out both the folded branch and the default of a nested call in
`timing::always_star_timed_task_calls_wait_on_nested_call_defaults`, and runs
the process at time zero; Icarus passes the nested-default case.

Icarus 13.0 reports a syntax error for an unparenthesized package-scoped delay
value (`#delays::D;`) in `timing::package_scoped_delay_values_wait_their_value`;
IEEE 1800-2023 A.6.5 permits a `ps_identifier` as a `delay_value`. Verilator
passes the case.

Verilator 5.052 repeats activation of the self-reading latch in
`latch::guarded_activation`, producing 3 where one `seed`/`en` change should
produce 1. The [adapter-free reproduction](verification/repros/latch_activation.sv)
produces 2 in Verilator and 1 in Icarus 13.0; Icarus also passes the unchanged
shared assertions. IEEE 1800-2023 9.2.2.2.1 excludes written expressions from
implicit sensitivity. The [retained observation](verification/repros/latch_activation_verilator.json)
records both executions. This exact Verilator case is excluded, with its
original assertions available through `--include-ignored`.

The two pulse regressions `latch::same_activation_change_return` and
`latch::hierarchy_same_activation_change_return` also fail to settle in
Verilator 5.052, before reaching their timed assertions. Their unchanged
[standalone sources](verification/repros/latch_change_return.sv) and
[hierarchical variant](verification/repros/latch_hierarchy_change_return.sv)
complete in Icarus 13.0. An `always_latch` excludes its written expressions
from sensitivity (IEEE 1800-2023 9.2.2.3, 9.2.2.2.1); an `@*` procedure does
not wait on events while executing its body (9.2.2.1, 9.4.2.2). Their own
blocking writes therefore cannot cause endless reactivation. The
[focused retained failures](verification/repros/latch_change_return_verilator.json)
record both adapter and standalone nonconvergence. Only these two additional
Verilator cases are excluded; their original assertions remain available with
`--include-ignored`.

Verilator 5.052 also wakes the task-calling `always_comb`/`always_latch`
procedures in `latch::task_call_sensitivity` when only task-body globals change.
IEEE 1800-2023 9.2.2.2.1 adds task argument expressions, but excludes task
contents, from implicit sensitivity; 9.2.2.3 applies the same rule to
`always_latch`. Icarus 13.0 passes the [unchanged shared source](verification/repros/task_call_sensitivity.sv),
including its positive check that changing the argument does wake the procedures.
The [standalone comparison](verification/repros/wildcard_task_sensitivity_verilator.json)
records both executions. Only this Verilator case is excluded for this reason.

The SystemVerilog suite's exclusions are listed in [`verification/sv/limitations.json`](verification/sv/limitations.json), with the failures observed before them in [`verification/sv/limitations/`](verification/sv/limitations). A case is excluded for a tool only when the expected value follows IEEE 1800 and the tool either cannot run the design or disagrees with the standard; where the two tools disagree with each other, the entry says so. A case tagged with an expectation stronger than SV (see the [tag table](README.md)) may also be excluded for a tool whose state model cannot produce it.

The interface-array connection cases cover packed distribution, unpacked
connections and array ports, and multidimensional instance arrays (IEEE
1800-2023 23.3.3.5). Verilator 5.052 passes the three one-dimensional cases,
but counts only the inner instance dimension when validating the packed
connection width of a 2x2 array. Icarus 13.0 passes the packed case; it rejects
unpacked actuals, aborts on interface array ports, and explicitly rejects
multidimensional instance arrays. The exact case exclusions retain the
[Verilator diagnostics](verification/repros/sv_interface_array_connections_verilator.json)
and [Icarus diagnostics](verification/repros/sv_interface_array_connections_icarus.json).
Celox runs all four unchanged cases on every backend.

The `indexed_select::constant_part_select_indices_of_concatenations` case also
uses these standard concatenation and replication selections as array indices
(IEEE 1800-2023 11.4.12). Icarus 13.0 rejects them with syntax errors before
simulation; its exact diagnostic is retained in
[`verification/sv/limitations/icarus.json`](verification/sv/limitations/icarus.json).
Verilator 5.052 passes the case on its unchanged stimulus and assertions.

The four-state `indexed_select::constant_parameter_initializers_preserve_unknown_indices`
case is also covered by the existing Icarus packed-array-parameter limitation.
Its [focused pre-exclusion diagnostic](verification/repros/sv_parameter_unknown_indices_icarus.json)
records both rejected packed declarations. Verilator cannot validate the case's
four-state expectations; the assertions remain enabled on all Celox variants.

| Group | Stage | Tool | Cases | Reason |
| --- | --- | --- | ---: | --- |
| `sv_icarus_automatic_task_arrays` | execute | icarus | 1 | Icarus 13.0 shares an automatic task's unpacked local array across overlapping calls. IEEE 1800-2023 13.3.1–13.3.2 require separate storage for each activation. The [pre-exclusion result](verification/repros/sv_timed_task_arrays_icarus.json) retains the first call returning 31 instead of 11. The scalar activation case remains enabled for Icarus. |
| `sv_icarus_expression_event_sample` | execute | icarus | 1 | Icarus 13.0 wakes a process that writes an operand of an event expression and then waits on it (`a = 1; @(a ^ b);`). IEEE 1800-2023 9.4.2 synchronizes the statement with a value change of the expression, and the write came before the event control. Verilator 5.052 agrees with the expected value. The [pre-exclusion result](verification/repros/sv_expression_event_sample_icarus.json) is retained. |
| `sv_verilator_implicit_event_in_process` | compile | verilator | 1 | Verilator 5.052 rejects `@*` in an `initial forever` ("Unsupported: Event control with implicit sensitivity (@*) in this context"); IEEE 1800-2023 9.4.2.2 permits it in any procedural timing control. Icarus 13.0 passes the case. The [pre-exclusion diagnostic](verification/repros/sv_implicit_event_in_process_verilator.json) is retained. |
| `sv_icarus_genvar_dimensions` | execute | icarus | 1 | Icarus 13.0 returns 0 for `$dimensions` of a generated loop index's implicit integer localparam; IEEE 1800-2023 27.4 and 20.7 require one packed dimension. Celox and Verilator pass the unchanged assertions. The [standalone reproducer and pre-exclusion result](verification/repros/sv_genvar_dimensions.json) retain the observed disagreement. |
| `sv_icarus_constant_array_queries` | compile | icarus | 1 | Icarus 13.0 rejects array queries of variables in constant expressions and typedef operands; IEEE 1800-2023 20.7 permits them. Verilator passes. |
| `sv_icarus_runtime_array_queries` | execute | icarus | 1 | Icarus 13.0 aborts on a runtime dimension argument. A standalone reproducer also rejects an integer variable as the dimension. IEEE 1800-2023 20.7 permits querying fixed-size dimensions with runtime expressions. |
| `sv_icarus_immediate_cover_actions` | execute | icarus | 1 | Icarus 13.0 drops immediate cover pass statements even with `-gassertions`. IEEE 1800-2023 16.3 requires the action for a true condition. Verilator passes with the adapter's `--coverage-user` flag. |
| `sv_icarus_generate_binding` | compile | icarus | 4 | Icarus 13.0 cannot bind generate-scope parameters and signals that the block declares after their use ("Unable to bind"). Verilator passes these cases. |
| `sv_icarus_generate_forward_reference` | execute | icarus | 1 | Icarus 13.0 resolves a name used before its declaration in the same generate block to the enclosing scope. Verilator (which warns VARHIDDEN) and Celox resolve it to the block's own declaration. |
| `sv_icarus_generate_case_context` | execute | icarus | 1 | Icarus 13.0 compares a case-generate selector without the common width and signedness of the selector and all labels; IEEE 1800-2023 12.5 makes the comparison unsigned when any operand is unsigned. Verilator agrees with the expected value. |
| `sv_icarus_input_port_width` | execute | icarus | 1 | Icarus 13.0 evaluates an input port connection at its self-determined width before padding, losing the carry; IEEE 1800-2023 10.8 and 11.8.2 widen the operands to the port first. Verilator agrees with the expected value. |
| `sv_icarus_syntax` | compile | icarus | 12 | Icarus 13.0 rejects syntax used by these designs: selects of concatenations and replications (IEEE 1800-2023 A.8.4, primary), keyed and typed assignment patterns, packed-structure parameter declarations, and parameter type declarations in generate blocks. |
| `sv_icarus_packed_array_parameters` | compile | icarus | 5 | Icarus 13.0 reports that packed array parameters are not supported, or fails a cast size that depends on a type alias dimension. Verilator passes or reports these four-state cases as unsupported. |
| `sv_icarus_packed_parameter_unknown_indices` | compile | icarus | 1 | Icarus 13.0 rejects both packed parameter declarations before the X/Z-index regression executes (IEEE 1800-2023 11.5.1). The focused diagnostic is retained; Verilator cannot validate its four-state expectations. |
| `sv_icarus_unpacked_array_parameters` | compile | icarus | 13 | Icarus 13.0 reports that unpacked array parameters are not supported (IEEE 1800-2023 6.20.2) and rejects the typed assignment pattern of a struct parameter. Verilator passes the struct-parameter case; its internal error on the $isunbounded array query is recorded separately. |
| `sv_verilator_default_argument_scope` | execute | verilator | 1 | Verilator 5.052 binds the default b=a to the earlier formal a and returns 22 for f(.a(2)) when the enclosing module a is 5. IEEE 1800-2023 13.5.3 evaluates defaults in the scope containing the subroutine declaration, requiring 25. Celox passes all five backend variants. |
| `sv_verilator_typed_pattern_sign_extension` | execute | verilator | 1 | Verilator 5.052 zero-extends int'{default: 1} to a 64-bit parameter (0x00000000ffffffff). IEEE 1800-2023 10.9 gives the expression the self-determined int type, signed under 6.11.3; 6.20.2 requires assignment conversion to the parameter, giving 0xffffffffffffffff. The same Verilator probe sign-extends through an int variable or explicit int cast. |
| `sv_icarus_dynamic_argument_defaults` | compile | icarus | 1 | Icarus 13.0 requires function port defaults to be constant expressions and rejects a reference to a net or variable. IEEE 1800-2023 13.5.3 evaluates a used default in the enclosing declaration scope on each call; the shared case retains its dynamic module-signal default. |
| `sv_icarus_internal_error` | compile | icarus | 3 | Icarus 13.0 crashes on these designs: a segmentation fault while compiling, or a vvp assertion (vthread.cc of_DISABLE_FLOW) while running. Verilator passes these cases. |
| `sv_icarus_generate_function_scope` | execute | icarus | 1 | Icarus 13.0 evaluates a size query in a generate block with a function from another scope and reaches a $fatal in an active branch. Verilator agrees with the expected value. |
| `sv_icarus_unknown_generate_condition` | compile | icarus | 1 | Icarus 13.0 rejects a loop-generate condition with unknown bits; Verilator accepts it, and Celox treats an unknown condition as false, as for if-generate (IEEE 1800-2023 12.4, 27.5). |
| `sv_icarus_function_outputs` | compile | icarus | 9 | Icarus 13.0 rejects output and inout function arguments (function port is not an input port). |
| `sv_icarus_instance_array_unpacked_port` | compile | icarus | 4 | Icarus 13.0 does not distribute an unpacked array connection over the elements of an instance array (IEEE 1800-2023 23.3.3.5) and reports multiple drivers. Verilator passes these cases. |
| `sv_icarus_instance_array_fill_literal` | compile | icarus | 1 | Icarus 13.0 accepts a one-bit '0 connected to a wider port of an instance array, which IEEE 1800-2023 23.3.3.5 makes an error. Verilator rejects it. |
| `sv_icarus_inside` | compile | icarus | 1 | Icarus 13.0 does not support inside expressions ("sorry: inside expressions not supported yet"). Verilator passes the case. |
| `sv_icarus_packed_element_size` | execute | icarus | 1 | Icarus 13.0 returns 2 for $size of an element of a multidimensional packed struct member (value.m[0] of logic [1:0][3:0]); IEEE 1800-2023 20.7 makes it the element's size, 4. Verilator agrees with the expected value. |
| `sv_icarus_type_size_queries` | compile | icarus | 1 | Icarus 13.0 rejects a data type as the argument of $size ("Type names are not valid expressions here"), which IEEE 1800-2023 20.7 permits. Verilator passes the case. |
| `sv_icarus_mixed_state_struct_members` | execute | icarus | 1 | Icarus 13.0 does not convert a 2-state member of a packed struct with 4-state members from four state when reading it, so a compound assignment to the member keeps X; IEEE 1800-2023 7.2.1 converts it. Verilator cannot check four-state values. |
| `sv_icarus_unary_plus_unknown` | execute | icarus | 1 | Icarus 13.0 returns the operand of a unary plus unchanged when it has X or Z bits; IEEE 1800-2023 11.4.3 makes the result of an arithmetic operator all X. Verilator cannot check four-state values. |
| `sv_icarus_runtime_packed_subselect` | compile | icarus | 3 | Icarus 13.0 rejects a further select after a runtime index into a multidimensional packed array, or after runtime indices into an unpacked array of them (t[k][j], t[k][2:1], u[j][k][i]: "Array index expressions must be constant here"). Verilator passes the two-state case; the four-state one is outside what it checks. |
| `sv_verilator_multidimensional_interface_connections` | compile | verilator | 1 | Verilator 5.052 validates a 2x2 scalar interface array against a two-bit connection instead of four bits. See the focused interface-array evidence above. |
| `sv_icarus_interface_array_connections` | compile | icarus | 3 | Icarus 13.0 rejects distributed unpacked actuals and multidimensional instances, and aborts on interface array ports. See the focused interface-array evidence above. |
| `sv_icarus_interfaces` | compile | icarus | 15 | Icarus 13.0 rejects interface port declarations (syntax error in port declarations), modport function imports ("sorry: modport task/function ports are not yet supported") and multidimensional interface instance arrays ("sorry: Multi-dimensional arrays of instances are not yet supported"). Verilator passes these cases. The implicit named interface cases have [focused pre-exclusion evidence](verification/sv/limitations/implicit-interface-ports-icarus.json). The mixed module-contract/local-interface case retains its [port-declaration diagnostics](verification/repros/sv_mixed_interface_connections_icarus.json). |
| `sv_icarus_packed_inner_index_range` | execute | icarus | 1 | Icarus 13.0 flattens an out-of-range inner index of a multidimensional packed array into the neighbouring element: a read gives a known value instead of X, and a write lands in that element; IEEE 1800-2023 7.4.6 and 11.5.1 make the select out of range. Verilator cannot check four-state values, and it truncates out-of-range packed indices. |
| `sv_icarus_string_methods` | compile | icarus | 3 | Icarus 13.0 does not implement the string methods getc, putc, compare, icompare, toupper, tolower, substr, atoi/atohex/atooct/atobin and itoa/hextoa/octtoa/bintoa ("Method getc is not a string method"), and requires a constant replication count for a string ("A reference to a net or variable is not allowed in a constant expression"). IEEE 1800-2023 6.16 defines the methods and 11.4.12.2 permits a non-constant count for a string replication. Verilator passes the cases. |
| `sv_icarus_unpacked_subroutine_ports` | compile | icarus | 2 | Icarus 13.0 reports that subroutine ports with unpacked dimensions are not yet supported. Verilator passes the valid case. |
| `sv_icarus_classes` | compile | icarus | 5 | Icarus 13.0 does not support extern methods ("sorry: External methods are not yet supported") or string methods called on a class property ("Can't find task itoa in class Named") or comparisons of class handles ("SORRY: Compare class handles not implemented"), aborts on a failed assertion in dup_expr.cc while compiling objects_referring_to_objects, and crashes with a segmentation fault while compiling implicit_constructors_and_this. IEEE 1800-2023 8.4, 8.7, 8.8 and 8.24 define these constructs. Verilator passes all but the string-method case, see sv_verilator_class_property_string_methods. |
| `sv_icarus_nested_class_members` | execute | icarus | 1 | Icarus 13.0 reads an empty string through a nested property path (kept.inner.text) after reporting "sorry: Nested member path not yet supported for class properties" for its assignment. IEEE 1800-2023 8.5 permits accessing a property of an object referred to by a property. Verilator passes the case. |
| `sv_icarus_class_scope_resolution` | compile | icarus | 3 | Icarus 13.0 rejects class scope resolution with a syntax error: reading a static property (Counter::created), calling a static method (Counter::next()) and assigning a static property (Counter::last = "set"). IEEE 1800-2023 8.9, 8.10 and 8.23 define these accesses. Verilator passes both cases. |
| `sv_icarus_virtual_dispatch` | execute | icarus | 1 | Icarus 13.0 binds a virtual function called through a base-class handle to the handle's class instead of the object's: the shared case prints area=0 for every shape, and the adapter-free source prints 1 1 1 3 where IEEE 1800-2023 8.20 requires 2 3 4 4. Verilator and Celox print 2 3 4 4 and pass the case. |
| `sv_icarus_abstract_and_interface_classes` | compile | icarus | 8 | Icarus 13.0 rejects pure virtual method declarations and interface classes with syntax errors ("Invalid class item", "Invalid module item") and fails code generation without a diagnostic ("Code generation had 2 error(s)") for a method called on an element of an array of class handles. IEEE 1800-2023 8.21 and 8.26 define pure virtual methods and interface classes, and 7.4 and 8.4 permit arrays of class handles. Verilator passes all but two cases; see sv_verilator_interface_class_casts and sv_verilator_extended_interface_class_methods. |
| `sv_verilator_interface_class_casts` | execute | verilator | 1 | Verilator 5.052 returns 0 from $cast of a handle of one interface class to a variable of another interface class that the object's class also implements, so the case dereferences the unassigned null handle; the adapter-free source prints 0 1 where IEEE 1800-2023 8.16 and 8.26.5 require 1 1. Celox prints 1 1. Icarus rejects interface classes, see sv_icarus_abstract_and_interface_classes. |
| `sv_verilator_extended_interface_class_methods` | compile | verilator | 1 | Verilator 5.052 does not find a method that an interface class inherits from an interface class it extends when the method is called through a handle of the extending class ("Class method 'sides' not found in class 'NamedShape'"). IEEE 1800-2023 8.26.3 makes the methods of the extended interface classes members of the extending one. Icarus rejects interface classes, see sv_icarus_abstract_and_interface_classes. |
| `sv_icarus_cast` | execute | icarus | 1 | Icarus 13.0 does not implement $cast ("System task/function $cast() is not defined by any module"), which IEEE 1800-2023 6.24.2 and 8.16 define. Verilator passes the case. |
| `sv_verilator_class_specialization_identity` | execute | verilator | 1 | Verilator 5.052 makes a separate specialization of a parameterized class for each way a parameter value assignment is written, so Box#(2, int)::made and Box#(.N(2), .T(int))::made read 0 after an object of Box#(.T(int), .N(2)) was constructed; the adapter-free source prints 0 1 0 where IEEE 1800-2023 8.25 requires 1 1 1, as the three assignments give the same values. Celox prints 1 1 1. Icarus rejects parameterized classes, see sv_icarus_parameterized_classes. |
| `sv_verilator_untyped_class_parameters` | execute | verilator | 1 | Verilator 5.052 gives a class parameter declared without a type the 32-bit type of its default even when a specialization assigns it a sized value, so $bits(V) of Sized#(8'h1) is 32; the adapter-free source prints 32 32 where IEEE 1800-2023 6.20.2 requires 8 32, as the parameter takes the type of its final value after overrides. Celox prints 8 32. Icarus rejects parameterized classes, see sv_icarus_parameterized_classes. |
| `sv_icarus_parameterized_classes` | compile | icarus | 13 | Icarus 13.0 does not parse a parameter port list of a class (class Box #(int N = 4); is a syntax error), which IEEE 1800-2023 8.25 defines; each case declares a parameterized class. Verilator passes all but two cases, see sv_verilator_class_specialization_identity and sv_verilator_untyped_class_parameters. |
| `sv_icarus_shallow_copies` | compile | icarus | 1 | Icarus 13.0 does not parse a shallow copy (second = new first; is a syntax error, "Malformed statement"), which IEEE 1800-2023 8.12 defines; the case also compares class handles, which Icarus does not implement, see sv_icarus_classes. Verilator passes the case. |
| `sv_verilator_class_property_string_methods` | compile | verilator | 1 | Verilator 5.052 stops with an internal error ("AstNode is not of expected type, but instead has type 'MEMBERSEL'") on a string method called on a class property, n.name.itoa(42), which IEEE 1800-2023 6.16 and 8.5 permit. Icarus rejects the case too, see sv_icarus_classes. |
| `sv_icarus_unpacked_array_patterns` | compile | icarus | 1 | Icarus 13.0 reports that an assignment pattern with unpacked array items assigned to a multidimensional unpacked array is not yet supported ("Procedural assignment of array or array slice"), so the run cannot count it as a language rejection. The design is invalid (IEEE 1800-2023 7.6, 10.8). |
| `sv_icarus_display_field_width` | execute | icarus | 1 | Icarus 13.0 zero-pads a value wider than an explicit field width of %h or %o to the argument's full width (%3h of 32'h1234 prints 00001234); IEEE 1800-2023 21.2.1.2 expands the field to the value without leading zeros (1234). Verilator agrees with the expected value. |
| `sv_verilator_constant_unknown_bits` | execute | verilator | 1 | Verilator 5.052 is two-state: unknown bits in constant system function arguments ($onehot(2'bx1)) are not kept unknown, so a generate condition differs from IEEE 1800-2023 20.9. Icarus agrees with the expected values. |
| `sv_icarus_two_state_time_zero_settle` | execute | icarus | 2 | Icarus 13.0 simulates the emitted design in four-state SV, where `always @*` and a sensitivity-list process wait for a change (IEEE 1800-2023 9.2.2.2, 9.4.2), so an output whose inputs never change stays X. The case's two-state oracle follows Verilator 5.052, which runs these processes once at time zero after the initial blocks, as Celox's two-state mode does; a [standalone probe](verification/repros/sv_two_state_time_zero_settle.json) prints `1 ffff 1` in Verilator and `x xxxx x` in Icarus. |
| `sv_verilator_generate_case_unknown_label` | compile | verilator | 1 | Verilator 5.052 rejects an x or ? digit in a case-generate label ("no such thing as generate casez"). Icarus passes the case. |
| `sv_verilator_self_determined_index` | execute | verilator | 1 | Verilator 5.052 does not wrap a self-determined index expression (2'd3 + 2'd1) to its width before selecting; IEEE 1800-2023 11.5.1 and 11.6.1 make the index 0. Icarus agrees with the expected value. |
| `sv_verilator_short_circuit_calls` | execute | verilator | 1 | Verilator 5.052 runs a function call in the right operand of && in a select index when the left operand is false, so its output argument is written; IEEE 1800-2023 11.4.7 does not evaluate that operand. Icarus cannot compile the design (sv_icarus_function_outputs). |
| `sv_verilator_function_outputs` | compile | verilator | 1 | Verilator 5.052 fails with an internal error (V3DfgSynthesize: Non-ReadOnly reference) on output and inout function arguments. |
| `sv_verilator_generate_name_collision` | compile | verilator | 1 | Verilator 5.052 rejects a generate block named like a port with an "Unsupported" diagnostic instead of a duplicate-name error, so the run cannot count it as a language rejection. The design is invalid (IEEE 1800-2023 5.6.1, 23.9) and Icarus rejects it as a duplicate declaration. |
| `sv_verilator_part_select_write_overhang` | execute | verilator | 4 | Verilator 5.052 writes bits that IEEE 1800-2023 11.5.1 discards: the out-of-range bit of a partially out-of-range indexed part-select (minus_write[i -: 2] with i = 0) lands in bit 7, a constant part-select wholly outside the vector (comb_out[11:8]) writes its low bits, a partly outside one of an ascending vector (comb_up[6:9]) takes the wrong end of the value, and run-time selects within one array element whose positions are known at compile time reach past it. Icarus agrees with the expected values. |
| `sv_verilator_unpacked_array_extension` | compile | verilator | 2 | Verilator 5.052 rejects an unpacked array argument or assignment pattern item with a narrower element type through "EXTEND unexpected in assignment to unpacked array" (EXTENDS for a signed element), a diagnostic about its own width extension rather than the element types, so the run cannot count it as a language rejection. The designs are invalid (IEEE 1800-2023 7.6, 10.8). |
| `sv_icarus_dimensions_type_queries` | compile | icarus | 2 | Icarus 13.0 rejects data type arguments to `$dimensions`, with syntax/type-name errors and an assertion for a multidimensional cast. IEEE 1800-2023 20.7 permits these queries. Verilator passes the integral/string/real type case. |
| `sv_verilator_chandle_dimensions` | execute | verilator | 1 | Verilator 5.052 returns 1 for `$dimensions(chandle)`; IEEE 1800-2023 6.14 and 20.7 require 0 for this nonarray pointer type. |
| `sv_icarus_subroutine_unpacked_ports` | compile | icarus | 4 | Icarus 13.0 rejects subroutine arguments with unpacked dimensions and passing an array to those arguments. IEEE 1800-2023 13.5 permits unpacked subroutine arguments. Verilator passes the case. |
| `sv_icarus_function_return_dimensions` | execute | icarus | 2 | Icarus 13.0 returns 0 for $dimensions of package functions returning multidimensional packed values and named/defaulted integral function calls. IEEE 1800-2023 20.7 requires the declared return rank. Verilator passes these cases. |
| `sv_icarus_one_bit_expression_dimensions` | execute | icarus | 3 | Icarus 13.0 reports 0 for one-bit vector operands of $dimensions, including sized literals, one-bit vector parameters and signing conversions. IEEE 1800-2023 6.20.2, 6.24.1 and 20.7 require vector rank 1; Slang 12.0.0 corroborates these type shapes. |
| `sv_verilator_one_bit_expression_dimensions` | execute | verilator | 2 | Verilator 5.052 reports 0 for one-bit vector operands of $dimensions, including sized literals and signing conversions. IEEE 1800-2023 6.20.2, 6.24.1 and 20.7 require vector rank 1; Slang 12.0.0 corroborates these type shapes. |
| `sv_verilator_timescale_constant` | compile | verilator | 1 | Verilator 5.052 cannot evaluate $timeprecision in a parameter initializer. IEEE 1800-2023 11.2.1 lists the timescale system functions in 20.4 among constant system function calls with constant arguments; this case uses no arguments. Slang 12.0.0 also rejects both timescale functions in constant expressions. |
| `sv_verilator_timescale_unit_argument` | compile | verilator | 3 | Verilator 5.052 parses $unit only as the start of a qualified name and rejects $timeunit($unit). IEEE 1800-2023 20.4.1 explicitly accepts $unit and $root as standalone query arguments; Slang in SV 2023 mode accepts this source. |
| `sv_verilator_timescale_scope` | execute | verilator | 5 | Verilator 5.052 ignores the design-element argument of $timeunit and returns the caller module unit, while $timeprecision returns the design-wide minimum even in a package function. IEEE 1800-2023 20.4.1 requires the selected/current design element scale. A standalone probe with own, child, leaf and package scales reproduces the disagreement without the suite testbench; Slang accepts the sources in SV 2023 mode. |
| `sv_icarus_timescale_queries_compile` | compile | icarus | 8 | Icarus 13.0 does not implement the SV 2023 $timeunit/$timeprecision functions: it rejects their syntax, cannot synthesize calls, or cannot evaluate parameter initializers. IEEE 1800-2023 20.4.1 defines the queries and 11.2.1 permits constant timescale calls with constant arguments. |
| `sv_icarus_timescale_queries_runtime` | execute | icarus | 1 | Icarus 13.0 compiles procedural and package calls of the SV 2023 timescale functions but vvp exits with errors that $timeunit/$timeprecision are not defined. IEEE 1800-2023 20.4.1 defines both functions; Slang accepts this source in SV 2023 mode. |
| `sv_icarus_isunbounded` | compile | icarus | 9 | Icarus 13.0 rejects parameters initialized to the symbolic value $ and cannot evaluate $isunbounded on bounded parameters in constant expressions. IEEE 1800-2023 6.20.7 permits $ parameter values and 20.6.3 defines the one-bit query result. Slang accepts the cases; Verilator passes integer-parameter declarations, aliases and bounded queries, while scalar unbounded declarations have a separate recorded limitation. |
| `sv_verilator_unbounded_parameter_overrides` | compile | verilator | 1 | Verilator 5.052 rejects $ and a parameter whose value is $ as an instance parameter override (UNSUPPORTED / cannot convert defparam value to constant). IEEE 1800-2023 6.20.7 permits assigning the symbolic value to another parameter. Slang accepts all four instances and preserves the unbounded value. |
| `sv_verilator_isunbounded_array_parameter` | compile | verilator | 1 | Verilator 5.052 raises an internal error in V3EmitCConstInit.h for $isunbounded on an unpacked array parameter. IEEE 1800-2023 6.20.2 permits aggregate value parameters, and 20.6.3 returns false when the parameter is not $. Slang accepts the case and evaluates QUERY to 1'b0. |
| `sv_verilator_unbounded_scalar_parameter` | compile | verilator | 2 | Verilator 5.052 rejects $ assigned to scalar logic parameters in procedural blocks (UNSUPPORTED: Unbounded outside of queue or string operations). IEEE 1800-2023 6.20.7 permits $ for simple bit vector parameter types, including logic (6.11.1). Slang accepts both cases. |
| `sv_icarus_unpacked_dimensions_partial_array` | compile | icarus | 2 | Icarus 13.0 rejects queries on partially selected multidimensional arrays and unpacked slices. IEEE 1800-2023 20.7 permits querying their remaining type; Verilator passes the case. |
| `sv_icarus_unpacked_dimensions_element` | execute | icarus | 1 | Icarus 13.0 returns 1 for `$unpacked_dimensions(a[0])` of `logic [7:0] a [0:1]`; the selected element has no unpacked dimensions, so IEEE 1800-2023 20.7 requires 0. Verilator cannot check this four-state case. |
| `sv_icarus_wildcard_typedef_shadowing` | compile | icarus | 1 | Icarus 13.0 reports a syntax error when a local variable declared after a wildcard import has the same name as an unpacked typedef in the package. IEEE 1800-2023 26.3 gives local declarations precedence over potentially visible wildcard imports. Verilator passes the case. |
| `sv_definition_package_namespaces` | compile | verilator, icarus | 3 | Verilator 5.052 merges module and package identifiers, warns MODDUP and then cannot find P for a P:: reference. Icarus 13.0 rejects a module declaration that follows a same-named package. IEEE 1800-2023 3.13 gives definitions and packages separate global namespaces; Slang 12.0.0 accepts both declaration orders in this fixture. |
| `sv_icarus_separate_time_declarations` | compile | icarus | 1 | Icarus 13.0 rejects repeated initial timeunit/timeprecision declarations before the other precision/unit is first declared, reporting a missing precision or missing initial declaration. IEEE 1800-2023 3.14.2.2 permits matching repeats and separate unit/precision declarations before non-time items. Slang 12.0.0 accepts the time declarations (it separately rejects the constant timescale queries). |
| `sv_icarus_interface_timescale_queries` | compile | icarus | 1 | Icarus 13.0 rejects standalone $unit arguments of the SV 2023 timescale queries, interface ports and modport function imports. IEEE 1800-2023 20.4.1 requires the interface declaration scope for unqualified calls and accepts $unit/$root arguments; this case also checks cross-file interface copies and unused-interface precision. Slang 12.0.0 in SV 2023 mode accepts the source. |
| `sv_verilator_timescale_package_argument` | execute | verilator | 1 | Verilator 5.052 ignores bare package arguments of $timeunit/$timeprecision and returns the caller unit/global precision. IEEE 1800-2023 20.4.1 requires the selected design element scale; 3.13 gives packages and definitions separate namespaces. A standalone probe reproduces the wrong package and instance results. Slang 12.0.0 also rejects bare package query arguments, including a package without a same-named module. |
| `sv_icarus_leading_zero_time_magnitudes` | compile | icarus | 1 | Icarus 13.0 checks the first digit of timeunit/timeprecision literals and rejects leading-zero integer or fixed-point spellings whose numeric magnitudes are 1, 10 or 100. IEEE 1800-2023 5.8 permits integer/fixed-point time literals, and 3.14.2.2 permits their use in time declarations. A standalone declaration without queries reproduces the failure; Verilator and Slang 12.0.0 accept these declarations. |
| `sv_verilator_timescale_root_argument` | compile | verilator | 1 | Verilator 5.052 rejects the standalone $root argument of $timeprecision, expecting a dotted hierarchical name. IEEE 1800-2023 20.4.1 explicitly accepts $root and returns the global simulation time unit. Slang 12.0.0 accepts the argument syntax but separately rejects the constant timescale initializers in this type-specialization case. |
| `sv_icarus_interface_timescale_handles` | compile | icarus | 1 | Icarus 13.0 rejects the typed and generic interface port declarations in this timescale-query case with syntax errors in port declarations. IEEE 1800-2023 25.3 permits these interface ports; 20.4.1 selects the queried design element time scope. The case retains its assertions for direct instances, typed/modport ports, differently bound generic ports and local generate handles. |
| `sv_verilator_timescale_interface_argument` | execute | verilator | 1 | Verilator 5.052 ignores an interface-instance operand of $timeunit: the Top caller has unit 1ms and returns -3 for i, whose interface declares 100ns and requires -7. IEEE 1800-2023 20.4.1 queries the selected design element; 25.3 defines hierarchical interface instances and interface ports. Slang 12.0.0 also reports ExpectedModuleName for interface-instance query arguments. |
| `sv_verilator_printtimescale_special_scopes` | compile | verilator | 1 | Verilator 5.052 rejects standalone $unit and $root operands of $printtimescale, expecting :: and a dotted hierarchy respectively. IEEE 1800-2023 20.4.2 explicitly accepts both special scope operands. |
| `sv_verilator_printtimescale_scope` | execute | verilator | 1 | Verilator 5.052 prints the Top caller scale for a package operand of $printtimescale, reports 1ps instead of the declared 10ps precision inside the package procedure, and omits the required parentheses around the design element name. IEEE 1800-2023 20.4.2 selects the queried/current design element and specifies the display format; 3.14.2.3 preserves its declared precision. |
| `sv_icarus_printtimescale_operands` | compile | icarus | 2 | Icarus 13.0 rejects standalone $unit and bare package operands of $printtimescale with syntax errors. IEEE 1800-2023 20.4.2 accepts a design element argument and explicitly includes $unit and $root. The shared assertions retain their standard output and precision. |
| `sv_verilator_urandom_zero_seed` | execute | verilator | 2 | Verilator 5.052 treats a zero $urandom seed as an entropy request, so reseeding with zero does not reproduce the sequence. IEEE 1800-2023 18.13.1 permits any integral seed and requires the same sequence for the same seed. A standalone run reports zero_seed_repeat=0 in Verilator and 1 in Icarus; nonzero seeding repeats in both. |
| `sv_verilator_urandom_process_state` | execute | verilator | 1 | Verilator 5.052 uses the host thread/context RNG for $urandom, so separately seeded design processes interfere when clocked independently. IEEE 1800-2023 18.14.1-2 requires a separate stream per SystemVerilog process, independent of thread execution order. The shared case seeds two module-instance processes identically and compares their streams after alternating activations. |
| `sv_verilator_urandom_statement_order` | execute | verilator | 2 | Verilator 5.052 moves an unseeded $urandom draw before the preceding seeded call when optimizing the clocked body, breaking the reseeded reference sequence. Its generated C++ assigns second from VL_RANDOM_I before the preceding VL_URANDOM_SEEDED_II call. IEEE 1800-2023 18.13.1 requires the seed to determine the following stream; 18.14.2 localizes its state to the calling process. The original sequential assertions, short circuits and indexed draw are preserved. |
| `sv_icarus_urandom_seed_expression` | execute | icarus | 3 | Icarus 13.0 requires the $urandom seed to be a writable integer/time variable or register and terminates simulation for a constant or input-net operand. IEEE 1800-2023 18.13.1 accepts any integral expression and gives a literal seed as an example. A standalone literal-seed probe compiles but vvp exits with the seed-must-be-a-variable error; variable seeds reproduce their streams. |
| `sv_icarus_urandom_narrow_seed` | execute | icarus | 1 | Icarus 13.0 terminates for an 8-bit $urandom seed variable with 'seed variable is less than 32 bits (8)'. IEEE 1800-2023 18.13.1 accepts any integral expression, converted to the int seed formal. A standalone signed/unsigned 8-bit seed probe compiles, then vvp exits with this width error; Verilator passes the same nonzero-seed extension checks. |
| `sv_verilator_random_algorithm` | execute | verilator | 2 | Verilator 5.052 does not compute $random with the Annex N algorithm: seeding with 0 returns 1631467331 instead of 303379748, and an unseeded $random draws from its own RNG. IEEE 1800-2023 20.14.1 and Annex N define $random as rtl_dist_uniform(seed, LONG_MIN, LONG_MAX) so that implementations repeat the same values. A standalone probe prints the Annex N sequence in Icarus 13.0 and different values in Verilator. |
| `sv_verilator_time_in_combinational_logic` | execute | verilator | 1 | Verilator 5.052 evaluates an always_comb that reads $time again when the clocking event of the suite testbench fires, although no variable it reads has changed: at 3 ns y is 3 instead of 0. IEEE 1800-2023 9.2.2.2.1 makes always_comb sensitive to the variables it reads, and $time (20.3.1) is none. A standalone probe prints y=0 in Icarus 13.0 and Verilator without a clocking block, and y=3 in Verilator with one. |
| `sv_verilator_intermediate_writes` | execute | verilator | 8 | Verilator 5.052 wakes a process waiting on a variable only for the value an evaluation leaves: an always_comb or edge-triggered body that rewrites a variable 1 -> 0 -> 1, and nonblocking updates `y <= 0; y <= 1;` from 1, wake no waiter, nor does a pulse that one always_ff writes to some bits of a variable while other processes write its other bits, or a nonblocking update that rises from the final blocking value. IEEE 1800-2023 4.3 and 9.4.2 make every store an update event that triggers the waiter, and 10.4.2 applies nonblocking updates in issue order, each its own update. [Standalone probes](verification/repros/sv_intermediate_writes_verilator.json) print the spec counts in Icarus 13.0 and 1 or 0 in Verilator. |
| `sv_icarus_port_pulses` | execute | icarus | 1 | Icarus 13.0 collapses a child's output port with the parent's net, so a waiter in the parent sees each value the child's always_comb writes inside one evaluation and counts 3 instead of 1. Both are conforming: the port is a continuous assignment (IEEE 1800-2023 23.3.3), evaluated after the child's process yields, and 4.7 leaves that order open. The case asserts the settled count, which Verilator 5.052 and Celox give. The [standalone probe](verification/repros/sv_port_pulses_icarus.json) retains both outputs. |
| `sv_verilator_active_region_waiters` | execute | verilator | 2 | Verilator 5.052 resumes a process woken by a blocking write of an edge-triggered process only after the same edge's nonblocking updates: with `y = ~y; q <= q + 1;` a waiter on `@(y)` reads the updated q, and with `y = 1; y <= 0; y <= 1;` from 0 it does not wait again before the nonblocking updates and counts 1 instead of 3. IEEE 1800-2023 4.5 makes the blocking write an update event of the active region and enters the NBA region only when the active and inactive regions are empty, so the waiter runs first (9.4.2). [Standalone probes](verification/repros/sv_active_region_waiters_verilator.json) print the spec values in Icarus 13.0 and the post-NBA values in Verilator. |
| `sv_program_reactive_region` | execute | icarus, verilator | 10 | Verilator 5.052 and Icarus 13.0 run program procedures in the active region set: a program reads design values before the design's nonblocking updates of the time slot, its `#0` resumes before the design's, and a flip-flop clocked by a program write runs before the program's `#0` and its nonblocking update of the flip-flop's input. IEEE 1800-2023 24.3.1 schedules program procedures in the Reactive region, `#0` in the Re-Inactive region and their nonblocking updates in the Re-NBA region, and 4.5 empties the reactive region set before the active region set runs again. xezim 0.11.3 agrees with every case except the flip-flop one, where it also runs the flip-flop first. The pre-exclusion results are [Icarus](verification/sv/limitations/programs-icarus.json) and [Verilator](verification/sv/limitations/programs-verilator.json). |
| `sv_program_completion` | execute | icarus, verilator | 2 | Neither tool ends the simulation when every program `initial` procedure has ended; IEEE 1800-2023 24.3 calls `$finish` then. xezim 0.11.3 does not either. |
| `sv_icarus_program_exit` | execute | icarus | 1 | Icarus 13.0 does not define `$exit`; IEEE 1800-2023 24.7 ends the calling program's `initial` procedures. |
| `sv_verilator_program_exit` | execute | verilator | 1 | Verilator 5.052 continues after `$exit` and keeps the program's other `initial` procedures (24.7). Icarus does not run the case; xezim 0.11.3 agrees with the expected output. |
| `sv_verilator_program_items` | compile | verilator | 2 | Verilator 5.052 rejects an `always` procedure and a module instance in a program with syntax errors, which the run cannot count as language rejections. Both are illegal (IEEE 1800-2023 24.3); Icarus 13.0 rejects them with diagnostics that name the rule. |

The [system-function edge-case evidence](verification/repros/system_function_edge_cases.json)
retains the failed Icarus runs and minimal sources. Cover comparisons enable
Verilator user coverage so pass actions execute; this adds no coverage-report
contract to Celox. The readmem corroboration passes Icarus, which warns for
both short and long files. Verilator 5.052 aborts on the negative-index memory
in that reproducer; it is not counted as a successful comparison.

### Icarus nested interfaces

Icarus 13.0 reports `Module instantiations are not allowed in interfaces` for
`interface::test_nested_interface_instance`, whose instantiated target is an
interface. Verilator 5.052 executes the same emitted Veryl design successfully.
The [retained diagnostic](verification/repros/interface_nested_icarus.json)
records this compile limitation.

Class randomization (`sv_icarus_class_randomize`) excludes the two cases
`class::unconstrained_randomize` and `class::randomize_without_rand_members`
from Icarus 13.0. It rejects the built-in `randomize` and `srandom` methods
during elaboration; [the original diagnostics](verification/repros/sv_class_randomize_icarus.json)
remain available. Both cases pass Verilator and Celox.

## Veryl analyzer floating-point constant expressions

Veryl analyzer 0.22 can fold negative `f32` casts and dependent floating-point
constants or parameters incorrectly before Celox lowering. The Veryl consumers
of `floating_point::integral_constants_from_float` are ignored with their
assertions preserved; the emitted SystemVerilog consumer, Icarus, and Verilator
pass. Celox's display tests retain additional ignored reproductions for inline
negative constants, named constants, and hierarchical parameter overrides.
Runtime floating-point display and assertion tests remain enabled.

These upstream errors are deferred under
[#1657](https://github.com/celox-sim/celox/issues/1657). The analyzer's default
constant folding remains enabled: globally disabling it also breaks integer
constant selects used in generate conditions, including existing linear SEC
and LFSR regressions.

## Veryl reference simulator floating-point values

The Celox backend harness excludes Veryl 0.22.0's in-process reference simulator
for six `floating_point` cases while preserving their assertions. A focused
run on 2026-10-21 observed these mismatches:

| Case | Expected | Veryl reference result |
| --- | --- | --- |
| `arithmetic_and_rounding` | integer conversion of 7.0 / 2.0 = 4 | 3 |
| `f32_precision_and_conversion` | numeric integer result | IEEE f32 payload `0x40800000` |
| `scalar_port_and_function_conversions` | implicit port conversion of 3.5 = 4 | 3 |
| `sequential_float` | clocked integer conversion of 1.5 = 2 | 0 |
| `unknown_integer_conversion` | unknown integer bits become zero, giving `0xa0` | unknown bits retained (`0xax`) |
| `formatted_display_and_write` | numeric `%0.2f` output bytes | literal `%0.2f` text; in-process adapter has no output capture API |

These exclusions concern the in-process Veryl simulator only. The Verilator and
Icarus adapters have their own case-specific verification and tool limitations.

### External floating-point observations

Focused runs on 2026-10-21 with Verilator 5.052 and Icarus 13.0 observed:

- Both tools return `16777217` where FP32 storage must round to `16777216`
  in Veryl `floating_point::f32_precision_and_conversion` and SV
  `floating_point::shortreal_storage_precision`. The FP64 comparisons pass.
- Verilator rejects the real-output-to-integer-port connection in Veryl
  `floating_point::scalar_port_and_function_conversions` and SV
  `floating_point::real_ports_and_subroutines`. Icarus passes these cases.
- Verilator reports the Veryl `unknown_integer_conversion` case unsupported
  because its adapter does not execute four-state expectations; Icarus passes.

The assertions remain enabled and the external runners continue reporting these
failures/unsupported cases; no retained JSON baseline or exclusion manifest was
changed. Other FP64 arithmetic, rounding, bit conversion, comparison and
sequential cases passed both external tools in these focused runs.

The additional SV conversion-function and zero-divisor cases pass Icarus.
Verilator 5.052 instead interprets unsigned `32'hffffffff` passed to `$itor`
as positive `4294967295.0` rather than the signed 32-bit integer `-1.0`, and
returns false for the self-inequality check on `0.0 / 0.0`. Its positive and
negative infinity checks pass. These differences remain visible in the
external results for `real_conversion_functions` and `real_zero_divisor`.

The SV `real_display_formats`, `real_display_negative_zero`, and
`real_file_display_formats` cases pass Icarus 13.0, including real/shortreal
arguments, numeric integer conversion, expressions, field width, precision,
scientific/general output, negative zero, and file display/write formatting.
Verilator 5.052 rejects the `+` formatting flag in `real_display_formats` as
an unknown display format code. It prints a lowercase `e` for the uppercase
`%E` conversion in `real_display_negative_zero`; the negative-zero value is
otherwise correct. Its `real_file_display_formats` case passes. The shared
assertions remain enabled, and these external differences are not exclusions.

Randomization control (`sv_icarus_randomization_control`) excludes four new
`class::randomization_*` cases from Icarus 13.0: it rejects `randomize`, `srandom`
and property/object `rand_mode` calls. [Retained diagnostics](verification/repros/sv_randomization_control_icarus.json)
record the failures; they are not passing simulation results.

Verilator 5.052 has two narrower exclusions, recorded in the
[randomization control evidence](verification/repros/sv_randomization_control_verilator.json):

- `sv_verilator_inherited_srandom`: `class::randomization_inheritance` fails
  compilation with duplicate `srandom` declarations when the same derived
  object is seeded through derived and base handles (IEEE 1800-2023 18.6.2,
  18.13.3). Assertions for hidden inherited members and hook dispatch remain.
- `sv_verilator_rand_mode_bit_argument`: `class::randomization_mode_bit_argument`
  reports two errors because both forms of `rand_mode(2)` enable the variable.
  The setter's formal is `bit` (18.8), so the required conversion truncates 2 to
  0. Generated C++ confirms that Verilator uses 1. The normal 0/1 mode control,
  inline member selection and pre-hook control cases pass Verilator.

Enum class randomization (`sv_icarus_enum_randomization`) excludes the four
`class::randomization_enum_*` cases from Icarus 13.0 because it cannot elaborate
class randomization methods. [Original diagnostics](verification/repros/sv_enum_randomization_icarus.json)
retain the failures without changing the shared assertions.

Verilator 5.052 passes the direct enum and inherited enum cases, but treats a
typedef alias of an enum as the underlying integer during randomization
(`sv_verilator_enum_alias_randomization`). The generated randomizer assigns raw
8-bit random values rather than selecting declared enum values, contrary to
IEEE 1800-2023 18.3. Only `class::randomization_enum_alias` is excluded;
[the mismatch evidence](verification/repros/sv_enum_randomization_verilator.json)
retains its unchanged domain assertion.

Scope enum randomization has two narrow tool exclusions for
`system_functions::scope_randomize_enum_values` and
`system_functions::scope_randomize_enum_order`. Icarus 13.0 rejects scope
randomization calls (`sv_icarus_scope_enum_randomize`;
[compile diagnostics](verification/repros/sv_scope_enum_randomize_icarus.json)).
Verilator 5.052 generates `basicStdRandomization` calls using only integer widths,
without the enum domain, and draws undeclared values (`sv_verilator_scope_enum_randomize`;
[mismatch evidence](verification/repros/sv_scope_enum_randomize_verilator.json)).
IEEE 1800-2023 18.3 and 18.12 require the declared enum domain. The shared
assertions remain unchanged, including the direct-enum case without an alias.

Cyclic randomization (IEEE 1800-2023 18.4.2) retains narrow external
exclusions: Icarus 13.0 cannot elaborate the three `randomization_cyclic_*`
cases ([diagnostics](verification/repros/sv_randc_icarus.json)). Verilator
5.052 passes the integer and direct-enum domain case, but emits duplicate
`srandom` declarations for the inheritance/control case and draws undeclared
values for an enum typedef alias ([evidence](verification/repros/sv_randc_verilator.json)).
The original cycle, mode and domain assertions remain unchanged.

## SystemVerilog packed unions

The `unions` shared group covers hard and soft packed untagged unions. With
Verilator 5.052, `packed_overlay`, `soft_overlay`, and `soft_signed_ports` pass;
the two four-state cases report `unsupported` through the existing state-mode
check.

Icarus 13.0 rejects `union soft` declarations in `soft_overlay`,
`soft_signed_ports`, and `soft_wide_four_state` during compilation.
`packed_overlay` passes. `packed_mixed_state` fails when reading a `bit` member
from mixed `logic`/`bit` union storage: for `8'b1010xxxx`, it returns
`8'b1010xxxx` instead of `8'b10100000`. A direct procedural probe reproduces
this independently of the shared harness; writing through the `bit` member
converts X to zero correctly. IEEE 1800-2023 7.3.1 requires conversion from
four-state to two-state when reading a two-state member.

These cases remain enabled with their original assertions and no exclusions.
The [retained Icarus evidence](verification/sv/icarus/unions.json) records the
three compilation failures and the read mismatch with their exact diagnostics;
the full-suite gate accepts only those unchanged failures, never a passing
result in their place. Reproduce with `verify-sv-verilator` or
`verify-sv-icarus` and `--filter unions::`. Normal reports are written under
`target/sv-verilator/` and `target/sv-icarus/`.

## SystemVerilog tagged packed unions

The `tagged_unions` shared group covers constructors (including `void`),
nested tags, checked member reads and writes, conditional/short-circuit access,
typed casts and ports, clocked copies, signed payloads and wide four-state values.
Task/function output and inout arguments retain writable members (including
concatenations and nested members), with live tag checks before the call and
before copy-out. System-function destinations and seeds use the same checks.
Whole-union output/inout actuals require the same declared union type as the
formal; integral formals and system outputs cannot fabricate a union tag.
Function returns and argument defaults accept context-typed `tagged` constructors;
default expressions retain the enclosing declaration scope. Checked
member arguments retain their runtime checks even when expression inlining
would otherwise discard an unused formal.
Layout and access rules follow IEEE 1800-2023 7.3.2 and 11.9. Invalid active-tag
access is tested separately in Celox's runtime-event harness.

Verilator 5.052 rejects `void` and `tagged` expressions in the seven two-state
cases; its adapter reports the two four-state cases as unsupported. Icarus 13.0
rejects all nine cases during parsing. The cases remain executable with their
original assertions and no exclusions. Reviewed compilation diagnostics are
retained in [Verilator evidence](verification/sv/verilator/tagged_unions.json)
and [Icarus evidence](verification/sv/icarus/tagged_unions.json), with exact
failure matching in the full-suite gate. Reproduce with `verify-sv-verilator`
or `verify-sv-icarus` and `--filter tagged_unions::`.

This implementation supports scalar packed tagged unions. Unpacked tagged
unions, `matches`/`case matches`, and continuous or concatenation drivers of
tagged members remain unsupported. Whole-union copies through ports and
continuous assignments are supported. Port connections currently require a
signal of the same declared tagged union type; constructor expressions at
instance connections require an intermediate typed signal. Use four-state simulation to preserve
undefined initial tags and unused payload bits.


Whole-value equality currently compares the packed representation, including
undefined payload padding. The review request for active-payload-only equality
needs a specification ruling and is tracked in
[issue #1889](https://github.com/celox-sim/celox/issues/1889); it is not yet a
confirmed conformance defect. Equality behavior is unchanged pending that
investigation.


### Detached children of functions in Icarus

IEEE 1800-2023 13.4.4 permits `fork ... join_none` inside a function,
including delays and task calls in its detached children. The six
`fork_control::function_join_none_*` cases exercise these exceptions while
keeping the function itself nonblocking. Icarus 13.0 rejects all six with
`functions cannot have delay statements`; the timed-task child also reports
`Functions cannot enable/call tasks`. The exact compile failures remain
executed evidence in [the Icarus report](verification/sv/icarus/fork_control.json),
not exclusions or passing assertions. Pinned xezim 0.11.3 passes all six;
Verilator 5.052 passes the five two-state cases and its adapter does not support
the four-state case. Reproduce with `verify-sv-icarus --filter
fork_control::function_join_none_`.
