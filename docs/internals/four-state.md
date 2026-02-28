# 4-State Simulation

Celox supports IEEE 1800-compliant 4-state simulation with X (unknown) propagation.

## Representation Model

4-state values are represented as **value/mask pairs**. For each bit:

| mask | value | Meaning |
|------|-------|---------|
| 0    | 0     | `0`     |
| 0    | 1     | `1`     |
| 1    | 0     | `X`     |
| 1    | 1     | Reserved (eliminated by normalization) |

Signals wider than 64 bits are split into chunks (`i64 x N`) and stored as a pair of value chunk arrays and mask chunk arrays (`TransValue::FourState { values, masks }`).

## Normalization Invariant

**IEEE 1800 normalization: `v &= ~m`**

At bit positions where the mask is 1, the corresponding value bit is always kept at 0. This eliminates the invalid state `(mask=1, value=1)` and guarantees consistency for comparisons and debug output.

### Application Points

Normalization is applied in **all computation paths that produce a `TransValue::FourState`**.

| Location (arith.rs) | Operation | Width |
|-----------------|------|-----|
| Assign (single chunk) | Assignment/type conversion | ≤ 64bit |
| Assign (multi-chunk) | Assignment/type conversion | > 64bit |
| Binary ops (single) | Arithmetic/logic/comparison/shift | ≤ 64bit |
| Binary ops (multi) | Same as above | > 64bit |
| Unary ops (single) | Bitwise inversion/negation/reduction | ≤ 64bit |
| Unary ops (multi) | Same as above | > 64bit |
| Concat (single) | Concatenation | ≤ 64bit |
| Concat (multi) | Concatenation | > 64bit |

### Why Normalization Is Not Needed on Memory Load

The Load operation in `memory.rs` does not perform normalization. Values written to memory are always one of the following, both of which are already normalized:

1. Results from operations in arith.rs (normalized as described above)
2. Input values via the external API (`set_four_state`)

Therefore, the invariant that **values in memory are always normalized** holds, and re-normalization on Load is unnecessary.

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

## Boundary with 2-State Variables

When storing to a `bit`-type (2-state) variable, the mask is forcibly reset to 0 (post-store processing in `memory.rs`). This prevents unintended propagation of X through 2-state variables.

## Test Coverage

4-state related tests are located in `tests/four_state.rs`.
For detailed coverage status and plans for additional tests, see [four_state_test_plan.md](../four_state_test_plan.md).
