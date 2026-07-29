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
    // ... instance maps, clock domains, arena, etc.
    pub reset_clock_map: HashMap<AbsoluteAddr, AbsoluteAddr>,
    pub address_aliases: HashMap<AbsoluteAddr, AbsoluteAddr>,
    pub layout: Option<MemoryLayout>,
    pub initial_statements: Option<Vec<Statement>>,
    pub tb_functions: FxHashMap<VarId, Function>,
}
```

-   **`eval_apply_ffs`**: Standard synchronous flip-flop evaluation. Used when operating in a single domain.
-   **`eval_only_ffs`**: Phase that only computes the next state and writes it to the Working region.
-   **`apply_ffs`**: Phase that commits values from the Working region to the Stable region.
-   **`eval_comb_plan`**: Compilation plan for `eval_comb` when the estimated CLIF instruction count exceeds the safety threshold used to stay below Cranelift's instruction/value limits. See [Tail-Call Splitting](./optimizations.md#214-tail-call-splitting) for details.
-   **`reset_clock_map`**: Maps each reset `AbsoluteAddr` to its associated clock `AbsoluteAddr` (derived from `FfDeclaration`).
-   **`address_aliases`**: Memory layout aliases mapping non-canonical → canonical addresses. Variables with identity `Store→Load` roundtrips share physical memory (populated by `IdentityStoreBypass`).
-   **`layout`**: Pre-computed `MemoryLayout`. Built after optimization, before backend codegen. Centralizes offset calculation so all backends share the same layout.
-   **`initial_statements`**: Initial block statements from the top-level module (for native testbenches).
-   **`tb_functions`**: Functions defined in the top-level module (for testbench function calls via the bytecode VM).

### `EvalCombPlan`

Describes how `eval_comb` should be compiled when the default single-function approach would exceed Cranelift's instruction index limit.

```rust
pub enum EvalCombPlan {
    /// EU-boundary or single-block splitting with live regs passed as tail-call args.
    TailCallChunks(Vec<TailCallChunk>),
    /// Multi-block splitting with inter-chunk register values spilled through scratch memory.
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
-   `Concat(rd, [msb..lsb])`: Register concatenation (first element is MSB). Pure data movement that preserves Z bits in 4-state mode.
-   `Slice(rd, rs, offset, width)`: Bit range extraction (`rd = rs[offset +: width]`)

### Select
-   `Mux(rd, cond, then_val, else_val)`: Conditional select. In 4-state mode, preserves exact mask bits (including Z) of the selected branch. When `cond` has X/Z bits, the result is all-X.

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
    /// Value lives in simulation state at a known location.
    /// Reload = load from [sim_base + byte_offset] (+ optional shift/mask).
    SimState { addr: RegionedAbsoluteAddr, bit_offset: usize, width_bits: usize },
    /// Intermediate value with no home in simulation state. Spill to a stack slot.
    Stack,
    /// Constant that can be cheaply rematerialized (mov imm).
    Remat { value: u64 },
}

pub struct SpillDesc {
    pub kind: SpillKind,
    /// Estimated cost (in x86-64 instructions) to reload this value.
    pub reload_cost: u8,
    /// Estimated cost to spill. 0 if the value is already in memory.
    pub spill_cost: u8,
}
```
