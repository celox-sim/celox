# Veryl analyzer and simulator upstream issues

This is a worklist for fixes to send upstream to Veryl. It is not a list of
Celox compatibility rules. Each issue must first be reproduced with the
unmodified analyzer and simulator, then compared with the SystemVerilog emitted
from the same source.

Verified revisions:

- [latest release `v0.20.2`](https://github.com/veryl-lang/veryl/releases/tag/v0.20.2)
  (`db27d32e812141dbe6530b8c4cd5875a3f03dc5b`)
- [upstream `master`](https://github.com/veryl-lang/veryl/commit/0230fbe76b8b4fd67bc09b2aa0378a6e96e0683f)
  (`0230fbe76b8b4fd67bc09b2aa0378a6e96e0683f`)

The four issue groups below are present in 0.20.2 and remain in that `master`
revision. The 99 commits after 0.20.2 contain no fix that supersedes them.

## AIR facts that are not analyzer bugs

AIR does not encode signedness solely in an expression's result type. It also
keeps the operand evaluation context and represents a variable selection as a
base variable plus `VarSelect`. Consumers must apply the operator/select rules:

- A comparison has an unsigned one-bit result, but signed Div/Rem/relational
  evaluation is determined from the two operand contexts. The official
  simulator correctly recomputes it from
  `lhs.expr_context.signed && rhs.expr_context.signed`.
- An ordinary packed bit/part select is unsigned. AIR retains the base variable
  and `VarSelect`; a consumer must not use the base declaration's signedness as
  the selected expression's signedness.
- Unary `~` uses its operand evaluation context. Its result must not be inferred
  from unrelated parent/result metadata.

Celox treating any one of those metadata fields as universal was a Celox AIR
consumer bug. These cases must not be filed as analyzer bugs.

## Numeric width cast signedness differs from emitted SystemVerilog

Minimal reproduction:

```veryl
module Top (
    a: input signed logic<5>,
    y: output logic<16>,
) {
    assign y = a as 8;
}
```

For `a = 5'b11001` (`-7`), the emitter produces the SystemVerilog equivalent of
`8'(a)`. A SystemVerilog size cast preserves the self-determined expression's
signedness, so that code produces `16'hfff9`. The three existing paths disagree:

- the official simulator's runtime path treats the integer cast as transparent
  and produces `16'h0019`, without first resizing five bits to eight;
- analyzer constant evaluation resizes to `8'hf9` but marks the cast result
  unsigned, producing `16'h00f9` in the outer assignment;
- emitted SystemVerilog produces `16'hfff9`.

Upstream's current regression explicitly expects unsigned behavior for an
unsigned input, which cannot distinguish source-signedness preservation from
always-unsigned behavior. This requires one shared rule across the analyzer,
simulator, and emitter.

Cause:

- [`Expression::gather_context` forces `ctx.signed = false` for every numeric
  `as` target](https://github.com/veryl-lang/veryl/blob/0230fbe76b8b4fd67bc09b2aa0378a6e96e0683f/crates/analyzer/src/ir/expression.rs#L200-L207).
- The simulator drops most integer cast nodes as transparent, so runtime and
  constant evaluation do not even agree on resizing.

Upstream fix and regression:

- Implement the SystemVerilog size-cast rule used by the emitter: resize using
  the source expression's self-determined signedness and preserve that
  signedness on the result.
- Test signed and unsigned runtime inputs and the equivalent constants, both
  when widening and when feeding `/`, `%`, comparison, and `>>>`.

## Runtime packed select keeps the base signedness in downstream operations

Minimal reproduction:

```veryl
module Top (
    x: input signed logic<8>,
    y: output logic<8>,
) {
    assign y = x[7:0] >>> 1;
}
```

For `x = 8'hfe`, a packed part-select is unsigned even when it spans the whole
vector. The emitted SystemVerilog therefore produces `8'h7f`. All four official
simulator backends in 0.20.2 instead produce `8'hff`.

AIR correctly retains `VarSelect`, and evaluating the select as a `Value`
clears its signed flag. The simulator's ProtoExpression conversion separately
copies the base factor's signed expression context; the later arithmetic-shift
selection reads that stale context and chooses a signed shift.

The fix must derive the post-select context from the selected expression, not
blindly copy the base context. Regressions need bit selects, partial and
full-width part-selects, dynamic selects, and the boundary where selecting one
element of a packed array yields a named signed element type.

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

## Constant folding loses operation-specific type information

Constant folding replaces an expression with `Expression::create_value`, then
copies the old result type but not its expression context. Consumers can then
see three contradictory facts: the folded `Value` signedness, the copied type
signedness, and a default unsigned expression context. A folded ordinary packed
select is one example; an integer type cast is another. No downstream consumer
can recover the erased operator from those fields alone.

Packed enum selection also loses the selected value and width:

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

Constant selection must preserve both the selected element width and the
selected named type. A packed array of a named signed element type is signed
exactly when all dimensions outside that named type have been consumed.
Selecting further inside that element, selecting only part of the outer
aggregate, or taking a range is unsigned. Regressions therefore need enum
elements, typedef aliases, nested named signed elements, ordinary bit/part
selects, and constant/runtime forms. The ordinary unsigned-select rule is
already discussed in [Veryl issue #94](https://github.com/veryl-lang/veryl/issues/94).
