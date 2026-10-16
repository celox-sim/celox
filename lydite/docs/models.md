# CPU example models

These abstract custom-ISA fixtures are distinct from the imported [selected RV32I implementation](rv32i.md). Historical timings and run logs remain in `audit/` and `results/`; fresh validation commands are in the [audit index](../audit/README.md).

## Pipeline model

`examples/pipeline.lyd` / `pipeline.json` is a declarative model of an in-order CPU
that actually holds up to three instructions at once. It is separate from the earlier
example that alternates fetch→execute one instruction at a time.

- D: fetched instruction. Reads operands from registers or via forwarding and sends them to X
- X: computation. ALU results can be forwarded to D. LOAD results cannot be passed to D until they are registered in W
- W: retires in program order. Only W updates the architectural registers and the retirement PC
- W retire, X→W, D→X and fetch of the next instruction can happen in the same cycle
- If D uses a LOAD result in X, D/fetch is held and a bubble is inserted into X. The LOAD in X advances to W
- If both X and W write D's source register, the younger X wins. A load-use on the next cycle forwards from W
- An external stall holds all implementation state. Synchronous reset takes priority over stall and empties all stages

Data width 4 bits, 2 registers, 2-bit PC, a circular 4-instruction ROM, readonly data of 4 words.
ROM and data capture **arbitrary inputs** at reset and are immutable afterwards. No particular
program is assumed. PC and addition wrap. Branches, HALT, STORE, exceptions, caches, prediction and OoO are not included.
Execution does not stop after 4 instructions; it runs an arbitrary number of times while the PC wraps.

Instruction 8 bits: `op[7:6] rd[5] rs[4] imm[3:0]`.

| op | Instruction | Meaning |
|---|---|---|
| 0 | MOVI | r[rd] = imm |
| 1 | ADDI | r[rd] = r[rs] + imm (mod16) |
| 2 | XORI | r[rd] = r[rs] xor imm |
| 3 | LOAD | r[rd] = data[imm & 3] |

## Meaning of the unbounded verification

The ISA executes one instruction atomically. The ISA takes one step only when the implementation retires in W; otherwise it stutters.
The following relation is preserved inductively from reset.

1. The architectural registers, retirement PC and ROM/data of the ISA and the implementation agree
2. A valid W holds the instruction at the current retirement PC, and W.result is that instruction's result in the current ISA state
3. Ordering: a valid X follows W, and a valid D follows X. Each IR matches the corresponding ROM[PC]
4. When X uses a source, its operand equals the current register value with the pending W write applied
5. The fetch PC is the retirement PC + number of valid stages (mod4)

The payload of an invalid stage is given no meaning. Current inputs, concrete execution observations and guesses about reachable states
are not added to the binding. Nor are the ISA and binding weakened to let a bad implementation pass.

The 8 obligations are binding nonvacuity, reset, microstep refinement, commit eligibility, stall hold,
progress nonvacuity, commit nonvacuity, and rank decrease on noncommit. The rank is 0/1/2/3 according to the oldest
valid stage among W/X/D. It always decreases on a non-stalled noncommit cycle. Therefore, under the condition that
the external stall keeps being released, execution advances to the next retire. This does not prove that infinite stalls go away or that the program terminates.

This verification is a **one-step inductive proof** over a finite-size state space, not merely a success of
BMC unrolled to a finite number of cycles or of simulation. On the other hand, the adequacy of the specification and binding and agreement with actual RTL are
separate issues, and this example is not RTL import / synthesis either.

## Branch pipeline targets and instructions

`examples/build_branch_pipeline.py` is a generator separate from the earlier `build_pipeline.py`.
`branch_pipeline_w{4,8,16,32}.{json,lyd}` share the same D/X/W control structure and vary only the data width.
The existing pipeline example and past audit results are unchanged. This is not RTL import or a model of an out-of-order CPU.

At every width: 2 registers, 2-bit PC, 4-instruction ROM, readonly data of 4 words. ROM/data capture arbitrary inputs at reset
and ignore later input changes. No particular program is assumed.
Execution does not stop after 4 instructions; the target is an arbitrary number of executions including wrap, branches and self-loops.

With data width W, the instruction width is W+5 bits.
`op[W+4:W+2] rd[W+1] rs[W] imm[W-1:0]`.

| op | Instruction | Meaning |
|---|---|---|
| 0 | MOVI | r[rd] = imm, PC = PC+1 |
| 1 | ADDI | r[rd] = r[rs]+imm (mod 2^W), PC = PC+1 |
| 2 | XORI | r[rd] = r[rs] xor imm, PC = PC+1 |
| 3 | LOAD | r[rd] = data[imm & 3], PC = PC+1 |
| 4 | BZ | if r[rs]=0 then PC=imm & 3, otherwise PC=PC+1. No register write |
| 5–7 | NOP | No register write, PC = PC+1 |

The PC wraps mod4. The BZ target is an absolute 2-bit address, not a relative offset; rd and the upper bits of the immediate
do not affect behavior. Non-branch instructions have the same meaning as in the old pipeline, but the instruction encoding differs.
STORE, mutable memory, exceptions, HALT, caches, predictor training and OoO are out of scope.

## Cycle semantics of X resolution and flush

- Fetch always uses always-not-taken prediction, advancing to PC+1
- D selects its source from the registers, the ALU result in X, or the result in W. The younger X takes priority over W
- BZ also consumes a source. If it depends on the immediately preceding LOAD, D/fetch is held and a bubble is inserted into X.
  On the cycle after the LOAD advances to W, it can forward from W
- Branch/NOP are not writing instructions, so they are neither X/W forwarding candidates nor RAW producers
- If a valid BZ in X is taken, the current D and the sequential fetch of the same cycle are discarded.
  The next X/D are invalid and the fetch PC is redirected to the target. Payloads need not be zeroed
- Even in the same cycle, an older W retires normally. The resolved branch in X itself advances to W
- W holds `w_next_pc`, and the architectural retirement PC is updated only when the branch retires.
  The fetch PC redirect itself is not an architectural effect
- On not-taken, normal D→X and fetch→D continue
- An external stall holds all implementation state, including redirect/flush. Synchronous reset takes priority over stall,
  invalidates all stages, returns PC/registers to 0 and recaptures ROM/data

An instruction in X is never both LOAD and BZ, so a taken branch and a load-use hazard caused by the same X
cannot occur simultaneously. The interactions to check are waiting on LOAD→BZ, and external stall/reset at branch resolution.

## Inductive relation and claims

The ISA executes one instruction atomically and takes one step only on an implementation W commit. Otherwise it stutters.
The next binding is described using only the current state. Current inputs and simulation observations are not put into the assumptions.

1. The architectural registers, retirement PC and captured ROM/data agree between the ISA and the implementation
2. A valid W holds the ROM instruction at the current PC. If it is a writer, W.result matches the ISA result, and
   for every instruction W.next_pc matches the ISA's next PC
3. A valid X holds the instruction at W's resolved next_pc (the current PC if there is no W).
   For an X that uses a source, its operand equals the current register value with the pending W's effective write applied
4. D and fetch are at the predicted sequential positions after X. Even if X is taken, it is not yet resolved, so a wrong-path instruction in D is allowed.
   In the next state it is killed according to X's resolution
5. The payload of an invalid stage and W.result of a non-writer are given no meaning

With this, one-step preservation checks that even when a wrong-path instruction exists in D it never reaches a write to
the architectural registers, and that the legitimate write of an older W and the retirement of the branch itself are not lost.
Architectural effects are concentrated solely in W's valid/write guard.

The same 8 obligations as before are used (binding nonvacuity, reset, microstep refinement, commit eligibility, stall hold,
progress nonvacuity, commit nonvacuity, rank decrease on noncommit). The rank is 0/1/2/3 according to the oldest valid among W/X/D.
On a non-stall, noncommit cycle the next rank strictly decreases, so under the condition that the external stall keeps being released, execution advances to the next retire.
Program termination, reaching a branch target, and resolution of infinite stalls are not claimed.
Nonvacuity SAT shows the relation is nonempty; it is not a proof of reset reachability for an arbitrary state obtained from SAT.

This is a time-unbounded one-step inductive refinement over a finite-bit-width state space.
It is not merely a success of finite-cycle simulation/BMC.
UNSAT from the finite solver has no independent certificate; the Rust bit-blast/CDCL, the structural kernel, lowering and
the adequacy of the binding are trusted. The existing Lean memory theorems are not claimed to prove the new model or the solver.
