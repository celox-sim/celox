# 4-State Simulation

Celox supports IEEE 1800-compliant 4-state simulation with X (unknown) propagation.

## Representation Model

4-state values are represented as **value/mask pairs**. For each bit:

| mask | value | Meaning |
|------|-------|---------|
| 0    | 0     | `0`     |
| 0    | 1     | `1`     |
| 1    | 0     | `Z` (high-impedance) |
| 1    | 1     | `X` (unknown) |

Signals wider than 64 bits are split into chunks (`i64 x N`) and stored as a pair of value chunk arrays and mask chunk arrays (`TransValue::FourState { values, masks }`).

## Z vs X Encoding

Both `(v=0, m=1)` (Z) and `(v=1, m=1)` (X) indicate uncertain bits. The distinction is preserved throughout the pipeline — no normalization is applied. Z literals like `8'hzz` retain their Z encoding `(v=0, m=1)` through operations, and X literals retain `(v=1, m=1)`.

The VCD writer distinguishes Z and X per-bit using the `(mask, value)` pair for correct waveform output.

## X Propagation Rules

X (mask) propagation in each operation follows IEEE 1800 semantics.

### Bitwise Operations

| Operation | Mask computation | Notes |
|------|----------|------|
| `a & b` | `(ma \| mb) & ~(~va & ~ma) & ~(~vb & ~mb)` | A known 0 cancels X |
| `a \| b` | `(ma \| mb) & ~(va & ~ma) & ~(vb & ~mb)` | A known 1 cancels X |
| `a ^ b` | `ma \| mb` | X if either operand is X |

### Shift Operations

| Condition | Mask computation |
|------|----------|
| Shift amount is known | Shift the mask by the shift amount |
| Shift amount contains X | Entire result becomes all-X |

### Arithmetic and Comparison Operations

| Operation | Mask computation | Notes |
|------|----------|------|
| `+`, `-`, `*`, `/`, `%` | If either operand contains X, the entire result becomes all-X | Conservative propagation |
| `==`, `!=`, `<`, `>` etc. | Same as above (1-bit result) | |

### Mux (Ternary Operator)

| Condition | Behavior |
|------|------|
| Selector is known | Use value/mask of the selected branch |
| Selector contains X | Conservative mask: OR of both branches' masks |

### Reduction Operations

| Operation | Mask computation |
|------|----------|
| `&` (reduction AND) | If a known 0 exists, the result is a known 0; otherwise X |
| `\|` (reduction OR) | If a known 1 exists, the result is a known 1; otherwise X |
| `^` (reduction XOR) | If any bit is X, the result is X |

## SIR-Level Z Preservation

Two SIR instructions have dedicated 4-state handling to preserve Z (high-impedance) bits through the pipeline without converting them to X:

-   **`Mux(dst, cond, then_val, else_val)`**: Selects the value/mask pair of the chosen branch. When `cond` has X/Z bits, the result is all-X. Unlike the previous AND/OR lowering pattern, this preserves exact Z bits of the selected branch.
-   **`Concat(dst, [msb..lsb])`**: Pure data movement concatenation. Preserves the value/mask of each part without modification, keeping Z bits intact.

These instructions replaced the earlier Shl+Or (for Concat) and Branch-based (for Mux) lowering patterns, which could convert Z to X through bitwise operations.

## Boundary with 2-State Variables

When storing to a `bit`-type (2-state) variable, the mask is forcibly reset to 0 (post-store processing in `memory.rs`). This prevents unintended propagation of X through 2-state variables.

## Test Coverage

4-state related tests are located in `tests/four_state.rs`.
For detailed coverage status and plans for additional tests, see [four_state_test_plan.md](../four_state_test_plan.md).
