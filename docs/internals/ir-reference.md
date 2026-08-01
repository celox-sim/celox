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

### Phase artifacts

Compilation state is represented by distinct types. There is no general object whose valid fields
depend on which passes happened to run.

```rust
pub struct UnoptimizedSir {
    pub sir: SirProgram,
    pub layout_requirements: LayoutRequirements<AbsoluteAddr>,
    pub runtime: RuntimeProgram,
}

pub struct OptimizedSir {
    pub sir: SirProgram,
    pub layout_requirements: LayoutRequirements<AbsoluteAddr>,
    runtime: RuntimeProgram,
}

pub struct LaidOutProgram {
    pub sir: SirProgram,
    runtime: RuntimeProgram,
    layout: MemoryLayout,
}

pub struct RuntimeProgram {
    pub design: ElaboratedDesign<AbsoluteAddr>,
    pub frontend: VerylFrontendLookup,
    pub runtime_schema: RuntimeSchema<AbsoluteAddr>,
    pub testbench: Option<TestbenchProgram<AbsoluteAddr>>,
}

pub struct SirProgram {
    pub eval_apply_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    pub eval_comb_apply_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    pub eval_only_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    pub apply_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    pub eval_comb: Vec<ExecutionUnit<RegionedAbsoluteAddr>>,
}
```

-   **`UnoptimizedSir`**: Internal compiler-driver result before the backend-independent SIR pass pipeline.
-   **`OptimizedSir`**: The only pre-layout artifact accepted by layout construction.
-   **`LaidOutProgram`**: Immutable SIR plus finalized physical layout accepted by concrete backends. Layout requirements have been consumed.
-   **`RuntimeProgram`**: Design, source lookup, runtime schema, and compiled testbench retained after code generation. It cannot contain SIR or layout requirements.
-   **`eval_apply_ffs`**: Standard synchronous flip-flop evaluation. Used when operating in a single domain.
-   **`eval_comb_apply_ffs`**: Fused comb/FF evaluation selected by the frontend scheduler.
-   **`eval_only_ffs`**: Phase that only computes the next state and writes it to the Working region.
-   **`apply_ffs`**: Phase that commits values from the Working region to the Stable region.
-   **`layout_requirements`**: Semantic physical-layout constraints, including validated candidates mapping non-canonical → canonical state homes. `IdentityStoreBypass` populates these aliases; layout verifies representation compatibility before sharing memory and consumes the requirements when producing `LaidOutProgram`.

Cranelift oversized-function plans and x86 MIR/register allocation are backend-private results;
they are not fields of SIR artifacts.

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
