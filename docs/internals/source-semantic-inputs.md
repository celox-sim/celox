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
`ExpectedTypedConstantExpr`, source `TriIntent`, and
`ExpectedSourceValueGraph` relations and their aggregate input/output checks
have been implemented. A source artifact is not executable: hierarchy mapping
and port glue must later produce the complete occurrence `TriNet` relation
before an occurrence artifact can freeze.

The words *must*, *must not*, *required*, and *exactly* below are normative.

## Trust boundary

The semantic-input ownership chain is:

```text
RawTypedSourceHIR
  -> joint VerifiedTypedSourceHIR + ExpectedTypedConstantExpr relation
  -> verified canonical semantic-object table + source TriIntent relation
  -> complete ExpectedSourceValueGraph traversal
  -> verified canonical source-input table
  -> private InputSemanticFacts<SourcePhase>
  -> unclassified source-node replay
  -> complete source aggregate verification
  -> FrozenSourceArtifact
  -> hierarchy mapping, interface flattening, and port glue
  -> occurrence semantic-object table + complete occurrence TriNet relation
  -> complete ExpectedOccurrenceGraph and occurrence aggregate verification
```

The first arrow is itself an aggregate relation, not a call which may trust a
resolved analyzer type. It jointly contains the non-constant type
prerequisites, complete `ExpectedTypedConstantExpr` proofs for every extent
and explicit enum recipe, and final type/enum replay. None of those pieces may
be published as a separately trusted substitute for `VerifiedTypedSourceHIR`.
`TriIntent` proves the exact surface modifier, eligible declaration role, and
source read/drive provenance only. It is not a driver-resolution proof. The
occurrence `TriNet` consumes the immutable source catalog plus the complete
instance/interface/glue relation; it cannot participate in deriving a value
domain or repair an invalid source type.

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

ExpectedTypedConstantExprId: checked dense proof ID for one exact
(RawConstExprOccurrenceId, VerifiedGenericEnvironmentId, root role and typing
context) evaluation; it is not the raw occurrence index.

VerifiedSourceTriIntentId: checked dense proof ID for one exact eligible
source object and its retained surface Tri provenance.

OccurrenceTriNetId: checked dense electrical-net component ID in the complete
flattened driver-resolution relation.
OccurrenceTriResolutionMapId: checked dense proof ID for one occurrence
semantic object's complete disjoint lane-to-TriNet mapping. Neither ID is
inferred from the object's value domain or crosses a phase/artifact boundary.

VerifiedBitsId<P>: checked dense canonical bit-plane ID owned by
VerifiedBitsArena<P> in the same phase aggregate.
VerifiedTypedValueId<P>: checked dense typed-value ID owned by
VerifiedTypedValueArena<P> in the same phase aggregate.
VerifiedSourceTypedValueId = VerifiedTypedValueId<SourcePhase>
VerifiedOccurrenceTypedValueId = VerifiedTypedValueId<OccurrencePhase>
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
  declared_positive_type
  object_domain: Bit | Logic
  object_resolution: PhaseObjectResolution<P>
  default_role: None | ExplicitClock | ImplicitClock |
                ExplicitReset | ImplicitReset
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
  result_positive_type
  result_domain: Bit | Logic
```

The closed phase projections are:

```text
PhaseObjectResolution<SourcePhase> =
  Ordinary | TriIntent(VerifiedSourceTriIntentId)
PhaseObjectResolution<DraftOccurrencePhase> =
  Ordinary | MappedTriIntent(SourceInstanceId, VerifiedSourceTriIntentId)
PhaseObjectResolution<OccurrencePhase> =
  Ordinary | TriNetMap(OccurrenceTriResolutionMapId)
```

`SourceInputId` is not a variable ID. `SourceSemanticObjectId` is not an input
row ID. The types must not be aliases even if both currently use a dense
`u32` representation.

`object_domain` describes two-state versus four-state values; it does not
describe driver resolution. In particular, ordinary `Logic` remains
`Ordinary`. A source Tri object carries only its exact intent proof; only a
fully flattened occurrence object carries an exact TriNet proof ID.
`default_role` is a back-reference derived from the independently verified
module-level clock/reset selection, including whether selection was explicit
or implicit; it is never reconstructed from the type or analyzer witness.
The positive fields preserve the `p*` declaration class only for later
type/value-use checking; they are never a runtime nonzero fact.

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

Type normalization is a versioned, iterative checked relation over a retained
typed-HIR aggregate. It must not recurse on the host stack or use
`Shape::total`, `Type::total_width`, analyzer struct/union/enum width caches,
analyzer enum member caches, or Celox `resolve_total_width` as a proof oracle.

### Retained raw relation

The raw boundary separates reusable type declarations, exact syntax type
uses, modifiers, object ownership, and module context. Combining these into
one resolved analyzer `Type` loses facts which the verifier needs.

```text
RawTypeRow
  canonical surface declaration/template identity and source coordinate
  canonical generic-formal range and owning declaration scope
  kind: Bit | Logic | BBool | LBool |
        U8/U16/U32/U64 | P8/P16/P32/P64 | I8/I16/I32/I64 |
        Clock/ClockPosedge/ClockNegedge |
        Reset/ResetAsyncHigh/ResetAsyncLow/ResetSyncHigh/ResetSyncLow |
        Alias(target RawTypeUseId) |
        Enum(base rule, encoding, canonical variant range) |
        Struct(canonical member range) | Union(canonical member range) |
        closed unsupported tag

RawTypeUseRow
  exact surface syntax occurrence and owner role
  core: Concrete(RawTypeId) | GenericFormal(RawGenericFormalId)
  generic use: RawGenericUseId
  modifiers: canonical RawModifierRange
  unpacked extents: canonical RawExtentRange
  packed extents: canonical RawExtentRange

RawExtentRow
  exact resolved-expression occurrence
  expression: RawConstExprOccurrenceId

RawExtentResolutionWitnessRow
  exact RawExtentId / RawGenericEnvironmentWitnessId
  analyzer_resolution: Unresolved(exact resolution witness) |
                       Resolved(RawArbitraryBitsId, exact resolution witness)

RawEnumResolutionWitnessRow
  exact enum RawTypeId / RawGenericEnvironmentWitnessId
  analyzer width witness / canonical variant-value witness range

RawEnumVariantValueWitnessRow
  owner enum-resolution witness / exact RawEnumVariantId and ordinal
  analyzer value: Unresolved | Known(payload/mask RawArbitraryBitsIds,
                                     width/sign/domain witnesses)

RawGenericFormalRow
  exact owner declaration / source coordinate / declaration ordinal
  bound: Type |
         Inst(exact retained proto-interface path/bound) |
         Proto(exact retained fixed/named proto bound)
  default: None | Some(RawGenericArgumentId)

RawGenericUseRow
  exact surface owner: TypeUse(RawTypeUseId) |
    FunctionCall | ModuleInstance | InterfaceInstance | PackageUse |
    closed other language use
  exact source coordinate / parent lexical scope
  canonical RawGenericArgumentRange; a nongeneric use owns the canonical
    empty range

RawGenericArgumentRow
  exact owner: RawGenericUseId at source ordinal |
               declared default of one RawGenericFormalId
  syntax: Identifier(exact retained symbol-resolution occurrence) |
          FixedType(RawTypeUseId) |
          Const(RawConstExprOccurrenceId)

RawGenericEnvironmentWitnessRow
  analyzer specialization identity / exact surface generic use
  optional parent witness / canonical selected-binding witness range /
  canonical extent-resolution and enum-resolution witness ranges
  comparison witness only; it is never used to choose a verified environment

RawGenericBindingWitnessRow
  owner witness / exact formal ID / declaration ordinal
  selection: Explicit(RawGenericArgumentId) | DeclaredDefault
  analyzer resolved argument identity/value witness

RawConstExprOccurrenceId
  raw storage reference to one exact retained source expression/root role;
  it is not an ExpectedTypedConstantExprId

RawArbitraryBitsRow
  canonical range in borrowed RawArbitraryBytePool
  independently retained source/value role

RawModifierRow
  exact syntax token / occurrence ordinal
  kind: Signed | Tri | Default

RawObjectRow
  exact declaration/binding/ForFold reference and embedded variable witness
  owner source coordinate
  type_use: RawTypeUseId
  module_context: RawObjectTypeContextId

RawObjectTypeContextRow
  exact object / module identity
  owner scope: ModuleTop | Interface | Local | closed other scope
  declaration role: Port(direction) | Variable | Let | InterfaceMember |
                    Binding | ForFoldBinding | closed other role
  exact concrete-inout classification

RawModuleTypeContext
  exact module identity
  canonical port/declaration/binding traversal
  exact clock/reset candidates and their object/type-use identities
  exact analyzer default-clock/default-reset object witnesses, if any
  exact port/storage role required by source TriIntent and default-clock/reset
  rules
```

`RawTypedSourceHIR<'a>` borrows the arbitrary-bit pool; the verifier boundary
does not first decode an owned `BigUint`/`BigInt`. A `RawArbitraryBitsRow` is
one canonical unsigned magnitude encoded as little-endian bytes: zero has an
empty range, a nonzero value has a nonzero final byte, and no nonempty range
may be shared, overlap another row, escape the pool, or leave orphan bytes.
Distinct zero rows remain distinct owners at the same empty cursor. Exact
typed width, numeric sign, payload role, and X/Z-mask role are retained by the
owning extent or typed-constant-expression row rather than encoded by leading
zero bytes. Malformed encodings fail before arithmetic.

The `RawArbitraryBitsId` on an environment-qualified extent witness is only
the analyzer-resolution witness. The value proof is assigned a fresh
`ExpectedTypedConstantExprId` only after the exact raw occurrence/environment/
role/context passes the joint verifier, and the two
must agree bit-for-bit after the closed coercion. The same raw/proof namespace
split applies to enum recipes and generic constant actuals. Type normalization
and typed constant verification therefore form one joint dependency graph. `$bits`,
`$size`, casts, `type(expr)`, `msb`, and `lsb` contribute closed type-only
edges; constants, parameters, generics, enum values, and admitted constant
function calls contribute value edges. The verifier derives these edges from
retained source AST/symbol rows. Analyzer `Comptime`, `Shape`/`WidthExpr`, enum
caches, or an already resolved extent cannot replace either edge class.

All pools are dense, logically fully owned, gap-free, and canonical. The raw
view may borrow the one aggregate-owned byte buffer, but each range still has
one exclusive logical owner. Every type use has exactly one surface syntax
owner (object, alias target, member, enum base, or other closed typed-HIR role)
and owns its modifier/extent ranges once. A specialization never duplicates
that raw template row; only its verified `(use, environment)` instance is
distinct.
struct/union rows own member ranges; enum rows own variant ranges; objects and
named types own formal ranges; every `RawGenericUseRow` owns one argument
range; environment
witnesses own binding, extent-resolution, and enum-resolution witness ranges;
each enum-resolution witness owns its variant-value witness range; objects and named types have exactly one
matching root row. A root's coordinate must equal
its independently retained owner coordinate. Raw table indices are storage
references, not verified IDs.
Verified IDs are assigned on first discovery from canonical roots and fixed
child order. Referential row tables such as type cores may therefore be
permuted with every reference relocated without changing verified output.
Syntax-ordered owned pools are different: their physical order is part of the
canonical raw relation and a permutation is rejected rather than silently
normalized.

Pool canonicality is stronger than nonoverlap. Walking type uses in canonical
syntax-owner traversal order, not raw storage/index order, each unpacked range
must begin at the current extent cursor, its end becomes the cursor, and the
following packed range must begin at that cursor; empty ranges must also name
the current cursor. The final cursor must equal the extent-pool length.
Modifier ranges obey a separate cursor over that same owner traversal.
Struct/union member ranges and enum variant ranges obey the analogous
canonical owner-traversal cursor rule, as do generic formal, generic-use
argument, binding-witness, and extent-resolution-witness ranges in their
separate pools, together with enum-resolution and variant-value witness ranges.
Formal-default
argument rows occur at their formal's fixed traversal position between those
ranges. Owner bitmaps then independently reject
duplicate ownership and orphans. A set of disjoint ranges with a gap or
noncanonical block order is invalid.

Arbitrary-bit rows and their borrowed byte ranges obey the same canonical
owner-traversal cursor rule. Extents and the retained raw constant-expression
rows own exact, role-tagged references; two
equal byte strings do not authorize one row to impersonate or alias another
proof input.

Generic specialization is replayed rather than accepted as an analyzer-chosen
resolved type. Formal bound/kind/order, explicit-versus-default selection,
actual owner scope, parent environment, and every type/instance/proto/constant
actual are checked from retained source rows. `Type`, `Inst`, and `Proto` have
distinct verified argument variants and compatibility relations; an instance
identifier cannot masquerade as an equal-shaped type, and an unsupported bound
is rejected rather than coerced into `Type`. Number and boolean arguments are
raw constant-expression occurrences and receive their own typed proofs.

A verified type-use identity is the pair of its surface `RawTypeUseId` and a
verifier-derived `VerifiedGenericEnvironmentId`; equal coordinates or equal
resolved member widths cannot merge specializations. The verifier interprets
the declaration-template alias/member/enum child uses under that environment,
so the template never contains analyzer-substituted child IDs. Canonical
environments are allocated in root traversal and formal declaration order.
Repeated exact specializations may share a completed verified environment only
after their complete canonical actual keys compare equal by content. The
analyzer environment/binding witness tables are then compared bidirectionally
to this independently derived set; missing, extra, or differently parented
witnesses fail. Each root scope has one canonical empty environment/witness for
nongeneric uses, so their extent-resolution rows follow the same relation.

The joint static dependency graph has closed node kinds `TypeUseInstance`,
`GenericEnvironment`, `ConstBinding`, `ConstExprRoot`, and `EnumReplay`, with
every type-only and value edge independently derived from source ownership.
An iterative SCC pass rejects every self-edge or nontrivial SCC in this static
graph, including a width/enum/generic constant which depends back on itself.
Enum predecessor edges must point to the immediately preceding variant and a
source reference to a later enum variant is rejected even if another path
would make it acyclic. Constant-function call/backedge execution edges are not
static definition edges: recursive concrete calls and loops use the separate
finite execution-trace relation, so this SCC rule is not a recursion or
iteration cap. Completed SCC condensation order fixes the one evaluation
order; analyzer dependency order is not an oracle.

`Signed`, `Tri`, and `Default` are not interchangeable type flags:

- `Signed` participates in executable arithmetic signedness and is accepted
  only by the closed rules below.
- `Default` does not change width, domain, or signedness. For Veryl 0.20 it is
  admitted only on a bare clock/reset type use whose exact object is proved by
  `RawModuleTypeContext` to be that module's unique default clock/reset.
- `Tri` does not mean four-state `Logic`. It requests net/driver resolution
  on an eligible Bit/Logic object use. The source relation proves one exact
  `TriIntent`; the complete occurrence `TriNet` later proves flattened drivers
  and resolution. Neither may normalize it as `Logic`.

The resolved analyzer IR cannot reconstruct these distinctions. In
particular, matching a `VarId` to a symbol later by path, token, or shape is
not proof of the raw relation.

### Tri and default object context

For V0_20, `Tri` is syntactically a modifier of a direct `Bit` or `Logic` type
use; it is not inherited through a user-defined alias and it is orthogonal to
`Signed`. Its underlying object domain and arithmetic signedness are derived
exactly as for the unmodified Bit/Logic type. The complete owner allow-list is
a module/interface port, module/interface variable, module `let`, or interface
member. Alias targets, enum bases, aggregate members, constants/parameters,
generic formals/actuals, function arguments/returns/locals, nested statement
locals, and ForFold bindings reject `Tri`. Every concrete `inout` port must
have this exact source intent; a non-Tri concrete inout is rejected. A module
local Tri declaration need not be an inout. Port glue, interface flattening,
multiple drivers, Z contribution, resolution order, and read/write conversion
are owned by the complete occurrence `TriNet` relation, not by type
normalization. A `FrozenSourceArtifact` may retain a proved `TriIntent`, but no
mapped occurrence containing one may freeze or enter planning until its full
TriNet relation passes.

The source intent relation is independently derived from retained typed HIR:

```text
ExpectedSourceTriIntent
  intent / exact source object / exact Tri modifier and direct type use
  underlying read type and width/signedness/domain
  canonical expected driver range / expected read-site range

ExpectedSourceTriDriver
  owner intent / canonical driver identity and ordinal
  kind: SingleContribution(exact declaration/continuous/port/interface site,
                           value root, guard, source type,
                           TriDriveCoercion) |
        ProceduralProcess(source process ID, canonical update range,
                          initial driver-state rule)

ExpectedSourceTriDriverUpdate
  owner procedural driver / exact statement site and source ordinal
  exact lane access / guard / expected value root / source type
  TriDriveCoercion / closed overwrite-or-retain transition rule

ExpectedSourceTriRead
  owner intent / exact expected source use and role/site
  independently derived TriReadCoercion

VerifiedSourceTriIntent
  exact bidirectional match to all expected intent/driver/update/read rows
  no resolved value and no occurrence driver graph
```

The expected rows come from the complete typed-HIR traversal, not from a
producer driver summary. Every `ExpectedSourceValueGraph` read, write, port
boundary, and interface exposure which belongs to a Tri object maps to exactly
one expected Tri row, and every Tri row maps back. Omitting the same site from
both a producer summary and its nodes therefore cannot pass. A normal
non-contributing read is not a driver, and equal-shaped sites/objects are not
interchangeable.

Driver identity is one contribution source, not one assignment syntax site.
All writes from the same procedural process to the same Tri object belong to
one `ProceduralProcess` driver. Its ordered update rows replay guards, partial
lanes, blocking/nonblocking timing, overwrite, and retain semantics to produce
that driver's one current contribution. Two sequential writes in one process
therefore do not resolve against each other. Distinct continuous assignments,
port boundaries, declarations, interfaces, or procedural processes remain
distinct drivers. The expected control/action/token graph owns the final
versioned contribution at every observation point and is matched
bidirectionally to these update transitions.

Tri drive values use a separate four-state plane even when the underlying
object domain is Bit. `TriDriveCoercion` applies the target width and signed
extension rules but preserves X and Z; ordinary Logic-to-Bit assignment must
not turn a Z disconnect into a driven zero. `TriReadCoercion` preserves the
resolved plane for a Logic object and maps only known one to one for a Bit
object, with 0/X/Z becoming zero. Thus object domain, drive domain, and
resolution are three distinct facts.

After hierarchy mapping, the occurrence relation is:

```text
OccurrenceTriNet
  canonical nonempty lane-member range / complete driver range / read range
  net width / closed lane-wise resolution rule

OccurrenceTriLaneMember
  exact occurrence object / mapped source intent
  nonempty disjoint object bit range / equal-width net bit range / orientation

OccurrenceTriResolutionMap
  exact occurrence object / canonical segment range
  segments are disjoint, ordered, and exactly cover object width
  each segment names one OccurrenceTriNetId and exact net range/orientation

OccurrenceTriEquivalenceEdge
  exact inout port-glue or interface/modport electrical connection
  equal-width normalized endpoint lane atoms / orientation / lineage

OccurrenceTriDriver
  MappedSourceDriver(SourceInstanceId, ExpectedSourceTriDriverId) |
  DirectedPortContribution(GlueOriginId) |
  DirectedInterfaceContribution(exact interface/modport/member IDs) |
  ExternalBoundary(exact top port and direction)
  exact contribution value/guard/coercion and lineage

OccurrenceTriRead
  mapped or glue read / exact consumer role/site / TriReadCoercion
```

Inout and electrically aliased interface connections are equivalence edges,
not drivers from one already resolved object into another. The verifier gathers
all connection endpoints, splits object ranges only at canonical connection
boundaries, and uses an iterative deterministic union-find over those interval
atoms. It never expands one row per bit. Each connected component receives its
`OccurrenceTriNetId` from the lexicographically first member; orientations and
offsets then derive lane mappings. A cyclic inout topology is therefore one
component rather than a cyclic value graph.

Every mapped intent has exactly one complete resolution map, every map segment
belongs to exactly one net, every net member maps back, and every expected
mapped/glue/interface/boundary driver and read occurs exactly once. No ordinary
object may own a map. Driver completeness is checked bidirectionally against
the mapped expected occurrence graph and port/interface lineage, not a producer count.
For each bit, Z contributors are inactive; no active contributor resolves to
Z, unanimous known zero/one resolves to that value, and any active X or
conflicting known values resolves to X. Veryl V0_20 has no strength/delay rule
at this boundary. Contributor storage is in canonical lineage order, while the
resolution function is order-independent. Width lanes, guards, port direction,
interface exposure, interval partition/orientation, and read conversion are all
replayed before the intent is replaced by its
`OccurrenceTriResolutionMapId`.

`Default` is accepted only on a direct bare clock/reset type use: no alias,
packed width, unpacked dimension, `Signed`, or `Tri`. Its object context must
have `owner scope = ModuleTop` and role `Port`, `Variable`, or `Let` in the
same `RawModuleTypeContext`. Clock and reset are selected independently. At
most one explicit Default object of each class is permitted. If one exists it
is the selected object. If none exists, exactly one eligible bare candidate
is selected implicitly; zero or multiple eligible candidates select `None`.
Every analyzer default-clock/default-reset witness is compared for exact
object identity with this independently derived result. Equal path, type, or
shape is insufficient. An `always_ff` or other use which requires a default
when the derived result is `None` is rejected by its expected-HIR rule.

### Closed Veryl-0.20 kinds and modifiers

The accepted executable kinds and their terminal primitive facts are:

- `Bit`: domain `Bit`, width one when bare; an explicit `Signed` modifier sets
  signedness.
- `Logic`: domain `Logic`, width one when bare; an explicit `Signed` modifier
  sets signedness.
- `BBool`: unsigned domain `Bit`, exactly `Packed(1)`.
- `LBool`: unsigned domain `Logic`, exactly `Packed(1)`.
- `u8/u16/u32/u64`: unsigned domain `Bit` and their fixed width.
- `p8/p16/p32/p64`: the same executable storage facts, while retaining the
  declared positive-assignment predicate `known two-state value > 0`. This is
  a static value-use constraint, not permission for an optimizer to assume
  that every runtime bit pattern is nonzero.
- `i8/i16/i32/i64`: signed domain `Bit` and their fixed width.
- clock and reset variants: unsigned domain `Logic`, `Packed(1)` when bare;
  explicit packed extents replace that implicit terminal exactly as for direct
  Bit/Logic, and ordinary unpacked extents are retained.
- aliases, enums, structs, and unions only through the complete rules below.

Fixed integer/boolean kinds reject explicit packed widths. Fixed
integer/boolean and clock/reset kinds reject `Signed`. User-defined aliases,
enums, structs, and unions reject an outer `Signed` modifier under Veryl 0.20;
an alias inherits its target signedness. Width or array dimensions make a
clock/reset ineligible for `Default`, but are valid on a non-default use.
Aliases to `p*` also retain the positive-assignment predicate.
Unknown, unresolved, SystemVerilog, interface/module/package, abstract,
modport, type-valued, string, floating-point, and void kinds are rejected.
Supporting one later requires a new versioned value-domain rule and must not
fall back to `Bit`.

Every normalized selected type retains this closed storage/value-use row:

```text
VerifiedNormalizedType
  width / signed / domain: Bit | Logic
  positive_type: Plain | Positive
  shape/member/intrinsic facts as applicable
```

`Positive` survives only the alias/selection rules stated below. It is not a
value-range proof.

### Closed assignment and lossless constant coercion

The verifier derives coercions from source and target types; a producer never
supplies the recipe. For an integral value with canonical payload/mask planes,
ordinary assignment applies these operations in order:

1. If the target is wider, extend payload and mask with the source sign bit
   when the source type is signed, otherwise with zero. If the target is
   narrower, discard the high bits. Equal widths are unchanged.
2. A Logic target preserves both planes. A Bit target maps each retained bit
   to `payload AND NOT mask`, so known one remains one and 0/X/Z become zero;
   its result mask is empty.
3. The result receives the target signedness, domain, and positive-type class;
   those metadata never change step 1 retroactively.

`LosslessConstantCoercion`, used by fixed enum bases and every other role which
requires exact representability, is stricter:

- a known two-state source is interpreted as a mathematical integer under its
  source signedness; it must be representable under the target width and
  signedness, and converting back must yield the same integer;
- an X/Z-bearing source requires a Logic target, no narrowing, and exact
  payload/mask preservation after the specified extension;
- any Logic-to-Bit conversion containing X/Z is lossy even when X/Z would map
  to zero; and
- a Positive target additionally requires the final known two-state
  mathematical value to be greater than zero.

The static Positive assignment rule for ordinary non-enum uses is also closed.
If an independent constant proof exists, its final coerced value must be known
two-state and greater than zero. Without a constant proof, the source selected
type must itself be `Positive`; an unproved dynamic Plain value cannot enter a
Positive target. This same rule applies to assignments, ports, generic actuals,
function arguments/returns, aliases/member projections, and constructors.
Even when accepted, the target's Positive class is not a runtime nonzero fact.
The pinned analyzer's cached `is_positive` or exception for an X/Z-bearing
positive-typed constant is not an oracle.

Veryl 0.20 requires every member of one packed struct or union to have the
same value domain: all `Bit` or all `Logic`. Mixed-domain aggregates are
rejected, even if a later implementation could represent their joined domain.
A future language-semantics version may define the domain join
`Bit < Logic`; that future rule must have a distinct version tag and fixtures.
It must not silently change the V0_20 relation. A struct is unsigned and has
the checked sum of member packed widths. A union is unsigned, nonempty, and
all members have the same checked packed width. Unpacked members are rejected.

Member selection still derives the selected member's independently verified
type. This matters for future mixed-domain versions, but does not relax the
V0_20 uniform-domain requirement.

### Enum base and variant relation

An enum retains one closed base rule rather than a producer-selected width:

```text
RawEnumBaseRule =
  Omitted |
  ExplicitFixed(base RawTypeUseId) |
  InferBit { signed: bool } |
  InferLogic { signed: bool }

RawEnumEncoding = Sequential | OneHot | Gray

RawEnumVariantRow
  owner enum RawTypeId / declaration ordinal / source coordinate
  recipe: Implicit | Explicit(RawConstExprOccurrenceId)
```

The variant pool is nonempty, gap-free, uniquely owned, and in declaration
order. `ExplicitFixed` accepts only a completely normalized `Bit`, `Logic`,
`BBool`, `LBool`, fixed integer, or alias to one of those primitives, with no
unpacked dimension or inferable extent. Clock/reset and every composite or
non-data kind are not enum bases. `Omitted` means unsigned `Logic` with
inferred width. `InferBit`/`InferLogic` are the exact single inferable-width
forms; `_` in any other position is rejected. Enum signedness and domain come
from this base rule. The enum contributes exactly one
`Intrinsic(enum_width)` dimension, including width one.

An enum whose explicit fixed base is `p8/p16/p32/p64` applies that base's
positive predicate to every final variant. Consequently an implicit initial
Sequential or Gray value zero is invalid; an explicit positive first anchor
may establish a valid sequence, and OneHot begins at one. X/Z, zero, a
negative mathematical value, or a value which becomes zero after the closed
base coercion fails the enum relation. The analyzer's plain unsigned
width/domain summary is insufficient to prove this refinement.

Every explicit recipe's raw occurrence must receive a completed
`ExpectedTypedConstantExprId` from the joint relation. Its raw rows refer to
canonical borrowed `RawArbitraryBitsId` values
for numeric magnitude, payload, and X/Z mask, with exact role, sign, and typed
width retained separately. Its verified proof retains the exact source type,
fallibly verified arbitrary-width signed value, payload, X/Z mask, result
width/domain/signedness, and the closed lossless assignment coercion to the
enum base. Analyzer `EnumProperty.width`, cached member values, owned
`BigUint`/`BigInt` input, `usize` conversion, and the legacy constant evaluator
are not oracles. Until the complete typed constant-expression relation exists,
a raw enum with any explicit variant cannot pass the typed-HIR aggregate
verifier.

Starting with no previous numeric value, implicit values use this exact
arbitrary-width recurrence. `g(i) = i XOR (i >> 1)` is the reflected Gray
encoding and `decode_gray` is its unique inverse on nonnegative bit strings:

```text
                 first implicit       after previous two-state value p
Sequential             0                         p + 1
OneHot                  1                         p << 1
Gray                    0               g(decode_gray(p) + 1)
```

All addition, shifts, XOR, and Gray decoding use verifier-owned fallible limb
routines; ordinary `BigUint` or `BigInt` operators are not used on the proof
path. The conceptual recurrence is not permission to materialize every growing
OneHot/Gray value. Replay uses one closed cursor per encoding:

```text
SequentialCursor = explicit mathematical anchor + checked delta
OneHotCursor      = explicit one-bit proof + arbitrary-width set-bit index
GrayCursor        = arbitrary-width decoded ordinal
```

An explicit two-state value replaces the current anchor. Under V0_20, OneHot
and Gray anchors must be known mathematical nonnegative values before
encoding; a negative signed expression is rejected rather than interpreted as
a width-dependent two's-complement pattern. `OneHot` scans the nonnegative
magnitude once, requires exactly one set bit, and records its bit index; an
implicit successor increments only that index. A `Gray` explicit value after a
predecessor must equal `g(ordinal + 1)`. A first explicit Gray value is a legal
anchor: the verifier decodes it once, and the next implicit or explicit row
must encode the following ordinal. Thus an all-implicit Gray enum is exactly
`0, 1, 3, 2, 6, 7, 5, 4, ...`; using the previous encoded value directly as
the next ordinal is forbidden. Sequential explicit values may be negative on a
signed base and have no encoding predicate beyond the base/coercion rules.

An X/Z mask is forbidden for every Bit-domain base and under either OneHot or
Gray encoding. A Logic-domain Sequential enum may contain an explicit X/Z
value when its typed constant and lossless base coercion prove the exact
payload and mask, but that value has no numeric successor: the immediately
following implicit variant is rejected. X/Z is never converted to an integer.

For `ExplicitFixed`, every recipe must fit the fixed base through the verified
lossless assignment coercion; inferred width does not widen it. For inferred
bases, `enum_width` is the maximum of one and the exact minimum width required
by every verified recipe result, including the complete payload/mask width of
an admitted Logic X/Z value and signed two's-complement representability when
signed. The recurrence is replayed first and the width is derived second; no
step truncates to a provisional width. Every final variant is then replayed
against that width and must fit losslessly. `Omitted` uses the same inference
with unsigned Logic semantics.

For a signed inferred OneHot/Gray base, minimum width includes the leading zero
needed to keep every nonnegative anchor/recipe nonnegative. Zero extension to
the final inferred width therefore preserves its set-bit index or Gray
magnitude. The verifier nevertheless repeats the OneHot population or exact
Gray-ordinal predicate after final coercion; a changed final bit pattern is a
hard error, never an updated cursor.

The durable enum fact stores only `(base rule, encoding, canonical compact
variant recipes, enum_width, domain, signedness)`. Recipes are sequential
anchor/delta, OneHot bit index, Gray ordinal, or an explicit X/Z proof ID. It
does not store analyzer cached values or expand a table indexed by all enum
values. Required cursor/result/scratch limb capacity is computed and fallibly
reserved before mutation. In particular, N implicit OneHot variants require N
checked index increments and O(log N) live cursor limbs, not Θ(N²) shifting or
payload storage. A concrete full-width value is materialized fallibly only for
an exact later use which requires it.

Every final coerced variant bit pattern, including its complete X/Z mask, must
be unique. Gray's strictly consecutive decoded ordinals prove uniqueness
directly. Sequential anchor/delta runs and OneHot bit-index runs enter a
fallibly built ordered disjoint-interval index; overlap rejects a duplicate
without expanding each value. Explicit X/Z patterns enter a content-ordered
canonical limb index. Analyzer member caches and hash equality are not proof.
Construction is bounded by canonical recipe/explicit-limb input and a
logarithmic number of interval/content comparisons; pairwise variant
comparison is forbidden.

Veryl 0.20.1's analyzer used the previous Gray-encoded value directly in
`g(p + 1)`, which ceases to be the reflected Gray sequence after the first
few members. That implementation is a witness to compare and diagnose, not
the semantic oracle. The verifier uses the corrected decode/increment/encode
relation above and requires the adapter or producer cache to be fixed when it
disagrees.

### Iterative normalization and persistent shapes

The verifier uses `Enter(type/use)` and `Finish(type/use)` frames with
`Unseen`, `Visiting`, and `Done` marks. A `Visiting` child is a recursive type
cycle. Children are visited by fixed language ordinal. There is no recursion
depth cutoff, recovery fallback, or type-depth cap.

Every extent remains a `RawExtentRow`, its exact raw constant-expression
occurrence, and its analyzer-resolution witness at the raw boundary. `Unresolved`, a
resolved canonical zero magnitude, malformed raw bytes, constant-proof/witness
disagreement, and failure of checked `usize` conversion are distinct errors.
The verifier decodes only into pre-reserved fallible limb storage, then
performs checked conversion; it never requires construction of an owned
`BigUint` as a prerequisite. Checked products are derived independently for
unpacked extents, explicit packed extents, terminal/intrinsic width, composed
packed width, total width, and the final suffix-stride replay.

Normalized shapes are persistent and must not copy a target's complete
dimension vector into every alias:

```text
VerifiedExtentArena: canonical normalized usize extents

ShapeSegment
  extents: nonempty canonical range in VerifiedExtentArena
  next: optional earlier ShapeSegmentId

VerifiedTypeShape
  unpacked_head: optional persistent segment chain
  packed_head: optional persistent segment chain
  terminal: None | Packed(1) | Packed(fixed width) |
            Intrinsic(enum/struct/union width, including 1)
  checked unpacked_count / dimension_count
  checked unpacked_product / packed_width / total_width
```

The terminal is derived by the exact core/use relation. A direct `Bit`,
`Logic`, clock, or reset use has `Packed(1)` only when its own explicit packed
range is empty; a nonempty explicit range replaces that implicit one-bit
dimension and uses terminal `None`. `BBool`, `LBool`, and fixed integers retain
their fixed `Packed` terminal and reject explicit packed ranges. A direct
enum/struct/union always has its `Intrinsic` terminal, including width one.
An alias shares its target terminal unchanged: an alias's own packed segment
is outer shape and therefore does not suppress the target's terminal.

Each nonempty type-use extent range is copied once into the verified extent
arena and creates at most one unpacked and one packed segment. An alias with an
empty own range shares the target head; otherwise its segment points to the
target head. Alias target order is therefore
`own unpacked ++ target unpacked`, then
`own packed ++ target packed ++ target terminal`. In Veryl 0.20 an explicit
outer alias width precedes the target width, so aliasing a bare `Bit` may
legitimately retain both `Packed(outer)` and the target's selectable
`Packed(1)`.

Only a semantic object or other required root materializes dimensions: walk
the unpacked chain outer-to-target, then the packed chain outer-to-target,
append the one terminal dimension, and derive suffix strides from right to
left. The final suffix product must equal the independently checked summary.
Construction is `Theta(type rows + raw extents + shape segments)` plus the
size of materialized root dimensions. Copying every target vector into every
alias, and therefore `Theta(depth^2)` alias behavior, is forbidden.

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

### Reservation and failure policy

Every raw count, range endpoint, verified ID, segment ID, and dimension count
is proved representable before publication. Each allocation site uses a
fallible exact reservation owned by one prepared aggregate. Tests inject
failure at reservation ordinal `N` for every `N` below the successful
reservation count; `N` equal to that count is the successful control. Sites
include raw-owner bitmaps, visit marks, worklists, verified
limb/extent/segment/type/member/object pools, enum recipe scratch, and root
materialization. Failure returns one allocation-free structured error and
leaves all externally visible lengths, mappings, brands, and owners unchanged.
No `String`, formatting allocation, panic, partial commit, retry with a smaller
representation, or fallback path is permitted.

### Shared fallible value and payload substrate

Types, constants, phase nodes, enum replay, and later cost arithmetic use one
implementation substrate but distinct typed ID namespaces. The end-state
phase representation is:

```text
RawMagnitudeRef
  disjoint canonical byte range in the one borrowed/aggregate-owned raw pool
  exact payload/mask/numeric role and sign role

VerifiedBitsArena<P>
  rows: flat fallibly reserved VerifiedBitsRow table
  words: flat fallibly reserved u64 word pool

VerifiedBitsRow
  disjoint canonical word range / exact bit length
  zero is an empty range at the current cursor
  nonzero has a nonzero final word; unused high bits are zero

VerifiedTypedValueArena<P>
  flat fallibly reserved VerifiedTypedValueRow<P> table

VerifiedTypedValueRow<P>
  payload: VerifiedBitsId<P>
  xz_mask: VerifiedBitsId<P>
  width: nonzero usize
  signed
  domain: Bit | Logic
  exact ExpectedTypedConstantExprId or closed derived-value origin
```

Bits above `width` are zero. A Bit-domain value has an empty X/Z mask. For a
Logic value, mask zero means a known data bit; mask one with payload zero means
X and mask one with payload one means Z. Identity/select/concat preserve X
versus Z, while an operation whose closed truth table produces an unknown uses
the specified canonical X result. Mathematical enum/extent/cost arithmetic
uses separately typed `VerifiedNatural` and `VerifiedSignedMagnitude` views on
the same word substrate; a fixed-width Logic payload is never reinterpreted as
one of them merely because its mask is zero.

Phase nodes and their variable-size payloads contain only checked IDs/ranges:

```text
PhaseSLTNode::Constant { value: VerifiedTypedValueId<P> }
PhaseSLTLoopBound::Const { value: VerifiedTypedValueId<P>, coercion }
PhaseForFoldNode.step: VerifiedTypedValueId<P>
PhaseInputNode { input, index_range }
PhaseConcatNode { part_range }
PhaseForFoldNode { state_range, effect_range, ... }
PhaseForFoldEffect { argument_range, ... }
```

There is no `PhaseOwnedPayload<T>` wrapper. Reserving a one-element outer
vector after a `BigUint`, nested `Vec`, or diagnostic `String` was already
constructed does not satisfy this contract. Canonical interning uses an arena-
aware `cmp_nodes` which compares referenced typed-value and range contents;
numeric ID order is not semantic order. All error types are allocation-free
closed `(rule, phase, owner, context)` records whose `Display` formatting is
lazy. The current private phase arena's owned `BigUint` constants, nested
vectors, string-bearing errors, and context-free derived `Ord` are therefore
migration input, not a representation that may be connected or frozen.

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

## Result signedness, positive type, and domain

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

The declared positive-type class follows the same selected-type provenance,
but remains only a value-use class:

- whole-object and unpacked-only selection preserves the object's class;
- named member projection uses the selected member's class;
- any packed bit/part selection clears it; and
- no later projection restores it.

It is retained in `InputAccess.result_positive_type` for subsequent assignment,
port, generic-actual, return, and constructor checks. Node execution facts do
not contain a `known_nonzero` bit and no optimization may derive one.

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
  object_width / declared_signed / declared_positive_type / object_domain
  exact PhaseObjectResolution<P> / default_role
  canonical dimensions with extent and stride

InputAccessFact<P>
  input / object
  compact normalized access and ordered runtime role geometry
  optional verified selected-member type projection
  selected_width / result_signed / result_positive_type / result_domain
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

- raw type/use/modifier/object/module-context/arbitrary-bit pools with gaps,
  overlaps, duplicate ownership, orphan rows, noncanonical ranges or integer
  encodings, root/owner coordinate mismatch, wrong embedded `VarId`, and
  referential-row permutation with exact ID relocation; permuting a
  syntax-ordered owned pool is rejected;
- raw constant-expression occurrence/proof-ID substitution, missing type-only
  or value dependency,
  analyzer-resolution disagreement, and analyzer `Comptime`,
  `Shape`/`WidthExpr`, or enum-cache values masquerading as proof;
- unresolved, zero, and unrepresentable packed/unpacked extents;
- distinct checked overflow in struct member sum, union/enum intrinsic width,
  own unpacked product, own explicit-packed product, composed packed width,
  total width, dimension count, and materialized suffix-stride replay;
- empty struct/union, unequal union member widths, recursive type cycle, and
  unpacked member in a packed aggregate;
- unknown, floating, string, SystemVerilog, module/interface, and non-data
  kinds masquerading as `Bit` or `Logic`;
- `bbool` and `lbool` retain distinct raw tags and normalize respectively to
  unsigned `Bit` and unsigned `Logic` with exactly `Packed(1)`;
- `bbool`/`lbool` reject `Signed`, `Tri`, `Default`, and explicit packed width
  instead of being rewritten to a permissive Bit/Logic type use;
- bare, packed, unpacked, and multidimensional clock/reset uses are accepted
  with Bit/Logic-style terminal replacement, while the same non-bare uses with
  `Default` are rejected;
- `p8/p16/p32/p64` retain their positive-assignment predicate through aliases;
  zero, negative, X/Z, wrapped-zero, and implicit-zero enum variants fail,
  while a positive explicit anchor and OneHot one pass;
- two specializations of one generic type declaration with different type or
  constant actuals derive distinct member shapes; swapped actual/default
  selection, parent environment, specialization witness, or environment ID
  fails even when final widths match;
- static type/extent/enum/generic dependency self-cycles and multi-node SCCs,
  forward enum references, and analyzer-selected dependency order are rejected
  without rejecting terminating recursive constant-function executions;
- mixed Bit/Logic structs and unions are rejected under V0_20; a separately
  tagged future-version fixture may prove domain join and selected-member
  domain only after that future semantics is specified;
- `Signed`, `Tri`, and `Default` rows cannot be swapped, omitted, duplicated,
  attached to the wrong type use, or reconstructed from resolved analyzer
  flags;
- `Default` is accepted only with an exact unique module default-clock/reset
  relation; a source Tri-bearing aggregate retains only exact intent, while a
  mapped occurrence remains non-freezable until its complete TriNet relation
  passes;
- direct signed/unsigned Bit/Logic Tri declarations retain independent domain
  and signedness; Tri through an alias, Tri on another kind, and a non-Tri
  concrete inout are rejected;
- Tri on an alias target, enum base, aggregate member, constant/parameter,
  generic row, function signature/local, nested local, or ForFold binding is
  rejected by the closed source-intent owner allow-list;
- Tri ports, ordinary variables/lets, and interface members retain exact
  object roles; a source intent cannot impersonate an occurrence TriNet proof,
  while swapped occurrence object IDs, driver owners, port directions, or
  aggregate/glue origins fail that complete relation;
- explicit default clock/reset positives for module ports, variables, and
  lets; duplicate explicit defaults, wrong module/role/kind, aliases, arrays,
  widths, and analyzer-witness ID substitutions are rejected;
- with no explicit default, exactly one eligible candidate is selected and
  zero or multiple candidates produce `None`; clock/reset selections remain
  independent and a required use of `None` is rejected;
- nested struct field offsets compose according to declaration-order packed
  layout, while every union field offset remains zero;
- enum base modes `Omitted`, `ExplicitFixed`, `InferBit`, and `InferLogic`,
  including domain and signedness derivation and rejection of `_` in every
  other position;
- canonical nonempty enum variant pools with missing, duplicate, reordered,
  wrongly owned, or orphan recipes;
- Sequential, OneHot, and reflected-Gray first values and recurrence, first
  explicit Gray anchor, explicit restart, OneHot population-count failure,
  Gray predecessor mismatch, and checked arbitrary-width cursors beyond
  `usize`;
- an implicit OneHot enum large enough to expose quadratic limb shifting keeps
  one bit-index cursor and linear recipe work/storage; overlapping Sequential
  or OneHot intervals and duplicate explicit X/Z patterns are rejected;
- explicit enum recipes with missing/wrong typed-constant-expression proof,
  lossy base coercion, fixed-width overflow, signed fit boundary, and analyzer
  width/member cache disagreement;
- Bit-domain and OneHot/Gray X/Z rejection, Logic Sequential X/Z preservation,
  and rejection of an implicit successor after a nonnumeric X/Z value;
- inferred enum width from exact positive, signed-negative, and Logic X/Z
  recipes, followed by complete lossless replay at the derived width;
- bare one-bit Bit/Logic/clock/reset retains one selectable Packed extent of
  one, while a direct explicit packed range replaces rather than appends that
  implicit terminal;
- a width-one enum/struct/union retains one Intrinsic extent of one, while an
  alias does not duplicate its resolved target's dimension;
- an outer-width alias of a bare primitive retains the Veryl-0.20 ordered
  packed dimensions for both outer width and target `Packed(1)`;
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
- deterministic fail-at-`N` at every raw-owner, verified-limb/scratch,
  visit/worklist, extent/segment/type/member/object, enum-recipe, input/fact,
  and root-materialization reservation site;
- no semantic length or mapping change after any failed derivation/replay;
- iterative deeply nested type and access graphs without host recursion;
- a deep alias chain with at least one own extent per alias proving linear
  verified extent/segment storage and forbidding quadratic copied vectors; and
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

The current partial typed-HIR normalizer is also not connection permission.
In particular, the complete `ExpectedTypedConstantExpr` relation needed by
every extent and explicit enum variant and the source `TriIntent` relation do
not yet exist. Rejecting or skipping all examples which exercise one of those
relations does not make the remaining partial implementation a complete
Veryl-0.20 adapter. Neither analyzer resolved-expression/enum caches nor
treating `Tri` as ordinary `Logic` may bridge the gap. Separately, no mapped
occurrence carrying a Tri intent is valid until complete flattened TriNet
verification.

Before producer connection, the implementation must have:

1. a verified canonical typed-HIR snapshot with the separate raw type,
   type-use, modifier, object, module-context, member, enum-variant, and root
   relations above, including every syntax-level modifier required by its
   selected semantics version;
2. the complete iterative `ExpectedTypedConstantExpr` graph, its closed
   type-only/value dependency relation, and the lossless constant-coercion
   verifier used by every extent and explicit enum recipe;
3. the complete source `TriIntent` object/role/modifier/read/drive provenance
   relation for every admitted `Tri` modifier;
4. the full iterative `ExpectedSourceValueGraph` traversal for every accepted
   declaration, statement, expression, observer, dynamic-address, environment,
   static-composite, and `ForFold` variant;
5. canonical source-object and source-input rows derived only from the
   verified typed-HIR, constant-expression, source-TriIntent, and
   expected-value relations;
6. a bidirectional match from every expected read/index role to producer nodes
   and from every producer Input node back to an expected recipe;
7. complete expected-node reachability and ordinary/gated classification; and
8. consuming aggregate prepare/commit ownership with no standalone facts or
   arena publication.

Until all eight hold, `InputSemanticFacts<SourcePhase>` and source-node replay
remain private verifier/test stages. Passing structural node tests, legacy
lowering tests, or synthetic scale measurements is not evidence that the
source semantic-input relation is complete.

After those source requirements hold, occurrence preparation must additionally
derive instance-specific mapped intents, interface/modport exposure, every
source and glue driver, Z contribution, read conversion, and resolution order;
prove bidirectional complete lane components/resolution maps for every Tri
occurrence; and replace each mapped intent with its
`OccurrenceTriResolutionMapId`. Failure leaves the
source catalog and occurrence draft unchanged. No occurrence artifact with a
remaining mapped intent can freeze, lower, allocate registers, or execute.

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
- [Clock / Reset](https://doc.veryl-lang.org/book/05_language_reference/03_data_type/04_clock_reset.html)
- [Bit select](https://doc.veryl-lang.org/book/05_language_reference/04_expression/06_bit_select.html)
- [Veryl corrected reflected-Gray recurrence](https://github.com/veryl-lang/veryl/commit/95a14877823a4b9214729ab48152a09ab94b8412)
- [Veryl duplicate enum-value validation](https://github.com/veryl-lang/veryl/commit/22a722a0a6ef483bf3ea54464d83068e38d2fbef)
