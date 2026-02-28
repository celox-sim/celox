# SIR Intermediate Representation Reference

SIR (Simulator Intermediate Representation) is the execution IR for Celox.
It lowers Veryl analysis results into a register-based instruction sequence that serves as input to the Cranelift JIT.

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
    // ... other metadata
}
```

-   **`eval_apply_ffs`**: Standard synchronous flip-flop evaluation. Used when operating in a single domain.
-   **`eval_only_ffs`**: Phase that only computes the next state and writes it to the Working region.
-   **`apply_ffs`**: Phase that commits values from the Working region to the Stable region.

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

-   `Imm(rd, value)`: Immediate value assignment
-   `Binary(rd, rs1, op, rs2)`: Binary operation
-   `Unary(rd, op, rs)`: Unary operation
-   `Load(rd, addr, offset, bits)`: Memory load
-   `Store(addr, offset, bits, rs, triggers)`: Memory store (RMW) with trigger notifications
-   `Commit(src, dst, offset, bits, triggers)`: Cross-region copy with trigger notifications
-   `Concat(rd, [msb..lsb])`: Register concatenation

## Control Flow

-   `Jump(block_id, args)`: Unconditional branch (with block arguments)
-   `Branch { cond, true_block, false_block }`: Conditional branch
-   `Return`: End of execution
-   `Error(code)`: Runtime error
