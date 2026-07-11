# Source semantic objects and input accesses

This document is the normative specification for semantic object identity,
source input identity, type normalization, and source-to-occurrence input
mapping in the verifier-first pipeline. It refines the corresponding rules in
[Decision-region and native-allocation architecture](./decision-region-architecture.md)
and is consumed by the private staging boundary described in
[Private source-wire framing and staging](./source-wire-format.md).

It is not a source-wire schema, an arena format, or permission to connect the
new arena to symbolic evaluation. In particular, no production source
producer may construct an `InputSemanticFacts` value until the complete
`ExpectedSourceValueGraph` traversal and its aggregate input/output relation
have been implemented.

The words *must*, *must not*, *required*, and *exactly* below are normative.

## Trust boundary

The semantic-input ownership chain is:

```text
RawTypedSourceHIR
  -> VerifiedTypedSourceHIR
  -> verified canonical semantic-object table
  -> complete ExpectedSourceValueGraph traversal
  -> verified canonical source-input table
  -> private InputSemanticFacts<SourcePhase>
  -> unclassified source-node replay
  -> complete source aggregate verification
```

The object table cannot be derived from SLT nodes. The input table cannot be
derived merely by scanning `Input` nodes: symbolic state, static composites,
dynamic access plans, observer reads, environment reads, and `ForFold` state
all affect the complete expected read relation. Both tables are derived from
verified typed HIR and closed verifier rules, never from producer-supplied
width, signedness, domain, access, or stride summaries.

The first four arrows above may exist as private testable stages. None of them
is a planner-ready artifact, has a public constructor, or can be frozen or
committed independently.

## Distinct identity namespaces

Every SLT phase has two distinct checked namespaces:

```text
PhaseSemanticObjectId<P>: checked dense u32 object identity
PhaseInputId<P>:          checked dense u32 access identity

SourceSemanticObjectId       = PhaseSemanticObjectId<SourcePhase>
DraftOccurrenceSemanticObjectId
                               = PhaseSemanticObjectId<DraftOccurrencePhase>
OccurrenceSemanticObjectId   = PhaseSemanticObjectId<OccurrencePhase>

SourceInputId                = PhaseInputId<SourcePhase>
DraftOccurrenceInputId       = PhaseInputId<DraftOccurrencePhase>
OccurrenceInputId            = PhaseInputId<OccurrencePhase>

ExpectedSourceObjectId: checked dense proof ID in the canonical typed-HIR
object traversal; it is not a producer or storage ID.
```

A semantic object is a declaration, binding, or explicitly derived storage
identity. An input is one exact semantic read of such an object. One object may
therefore have several inputs with different access geometry. Equal width,
domain, signedness, or raw bit ranges never establish object identity.

```text
SemanticObject<P>
  id: PhaseSemanticObjectId<P>
  origin: SourceDeclaration(ExpectedSourceObjectId) |
          SourceBinding(ExpectedSourceObjectId) |
          SourceForFoldBinding(ExpectedSourceObjectId) |
          MappedSource(SourceInstanceId, SourceSemanticObjectId) |
          PortGlue(GlueOriginId) |
          ClosedSynthetic(SyntheticOriginId, derivation rule)
  normalized executable type
  object_width
  declared_signed
  object_domain: Bit | Logic
  dimensions: [SemanticDimension]

SemanticDimension
  kind: Unpacked | Packed | Intrinsic
  extent: nonzero usize
  stride: nonzero usize

InputAccess<P>
  id: PhaseInputId<P>
  object: PhaseSemanticObjectId<P>
  origin: SourceExpected(first ExpectedSourceUseId) |
          MappedSource(SourceInstanceId, SourceInputId) |
          PortGlue(GlueOriginId) |
          ClosedSynthetic(SyntheticOriginId, derivation rule)
  resolution_class: Memory | Environment | StaticComposite |
                    DynamicOverlay(expected dynamic-plan ID)
  member_projection: ordered verified field IDs / checked flat offsets /
                     selected member type, or empty
  normalized access
  ordered runtime index roles
  selected_width
  result_signed
  result_domain: Bit | Logic
```

`SourceInputId` is not a variable ID. `SourceSemanticObjectId` is not an input
row ID. The types must not be aliases even if both currently use a dense
`u32` representation.

`ForFold` state targets and results are object atoms:

```text
PhaseObjectAtom<P>
  object: PhaseSemanticObjectId<P>
  access: nonempty in-bounds BitAccess
```

Their canonical order is `(object, access.lsb, access.msb)`. Adjacent ranges
for the same object must be disjoint. Comparing `PhaseInputId` values cannot
prove this relation because two exact read geometries of one object may have
different input IDs.

## Canonical source IDs

The verified typed-HIR traversal assigns semantic objects in canonical
declaration/binding order. Variable maps, analyzer hash iteration, string
interner order, SLT allocation order, and producer cache order must not affect
the result. Each declaration row verifies that its embedded variable identity
matches the canonical typed-HIR reference that owns it.

Source inputs are assigned during the complete expected-HIR value traversal:

1. Visit declarations by canonical module/source coordinate.
2. Visit statements in language order.
3. Visit expression operands by the closed operator ordinal.
4. Visit derived action, control, projection, observer, dynamic-address, and
   `ForFold` roles by their fixed rule ordinal.
5. At an expected semantic read, derive its complete input key. Retain the
   first `ExpectedSourceUseId` as the source row's canonical origin witness.
6. Reuse an earlier row only when every field of that complete key is equal;
   otherwise append the next dense `SourceInputId`.

The complete key is:

```text
(semantic object,
 resolution class,
 normalized static base and selected dimension provenance,
 ordered member projection and selected member type,
 normalized part-select rule,
 ordered runtime index role types/extents/strides/coercions,
 selected width,
 result signedness,
 result domain)
```

It does not contain an SLT node ID, source occurrence ID, use-site role, or
hash-cache key. Several expected value occurrences with different owners or
roles may use one exact input row; the expected graph still records and
verifies each occurrence, role/site, static proof, and ordered runtime index
child separately. The retained `first ExpectedSourceUseId` fixes allocation
order and is checked to be the first canonical use of that complete key; it is
not part of input identity.

Dense-table representability is checked before allocation. Reservation and
verification failure changes no published object/input length or mapping.

## Verified executable type normalization

Type normalization is an iterative checked relation over verified typed-HIR
type rows. It must not recurse on the host stack or use the legacy
`Shape::total`, `Type::total_width`, struct/union width helpers, or Celox
`resolve_total_width` as a proof oracle.

### Closed data kinds

The accepted executable data kinds are:

- two-state integral `Bit` and fixed integral aliases normalized to `Bit`;
- four-state `Logic`, clock, and reset variants;
- enum, struct, union, and resolved user-defined aliases whose complete
  transitive type graph normalizes to these integral kinds.

Unknown, unresolved, SystemVerilog, interface/module/package, abstract,
modport, type-valued, string, floating-point, and void kinds are rejected at
this boundary. Supporting one later requires a new closed value-domain rule;
it must not reuse `Bit` as a fallback.

Kind normalization derives intrinsic width and Bit-or-Logic domain. It does
not choose object signedness:

- `Bit` has intrinsic width one and domain `Bit`.
- `Logic` and clock/reset variants have intrinsic width one and domain
  `Logic`.
- An enum derives intrinsic width and domain from its verified base integral
  type.
- A struct derives the checked sum of member packed widths. Its domain is
  `Logic` if any member domain is `Logic`, otherwise `Bit`.
- A union must be nonempty and every member must have the same checked packed
  width. Its intrinsic width is that common width. Its domain is `Logic` if
  any member domain is `Logic`, otherwise `Bit`.

An unpacked member inside a packed enum/struct/union kind is rejected until a
separate packed-member layout rule exists. Empty and zero-width packed kinds
are rejected.

For the current `Veryl-0.20` semantics, declared signedness is closed as
follows:

- `signed bit` and `signed logic` use their verified explicit modifier;
- `i8/i16/i32/i64` normalize to signed `Bit`, while unsigned/positive integer
  aliases normalize to unsigned `Bit`;
- an enum inherits its verified base integral type's signedness;
- a struct or union is unsigned because Veryl 0.20 rejects an outer signed
  modifier on a user-defined type; and
- a resolved type alias inherits the target type's signedness. Veryl 0.20
  rejects an outer signed override on that user-defined alias.

Member selection derives the selected member type independently. A mixed
`Logic`/`Bit` struct or union has `Logic` object domain, but an exact access to
a `Bit` member has `Bit` result domain. `InputAccess.result_domain` therefore
need not equal its owning object's aggregate domain; the expected typed-HIR
projection proves the selected type.

`declared_signed` is the signedness established by the selected closed
language-semantics version. It must not be recomputed from member signedness.
For the current `Veryl-0.20` adapter, a `signed` modifier on a user-defined
type is rejected by the analyzer and the resolved analyzer IR overwrites the
outer modifier with the aliased type's signedness. The current verifier must
therefore reject such an input at the typed-HIR boundary; it cannot infer or
pretend to preserve provenance that the adapter did not retain.

A future semantics version may support an outer signed modifier on a resolved
user-defined struct/union, but only when `RawTypedSourceHIR` retains the exact
surface modifier and the analyzer/adapter is changed to validate and preserve
it. Under that future rule, alias resolution applies the verified outer
modifier after deriving recursive kind width/domain. Merely observing
`Type.signed` in Veryl 0.20.1 is not proof of that rule.

### Iterative algorithm

The verifier uses an explicit worklist of `Enter(type)` and `Finish(type)`
frames plus `Unseen`, `Visiting`, and `Done` marks:

1. `Enter` validates the type tag and flat child-ID range. A `Visiting` child
   is a recursive type cycle and fails. Unseen children are pushed before the
   corresponding `Finish` frame.
2. `Finish` reads only completed child facts and computes intrinsic width and
   domain with checked addition/multiplication.
3. Every explicit packed and unpacked extent must be resolved, representable
   as `usize`, and nonzero.
4. The object's dimension vector is, in this exact order:

   ```text
   all unpacked extents in HIR order
   ++ the normalized primitive packed-width extents in HIR order
      (a bare Bit/Logic has one selectable Packed extent of 1)
   ++ [intrinsic kind width] for an enum/struct/union, including width 1
   ```

   Alias resolution first determines whether the final kind is primitive or
   has a distinct intrinsic packed layout, so the same width is not appended
   twice. An explicit outer width on a user-defined packed kind precedes its
   one intrinsic dimension. Extent one is retained because it is still a
   selectable packed dimension and affects index arity and signedness even
   though multiplying by it does not change storage width.

5. Starting with suffix product one, visit dimensions from last to first.
   The current suffix product is that dimension's stride; multiply it by the
   extent with checked arithmetic to obtain the next suffix product.
6. The final suffix product is `object_width`. It must be nonzero and must
   equal the separately checked product of unpacked extents, packed extents,
   and normalized intrinsic width.

No type depth cutoff is permitted. Cycle rejection and an explicit worklist,
not a recursion limit, establish termination.

For example:

```text
logic<8>[4]
  dimensions = [(Unpacked, 4, 8), (Packed, 8, 1)]
  object_width = 32

logic<8, 4>[2, 3]
  extents = [2, 3, 8, 4]
  strides = [96, 32, 4, 1]
  object_width = 192
```

### Named member projection normalization

The verified type graph derives one packed member layout independently from
the analyzer's cached offsets. For a struct with declaration-order members
`f[0..n)`, each checked member width is `W[i]` and:

```text
field_lsb(i) = checked_sum(W[i + 1..n])
field_msb(i) = field_lsb(i) + W[i] - 1
```

Thus the last declared member occupies the least-significant bits, matching
the packed Veryl/SystemVerilog layout. A union requires equal checked member
widths and every member has `field_lsb = 0`. Enum variants are constants, not
member projections. Every field ID, declaration ordinal, width, offset, and
selected type is retained in the expected projection row; a producer offset
is never an oracle.

Access normalization uses a projection cursor:

```text
ProjectionCursor
  base object
  checked flat static base within that object
  current verified selected type
  active dimensions of that selected type
  ordered projected field IDs
```

Before projecting a named member, every unpacked dimension of the current
object and every packed dimension preceding its final `Intrinsic` dimension
must have been selected to one element. The projection does not consume the
`Intrinsic` dimension numerically. Instead it:

1. verifies that the current type is the owning struct/union and that the
   requested field ID is its exact member;
2. adds `field_lsb` to the cursor's flat base with checked arithmetic and
   proves `field_lsb + member_width` remains within the current packed value;
3. replaces the current type's final `Intrinsic` dimension with the selected
   member type's normalized packed dimensions, including an extent-one
   primitive Packed or composite Intrinsic dimension; and
4. derives subsequent indices, nested member projections, result signedness,
   and result domain only from that selected member type.

Nested projections repeat these steps, so their checked offsets compose by
addition relative to the previously selected member. A projection can never
consume an index intended for the aggregate's replaced Intrinsic dimension.
For example, in `s[i].member[j]`, `i` first consumes an outer dimension of
`s`; the member projection then replaces only the scalar struct Intrinsic
dimension; and `j` consumes the selected member's first packed dimension.

If a named member is requested while an outer packed dimension preceding the
struct/union Intrinsic dimension is still unconsumed, its lanes need not form
one contiguous flat range. That form is not encoded as one `InputAccess`.
The closed `StaticComposite` rule expands canonical lane projections in
dimension order and constructs the exact concat/projection relation in the
expected value graph. Each lane still uses the same checked member layout,
and the aggregate verifier proves complete, nonoverlapping lane coverage and
the final result type. It may not pretend the strided member set is one
contiguous memory input.

## Exact access normalization

Let a verified semantic object have dimensions `D[0..N)`, strides `S[0..N)`,
and `U` leading unpacked dimensions. Access normalization consumes dimensions
from zero upward. Each HIR index is tied to exactly one dimension and retains
whether that dimension is unpacked or packed.

Each index expression is classified by the verified constant evaluator into
exactly one of three forms:

- `KnownTwoStateConstant` is converted only after its arbitrary-width value is proved
  representable and in `0..extent`. Its checked `index * stride` contribution
  is added to `static_base`; it creates no runtime index child.
- `ConstantUnknownXZ` retains the exact constant width/payload/mask proof but
  creates no runtime child and is never converted to `usize`. It makes
  `address_known` identically false. The read result is therefore zero for a
  selected `Bit` type and all-X for a selected `Logic` type; a write is a
  no-op. Bounds and offset arithmetic are not evaluated on that false path.
- `RuntimeValue` creates exactly one ordered index role and later exactly one
  phase-node child. The role records the expected HIR operand, source width,
  source signedness/domain, normalization coercion, extent, and stride.

The legacy `eval_constexpr` helper is not an oracle here because it discards
the X/Z mask. A constant is `KnownTwoStateConstant` only when its verified
mask is zero. These classifications and their constant proof rows remain in
the expected input specification even though the compact phase fact contains
no child for either constant form.

Runtime index arithmetic and bounds comparison remain in their original
normalized arbitrary-width domain. Conversion to `usize` or a machine pointer
occurs only on the verified in-bounds path.

Every ordinary index, static or runtime, has the exact bound
`normalized_index < extent`. A runtime index contributes
`normalized_index * stride` to the guarded offset. Static and runtime index
roles are visited in HIR order; canonicalization cannot reorder equal-typed
indices.

The normalized input access is:

```text
NormalizedInputAccess
  static_base
  consumed_dimensions
  part: None | NormalizedPartSelect
  runtime_indices: [NormalizedIndexRole]
  selected_width
  packed_selection_occurred

NormalizedPartSelect
  Colon { low, elements, dimension, stride } |
  PlusColon { anchor role, elements, dimension, stride } |
  MinusColon { anchor role, elements, dimension, stride } |
  Step { anchor role, elements, dimension, stride }
```

`anchor role` is a `KnownTwoStateConstant` folded into `static_base`, a
`ConstantUnknownXZ` proof which makes the access guard false, or one ordered
`RuntimeValue` role. The three forms have one result-type rule.

A part-select applies to exactly the next unconsumed packed dimension, is the
last selector in that input access, and sets `packed_selection_occurred`.
Applying these bit-select forms to an unpacked dimension is not an Input part
select: a language-level unpacked range, if supported by a future complete HIR
schema, requires its own closed static-composite rule. A part-select anchor
does not consume a second dimension.

### No part-select

With no consumed dimension, `selected_width = object_width`. After consuming
`m > 0` dimensions, `selected_width = S[m - 1]`. Every contribution and the
final `static_base + selected_width` check uses checked arithmetic.

### Colon

`[high:low]` requires both bounds to be `KnownTwoStateConstant`; a constant
X/Z bound is rejected because it cannot define a fixed result width. Then:

```text
low <= high < extent
elements = high - low + 1
static_base += low * stride
selected_width = elements * stride
```

All subtraction, addition, and multiplication is checked.

### Plus-colon

`[anchor +: elements]` requires static nonzero `elements <= extent` and uses:

```text
low = anchor
bounds = anchor + elements <= extent
offset contribution = anchor * stride
selected_width = elements * stride
```

### Minus-colon

`[anchor -: elements]` requires static nonzero `elements <= extent` and uses:

```text
low = anchor - (elements - 1)
bounds = anchor < extent && anchor + 1 >= elements
offset contribution = low * stride
selected_width = elements * stride
```

The low value is formed only after the corresponding static proof or runtime
guard; unchecked unsigned underflow is never an address.

### Step

`[anchor step elements]` is normalized as
`[(anchor * elements) +: elements]`:

```text
low = anchor * elements
bounds = low + elements <= extent
offset contribution = low * stride
selected_width = elements * stride
```

The multiplication is part of the arbitrary-width checked address plan.

### Static versus dynamic access

An access is statically addressable exactly when every index/anchor is
`KnownTwoStateConstant`. Its checked result is one exact flat bit range:

```text
[static_base, static_base + selected_width - 1]
```

An access containing `RuntimeValue` retains `static_base`, the ordered runtime
roles, extents, strides, part rule, and selected width in one semantic input
row and its dynamic-address plan. An access containing
`ConstantUnknownXZ` retains its proof and an identically-false
`address_known`, whether or not it also contains runtime roles. An `Input`
node names that row and the runtime index children only. It directly has the
row's selected result width, signedness, and domain.

In particular, a dynamic unpacked-array element read is not represented as a
signed whole-object input followed by a generic unsigned `Slice`. The phase
input itself returns the selected element type. Any backend load narrowing is
an implementation of this verified input relation, not an IR signedness rule.

## Result signedness and domain

Signedness is derived from dimension provenance, never by comparing one flat
range with `object_width`:

- A whole-object read preserves `object.declared_signed`.
- A read selected only through unpacked dimensions preserves
  `object.declared_signed`, including a dynamic unpacked-array read.
- A named struct/union member projection uses the independently verified
  selected member type's signedness/domain; the aggregate object's domain is
  not substituted for it.
- Applying a packed bit/part select after either the object or a member makes
  the result unsigned.
- Consuming a numeric packed dimension by bit selection makes the result
  unsigned.
- A packed select that happens to cover the complete packed width is still
  unsigned.
- A later unpacked or whole-width projection does not restore signedness.

The result domain is independently derived from the exact selected semantic
type and is either `Bit` or `Logic`. It is not derived from an index type. For
node facts:

```text
Input.width     = input.selected_width
Input.signed    = input.result_signed
Input.zero_mask = (input.result_domain == Bit)
```

Thus a `Bit` result remains known two-state when a dynamic `Logic` index or
anchor contains X/Z. Such X/Z makes `address_known` false and selects the
verified zero result; it does not make the Bit value four-state. A `Logic`
result remains potentially four-state even when every index is Bit. Index
children still contribute to lowerability, dependency, address-known, and
bounds facts, but never to the input result's Bit-or-Logic domain.

## Phase input representation and private facts

The canonical new node shape is:

```text
PhaseSLTNode::Input {
  input: PhaseInputId<P>,
  ordered_runtime_index_children: [PhaseNodeId<P>]
}
```

It has no producer-selected object ID, access range, stride, width,
signedness, domain, or zero-mask field. Structural replay checks the compact
fact projection: exact runtime-child count, child existence/append order,
nonzero child widths, retained extent/stride/part geometry, selected result
facts, and node-local coercions. It does not claim that a child is the right
HIR occurrence merely because its type and ordinal fit.

The complete `ExpectedSourceValueGraph` separately retains every static
constant/XZ proof and every runtime role's exact `ExpectedSourceUseId`,
owner/role/site, source type/domain, and normalization coercion. Aggregate
verification compares that expected specification bidirectionally with the
input row and each producer child occurrence. Compact `InputSemanticFacts`
may omit proof-only occurrence IDs and static proofs only because the owning
verified expected graph retains them in the same prepared aggregate. If a
temporary migration representation repeats access or stride fields, replay
must compare them for exact equality and drop them before freeze; in-bounds or
same-length checks alone are insufficient.

The private derived context is:

```text
InputSemanticFacts<P>
  private artifact brand
  objects: [SemanticObjectFact<P>]
  inputs: [InputAccessFact<P>]

SemanticObjectFact<P>
  object
  object_width / declared_signed / object_domain
  canonical dimensions with extent and stride

InputAccessFact<P>
  input / object
  compact normalized access and ordered runtime role geometry
  optional verified selected-member type projection
  selected_width / result_signed / result_domain
```

Only the aggregate semantic verifier can construct it. It has no public row
constructor, standalone verifier, serializer, deserializer, wire form, or
freeze method. A phase arena and its facts carry the same private in-memory
artifact brand; facts from another source artifact or phase fail before node
replay.

The verifier recomputes all fact rows from the retained verified HIR and
expected graph. A producer-supplied facts table, even if internally
self-consistent, is ignored or rejected as a current-schema field.

## Source-to-occurrence relation

### Canonical instances

`SourceInstanceId` is dense in preorder DFS beginning with the top instance.
Children are visited in typed declaration order and instance-array ordinal.
Analyzer maps, instance-name hashes, and relocation order do not allocate IDs.

### Mapped semantic objects

For each canonical instance and each source semantic object owned by its
source artifact, the verifier derives at most the exact mapped object required
by the closed occurrence rules:

```text
OccurrenceSemanticObject
  origin: MappedSource(SourceInstanceId, SourceSemanticObjectId)
  independently rechecked flattened type and object facts
```

Two instances of the same module have distinct occurrence object IDs even
when their source VarIds and types are equal. One mapped origin cannot name two
objects, and one mapped object cannot claim two origins.

### Mapped inputs

A mapped source input substitutes only its object and phase namespaces:

```text
MappedSourceInputKey
  instance
  source input
  mapped occurrence object
  unchanged normalized access/result relation
```

Width, signedness, domain, static base, part rule, runtime role order, extent,
and stride are rederived from the frozen source catalog and canonical instance
type row. They are not copied from a source node or occurrence producer.

### Port glue

Glue rows are derived from the canonical typed `InstDeclaration`, ordered
input connections, ordered output connections, and ordered multi-destination
parts. A glue read carries `PortGlue(GlueOriginId)` but refers to the existing
mapped parent or child semantic object. Equal-shaped ports do not permit an ID
swap. Ordinary port glue must not create a second alias object merely to make
its own type summary self-consistent.

A new `PortGlue` semantic object is permitted only when the closed occurrence
derivation rules independently require an actual composite or synthetic
storage identity and the expected occurrence graph names that exact object.

The verifier independently proves:

- parent/child side and instance ownership;
- source and destination port identity and direction;
- exact source access and destination range;
- selected source type, target type, and the closed port coercion;
- dynamic destination geometry and address-plan ownership;
- multi-destination source slice order and complete width coverage; and
- a bidirectional relation between every glue origin and expected input,
  object, action, root, and node recipe.

Input signedness is the selected source access signedness. Assignment or port
target coercion is a separate explicit value-use rule and does not rewrite the
input row's signedness.

### Draft-to-final mapping

Each isolated unit draft assigns local object/input IDs in its own canonical
expected traversal order. `OccurrenceArtifactTxn` verifies total functions:

```text
DraftOccurrenceSemanticObjectId -> OccurrenceSemanticObjectId
DraftOccurrenceInputId          -> OccurrenceInputId
```

Many draft inputs may map to one final input only when object, normalized
access, result type, origin, and resolution are all exactly equal. Equal shape
or equal raw address is insufficient. Every remapped input node must preserve
ordered runtime child roles. Failure leaves global tables and the draft
unchanged.

Atomization may derive exact root/action projections and ranges, but it cannot
invent an object type, change an input's selected type, or turn packed access
into unpacked access. Observer and synthetic rows obey the same closed origin
and exact-result rules.

## Required adversarial fixtures

These fixtures are required before the source producer can use the new phase
arena. Each malformed case must return a stable structured rule ID and leave
its input owner unchanged.

### Type and object fixtures

- unresolved, zero, and unrepresentable packed/unpacked extents;
- checked overflow in struct member sum, packed product, unpacked product,
  intrinsic-width product, and stride suffix product;
- empty struct/union, unequal union member widths, recursive type cycle, and
  unpacked member in a packed aggregate;
- unknown, floating, string, SystemVerilog, module/interface, and non-data
  kinds masquerading as `Bit` or `Logic`;
- mixed Bit/Logic struct and union domain derivation;
- a Bit member selected from a mixed-domain object has Bit result domain,
  while a Logic member has Logic result domain;
- nested struct field offsets compose according to declaration-order packed
  layout, while every union field offset remains zero;
- enum base width/domain derivation;
- bare one-bit Bit/Logic retains one selectable Packed extent of one;
- a width-one enum/struct/union retains one Intrinsic extent of one, while an
  alias does not duplicate its resolved target's dimension;
- a signed modifier on a user-defined struct/union is rejected by the current
  Veryl-0.20 typed-HIR adapter instead of being silently lost;
- a future-version fixture, enabled only with retained raw modifier
  provenance and an updated analyzer/adapter, proves the outer modifier is
  applied after recursive kind normalization;
- a variable table key that disagrees with the declaration's embedded ID;
- equal-shaped different declarations substituted for one another; and
- randomized analyzer-map insertion order producing byte-for-byte identical
  canonical object and input tables.

### Dimension and access fixtures

- `logic<8>[4]`: unpacked read stride 8 and packed select stride 1;
- `logic<8,4>[2,3]`: extents `[2,3,8,4]`, strides `[96,32,4,1]`;
- same object with `mem[i]`, `mem[i][j]`, and a dynamic part-select producing
  distinct complete input keys but one object ID;
- out-of-order, missing, duplicate, zero, or wrong runtime index stride/extent;
- static indices folded into the wrong base or retained as unexpected runtime
  children;
- runtime index child swapped with an equal-typed child from another HIR use;
- constant X/Z index/anchor has no runtime child, retains its exact mask proof,
  and makes `address_known` identically false;
- an X/Z `:` bound is rejected rather than treated as a known width;
- too many indices, selecting beyond the dimension vector, and a zero selected
  width;
- static access outside object bounds and checked base/width overflow;
- colon with dynamic bound, reversed/out-of-range bounds, and width overflow;
- plus-colon zero/oversized width and anchor-add overflow;
- minus-colon underflow and `anchor + 1` overflow;
- step anchor multiplication and end-add overflow;
- a packed range covering the whole packed width but incorrectly marked
  signed;
- a dynamic unpacked element incorrectly represented as whole-object Input
  plus unsigned generic Slice;
- `s[i].member[j]` consumes the outer dimension, replaces the struct
  Intrinsic dimension, then consumes the member dimension; and
- an unconsumed outer packed array's `s.member` is expanded by the exact
  `StaticComposite` lane rule rather than forged as one contiguous range.

### Domain and signedness fixtures

- signed whole-object and static/dynamic unpacked-only reads remain signed;
- named member projection uses the selected member's signedness/domain rather
  than the aggregate object's, before any further packed select;
- any static/dynamic packed bit/part select is unsigned;
- equal flat ranges reached once through unpacked provenance and once through
  packed provenance have their independently derived signedness;
- Bit result with Bit index has zero-mask true;
- Bit result with X/Z-bearing Logic index or anchor also has zero-mask true;
- Logic result with Bit index has zero-mask false;
- Logic result with X/Z-bearing Logic index has zero-mask false; and
- dynamic unknown-address execution returns zero for Bit and all-X for Logic
  without forming an out-of-object pointer.

### Object/input separation and `ForFold` fixtures

- several input IDs for one object are accepted when their exact access keys
  differ;
- two distinct input IDs targeting overlapping ranges of one `ForFold` object
  are rejected by object/range identity;
- equal ranges on distinct objects do not falsely overlap;
- ForFold rows ordered by input ID but not by object/range are rejected;
- missing, extra, duplicated, or orphan object/input rows are rejected
  bidirectionally; and
- a non-first `ExpectedSourceUseId`, wrong resolution class, wrong mapped
  origin, or producer-only invented role cannot allocate an otherwise equal
  input row; and
- facts branded for another phase or source artifact are rejected.

### Flattening and glue fixtures

- sibling instance maps inserted in different hash order produce identical
  instance/object/input IDs;
- two instances of one source module cannot alias equal source objects;
- swapped source instance/module/object/input references;
- parent/child glue side swap and equal-shaped port-ID swap;
- input/output direction, width, domain, signedness, or coercion mismatch;
- dynamic output destination with altered dimension, stride, part rule, or
  selected width;
- multi-destination output with reversed or incomplete source coverage;
- PortGlue origin masquerading as MappedSource or Synthetic;
- ordinary glue inventing a duplicate alias object; and
- missing, wrong, or merely equal-shaped draft-to-final mappings, with exact
  many-local-to-one mapping accepted.

### Failure and scale fixtures

- dense object/input ID exhaustion without allocating an impossible table;
- deterministic failure at every object/input/fact reservation site;
- no semantic length or mapping change after any failed derivation/replay;
- iterative deeply nested type and access graphs without host recursion; and
- 100k/1M input-access derivation/replay measurements including large
  multidimensional and part-select tables.

Existing positive regressions for packed-bit stride, hierarchical dynamic
access, and dynamic indexed part-select sensitivity should remain, but they do
not replace malformed aggregate fixtures.

## Producer-connection boundary

The semantic object relation can be implemented and tested before the complete
source producer. The input relation cannot be declared complete merely because
all currently emitted `Input` nodes found a matching row. That would let the
producer omit the same semantic read from both its node arena and its own
summary.

Before producer connection, the implementation must have:

1. a verified canonical typed-HIR snapshot, including every syntax-level type
   modifier required by its selected semantics version (the current adapter
   rejects any modifier provenance it cannot retain);
2. the full iterative `ExpectedSourceValueGraph` traversal for every accepted
   declaration, statement, expression, observer, dynamic-address, environment,
   static-composite, and `ForFold` variant;
3. canonical source-object and source-input rows derived only from those two
   inputs;
4. a bidirectional match from every expected read/index role to producer nodes
   and from every producer Input node back to an expected recipe;
5. complete expected-node reachability and ordinary/gated classification; and
6. consuming aggregate prepare/commit ownership with no standalone facts or
   arena publication.

Until all six hold, `InputSemanticFacts<SourcePhase>` and source-node replay
remain private verifier/test stages. Passing structural node tests, legacy
lowering tests, or synthetic scale measurements is not evidence that the
source semantic-input relation is complete.

## Language references

The normalization above follows Veryl's distinction between packed width
(`<>`) and unpacked array dimensions (`[]`), its packed user-defined
struct/union types, and its exact bit-select forms. The formal grammar admits
type modifiers before user-defined types, but the pinned Veryl 0.20.1 analyzer
currently rejects `signed` there and discards the outer modifier during type
resolution. The versioned rule above makes that mismatch explicit instead of
using the resolved `Type.signed` field as invented provenance.

- [Veryl formal syntax](https://doc.veryl-lang.org/book/07_appendix/01_formal_syntax.html)
- [Builtin types](https://doc.veryl-lang.org/book/05_language_reference/03_data_type/01_builtin_type.html)
- [User-defined types](https://doc.veryl-lang.org/book/05_language_reference/03_data_type/02_user_defined_type.html)
- [Arrays](https://doc.veryl-lang.org/book/05_language_reference/03_data_type/03_array.html)
- [Bit select](https://doc.veryl-lang.org/book/05_language_reference/04_expression/06_bit_select.html)
