# Selected RV32I contract and validation

The selected four-stage source is
`conformance/veryl-symbolic/rv32i_pipeline_split_onehot_candidate.veryl`, SHA-256
`b5c634c60b31c07313f5c54b069b5428013fa162b63f96e24cbb0f56f74722ab`.
Its manual proof path closes 436/436 preservation children plus seven globals,
with fresh 4/16/64-word memory compositions. The original monolithic query stays
Unknown. Hint-free automatic discovery closes 431/436 and remains Unknown overall.
Current native proposals and their source-linked failures are covered by
[the lemma gate](lemmas.md). These are separate from the original D/X/W baseline,
whose full architectural induction remains a research/Unknown result.

## Fresh acceptance

```sh
./conformance/veryl-symbolic/run_rv32i_ci.sh /tmp/fresh-selected-rv32i
```

After the original proof-corpus gate prepares the pinned frontend, this gate
imports actual RTL and checks independent ISA
and memory specifications, required source lemmas, full architecture, token
latency/selector alignment and fault controls. Unknown fails closed. Fresh
source-bound handles are required; source/helper/tool hashes are checked again
before accepting composition. Per-query limits remain 100M work, 1M clauses and
10 seconds; independent bundles have larger, reported aggregate costs. Current
CI and archived results must be distinguished; [the audit index](../audit/README.md)
locates both.

## Instruction scope

The decoder implements all 40 base instructions:

- LUI, AUIPC, JAL, JALR
- BEQ, BNE, BLT, BGE, BLTU, BGEU
- LB, LH, LW, LBU, LHU, SB, SH, SW
- ADDI, SLTI, SLTIU, XORI, ORI, ANDI, SLLI, SRLI, SRAI
- ADD, SUB, SLL, SLT, SLTU, XOR, SRL, SRA, OR, AND
- FENCE, ECALL, EBREAK

Instruction words, byte PCs and arithmetic are 32 bits. All 32 architectural
registers are present. x0 reads zero and ignores writes; a load to x0 still
requests memory and may trap. Immediate extraction follows the actual I/S/B/U/J
formats, including sign extension before SLTIU. Register shifts use only rs2[4:0].
JALR reads the old source register, adds its signed immediate, and clears bit 0
before testing IALIGN=32. An untaken branch never faults for its hypothetical
misaligned target. All seven funct7 bits are checked where required.

Reserved or unsupported encodings raise illegal-instruction traps. RV32M,
compressed instructions, CSR instructions, FENCE.I and privileged instructions
are not implemented. FENCE's reserved rd/rs1/fm/pred/succ fields are ignored as
required for forward compatibility, treating each accepted FENCE conservatively
as a full fence. In this single-hart, ordered, side-effect-free memory
EEI, all older writes retire before subsequent accesses can observe memory, so
FENCE requires no additional state. This is not a claim of MMIO, multicore or
self-modifying-code support.

## Explicit execution environment

The reset entry is byte address 0; all GPRs reset to zero. The instruction and data
ports are zero-latency views of one disjoint byte-address map: ROM starts at
0 (instruction fetch and data reads; stores fault), and RAM starts at 0x1000
(data reads/writes; instruction fetch faults). Each region contains N words,
with separately checked N=4/16/64 implementations. Both ports use full32-bit
byte addresses. The memory environment
returns the aligned little-endian 32-bit word containing `dmem_address` and a
separate access-fault bit. Address validation precedes finite backing-cell
selection; high address bits are never silently discarded. Natural alignment is
mandatory: byte accesses have no alignment restriction, halfwords require bit 0
zero, and words require bits 1:0 zero.


## Precise terminal traps

`commit` means successful W retirement. `trap_valid` is a separate, one-cycle
terminal fault event, and `retire = commit || trap_valid`. Faults propagate through the selected M/W stages and are reported once when not stalled; older work retires and younger work is squashed. No destination or
store effect of the faulting instruction is applied. The architectural PC is the
faulting instruction's PC; `halted` becomes true. Reset is the only exit from
this terminal state. There is no trap vector, handler, CSR state or privileged
architecture claim.

The EEI reports these cause numbers and trap values:

- 0: instruction-address misalignment; offending byte address
- 1: instruction-access fault; instruction PC
- 2: illegal instruction; complete instruction word
- 3: EBREAK; instruction PC
- 4 / 6: load / store address misalignment; effective byte address
- 5 / 7: load / store access fault; effective byte address
- 8: ECALL from the chosen U-mode EEI; zero

Fault priority is incoming-PC alignment, fetch access, illegal instruction,
ECALL/EBREAK, taken-target alignment, data alignment, then data access. The
incoming-PC alignment case is unreachable from aligned reset and successful
control flow, but remains explicitly defined. A jump to an aligned unmapped
address successfully retires its link and target PC; the target instruction then
reports an instruction-access fault. A misaligned jump target instead faults the
jump itself and suppresses its link write.

Progress must count either successful retirement or this terminal trap, exclude
already halted states, and retain continuously-enabled/no-future-reset fairness.
It must not assume an illegal instruction is a NOP or call permanent post-trap
stuttering forward progress.

## Selected pipeline and global relation

D captures instruction, fault and one-hot read selectors together and registers
predecode, operands and immediates into X. X computes ALU/address/control payloads
into M. M handles load/store lanes, exception priority and final payload selection.
W alone retires, stores, redirects or traps. A W redirect/trap flushes D/X/M,
cancels the current M-to-W transfer and suppresses M requests. Stall freezes all
state and suppresses request/retirement/store pulses.

The selected source uses youngest M-over-W forwarding only for registered safe
ALU/LUI/AUIPC results. D interlocks on matching X writers and M writers without
safe bypass; loads and jumps wait for W. The safe-M predicate requires legality,
aligned instruction PC, no fetch fault and nonzero destination, independently of
the current combinational M fault. No additional pipeline stage is introduced by
registered one-hot RF selection.

The global relation tracks pending PCs in W/M/X/D order. M uses registers after
pending W; X also uses the safe M bypass. Younger semantic relations retain older
redirect/fault guards, while structural provenance remains checked. W effects
are never dropped. M reads memory after the same-edge W byte-mask update; finite
memory permission/address checks precede backing-cell selection. The source model
must not introduce a later store failure after its permission check. No changing
permissions, MMIO or side-effecting speculative reads are modeled.

All 91 sequential fields are imported with actual reset/next equations. Reference
expressions never replace DUT logic without a fresh exact source equation. The
selected recipe uses RF normalization, four X arithmetic next-field equalities,
transport/control invariants, D dispatch normalization and checked proof programs;
it does not use the optional history observer. Source lemmas cover complete D
decode/selectors, X-to-M, M-to-W and W retirement, including illegal encodings.
Guarded payload lemmas need the global induction to establish their premises.

Nine actual-source CPU mutants and eighteen memory mutants are acceptance
controls; SAT witnesses are checked against the original generated formulas.
Concrete imported execution is complementary evidence, not a substitute for the
full induction. No ACT4/RISCOF/Sail or silicon certification is claimed.

## Token latency and selector alignment

### Architecture

- `rv32i_latency_variants.py`: immutable source/top/SHA/family manifest and explicit
  nonzero-reset requirements
- `rv32i_latency_models.py`: two separate, explicit source/control builders,
  baseline D/X/W and four-stage D/X/M/W; they are not inferred from stage count
- `rv32i_latency_common.py`: source import, reset projection checks, exact named
  conjunct/bit partitioning, strict finite verdict gates, mutation replay gates,
  dependency snapshots, artifact hashes, and fresh execution
- `rv32i_latency_witnesses.py`: shared actual-import nonvacuity traces, parameterized
  by the pinned variant without importing unittest modules
- `rv32i_latency_test_support.py` and `test_rv32i_latency_shared.py`: compatibility
  tracker tests and fail-closed runner regression coverage

The importer rejects unknown fixture syntax, missing reset assignments, unknown
sequential widths, source hashes outside the manifest, and frontend diagnostics.
Actual nonzero source reset values are preserved. Only the modeled control flags
are required to reset false; the one-hot selectors explicitly reset to BV32(1).

Every run invokes the frontend, lifter, and finite checker again. It hashes the
checker/frontend/lifter and executable helper dependencies, including transitive
expression helpers and the concrete interpreter. Python source snapshots are
captured under their content hashes. Every output file is hashed in the final
certificate, and dependencies are checked again before success. Unknown fails
closed; no saved report can create a proof handle. No elevated work budget or
Z3 fallback is enabled. The existing Rust checker/kernel remain trusted code,
not an independently checked Lean certificate.

### Independent selector-alignment proofs

For the selected one-hot variant, two additional, separately reported proofs
establish:

- d_sel1 = 32-bit(1) << zero_extend(d_ir[19:15])
- d_sel2 = 32-bit(1) << zero_extend(d_ir[24:20])

`selector_alignment_document(raw, selector)` in the model module preserves the
actual D instruction and chosen selector as state, retaining their exact imported
reset/next equations. Other source state is universally abstracted into prestate
inputs. The result is a conservative projection whose invariant is proved by
reset establishment and induction, covering reset, holds, and flushes. No latency
invariant or instruction semantics is assumed. Conversely, these selector lemmas
are not assumptions of token latency. Their reports appear under
`auxiliary_proofs`, separately from the latency conjunction and token proof.

An imported-IR stale-selector next-state mutation for each selector (not a
recompiled RTL mutation) must produce finite SAT
with original-formula replay. This guards against a vacuous alignment harness.
For use in a later architectural proof, build and freshly check these documents
inside that proof's source-bound session; do not treat saved reports as authority.

### Running and scope

```
python -m audit.veryl_scaling.rv32i_latency --checker /final/checker --out /new/baseline
python -m audit.veryl_scaling.rv32i_latency_onehot_candidate --checker /final/checker --out /new/onehot
python -m unittest audit.veryl_scaling.test_rv32i_latency_shared \
  audit.veryl_scaling.test_rv32i_latency \
  audit.veryl_scaling.test_rv32i_latency_onehot_candidate
```

The baseline bound is four subsequent enabled edges, nominal three. The selected
four-stage bound is six, nominal four. A token terminates by its own retirement,
its own trap, or squash by an older control event. Reset cancels the epoch.
Unbounded external stalls prevent an elapsed-time bound; zero-wait memory-port
assumptions remain. These proofs do not establish whole-CPU ISA correctness or
physical timing. Fresh final-checker evidence and independent audit are required
after changing the runner, model, source, or checker.

## Physical timing

[Synthesis reproduction](https://github.com/tignear/hwverify/blob/ae8f84cca8859097ac2b0a7a4f6e311355c2b48d/synthesis/README.md) preserves the five distinct
source-bound experiments and every measured seed. Selected one-hot seeds 1 and 2
meet the exploratory 100 MHz model; seed 3 fails. This is not board sign-off or
arbitrary-placement assurance. Timing, bounded emitted-SV simulation, token-cycle
latency and architectural refinement remain separate claims.
