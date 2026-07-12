# Veryl analyzer upstream issues

This is a worklist for fixes to send upstream to Veryl. It is not a list of
Celox compatibility rules. Each issue must first be reproduced with an
unmodified `veryl-analyzer`, and the upstream regression should compare the
analyzer/simulator result with the SystemVerilog emitted from the same source.

Verified revisions:

- [latest release `v0.20.2`](https://github.com/veryl-lang/veryl/releases/tag/v0.20.2)
  (`db27d32e812141dbe6530b8c4cd5875a3f03dc5b`)
- [upstream `master`](https://github.com/veryl-lang/veryl/commit/0230fbe76b8b4fd67bc09b2aa0378a6e96e0683f)
  (`0230fbe76b8b4fd67bc09b2aa0378a6e96e0683f`)

The four issues below are present in 0.20.2 and remain in that `master`
revision. The 99 commits after 0.20.2 contain no signed-cast or packed-select
fix that supersedes them.

## Numeric width casts do not preserve source signedness

Minimal reproduction:

```veryl
module Top (
    a: input signed logic<5>,
    y: output logic<16>,
) {
    assign y = a as 8;
}
```

For `a = 5'b11001` (`-7`), the emitted SystemVerilog size cast is equivalent to
`8'(a)`. A SystemVerilog size cast preserves the self-determined expression's
signedness, so the eight-bit result is signed and the assignment must produce
`16'hfff9`. The analyzer instead marks every numeric-width cast unsigned, so
the runtime expression produces `16'h00f9`. Constant evaluation still widens
from the source type's signedness, which makes constant and runtime paths
disagree as well.

Cause:

- [`Expression::gather_context` forces `ctx.signed = false` for every numeric
  `as` target](https://github.com/veryl-lang/veryl/blob/0230fbe76b8b4fd67bc09b2aa0378a6e96e0683f/crates/analyzer/src/ir/expression.rs#L200-L207).
- The existing unsigned-source regression cannot distinguish "preserve the
  source" from "always unsigned" because its input is itself unsigned.

Upstream fix and regression:

- For a numeric width target, copy the source expression's signedness; for a
  type target, use the target type's signedness.
- Test signed and unsigned runtime inputs and the equivalent constants, both
  when widening and when feeding `/`, `%`, comparison, and `>>>`.

## Boolean conversion loses wide and four-state values

Minimal reproductions:

```veryl
module Top (
    wide_result: output logic,
    x_result: output logic,
) {
    const WIDE_ONE: logic<65> = 65'd1;
    const ONE_X: logic<2> = 2'b1x;

    always_comb {
        if WIDE_ONE { wide_result = 1'b1; }
        else          { wide_result = 1'b0; }

        if ONE_X { x_result = 1'b1; }
        else       { x_result = 1'b0; }
    }
}
```

Both conditions are true: one is a 65-bit value containing a known one, and
the other contains a known one plus an unknown bit. The analyzer converts a
condition through `Value::to_usize().unwrap_or(0)`. `Value::to_usize()` returns
`None` for every `BigUint` value and for every value containing X/Z, so both
conditions are folded as false. The same pattern affects procedural `if`,
constant function execution, and generate-if selection.

A related constant ternary is also wrong:

```veryl
const C: logic = 1'bx;
const Y: logic = if C ? 1'b0 : 1'b1;
```

The emitted SystemVerilog yields X by merging the two arms. Analyzer constant
evaluation selects the false arm because it uses the same lossy conversion.

Cause:

- [procedural condition folding uses `to_usize().unwrap_or(0)`](https://github.com/veryl-lang/veryl/blob/0230fbe76b8b4fd67bc09b2aa0378a6e96e0683f/crates/analyzer/src/conv/statement.rs#L417-L421)
- [generate-if conversion uses `to_usize().unwrap_or(0)`](https://github.com/veryl-lang/veryl/blob/0230fbe76b8b4fd67bc09b2aa0378a6e96e0683f/crates/analyzer/src/conv/declaration.rs#L207-L229)
- [constant ternary evaluation uses `to_usize().unwrap_or(0)`](https://github.com/veryl-lang/veryl/blob/0230fbe76b8b4fd67bc09b2aa0378a6e96e0683f/crates/analyzer/src/ir/expression.rs#L459-L473)

Upstream fix and regression:

- Add a value-level boolean conversion returning `False`, `True`, or
  `Unknown`, without converting the payload to a host integer. A known one
  makes the value true; no known one plus any X/Z makes it unknown; otherwise
  it is false.
- Procedural and generate conditions take the true side only for `True`.
  Constant ternaries select an arm for `True`/`False` and merge arm bits for
  `Unknown`.
- Cover U64 and BigUint storage, pure X/Z, known-one-plus-X/Z, and known bits
  above bit 64.

## Packed selects retain the declaration's signedness

Minimal reproduction:

```veryl
module Top (
    a: input signed logic<8>,
    bit_result: output logic<16>,
    part_result: output logic<16>,
) {
    assign bit_result = a[7];
    assign part_result = a[3:0];
}
```

For `a = 8'h8f`, SystemVerilog bit- and part-select results are unsigned, so
the outputs must be `16'h0001` and `16'h000f`. The analyzer updates the
selected shape but leaves `comptime.r#type.signed` copied from the full
declaration, allowing the results to sign-extend.

Constant folding can fail more severely for packed enum elements:

```veryl
module EnumSelect (
    y: output logic<2>,
) {
    enum Color: logic<2> {
        Red = 2'd0,
        Blue = 2'd2,
    }
    const COLORS: Color<2> = 4'b1010;
    assign y = COLORS[0];
}
```

Unmodified 0.20.2 analyzer IR lowers the final assignment to `1'h0`; the
selected enum element should retain its two-bit `Color` type and value
`2'h2`. This is an analyzer result, not a Celox source-rewrite artifact.

Cause:

- [`Factor::Variable::gather_context` applies the select width and then returns
  the unchanged type signedness](https://github.com/veryl-lang/veryl/blob/0230fbe76b8b4fd67bc09b2aa0378a6e96e0683f/crates/analyzer/src/ir/expression.rs#L754-L790).
- Constant folding can replace the selected variable with a value while
  retaining inconsistent type facts, so constants and runtime variables must
  be tested separately.
- The expected unsigned behavior for ordinary signed packed selects was
  already reported in [Veryl issue #94](https://github.com/veryl-lang/veryl/issues/94).

The fix must not blindly clear signedness for every bracket. A packed array of
a named signed element type has a signed result exactly when all dimensions
outside that named type have been consumed. Selecting further inside that
element, selecting only part of the outer aggregate, or taking any range is
unsigned. Regressions therefore need ordinary vectors, part selects, nested
packed dimensions, typedef aliases, named signed elements, enum elements, and
constant/runtime forms.

## Unary bitwise NOT has contradictory signedness

Minimal reproduction:

```veryl
module Top (
    a: input signed logic<8>,
    y: output logic,
) {
    assign y = (~a) <: (1 as i8);
}
```

For `a = 0`, `~a` is signed `8'hff` (`-1`), so the signed comparison is true.
The analyzer's expression-context calculation preserves the operand's
signedness, but its type calculation clears it. Downstream users of the IR can
therefore perform an unsigned comparison and produce false.

Cause:

- [`eval_context_unary` correctly returns the input context for
  `Op::BitNot`](https://github.com/veryl-lang/veryl/blob/0230fbe76b8b4fd67bc09b2aa0378a6e96e0683f/crates/analyzer/src/ir/op.rs#L128-L140).
- [`eval_type_unary` incorrectly sets the result type to
  unsigned](https://github.com/veryl-lang/veryl/blob/0230fbe76b8b4fd67bc09b2aa0378a6e96e0683f/crates/analyzer/src/ir/op.rs#L231-L255).

Remove the signedness reset for `Op::BitNot` and add tests that consume the
result at a signed comparison, division/remainder, arithmetic shift, and wider
assignment boundary. Reduction NOT/NOR and logical NOT remain one-bit
unsigned operations and should stay separate.
