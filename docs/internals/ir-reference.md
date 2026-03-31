# SIR Intermediate Representation Reference

SIR (Simulator Intermediate Representation) is the execution IR for Celox.
It lowers Veryl analysis results into a register-based instruction sequence that serves as input to the compilation backends (native x86-64 or Cranelift JIT).

## Overview

-   **Register-based**: SSA-like representation using virtual registers (`RegisterId`)
-   **CFG representation**: Control flow via `BasicBlock` + `SIRTerminator`
-   **Region-qualified memory**: Bit-precision access through `RegionedAbsoluteAddr` and `SIROffset`

## Address System

| Type | Purpose | Stage |
| :--- | :--- | :--- |
| `VarId` | Module-local variable ID | Within `SimModule` |
| `AbsoluteAddr` | Global variable (`InstanceId` + `VarId`) | After flattening |
| `RegionedAbsoluteAddr` | Address with memory region (Stable/Working) qualifier | Execution/optimization |
| `SignalRef` | Physical memory address handle for execution | Execution (fast access) |

## Key Data Structures

### `Program`

A struct representing the entire simulation. A notable characteristic is that flip-flop evaluation is split into three variants.

```rust
pub struct Program {
    pub eval_apply_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    pub eval_only_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    pub apply_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    pub eval_comb: Vec<ExecutionUnit<RegionedAbsoluteAddr>>,
    pub eval_comb_plan: Option<EvalCombPlan>,
    // ... other metadata (instance maps, clock domains, arena, etc.)
}
```

-   **`eval_apply_ffs`**: Standard synchronous flip-flop evaluation. Used when operating in a single domain.
-   **`eval_only_ffs`**: Phase that only computes the next state and writes it to the Working region.
-   **`apply_ffs`**: Phase that commits values from the Working region to the Stable region.
-   **`eval_comb_plan`**: Compilation plan for `eval_comb` when the estimated CLIF instruction count exceeds Cranelift's internal limit (~16M instructions). See [Tail-Call Splitting](./optimizations.md#27-tail-call-splitting) for details.

### `EvalCombPlan`

Describes how `eval_comb` should be compiled when the default single-function approach would exceed Cranelift's instruction index limit.

```rust
pub enum EvalCombPlan {
    /// Split into tail-call-chained functions with live regs passed as arguments.
    TailCallChunks(Vec<TailCallChunk>),
    /// Split with inter-chunk register values spilled through scratch memory.
    MemorySpilled(MemorySpilledPlan),
}
```

### `ExecutionUnit`

The smallest unit of execution.

```rust
pub struct ExecutionUnit<A> {
    pub entry_block_id: BlockId,
    pub blocks: HashMap<BlockId, BasicBlock<A>>,
    pub register_map: HashMap<RegisterId, RegisterType>,
}
```

## Instruction Set

### Data Movement
-   `Imm(rd, value)`: Immediate value assignment

### Memory Access
-   `Load(rd, addr, offset, bits)`: Memory load with bit-precision offset
-   `Store(addr, offset, bits, rs, triggers)`: Memory store (RMW) with trigger notifications
-   `Commit(src, dst, offset, bits, triggers)`: Cross-region copy with trigger notifications

### Arithmetic and Logic
-   `Binary(rd, rs1, op, rs2)`: Binary operation (Add, Sub, Mul, And, Or, Xor, Shift, comparison, etc.)
-   `Unary(rd, op, rs)`: Unary operation (Not, Neg, etc.)

### Bit Manipulation
-   `Concat(rd, [msb..lsb])`: Register concatenation (first element is MSB)
-   `Slice(rd, rs, offset, width)`: Bit range extraction (`rd = rs[offset +: width]`)

## Control Flow

-   `Jump(block_id, args)`: Unconditional branch (with block arguments)
-   `Branch { cond, true_block, false_block }`: Conditional branch
-   `Return`: End of execution
-   `Error(code)`: Runtime error

## MIR (Machine-level IR)

MIR sits between SIR and x86-64 machine code in the native backend pipeline. It is a word-level SSA IR where all operands are virtual registers (`VReg`).

### Key Differences from SIR

-   **Word-level**: Instructions operate on 64-bit values, not arbitrary bit widths
-   **3-operand form**: `(dst, src1, src2)` — the emit phase handles x86-64's 2-operand constraint
-   **Immediate forms**: Separate instruction variants for immediate operands (`AndImm`, `ShrImm`, `AddImm`, etc.)
-   **Hardware-specific**: Includes `UDiv`, `URem` (uses RAX/RDX), `Popcnt`, `Pext` (BMI2)

### MIR Instruction Categories

| Category | Instructions |
| :--- | :--- |
| Data movement | `Mov`, `LoadImm` |
| Memory access | `Load`, `Store`, `LoadIndexed`, `StoreIndexed` |
| ALU (register) | `Add`, `Sub`, `Mul`, `UMulHi`, `And`, `Or`, `Xor`, `Shr`, `Shl`, `Sar` |
| ALU (immediate) | `AndImm`, `OrImm`, `ShrImm`, `ShlImm`, `SarImm`, `AddImm`, `SubImm` |
| Comparison | `Cmp { kind }`, `CmpImm { kind }` |
| Division | `UDiv`, `URem` |
| Unary | `BitNot`, `Neg`, `Popcnt`, `Pext` |
| Select | `Select { cond, true_val, false_val }` (cmov) |
| Control flow | `Branch`, `Jump`, `Return`, `ReturnError` |

### Spill Descriptors

The register allocator uses `SpillDesc` to make cost-aware spill decisions:

```rust
pub enum SpillKind {
    /// Reload from simulation memory (zero spill cost — value already in memory)
    SimState { addr, bit_offset, width_bits },
    /// Spill to stack slot
    Stack,
    /// Rematerialize from constant (zero spill cost)
    Remat { value: u64 },
}
```
