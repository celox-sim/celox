# Declarative stuttering-refinement IR v2

Version 2 below is unchanged. Version 3 relational specification documents use
`kind: "specification"` and have a separate validated IR and checking path. See
[relational specifications](specifications.md) for its complete JSON/DSL
schema, finite-example semantics, product composition, and restricted state-only
implementation bridge. Neither finite-example tests nor the v3 safety bridge
inherit the v2 conditional progress or total-correctness claims.

Version 4 adds lexical input/output ports, explicitly connected reusable instances,
and exported groups of independently selectable leaf operations. See
[scoped specifications](scoped-specifications.md) for the v4 JSON shape,
action-set traces, inactive-private-state semantics, and state-only bridge.
The existing v2/v3 paths and their meanings remain unchanged.

The 0.9 prime/expectation DSL and bounded quantified examples are described in
[expectations and traces](expectations.md). Version 3/4 examples may
add an ordered `quantifiers` array, the new `expect` modes, and per-frame `ensure`;
version 2 is unchanged.

The 0.8 individual-declaration DSL is another surface for these schemas.
Omitted DSL declaration collections lower to explicit empty JSON objects; required
JSON fields and semantic validation are unchanged. See [language guide](language.md)
for the compatible old container syntax and migration printer.

The Rust engine owns JSON validation, type checking, named combinational DAG resolution, explicit memory normalization, proof-obligation generation, SMT-LIB emission, bounded structural UNSAT closure, Z3 fallback invocation, and evidence reporting. Python in `examples/` only generates JSON inputs. No Python is invoked by the checker.

## Machine and expression format

Required root fields: `version:2`, `inputs`, `reset_input`, `spec`, `impl`, `binding`, `commit`, `can_step`, `progress`. Optional: `name`, `hold_when`, `program_contract`, `proof_programs`. The latter contains untrusted [typed lemma proposals](lemmas.md), validated against the current design before proof execution.

Each machine has `state`, `reset`, `next`, `outputs`, and optional `wires`. Types are `"bool"`, `{"bv":N}`, `{"mem":[A,W]}` with widths 1–64. Registers and inputs are records with ASCII alphanumeric/underscore field names. Memory remains an SMT array of 2^A fixed-width words; it is never expanded into individual cells by the frontend.

`reset` and `next` assign every state field. Values are evaluated simultaneously. Active-high synchronous reset overrides all transitions. Reset expressions may use `i.NAME` to initialize both machines from a common symbolic program input. Normal next/output/wire expressions use `s.NAME`, `i.NAME`, and `w.NAME`. Wires form a DAG, independent of declaration order; missing names and cycles are rejected.

Expressions use JSON booleans, string references, or arrays:

- `["bv",width,integer]`, interpreted modulo 2^width (integer must fit signed/unsigned 64 bits)
- `["const_mem",address_width,word]`
- Boolean `not`, binary `and`, `or`, `xor`, `implies`
- Same-type `eq`, `ne`; `ite` with Boolean guard and equal-type branches
- Same-width word `add`, `sub`, `mul`, `band`, `bor`, `bxor`, `shl`, `lshr`; unary `bnot`
- Word comparisons `ult`, `ule`, `slt`, `sle`
- `read(memory,address)`, `write(memory,address,value)`
- `concat(high,low)`, `["extract",high_bit,low_bit,word]`
- `["zext"|"sext",additional_bits,word]`

No implicit type coercion, unknown opcode, unrecognized schema field, or duplicate JSON object key is accepted.

## Relation and commit semantics

`binding` is a Boolean expression over `spec.NAME` and `impl.NAME` only. It must not depend on inputs: this makes the relation compose across cycles with changing inputs.

`commit` names a Boolean implementation output. `can_step` names a Boolean specification output. Let C be implementation commit, R the binding, I' the implementation next state, and S' the ISA next state. Outside reset the checker establishes:

- R(S,I) ⇒ C implies spec.can_step
- R(S,I) ⇒ R(if C then S' else S, I')

Thus every implementation cycle corresponds to either one ISA step or a stutter. Reset independently establishes R for every reset input. This is not a bounded trace check: these UNSAT obligations are induction over arbitrary related states. A SAT witness may be unreachable from reset and can mean the supplied relation is insufficient.

The checker does not infer the binding or whether it captures the user's external intent. A tautological relation remains a bad contract; the example's relation compares every architectural state component and constrains the latched instruction in execute phase.

Optional `hold_when` is a Boolean expression over `spec.*`, `impl.*`, and `i.*`. Under the relation and this guard, outside reset, **all implementation state** must hold. The CPU uses `i.stall`.

## Conditional progress

`progress` has `enabled` (Boolean; may use state and input references) and `rank` (unsigned bitvector; **state-only**). The engine proves:

R ∧ !reset ∧ enabled ∧ !commit ⇒ rank(next) < rank(now)

A finite unsigned rank cannot descend forever. This rules out an infinite noncommit run on which enabled holds continuously and reset never occurs. It does not imply progress under weak fairness, recurring disabled intervals, permanent external stall, or arbitrary resets. Disabled steps are not generally required to preserve or decrease rank.

Binding, enabled operation, and commit are each checked for satisfiability as anti-vacuity checks. These are satisfiability within the supplied relation, not proofs of reset reachability or specification completeness.

## CPU example

Architectural state: 8-bit PC, 8-bit accumulator, 256x8 data memory, immutable 256x16 program memory, halted Boolean. Program loads from shared `i.program` on reset; data memory resets to zero. Input changes after reset cannot rewrite the stored program.

Instruction encoding: opcode bits 15:13, immediate/address bits 7:0; bits 12:8 ignored.

| Opcode | Meaning |
|---|---|
| 0 | ACC := ACC + immediate modulo 256 |
| 1 | ACC := data[immediate] |
| 2 | data[immediate] := ACC |
| 3 | PC := immediate if ACC=0; otherwise PC+1 |
| 4 | Set halted |
| 5 | ACC := ACC XOR immediate |
| 6,7 | NOP |

PC advances modulo 256 for all instructions except a taken JZ, including HALT. Halted cores stop committing. The implementation fetches to a 16-bit IR register in phase 0, then executes/commits in phase 1. Stall freezes all implementation state. Binding equates architectural state and requires IR=program[PC] in execute phase. Rank is 1 in fetch, 0 in execute, with enabled = !halted && !stall. Consequently an uninterrupted enabled interval needs at most one noncommitting fetch before a commit.

## Trust and evidence

Rust structural-kernel UNSAT and fallback Z3 UNSAT are trusted. Rust parsing/lowering, rule implementation, obligation generation, and the stated spec/binding remain trusted; no independent proof certificate or verified compiler is claimed. Existing Lean memory theorems do not certify the new kernel or its implementation. `ir.rs` isolates the pure expression/memory-law core as a possible later verification target, not an already verified component.

Each executed obligation emits its original standalone `.smt2` and backend-labelled `.out` (raw Z3 output on fallback); `report.json` maps named pre/post states, inputs, binding results, and rank to SMT symbols. SAT evidence is rechecked while requesting its model. UNKNOWN, an unsuccessful model recheck, tool errors, or unsupported input never produce a successful result.

The CLI emits `stuttering_refinement_verified`, `counterexample`, `inadequate_contract`, `unknown`, or `invalid_or_tool_error`. Without a program_contract, success is scoped to the encoded correspondence and conditional progress, not ISA adequacy, program correctness, RTL equivalence, CPU synthesis, or universal liveness.

## Optional program_contract (checker 0.3, IR version remains 2)

A design can supply `program_contract` in the same document as its refinement.
Required fields: `parameters`, `precondition`, `invariant`, `terminal`,
`postcondition`, `rank`. `parameters` is a typed named record of immutable symbolic
values referred to as `p.NAME`. They are proof parameters, not physical state.

`precondition` is Boolean over `i.*` and `p.*` and constrains the inputs sampled at
reset. All other contract expressions use `s.*` (specification state) and `p.*`,
never live `i.*` or implementation state. They are re-evaluated with reset/next
state substitution. Rank is an unsigned finite bitvector. The checker proves:

- reset asserted together with the precondition is satisfiable
- reset asserted and precondition imply invariant after reset
- invariant and nonterminal is satisfiable (a deliberately stronger anti-vacuity check)
- invariant and nonterminal implies spec can_step
- invariant and nonterminal implies invariant after one ISA transition
- invariant and nonterminal implies strictly smaller unsigned next rank
- invariant and terminal implies postcondition
- invariant and terminal implies not can_step

All preservation checks quantify over arbitrary current inputs, with no persistent
input assumptions. Initialization preconditions must not be reused as live-input
assumptions. Thus these are induction/termination obligations, not a trace bound.
This checker rejects a vacuous/immediately-terminal-only contract as inadequate;
this is a limitation of its anti-vacuity policy rather than a logical necessity.

Optional `split` is a named map, e.g.
`{"pc":{"expr":"s.pc","min":0,"max":2},"index":{"expr":"s.idx","min":240,"max":255}}`.
The engine partitions each word expression into each inclusive value plus a
complement (`other`), takes the Cartesian product and checks preservation on every
partition. Bounds must fit the word; at most 256 product partitions are accepted.
A coverage query is emitted as an additional check. Hints limit solver cost, never
exclude states. The current user must still choose useful split expressions and
ranges. Manual hints remain supported for compatibility; automatic selection is now the default when neither split nor cases is present. Automatic invariant discovery remains absent.

Alternatively `cases` is a named record of Boolean state/parameter guards. Every
case must preserve the invariant and their union must cover all active invariant
states. Missing cases therefore fail. `split` and `cases` are mutually exclusive.
Neither form introduces unproved lemmas or assumptions.

Success with a contract is `program_and_refinement_verified` (CLI exit 0), and
requires both all existing refinement obligations and all program obligations.
This is ISA total correctness for each reset satisfying the precondition. To
transfer termination to the implementation, no subsequent resets and the stated
implementation progress condition must hold until termination. In the array-sum
example a sufficient environment is `rst=false` and `stall=false` throughout that
interval. Binding preserves the architectural state on stutters, so the program
invariant/rank carry across them; the implementation rank bounds noncommit runs.
Permanent stalls or infinitely repeated resets are not covered. General
fairness-based progress is not proved.

### Extended array-sum CPU

`examples/cpu.json` retains its original opcode 6/7 NOP semantics. The separate
`examples/array_sum.json` variant adds an 8-bit architectural `idx` and changes:

- opcode 6 (`ADDX`): ACC := ACC + data[idx] modulo 256; PC := PC+1
- opcode 7 (`IXJ immediate`): idx := idx+1 modulo 256; branch to immediate iff the
  new idx is nonzero, otherwise PC := PC+1
- reset data memory := `i.data`; reset idx := `i.start_index`

Other opcodes, ignored instruction bits, fetch/execute, and stall semantics are
unchanged. These are explicit ISA/reset-interface changes, not compatibility
claims for binaries using old NOP opcodes. No generic variable-length array theorem
is claimed. The program fixes idx=240 initially and sums exactly addresses240..255.


## Automatic partition planning (checker 0.5)

Without `split` or `cases`, the checker extracts normalized word literals compared
with direct specification state words in the invariant and next-state model.
These are performance candidates, never inferred range assumptions. Each selected
axis contains singleton values and their complement. Full Cartesian coverage and
all preservation cases are proved. Negated/disjunctive/signed comparisons receive
no special logical interpretation, and syntactically different addresses are
never assumed non-aliasing.

Selection is deterministic greedy trial simplification: simplify the next invariant
under the current invariant and selected complete guards; score each candidate's
marginal mean weighted-DAG reduction divided by sqrt(case growth). Require at least
15% reduction and eight weighted nodes saved. Weights are normal node=1, ite=5,
select/store=8, multiply=4. Limits:32 values/axis,3 axes,128 product partitions,512
trial cases. Candidate iteration is by state name, so trial-budget exhaustion can
make renaming affect selection beyond score ties. Discovery still requires direct
BV-state/literal comparisons; Boolean or symbolic alias predicates are not inferred.

Over-budget candidates are skipped, never assumed impossible. `partition_plan`
reports every trial round, costs, score, rejection reason and total scoring time.
If none are selected, use the original unsplit obligation. `partitioning: "none"`
explicitly requests unsplit; `"auto"` explicitly selects the default. Neither may
be combined with manual split/cases. The invariant is an antecedent of preservation;
trial-derived results only choose guards and never add proof assumptions.

Auto preservation queries use a 1000ms solver timeout. After 30000ms elapsed in
the partition loop no new preservation query starts; each unfinished obligation
is recorded UNKNOWN/not_run. This is a scheduling budget, not an OS-level process
wall-time bound. Trial selection is additional; kernel computation, a running
query/model recheck, emission and I/O can overrun it;
other obligations retain 10000ms per check-sat. No fallback silently changes
UNKNOWN to success. See [proof search and partitioning](automatic-proofs.md)
for current modes and limits, and the [audit index](../audit/README.md) for historical evidence.

The structural kernel only closes false-reduced UNSAT obligations. Nonvacuity and
SAT witnesses always use Z3; failed closure sends the ORIGINAL query to Z3.
`LYDITE_KERNEL=off` disables local closure for ablation, while structural trials
remain enabled. `.kernel.json` is diagnostic rule counts, not a derivation
certificate. `.kernel-residual.smt2` can omit assumed facts and must not be treated
as equivalent to the original standalone query. `engine_summary` reports backend
counts and measured timing; whole-process wall time must be measured externally.
