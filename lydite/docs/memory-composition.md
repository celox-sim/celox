# CPU and memory proof composition

These specialized custom-ISA proof rules supplement the [selected RV32I contracts](rv32i.md). Source and interface identity, fresh live handles, and complete component obligations are mandatory; saved verdicts cannot authorize composition.

## CPU request-response contract

### Exact scope

- Independent handwritten Veryl D/X/W CPU, 32-bit datapath, eight writable GPRs
  including r0; 41-bit `[opcode:3 | rd:3 | rs:3 | immediate:32]` custom ISA
- Fixed **six-bit word PC and load/branch addresses**, wrapping modulo 64
- Separate zero-latency combinational instruction and data read ports
- Backing capacities 4,16,64 words **per memory**; all unbacked addresses return
  zero. Small-capacity addresses are **not** truncated/modulo capacity
- Reset captures arbitrary instruction/data seed words into the memory module;
  live seed inputs may subsequently change arbitrarily
- No stores, request queue, latency, ready/valid backpressure, cache, coherence,
  exceptions, interrupts, RV32 semantics, or OoO state

These are new fixtures. The earlier 8/16/32-GPR and two-GPR capacity fixtures,
relations, budgets, and CI gates are unchanged. Their small-capacity PC semantics
are not silently replaced by this new fixed 64-address-space variant.

### Interface and acceptance

CPU inputs are `imem_response: bv41`, `dmem_response: bv32`, reset and stall.
Outputs are `imem_address=fetch_pc`, `dmem_address=x_ir[5:0]`, and read enables.
A fetch is accepted on a nonreset, nonstall edge with no load-use hazard and no
taken branch. A data read is consumed by a valid LOAD in X on a nonreset,
nonstall edge. A response belongs to its **current** address on that edge.
There is no independent response-valid or transaction ID in this zero-latency
protocol. Stall stops pipeline advancement and retirement. Reset has priority
and discards in-flight instructions; combinational enables/commit may be high
while reset is asserted and do **not** denote acceptance on a reset edge.
A taken branch can update an invalid D payload from the live response; that
payload is killed and is not promised to retire.

The abstract CPU proof allows completely arbitrary responses on every cycle.
Repeated-read consistency is not assumed or inferred there. The memory contract
establishes it separately from one immutable reset snapshot.

### Local theorem, not a second datapath model

The version 4 scoped contract is checked against reset/next/output expressions
lifted from the actual compiled CPU. Its state invariant establishes:

- Valid W belongs to the architectural PC, its next PC is the sequential ISA
  next PC, and its result for a writing opcode is the ISA result
- Valid X operands equal the architectural register view including pending W;
  D/X/W PCs and speculative fetch PC have the correct ordering

The per-tick relation requires architectural PC/GPR updates to execute the
retired instruction under the independent ISA, or stutter when nothing retires.
It also checks fetch-response capture, D→X→W instruction/PC provenance, hazard
holding, branch killing, stall holding, actual request addresses/enables, and
retirement indication. The contract does not supply a reference implementation
of the CPU's forwarding datapath or compute its result from the DUT result.
`w_result` is used only in the pending-register view; a separate invariant
proves its ISA value and retirement checks independently re-evaluate the ISA.

A deterministic proof-only observer records the data response on each nonstall
X→W edge. The ISA result in W uses this **historical response**, never the live
response at retirement. It is harmless to sample on non-LOAD/invalid X cycles:
only a valid retiring LOAD interprets it. Five additional monitors sample the
**actual lifted** request-address, enable, and commit expressions. They update
even during stall, so output mistakes cannot hide behind a stall premise.
Monitors reset to zero. No original DUT reset/next/output/wire expression may
read the observer namespace; generation checks this noninterference before
adding the harness. Hardware state is 482 bits, observers 47 bits, total 529 bits.

Boolean samples are represented exactly as one-bit words, using
`ite(actual, 1, 0)` and the same conversion in the expected property. This matters
to current solver heuristics: Boolean monitors occurred even in dead scoped
stutter branches and pushed the original formula beyond its six-Boolean
cofactoring threshold, producing UNKNOWN at 100M work. The one-bit encoding
retains every assertion and witness meaning, and uses the existing 32-case
cofactor route. This is a documented representation sensitivity, not a claim
of representation-independent solver scaling or a relaxed contract.

### Concrete memory and composition

The memory module captures seed cells only on reset and uses complete case
reads with explicit zero defaults. Its contract independently records frozen
seed witnesses, checks reset equality against them, proves storage preservation
for arbitrary future seeds, and proves both observed read values equal the
frozen memory at the exact request address. All ports are explicitly typed;
only the two actual response outputs are sampled. The witnesses are proof-only,
not extra CPU or memory hardware.

`architectural_composition` alpha-renames actual CPU and memory wires and
substitutes their actual ports. Address dependencies must be state-only, all
interface ports must be connected, and a response/address combinational loop
is rejected. No contract conclusion is added as an assumption. The resulting
machine is checked with the prior architectural ISA binding style, including
ROM identity of every valid D/X/W instruction and immutable-data equality of
valid load results. Its eight obligations include reset, refinement, hold and
conditional progress. This full concrete proof is what connects the local
arbitrary-response theorem to fixed program behavior for each measured depth.

**Remaining gap:** the full architectural composition still expands all concrete
memory cells and does not reuse separately proved component certificates. The
local CPU theorem is reusable across these capacities because its interface
and document have no capacity parameter; this does not make full-program proof
cost capacity-independent. Immutable abstract functions plus a checked
consistency/composition rule are future work.

## Immutable-memory reuse

### Proof dependency graph

1. Compile and lift the actual CPU. Replace only its two memory response inputs
   by reads of immutable typed array snapshots at the actual imported request
   expressions. Request expressions must be state-only. Validate every wire,
   including unused wires, against cycles and missing dependencies.
2. Prove the exact current local CPU port contract, including valid/read-enable
   semantics, as a required prerequisite. Also prove reset, ISA
   microstep/stuttering refinement, hold and conditional progress
   using the existing version-2 checker. The independent ISA and provenance
   relation use the same abstract memory semantics, with separate ROM/data
   identities and all six address bits. There are no concrete backing cells or
   capacity parameter in this proof. Snapshot equality remains a state relation.
3. For each connected concrete memory, compile/lift it and regenerate the exact
   existing memory contract from that imported machine. Prove reset seed capture,
   nonreset storage preservation, and both total combinational read equations,
   including zero at unmapped addresses. This proof depends on capacity; the CPU
   theorem does not.
4. Check the exact typed wiring and consume the two completed independent proof
   handles. Neither proof assumes the composition conclusion or the other proof.

The emitted composition record is diagnostic evidence, **not an UNSAT
certificate**. A new process cannot load it as proof. Successful checker calls
create opaque handles in an execution-session ledger; composition rechecks source,
frontend/lifter binaries, compiled and lifted sidecars, actual machine, exact
contract, interface and checker hashes. A stale/mismatched import, altered source,
changed checker, fabricated JSON verdict, wrong wiring, or proof dependency cycle
fails closed. SHA-256 binds identity, not validity. The finite solver, lowering,
compiler/importer and checked substitution implementation remain trusted.

### Trusted substitution rule and justification

For a concrete memory with frozen cells `rom[j]` and `data[j]`, define total
functions `ROM(a)` and `DATA(a)` as the corresponding in-range cell, otherwise
zero. The independently verified reset establishes those cells from the provided
seeds; every nonreset step preserves all cells. The verified read equations make
the actual response at each connected address exactly the corresponding function
application. Thus every concrete execution has an abstract execution obtained by
replacing cells with these two total functions. CPU state, requests, responses,
retirements and architectural observations coincide step by step. The universally
quantified abstract ISA theorem therefore instantiates to that concrete memory.
This argument allows arbitrary contents and any supported backing capacity within
the fixed 64-address space (measured capacities: 4/16/64); it assumes no labels
about memory behavior. Reset starts the shared new snapshot on both sides.

The array solver's read-only elimination is a separate trusted semantic rule. It
must preserve all read applications and extensional equality in the fully lowered
whole obligation and validate SAT using reconstructed total-array semantics.
No user-provided list of congruences or weakened assumptions enters this harness.
Unsupported array operations or solver exhaustion must remain Unknown/failure.

### Run

```sh
source /workspace/scratch/07f10565586c/recovered/tools/env.sh
python3 -m unittest audit.veryl_scaling.test_cpu_memory_reuse
python3 audit/veryl_scaling/cpu_memory_reuse.py --out /fresh/evidence/path
```

Runtime is finite-only. The harness installs an external-solver tripwire and does
not alter existing solver budgets. `summary.json` records one abstract CPU proof
run reused across 4/16/64-word memories, with the actual obligation evidence.
Functional ISA refinement does not itself claim that unused valid/enable outputs
are correct; the CPU proof handle additionally requires the exact current local
port-contract gate to check those signals. The `load_enable` negative control
is therefore rejected by that prerequisite even though always-readable memory
makes it irrelevant to the architectural trace. Use `--negative-controls` for
three wiring and ten imported CPU fault controls. This is custom-ISA retirement/progress, not arbitrary program termination.

## Writable-memory STORE reuse

### Instruction and ordering

Opcode 5 is `STORE rs, immediate[5:0]`: write the value of the selected register to
the six-bit word address. `rd` is ignored and no register is written. All eight
registers, including r0, remain writable. The source goes through the ordinary
D-stage forwarding and load-use interlock and is captured in W's result register.
Only a valid, nonstalled W STORE writes. Reset discards in-flight instructions;
seed capture wins over writes. A taken X branch kills younger D/fetched work while
allowing the older W instruction to retire.

The memory has explicit zero-latency write-through: an X LOAD at the same address
as a retiring W STORE sees the newly written value on that edge. Different
addresses read old cells. A stalled CPU neither retires nor writes. Back-to-back
stores preserve program order. ROM never changes after reset. Unmapped reads
return zero; unmapped writes are ignored, including the bypass path. Addresses
are not truncated or wrapped to small backing capacity.

### Independent abstract ISA and invariant

The architectural reference separately decodes STORE and reads its source from
architectural registers. The relation proves W's captured store value equals
that source, and X operands equal the architectural register view after pending W
register writes. The reference never reads DUT result/payload registers.

ROM is an immutable typed array, data is an evolving typed array. Both sides
perform the same guarded array update at retirement. A W LOAD result is related
to current data: W cannot itself store, while the transition that moved the LOAD
from X to W already included the older retiring store's write-through. Thus a
historical load cannot silently be replaced with a later unrelated response.

A seven-bit immutable backing limit belongs to the proof's memory model, not the
CPU RTL. The CPU theorem universally quantifies this configuration and contains
no concrete capacity. Values above 64 map all six-bit addresses. Composition
instantiates the limit exactly to the independently proved concrete capacity.
The memory model guards reads/updates with zero-extended address < limit. Its
concrete instantiation is the total zero-padded array of physical cells.

### Checked step simulation, not immutable substitution

1. Import the actual CPU and prove its independent local retirement/port contract.
2. Substitute its actual memory request expressions into the abstract memory
   transition and write-through response. Prove reset, state relation,
   microstep/stuttering refinement, stall hold and conditional progress.
3. Independently import each concrete memory. Prove reset seed capture, accepted
   mapped write update, frame preservation of every other data cell, immutable
   ROM, exact read/write-through association, and unmapped-zero behavior.
4. Require a shared logical reset for CPU and memory (the CPU physical rst_n
   polarity is normalized by import), with stall connected to CPU only. Check
   all typed port connections and consume successful, current-session
   proof handles bound to source, frontend, lifter, imported machine, checker,
   exact contracts and capacity. Saved JSON verdicts cannot create handles.

The trusted composition rule is a *step-preserving simulation*: reset makes the
concrete zero-padded lookup equal to the abstract initial array; each accepted
write updates exactly the same mapped cell; frame/ROM conditions preserve all
others; the two read equations agree at every step. This rule does not assume
immutable data, and no component assumes the desired composition conclusion.
Reports are diagnostic evidence, not independently checkable UNSAT certificates.

The finite solver must support array store semantics and validate SAT witnesses
against the original total-array formula. Unknown, unsupported fragments or
backends, missing obligations and unvalidated SAT are failures. Budgets remain
100M work, 1M clauses and 10 seconds per query, with a Z3 tripwire and no runtime
fallback. Progress retains continuously enabled/no-future-reset qualifications;
no arbitrary program termination claim is made.

### Run

```sh
python3 -m unittest audit.veryl_scaling.test_cpu_store_reuse audit.veryl_scaling.test_cpu_writable_memory
python3 audit/veryl_scaling/cpu_store_reuse.py --out /fresh/store-evidence --negative-controls
```

Negative controls cover imported CPU store source/address/enable/retirement,
interlock/forwarding/flush and legacy request/data faults; write wiring and missing
write-through; concrete write acceptance/address/value/frame/ROM/reset/unmapped
faults. Real finite original-formula-validated counterexamples are required.

Historical capacity/timing tables and failures remain in the adjacent JSON/log evidence. Fresh required readonly, writable-memory and RTL controls run through `conformance/veryl-symbolic/run_ci.sh`; see the [audit index](../audit/README.md).
