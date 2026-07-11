# Raw source aggregate and private wire staging

This document specifies the production in-memory raw source boundary, the
optional byte-level framing substrate, and the first untrusted source-node
staging boundary for the verifier-first pipeline. It is a construction
specification, not a persistent artifact-format declaration. In particular,
it does **not** define `SourceWireV1`, make a source artifact planner-ready, or
permit an SLT arena to be frozen by itself.

`RawSourceAggregateV1` below is the production verifier input. Authoritative
source syntax is the `VerylParserV0_20_1_UEscape1` adapter tree plus exact
tokens. The flat logical rows described in this document,
[`source-semantic-inputs.md`](./source-semantic-inputs.md),
[`typed-constant-evaluation.md`](./typed-constant-evaluation.md), and
[`decision-region-architecture.md`](./decision-region-architecture.md) are
verifier-derived private staging, not producer-authored HIR. A byte
decoder is only one adapter which may construct the proposal/witness portion;
the first verifier implementation and a live producer use the in-memory
boundary directly. No `SourceWireV1` schema, serializer, or decoder is a
precondition for implementing or connecting the aggregate verifier. If a
persistent source wire is later required, one complete version must encode
exactly this aggregate and pass the same verifier; the private framing here is
not automatically that version.

## Boundary and trust model

The intended ownership chain is:

```text
borrowed adapter-parsed syntax/tokens plus producer proposal/witness rows
  -> RawSourceAggregateV1
  -> private flat raw topology stage
  -> verified typed source HIR and independently derived expected graphs
  -> private unclassified source-node stage and recomputed node facts
  -> complete source provenance and construction-identity verification
  -> prepared aggregate source artifact
  -> infallible commit to FrozenSourceArtifact
```

The raw-input and node-staging arrows covered here do not by themselves
establish the complete source relation. An optional raw decoder proves only
that its encoding is canonical, bounded by the supplied bytes, and
representable on the host. The unclassified node stage additionally proves
append-order graph structure and recomputes node facts against semantic input
facts independently derived from verified typed HIR. Neither result is a
verified source artifact.

Raw integers remain raw through the complete structural scan. A raw node index
becomes `PhaseNodeId<SourcePhase>` only after every node edge, including edges
inside input indices, coercion uses, `ForFold` states/effects, concatenations,
and loop bounds, has been checked. Raw control, input, runtime-site, HIR, and
provenance integers similarly become their checked ID types only at the
aggregate verifier that owns their complete relation.

## Production raw aggregate boundary

The production input is one borrowed closed sum-of-tables value:

```text
RawSourceAggregateV1<'a>
  semantics: exactly CeloxSourceV0_20
  syntax_adapter: exactly VerylParserV0_20_1_UEscape1
  syntax: canonical ordered adapter AST references plus exact source/token
          resources and preprocessing-origin coordinates
  canonical RawEnvironmentLineageV1 comparison rows
  canonical SyntaxRuntimeSourceExecutionLineageWitnessV1 comparison rows/pool
  RawSyntaxAnalyzerWitnessV1<'a>, keyed only by closed syntax lineage
  invocation-certificate rows and fixed logical RawTraceStep rows
  RawSourceProposalV1<'a>
    untrusted source node/control/root/action/observer/runtime-site rows
    every dedicated node operand/state/effect/index range pool
    source provenance and complete gated-key proposal registry

PrivateRawSemanticSyntaxV1<'a>                 -- verifier-owned staging
  one canonical source-coordinate/spelling table
  one scope/declaration/import/name-resolution relation
  one type/type-use/inference/extent/enum/generic/object/module-context relation
  one declaration/statement/expression/constructor/select relation shared by
    ordinary source and constant-function verification
  raw arbitrary-magnitude rows and borrowed token-byte ranges
```

This is an API schema rather than a Rust struct image.
`VerylParserV0_20_1_UEscape1` is exactly the generated parser AST from pinned
`veryl-parser 0.20.1` with the one scanner correction specified in
[`typed-constant-evaluation.md`](./typed-constant-evaluation.md): the string
escape alternative recognizes backslash, `u`, and exactly four hexadecimal
digits as one string token. It changes no other token, grammar production,
AST field/list order, token ordinal, source span, or preprocessing coordinate.
The syntax adapter performs an exhaustive match over every generated syntax
enum. It may classify a closed unsupported syntax variant, but cannot use
analyzer IR as a substitute. Tokens/spellings and parser-list order remain
available. Any other parser or scanner behavior is a different source-adapter
version and fails before private syntax rows are constructed.

The iterative syntax flattener is part of the verifier and fallibly creates
`PrivateRawSemanticSyntaxV1`. That private value uniquely owns all scope, type,
expression, and generic ID namespaces; constant/type/source consumers hold
references into it and never duplicate those tables. Every variable member is
a raw `{ start: u64, len: u64 }` into one dedicated flat slice. Optional syntax
is an explicit `None | Some(raw ID)`. There is no checked/phase ID, analyzer
pointer/target, hash-map iteration order, omitted-table default, or extension
field map in private raw staging.

The primitive producer-addressable source identity is fixed-size syntax
lineage:

```text
SyntaxOccurrenceKeyV1
  parsed-unit ordinal: u32 / canonical AST preorder ordinal: u64 /
  first and last exact token ordinals: u64 / preprocessing expansion ordinal: u32

RawEnvironmentLineageRefV1 = raw u32 reference into RawEnvironmentLineageV1;
  it becomes a checked environment-lineage ID only after the complete lineage
  table has passed parent/order/key/bijection verification

RawEnvironmentLineageRowV1
  optional parent RawEnvironmentLineageRefV1 /
  generic-use: optional SyntaxOccurrenceKeyV1 /
  role: SyntaxEnvironmentLineageRoleV1
```

`SyntaxEnvironmentLineageRoleV1` is a generated alias of the authoritative
closed enum in `source-semantic-inputs.md`; this document does not redeclare
its discriminants. `Root` identifies the surface owner's generic use, while a
`PathComponent { path, component_ordinal }` identifies the exact qualified-path
component whose optional generic use must be completed before the next
component lookup.

AST preorder is the pinned generated grammar's field/list order and counts
every node, including unsupported syntax; it cannot be analyzer traversal
order. Token and expansion fields make a wrong preorder independently
detectable. Environment rows are dense in canonical root/use discovery order,
parents precede children, and the verifier derives/compares their complete
relation before converting any row ID. Producer witnesses/certificates use
these keys, never IDs from `PrivateRawSemanticSyntaxV1`. Function input values
cannot form a program type in `CeloxSourceV0_20`, so no value-dependent
specialization lineage table exists; the execution lineage below contains only
syntax, generic environment, and independently resolved complete type identity.

### Canonical runtime source execution lineage

Syntax occurrence alone does not identify an executable source row. One
verified runtime-function program may be instantiated beneath several outer
calls, and the same nested call syntax then belongs to a different path in each
parent. A retained ForFold is a local program scope under that unchanged
root/call path, not an execution-lineage step. The aggregate therefore carries
the physical producer-comparison form of the authoritative runtime lineage
relation in [`source-semantic-inputs.md`](./source-semantic-inputs.md):

```text
SyntaxRuntimeSourceExecutionLineageWitnessRefV1 = raw u32 reference into
  SyntaxRuntimeSourceExecutionLineageWitnessV1; it becomes checked only after
  the complete parent/order/root/call/specialization/bijection relation succeeds

RuntimeSourceExecutionLineageId = verifier-owned checked u32 ID produced only
  by that complete relation; it is never a producer field

SyntaxExecutableTypeOriginKeyV1 =
  Explicit(SyntaxTypeUseKeyV1) |
  Inferred(SyntaxInferredTypeReplayKeyV1)

SyntaxRuntimeFormalTypeContentWitnessRowV1
  formal SyntaxEntityKeyV1 / declaration ordinal u32 /
  Input | Output | Inout /
  SyntaxExecutableTypeOriginKeyV1

SyntaxRuntimeFunctionSpecializationKeyV1
  template: SyntaxOccurrenceKeyV1
  generic environment: RawEnvironmentLineageRefV1
  canonical formal direction/type-content-key range
  optional return SyntaxExecutableTypeOriginKeyV1

SyntaxRuntimeSourceExecutionLineageWitnessRowV1 =
  Root {
    exact executable-unit/root SyntaxOccurrenceKeyV1 /
    role: RuntimeSourceRootRoleV1
  } |
  RuntimeCall {
    parent SyntaxRuntimeSourceExecutionLineageWitnessRefV1 /
    exact call SyntaxOccurrenceKeyV1 /
    SyntaxRuntimeFunctionSpecializationKeyV1
  }

SyntaxRuntimeSourceExecutionLineagePoolKindV1 = FormalTypeContent

SyntaxRuntimeSourceExecutionLineageWitnessV1<'a>
  rows: &'a [SyntaxRuntimeSourceExecutionLineageWitnessRowV1]
  formal_type_contents: &'a [SyntaxRuntimeFormalTypeContentWitnessRowV1]
```

`SyntaxEntityKeyV1` and `SyntaxTypeUseKeyV1` are the producer-facing syntax
keys defined in the next section; this relation never uses their private
flattened-row counterparts. A formal type-content key is obtained only by
independently resolving its syntax type origin and complete normalized type;
the producer row cannot choose content by alias spelling.
`RuntimeSourceRootRoleV1` and `SyntaxRuntimeLocalScopeV1` are generated aliases
of the authoritative closed enums in `source-semantic-inputs.md`; this document
does not own duplicate discriminant lists for either identity axis.

Every parent reference is in bounds and strictly precedes its child. `Root`
rows are ordered by canonical verified typed-HIR root traversal. For each
lineage, nested runtime calls are visited in the shared verified program's
source/program-point order; walking through a retained fold changes only the
local scope attached to slots and does not emit a lineage row. Complete equal
rows are interned by content at first encounter; this parent-first traversal is
the sole dense row order. Formal-type ranges exactly partition
`formal_type_contents` in row order and are in declaration ordinal; `Root`
owns the canonical empty range.

The verifier independently resolves the callee, generic environment,
every formal/return type origin, and complete normalized type content. It then
derives `VerifiedRuntimeFunctionSpecializationKeyV1` and compares the row
bidirectionally with the independently derived runtime lineage and
`ExpectedSourceRuntimeCallInstanceV1`. The comparison table
contains no private `RawSourceCallId`, private function/type ID,
`VerifiedRuntimeFunctionSpecializationId`, analyzer specialization, input
value, hash, or pointer. Equal normalized type content may select one shared
verified program, but distinct parent/call rows remain distinct because parent
lineage is part of complete row content.

The runtime-function template call graph is checked before specialization; the
first canonical self-edge or nontrivial SCC fails
`SOURCE.RUNTIME_FUNCTION_RECURSION`. The verifier does not truncate recursion,
impose a depth limit, or accept a finite producer prefix. Missing, duplicate,
extra, wrongly parented, wrongly specialized, or noncanonical rows fail before
proposal keys are converted. Only after the whole table succeeds may proposal
keys use a `SyntaxRuntimeSourceExecutionLineageWitnessRefV1`.

### Producer-facing analyzer witnesses

Producer comparison data is one closed syntax-keyed relation. It is not the
`Raw*WitnessRow` relation named by the private typed-HIR specifications. Those
private rows are created only after syntax flattening by resolving the keys
below and are verifier-owned mapped mirrors. In particular, no producer row
contains a `RawTypeId`, `RawTypeUseId`, `RawExtentId`, `RawNameOccurrenceId`,
`RawGeneric*Id`, `RawConstExprOccurrenceId`, `RawArbitraryBitsId`, or any other
ID owned by `PrivateRawSemanticSyntaxV1`.

```text
SyntaxEntityKindV1 =
  Scope | ParameterDeclaration | ConstantDeclaration |
  TypeDeclaration | TypeUse | GenericFormal | GenericUse | GenericArgument |
  EnumDeclaration | EnumVariant | StructMember | UnionMember |
  ObjectDeclaration | ModulePort | InterfacePort |
  FunctionTemplate | FunctionPort | LocalDeclaration | LoopBinding |
  PackageDeclaration | ModuleDeclaration | InterfaceDeclaration |
  InstanceDeclaration | ProtoDeclaration

SyntaxConcreteNameNamespaceV1 =
  Term | Type | TypeOrGenericType | TypeOrConstantWidth | Function |
  Member | Package | Instance | Proto

SyntaxNameNamespaceV1 =
  Concrete(SyntaxConcreteNameNamespaceV1) |
  DeferredGenericFormalKind(generic-argument SyntaxOccurrenceKeyV1)

SyntaxTypeOwnerRoleV1 = generated alias of the authoritative
  RawTypeUseOwnerRoleV1 in source-semantic-inputs.md

SyntaxInferredTypeReplayKeyV1 = generated alias of the authoritative
  syntax-keyed inferred-type replay key in source-semantic-inputs.md

SyntaxAnalyzerUnresolvedReasonV1 =
  NotFound | Ambiguous | Invisible | WrongNamespace | PathExhausted |
  ImportCycle | UnsupportedAnalyzerTarget

SyntaxEntityKeyV1
  occurrence: SyntaxOccurrenceKeyV1
  kind: SyntaxEntityKindV1
  member_ordinal: None | Some(u32)

SyntaxTypeUseKeyV1
  occurrence: SyntaxOccurrenceKeyV1 / SyntaxTypeOwnerRoleV1 /
  environment: RawEnvironmentLineageRefV1

SyntaxAnalyzerWitnessKeyV1 =
  NameResolution(name occurrence SyntaxOccurrenceKeyV1) |
  ResolvedType(owner SyntaxEntityKeyV1, RawEnvironmentLineageRefV1) |
  ExtentResolution(extent expression SyntaxOccurrenceKeyV1,
                   RawEnvironmentLineageRefV1) |
  EnumResolution(enum declaration SyntaxOccurrenceKeyV1,
                 RawEnvironmentLineageRefV1) |
  EnumVariantValue(enum declaration SyntaxOccurrenceKeyV1,
                   RawEnvironmentLineageRefV1, variant ordinal u32) |
  GenericEnvironment(generic-use SyntaxOccurrenceKeyV1,
                     optional parent RawEnvironmentLineageRefV1) |
  GenericBinding(generic-use SyntaxOccurrenceKeyV1,
                 optional parent RawEnvironmentLineageRefV1,
                 formal SyntaxEntityKeyV1) |
  ConstantExpr(SyntaxProofLineageKeyV1) |
  FunctionSpecialization(call proof SyntaxProofLineageKeyV1,
                         callee-use SyntaxOccurrenceKeyV1,
                         template SyntaxOccurrenceKeyV1,
                         RawEnvironmentLineageRefV1)
```

`SyntaxProofLineageKeyV1` is the closed execution-owner/expression/environment/
role/context key in
[`typed-constant-evaluation.md`](./typed-constant-evaluation.md). A syntax
entity key is valid only when the adapter independently finds exactly that
entity kind and member ordinal at the occurrence. An analyzer identity is
represented as a declaration `SyntaxEntityKeyV1` followed by canonical
source-path components; pointers, addresses, analyzer table indices, hashes,
display strings, and iteration order are forbidden.

The producer witness value domain is also closed:

```text
SyntaxTypeIdentityWitnessV1 =
  Builtin(closed CeloxSourceV0_20 builtin tag) |
  Declared(SyntaxEntityKeyV1) |
  GenericFormal(SyntaxEntityKeyV1)

SyntaxMagnitudeWitnessV1
  canonical minimal little-endian magnitude-byte range

SyntaxBitPlaneWitnessV1
  declared width: SyntaxMagnitudeWitnessV1
  canonical minimal little-endian magnitude-byte range

SyntaxPositiveTypeClassV1 = Plain | Positive

SyntaxResolvedTypeWitnessV1
  core: SyntaxTypeIdentityWitnessV1
  signedness / Bit-or-Logic domain / SyntaxPositiveTypeClassV1
  canonical modifier / unpacked-extent / packed-extent ranges

SyntaxProposedConstTypeV1 =
  Integral(SyntaxMagnitudeWitnessV1 width, signed, Bit | Logic) |
  PackedAggregate(SyntaxTypeUseKeyV1, SyntaxMagnitudeWitnessV1 width,
                  signed, Bit | Logic, SyntaxPositiveTypeClassV1) |
  FixedArray(SyntaxTypeUseKeyV1, canonical extent-witness range) |
  TypedString | TypeValue(SyntaxTypeUseKeyV1) | Unit

SyntaxTypedValueNodeRefV1
  owner witness: SyntaxAnalyzerWitnessKeyV1 /
  canonical value-node preorder: u64

SyntaxTypedValueNodeV1 =
  Integral(payload SyntaxBitPlaneWitnessV1, mask SyntaxBitPlaneWitnessV1,
           width SyntaxMagnitudeWitnessV1, signed, Bit | Logic,
           SyntaxPositiveTypeClassV1) |
  String(canonical decoded-byte range) |
  Array(canonical interval-row range) |
  Struct(canonical declaration-order member-row range) |
  TypeValue(SyntaxTypeUseKeyV1) | Unit

SyntaxProposedTypedValueV1 =
  None | Root(SyntaxTypedValueNodeRefV1, canonical value-node range)

SyntaxConstantValueWitnessRefV1
  proof: SyntaxProofLineageKeyV1
  role: FinalStaticOutput
```

Resolving a `SyntaxConstantValueWitnessRefV1` means resolving the unique
`ConstantExpr(proof)` row and comparing its complete
`SyntaxTypedValueNodeV1` graph; the reference is never accepted from proof
lineage alone. `None` is therefore not a valid target of this reference. Equal
content from two proof lineages remains two producer witness keys and may
canonicalize to one verified persistent value only after both proofs succeed.

The only producer witness row sum is:

```text
SyntaxAnalyzerWitnessRowV1 =
  NameResolution {
    key, expected SyntaxNameNamespaceV1,
    proposed: Unresolved(SyntaxAnalyzerUnresolvedReasonV1) |
              Resolved(SyntaxEntityKeyV1)
  } |
  ResolvedType { key, proposed SyntaxResolvedTypeWitnessV1 } |
  ExtentResolution {
    key, proposed: Unresolved(SyntaxAnalyzerUnresolvedReasonV1) |
                   Resolved(SyntaxBitPlaneWitnessV1)
  } |
  EnumResolution {
    key, proposed width SyntaxMagnitudeWitnessV1,
    canonical enum-variant-witness-key range
  } |
  EnumVariantValue {
    key, proposed: Unresolved |
                   Known(SyntaxTypedValueNodeRefV1,
                         canonical value-node range)
  } |
  GenericEnvironment {
    key, analyzer entity path,
    canonical generic-binding-key / extent-key / enum-key ranges
  } |
  GenericBinding {
    key, selection: Explicit(argument SyntaxOccurrenceKeyV1) |
                    DeclaredDefault,
    proposed: Type(SyntaxTypeUseKeyV1) |
              Inst(SyntaxEntityKeyV1) |
              Proto(SyntaxEntityKeyV1) |
              Const(SyntaxConstantValueWitnessRefV1)
  } |
  ConstantExpr {
    key, proposed natural/final SyntaxProposedConstTypeV1,
    canonical coercion/dependency ranges,
    proposed SyntaxProposedTypedValueV1,
    optional FunctionSpecialization witness key,
    PureNoCertificate |
      Certificate(root RawRootLabel, proposed trace-row count u64)
  } |
  FunctionSpecialization {
    key, analyzer entity path, canonical input-type range,
    optional return-type witness
  }

SyntaxAnalyzerWitnessPoolKindV1 =
  AnalyzerPathComponent |
  ResolvedTypeModifier |
  ResolvedTypeUnpackedExtent | ResolvedTypePackedExtent |
  ProposedConstTypeExtent |
  EnumVariantWitnessKey |
  GenericBindingWitnessKey | GenericExtentWitnessKey |
  GenericEnumWitnessKey |
  ConstantCoercion | ConstantDependency |
  TypedValueMagnitudeByte | TypedStringByte |
  TypedValueNode | TypedArrayInterval | TypedStructMember |
  FunctionInputType

SyntaxModifierTagV1 = Signed | Tri | Default

SyntaxCoercionTagV1 =
  Identity | IntegralZeroExtend | IntegralSignExtend | IntegralTruncate |
  SignednessReinterpret | PositiveTypeRetag |
  BitToLogic | LogicToBitMaterialize | PackedAggregateRepack |
  ArrayElementwise | StructMemberwise | StringIdentity |
  TypeValueIdentity | UnitIdentity

SyntaxGuardRoleV1 =
  LogicalRight | ConditionalThen | ConditionalElse |
  ArrayDefault(tail/member context ordinal u32) |
  StructDefault(member declaration ordinal u32) |
  DecisionPattern(arm ordinal u32, pattern ordinal u32) |
  DecisionArmResult(arm ordinal u32) | DecisionDefault |
  InsidePattern(pattern ordinal u32) | FunctionExecution

`SyntaxGuardRoleV1` is the syntax-keyed encoding of the complete
`RawGuardRoleV1` in `typed-constant-evaluation.md`. The mapped private row must
match variant and every ordinal exactly. It covers short-circuit operands,
conditional arms, array/struct defaults, ordered decision/pattern/result
edges, inside/outside patterns, and function-program execution; no untyped
“guarded” tag or omitted guarded form is accepted.

SyntaxAnalyzerWitnessPoolEntryV1 =
  AnalyzerPathComponent(SyntaxOccurrenceKeyV1) |
  ResolvedTypeModifier(SyntaxOccurrenceKeyV1, SyntaxModifierTagV1) |
  ResolvedTypeUnpackedExtent(SyntaxAnalyzerWitnessKeyV1) |
  ResolvedTypePackedExtent(SyntaxAnalyzerWitnessKeyV1) |
  ProposedConstTypeExtent(SyntaxAnalyzerWitnessKeyV1) |
  EnumVariantWitnessKey(SyntaxAnalyzerWitnessKeyV1) |
  GenericBindingWitnessKey(SyntaxAnalyzerWitnessKeyV1) |
  GenericExtentWitnessKey(SyntaxAnalyzerWitnessKeyV1) |
  GenericEnumWitnessKey(SyntaxAnalyzerWitnessKeyV1) |
  ConstantCoercion {
    owner SyntaxProofLineageKeyV1 /
    source and target SyntaxProofRoleV1 / SyntaxCoercionTagV1 /
    source and target SyntaxProposedConstTypeV1
  } |
  ConstantDependency {
    owner SyntaxProofLineageKeyV1 / target SyntaxProofLineageKeyV1 /
    TypePrerequisite | EagerValuePrerequisite |
    GuardedValueUse(SyntaxGuardRoleV1)
  } |
  TypedValueMagnitudeByte(u8) | TypedStringByte(u8) |
  TypedValueNode {
    key SyntaxTypedValueNodeRefV1 / value SyntaxTypedValueNodeV1
  } |
  TypedArrayInterval {
    owner SyntaxTypedValueNodeRefV1 / source ordinal u32 /
    start SyntaxMagnitudeWitnessV1 / length SyntaxMagnitudeWitnessV1 /
    value SyntaxTypedValueNodeRefV1
  } |
  TypedStructMember {
    owner SyntaxTypedValueNodeRefV1 / declaration ordinal u32 /
    member SyntaxEntityKeyV1 / value SyntaxTypedValueNodeRefV1
  } |
  FunctionInputType {
    specialization SyntaxAnalyzerWitnessKeyV1 / formal ordinal u32 /
    proposed SyntaxResolvedTypeWitnessV1
  }

RawSyntaxAnalyzerWitnessV1<'a>
  rows: &'a [SyntaxAnalyzerWitnessRowV1]
  one exact borrowed slice for every SyntaxAnalyzerWitnessPoolKindV1
```

Every range above names exactly the one pool kind implied by its field; there
is no generic range pool. Rows are strictly ordered by the total order of
`SyntaxAnalyzerWitnessKeyV1` and keys are unique. Canonical owner traversal is
witness-row order, fields in declared order, then the entries of a ranged field
in source ordinal; an encountered coercion, function-input-type, typed-value
node, or interval entry owns its nested ranges at that point before traversal
continues. Every pool is exactly partitioned by this one iterative traversal.
Witness rows thereby own analyzer paths, type extents, key lists,
coercions/dependencies, function inputs, and their root value-node ranges.
Typed-value nodes are ordered by `(owner witness key, value-node preorder)` and
own their magnitude/string/array-interval/struct-member ranges;
interval/member entries may reference only a node in the same owner witness's
declared value-node range. `TypedValueMagnitudeByte` is byte storage;
`SyntaxMagnitudeWitnessV1` and `SyntaxBitPlaneWitnessV1` additionally require
minimal unsigned encoding. The other pools have one fixed closed entry type.
Cross-witness references use
keys, never witness row ordinals. The verifier first proves syntax lineage and
environment lineage, then maps a row to the corresponding private raw row,
then derives the semantic answer, and only then compares the proposed payload.
A malformed/duplicate syntax key, invalid range, wrong key kind, wrong pool
ownership, noncanonical order, or failure to map a structurally present key is
source-owned topology and is
`SourceAggregateErrorV1::SourceLocal(SOURCE.AGGREGATE_WITNESS)`. After a
type/generic/constant row maps successfully, however, missing/extra expected
rows and semantic payload/content disagreement are owned by the typed verifier
and remain the exact nested `TypedConstantErrorV1` with respectively
`CONST.AGGREGATE_OUTPUT`, `CONST.AGGREGATE_ORPHAN`, or
`CONST.AGGREGATE_WITNESS`. The same input can never select between those routes.

The mapped-mirror relation is one-to-one: `NameResolution` creates the private
`RawAnalyzerResolutionWitnessRow`; `ResolvedType`, `ExtentResolution`,
`EnumResolution`, `EnumVariantValue`, `GenericEnvironment`, and
`GenericBinding` create their same-named private `Raw*WitnessRow`; and
`ConstantExpr` plus `FunctionSpecialization` create respectively the private
`RawConstantExprWitnessRow` and `RawSpecializationWitnessV1` view. Private
proposed coercion/dependency/value rows are flattened from only the ranges
owned by those mapped rows. No other private witness row has a producer-facing
constructor.

### Closed source proposal

`RawSourceProposalV1` is a closed producer proposal for the source rows in
`decision-region-architecture.md`. It is not a generic `RawMirror<T>` and does
not mechanically replace every checked ID by a number. The reference families
have different meanings:

```text
RawForFoldTemplateProposalRefV1 is the table-local `proposal_ref` generated
  for SourceProposalTableKindV1::ForFoldTemplate below

RawFoldRegionProposalRefV1 =
  { template: RawForFoldTemplateProposalRefV1, local_ordinal: u32 }
RawFoldPointProposalRefV1 =
  { template: RawForFoldTemplateProposalRefV1, local_ordinal: u32 }
RawFoldEdgeProposalRefV1 =
  { template: RawForFoldTemplateProposalRefV1, local_ordinal: u32 }
RawFoldActionProposalRefV1 =
  { template: RawForFoldTemplateProposalRefV1, local_ordinal: u32 }
RawFoldValueOccurrenceProposalRefV1 =
  { template: RawForFoldTemplateProposalRefV1, local_ordinal: u32 }
RawFoldDynamicAddressPlanProposalRefV1 =
  { template: RawForFoldTemplateProposalRefV1, local_ordinal: u32 }

RawAnyFoldProposalRefV1 =
  Region(RawFoldRegionProposalRefV1) |
  Point(RawFoldPointProposalRefV1) |
  Edge(RawFoldEdgeProposalRefV1) |
  Action(RawFoldActionProposalRefV1) |
  ValueOccurrence(RawFoldValueOccurrenceProposalRefV1) |
  DynamicAddressPlan(RawFoldDynamicAddressPlanProposalRefV1)

SyntaxRuntimeLocalScopeV1 = generated alias of the authoritative syntax-keyed
  local-scope enum in source-semantic-inputs.md

SyntaxSemanticObjectNamespaceV1 = ModuleSource | RuntimeProgram

SyntaxSemanticObjectKeyV1
  namespace: SyntaxSemanticObjectNamespaceV1 /
  declaration execution_lineage:
    SyntaxRuntimeSourceExecutionLineageWitnessRefV1 /
  local_scope: SyntaxRuntimeLocalScopeV1 /
  owner SyntaxOccurrenceKeyV1 / RawEnvironmentLineageRefV1 /
  Declaration | Binding | ForFoldCounter | ForFoldState(member ordinal) |
  ClosedSynthetic(SyntaxDerivedSemanticObjectRoleV1, member ordinal)

SyntaxDerivedSemanticObjectRoleV1 =
  CaptureEnableState | ObserverCaptureState | RuntimeEventState |
  DynamicOverlayState | ForFoldEffectState | PinnedSyntheticStorage

SyntaxInputReadRoleV1 =
  Expression | AssignmentOldValue | AssignmentAddress |
  RootValue | RootObservedOld | ObserverTrigger | RuntimeArgument |
  DynamicIndex | DynamicGuard | ForFoldOuter | ForFoldBody |
  EnvironmentRead | StaticCompositeRead

SyntaxInputNamespaceV1 = ModuleSource | RuntimeProgram

SyntaxInputAccessKeyV1
  namespace: SyntaxInputNamespaceV1 /
  read execution_lineage: SyntaxRuntimeSourceExecutionLineageWitnessRefV1 /
  local_scope: SyntaxRuntimeLocalScopeV1 /
  read SyntaxOccurrenceKeyV1 / RawEnvironmentLineageRefV1 /
  SyntaxSemanticObjectKeyV1 / SyntaxInputReadRoleV1 / source ordinal

SyntaxCanonicalInputKeyV1
  first canonical use: SyntaxInputAccessKeyV1

SyntaxPreparedTargetRoleV1 =
  Assignment | RuntimeCallOutput | RuntimeCallInout |
  RootStore | ForFoldState | ForFoldWriteback

SyntaxPreparedTargetNamespaceV1 = ModuleSource | SourceForFold | RuntimeProgram

SyntaxPreparedTargetKeyV1
  namespace: SyntaxPreparedTargetNamespaceV1 /
  execution_lineage: SyntaxRuntimeSourceExecutionLineageWitnessRefV1 /
  local_scope: SyntaxRuntimeLocalScopeV1 /
  target SyntaxOccurrenceKeyV1 / RawEnvironmentLineageRefV1 /
  SyntaxPreparedTargetRoleV1 /
  source ordinal: u32

SyntaxSemanticSlotKeyV1
  execution_lineage: SyntaxRuntimeSourceExecutionLineageWitnessRefV1
  local_scope: SyntaxRuntimeLocalScopeV1
  owner SyntaxOccurrenceKeyV1 / RawEnvironmentLineageRefV1
  role: HirOperand | HirResult | RootOperand | ActionOperand | ActionResult |
        GateCondition | GateResultOperand | GateResult |
        DecisionSelector | DecisionPatternOperand |
        DecisionPatternPredicate | DecisionArmPredicate |
        DecisionResultOperand | DecisionStepResult |
        ObserverTrigger | RuntimePredicate | RuntimeArgument |
        RuntimeCallSetup | RuntimeCallInvokeOperand |
        RuntimeCallInvokeResult | RuntimeCallReturn |
        RuntimeCallWriteback |
        DynamicIndex | DynamicGuard |
        ForRangeInput | ForInitialState | ForStep |
        FoldActionOperand | FoldActionResult | FoldRecurrence |
        FoldHeaderCondition | FoldContinueCondition |
        FoldTransitionOutcome | PinnedSyntheticResult
  primary_ordinal: u32 / secondary_ordinal: u32

SyntaxRuntimeCallInstanceKeyV1
  function execution_lineage:
    SyntaxRuntimeSourceExecutionLineageWitnessRefV1
    (row must be RuntimeCall)
  caller invoke slot: SyntaxSemanticSlotKeyV1

SyntaxRuntimeProgramOccurrenceKeyV1 =
  FormalObject(SyntaxSemanticObjectKeyV1, flattened formal ordinal u32) |
  LocalObject(SyntaxSemanticObjectKeyV1, local ordinal u32) |
  ValueSlot(SyntaxSemanticSlotKeyV1, program value ordinal u32) |
  ControlRegion(SyntaxSemanticSlotKeyV1, program region ordinal u32) |
  ControlPoint(SyntaxSemanticSlotKeyV1, program point ordinal u32) |
  ControlEdge(SyntaxSemanticSlotKeyV1, program edge ordinal u32) |
  Action(SyntaxSemanticSlotKeyV1, program action ordinal u32) |
  DynamicTarget(SyntaxPreparedTargetKeyV1, program target ordinal u32) |
  NestedCall(SyntaxRuntimeCallInstanceKeyV1, program call-site ordinal u32) |
  RetainedForFold(SyntaxOccurrenceKeyV1, program template ordinal u32)

SyntaxDerivedTypedValueRoleV1 =
  DeclarationMaterialization | AssignmentMaterialization |
  FunctionActualMaterialization | FunctionReturnMaterialization |
  AggregateElementMaterialization | AggregateMemberMaterialization |
  ExplicitBitCastMaterialization | UnpackedStorageDefault |
  RecursiveAggregateDefault | StaticCompositeDefault | PackedInvalidLaneX |
  EnumFinalization | DecisionPatternMask | ControlSentinel |
  ForFoldDefaultStep |
  ForFoldCounterMaterialization | ForFoldStateMaterialization |
  PinnedSynthetic

SyntaxTypedValueOriginKeyV1 =
  ConstantProof(SyntaxConstantValueWitnessRefV1) |
  DerivedSlot(SyntaxSemanticSlotKeyV1, SyntaxDerivedTypedValueRoleV1)

SyntaxPhaseCoercionRoleV1 =
  NodeOperand | InputIndex | Assignment | ExplicitCast |
  FunctionActual | FunctionReturn | AggregateElement | AggregateMember |
  ForFoldBound | ForFoldStep | ForFoldStateInitial | ForFoldStateUpdate |
  DecisionSelector | DecisionPattern

SyntaxPhaseCoercionOriginKeyV1
  slot: SyntaxSemanticSlotKeyV1 / SyntaxPhaseCoercionRoleV1 /
  coercion ordinal: u32

SyntaxStaticCompositeProjectionKeyV1
  input: SyntaxCanonicalInputKeyV1

SyntaxTriIntentKeyV1
  object: SyntaxSemanticObjectKeyV1
  tri modifier: SyntaxOccurrenceKeyV1
  declaration role: Port | Variable | Binding

RawSourceReferenceV1 =
  SourceRow(RawAnySourceProposalRefV1) |
  FoldRow(RawAnyFoldProposalRefV1) |
  SemanticObject(SyntaxSemanticObjectKeyV1) |
  InputAccess(SyntaxCanonicalInputKeyV1) |
  PreparedTarget(SyntaxPreparedTargetKeyV1) |
  NormalizedTypeUse(SyntaxTypeUseKeyV1) |
  SemanticSlot(SyntaxSemanticSlotKeyV1) |
  TypedValueOrigin(SyntaxTypedValueOriginKeyV1) |
  PhaseCoercionOrigin(SyntaxPhaseCoercionOriginKeyV1) |
  StaticCompositeProjection(SyntaxStaticCompositeProjectionKeyV1) |
  TypeMember(SyntaxEntityKeyV1) |
  RuntimeCallInstance(SyntaxRuntimeCallInstanceKeyV1) |
  TriIntent(SyntaxTriIntentKeyV1)
```

Every object key carries the root/runtime-call lineage in which that
declaration/storage instance exists and its independent `Body | ForFold`
local scope; every input-use, prepared-target, and semantic-slot key carries
the same two axes for the read/evaluation. A module/catalog object may
therefore be read by several call lineages without duplicating object identity,
while a function local at the same raw declaration syntax receives a distinct
object key for each runtime-call lineage. Entering a retained fold changes only
`local_scope` to that template syntax key; an iteration/backedge never creates
another lineage or local-scope identity. Typed-value and coercion derived
origins inherit both axes from the slot; canonical input, static-composite, and
Tri keys inherit them from their input/object. No consumer may reconstruct
either axis from owner syntax alone or silently substitute a parent/body scope.

After the execution-lineage table is independently verified, each key checks
that a `Root` lineage owns the stated root/catalog syntax or a `RuntimeCall`
lineage's verified shared program contains the stated function-body
object/read/slot. Independently, `Body` must name the program body and
`ForFold(template)` must name an exact retained template lexically/program-
relatively contained there. Its generic environment and complete type
specialization must equal the runtime-call lineage row. Thus two expansions of
one raw function body cannot collide, a retained loop cannot manufacture
iteration lineages, and a producer cannot separate or merge calls with an
analyzer specialization ID.

The mapping is total and fixed. A `PhaseNodeId<SourcePhase>` maps only to
`SourceRow(Node)`. Source-owned unit/control/root/action/gate/decision/
observer/runtime/dynamic/template/domain/binding/effect IDs map only to their
same-kind `SourceRow`; template-scoped `SourceFold*Id` maps only to the matching
`FoldRow`. A `SyntaxSemanticObjectKeyV1` whose namespace is `ModuleSource` is
bijective with `SourceSemanticObjectId`. A `RuntimeProgram` key is instead
bijective with the formal/local/call-scoped object row in the verified runtime
program occurrence named by its execution lineage and local scope; it cannot
enter a `SourceSemanticObjectId` field. An exact captured outer object crosses
that boundary only through an explicit `ModuleSource` key.

Input identity is deliberately different. Every `SyntaxInputAccessKeyV1` maps
bijectively to its exact expected input-read use in the namespace fixed by its
execution lineage/local scope. Independently derived complete
object/access/geometry/type identity then interns that use. For
`ModuleSource`, this yields canonical `SourceInputId`; for `RuntimeProgram`, it
yields the separate program/call-scoped input namespace and cannot enter a
`SourceInputId` field. Several use keys may resolve to one input in the same
namespace. The input row retains the first expected use in canonical HIR/
program traversal, and `SyntaxCanonicalInputKeyV1` is valid exactly when its
named use is that retained first use. Those canonical keys—not arbitrary read
occurrences—are bijective with their namespace's input rows. Equal access
shape, cross-namespace equality, or a producer-selected earlier use cannot
choose the inverse.

For an input read, the verifier also derives that expected use's
`SyntaxSemanticSlotKeyV1`; the input-access key and semantic-slot key must name
the same `ExpectedSourceUseId`. They are two projections of one expected row,
not two producer-selectable aliases.

Every `SyntaxPreparedTargetKeyV1` maps bijectively to one target handle derived
from that exact target syntax in its execution lineage/local scope.
`ModuleSource` yields `ExpectedSourceTargetHandleId`; `SourceForFold` yields a
template-scoped `SourceFoldPreparedTargetHandleId`; `RuntimeProgram` yields
`RuntimeFunctionProgramPreparedTargetHandleIdV1`. Outer, fold, and program
action rows require respectively those exact key namespaces. Runtime
output/inout setup and writeback must reuse this one key; selector equality,
cross-namespace equality, or a second evaluation cannot create an alias
handle. A dynamic-plan row names the same key as its owning
`DynamicTarget`; static and dynamic targets therefore share the one
once-evaluation rule without sharing an ID namespace.

`ExpectedSourceObjectId` maps only to a `ModuleSource`
`SyntaxSemanticObjectKeyV1`; runtime-program object keys map to their exact
program-relative expected object row under the call lineage/local scope.
`ExpectedSourceUseId`, `ExpectedSourceResultId`,
`ExpectedSourceFoldUseId`, `ExpectedSourceFoldResultId`, and
`SourceCanonicalProducerId`/`SourceFoldCanonicalProducerId` map to the exact
`SyntaxSemanticSlotKeyV1` from which the verifier derives them.

`SyntaxRuntimeCallInstanceKeyV1` is bijective with
`ExpectedSourceRuntimeCallInstanceV1`. Its lineage row must be `RuntimeCall`,
its caller invoke slot's execution lineage must equal that row's parent, and
the slot owner must be the row's exact call occurrence; the invoke slot's
`local_scope` records whether that call lies in the body or one retained fold.
The expected instance then selects the already verifier-derived shared runtime
program and complete call graph. `InvokeRuntimeFunction` action proposals
reference only this proof key; they cannot name a proposal row, private
call-instance ID, or verified program/specialization ID.

Typed-value content identity is also interned. The verifier derives a complete
`PhaseTypedValueOriginRow<SourcePhase>` for every
`SyntaxTypedValueOriginKeyV1`: `ConstantProof` maps to the exact completed
constant proof and content; `DerivedSlot` maps to the exact closed derived
value rule at that semantic slot and requires no analyzer witness. Origin keys
and origin rows are bijective, but multiple origin rows with equal complete
content may name one canonical `VerifiedSourceTypedValueId`. Proposal rows
therefore reference an origin key, resolve it through its origin row, and only
then obtain the canonical value ID; the value-ID namespace itself is never
claimed to be syntax-bijective.

The same provenance/content separation applies to coercions. Every
`SyntaxPhaseCoercionOriginKeyV1` maps bijectively to a phase-coercion origin row
whose independently derived complete coercion content resolves to the
canonical `VerifiedPhaseCoercionId<SourcePhase>`; equal coercion content may
share that ID. A `SyntaxStaticCompositeProjectionKeyV1` exists exactly for a
canonical input whose verified resolution class is `StaticComposite` and is
bijective with its compact projection recipe. A normalized source type member
ID maps only from the exact `StructMember`/`UnionMember`
`SyntaxEntityKeyV1`; field spelling or flat offset is not identity.

Every `SyntaxTypeUseKeyV1` maps to one verified type-use instance in its exact
environment and then to the canonical normalized type named by source rows.
Distinct aliases/type-use occurrences may resolve to one normalized type, so
the normalized-type ID is content-canonical rather than syntax-bijective.

`VerifiedSourceTriIntentId` maps bijectively to
`SyntaxTriIntentKeyV1`. Using a `RawAnySourceProposalRefV1` variant for any of
those proof-derived
names is a raw-tag error. The verifier constructs every checked ID only after
the corresponding complete expected relation and the required bijection or
many-to-one canonicalization above have succeeded.

The top-level row family and every table-local reference are generated from the
following normative input. `ref(T)` is encoded as one raw `u64`, but its schema
type is statically the named table; it has no encoded table-kind tag.
`use_ref(T)` and `def_ref(T)` are distinct schema types over the same table and
add the stated checked row-variant requirement. `range(P)` is exactly
`{ start: u64, len: u64 }` with the pool kind fixed in its schema type.
Every ID-free scalar field below names its authoritative closed V1 type
directly; the generator has no generic checked-field-copy operation.
Generation fails if a field contains an ID, range, pointer, native-width
integer, open tag, or unlisted subfield. There is no
`copy_rest`, default field rule, wildcard arm, or extension map.

```text
SourceProposalTableKindV1 =
  SemanticObjectWitness | InputWitness | Node |
  ControlUnit | PredicateRegion | ControlPoint | ControlEdge | Root | Action |
  ValueOccurrence | Gate | GateResultMerge | Decision | GatedMux |
  DecisionResultMerge | Observer | ObserverOccurrence | RuntimeEventSite |
  RuntimeCallInstanceWitness | DynamicAddressPlan | ForFoldTemplate |
  WriteDomain | Binding | EffectStream | PinnedSyntheticOrigin | GatedKey

SourceProposalTableKindV1::ALL = [the 26 variants above in written order]
SourceProposalTableKindV1::COUNT = 26

proposal_ref SemanticObjectWitness as RawSemanticObjectProposalRefV1
proposal_ref InputWitness as RawInputProposalRefV1
proposal_ref Node as RawPhaseNodeProposalRefV1
proposal_ref ControlUnit as RawControlUnitProposalRefV1
proposal_ref PredicateRegion as RawPredicateRegionProposalRefV1
proposal_ref ControlPoint as RawControlPointProposalRefV1
proposal_ref ControlEdge as RawControlEdgeProposalRefV1
proposal_ref Root as RawRootProposalRefV1
proposal_ref Action as RawActionProposalRefV1
proposal_ref ValueOccurrence as RawValueOccurrenceProposalRefV1
proposal_ref Gate as RawGateProposalRefV1
proposal_ref GateResultMerge as RawGateResultMergeProposalRefV1
proposal_ref Decision as RawDecisionProposalRefV1
proposal_ref GatedMux as RawGatedMuxProposalRefV1
proposal_ref DecisionResultMerge as RawDecisionResultMergeProposalRefV1
proposal_ref Observer as RawObserverProposalRefV1
proposal_ref ObserverOccurrence as RawObserverOccurrenceProposalRefV1
proposal_ref RuntimeEventSite as RawRuntimeEventSiteProposalRefV1
proposal_ref RuntimeCallInstanceWitness as RawRuntimeCallInstanceWitnessProposalRefV1
proposal_ref DynamicAddressPlan as RawDynamicAddressPlanProposalRefV1
proposal_ref ForFoldTemplate as RawForFoldTemplateProposalRefV1
proposal_ref WriteDomain as RawWriteDomainProposalRefV1
proposal_ref Binding as RawBindingProposalRefV1
proposal_ref EffectStream as RawEffectStreamProposalRefV1
proposal_ref PinnedSyntheticOrigin as RawPinnedSyntheticOriginProposalRefV1
proposal_ref GatedKey as RawGatedKeyProposalRefV1

RawAnySourceProposalRefV1 =
  SemanticObjectWitness(RawSemanticObjectProposalRefV1) |
  InputWitness(RawInputProposalRefV1) |
  Node(RawPhaseNodeProposalRefV1) |
  ControlUnit(RawControlUnitProposalRefV1) |
  PredicateRegion(RawPredicateRegionProposalRefV1) |
  ControlPoint(RawControlPointProposalRefV1) |
  ControlEdge(RawControlEdgeProposalRefV1) |
  Root(RawRootProposalRefV1) |
  Action(RawActionProposalRefV1) |
  ValueOccurrence(RawValueOccurrenceProposalRefV1) |
  Gate(RawGateProposalRefV1) |
  GateResultMerge(RawGateResultMergeProposalRefV1) |
  Decision(RawDecisionProposalRefV1) |
  GatedMux(RawGatedMuxProposalRefV1) |
  DecisionResultMerge(RawDecisionResultMergeProposalRefV1) |
  Observer(RawObserverProposalRefV1) |
  ObserverOccurrence(RawObserverOccurrenceProposalRefV1) |
  RuntimeEventSite(RawRuntimeEventSiteProposalRefV1) |
  DynamicAddressPlan(RawDynamicAddressPlanProposalRefV1) |
  ForFoldTemplate(RawForFoldTemplateProposalRefV1) |
  WriteDomain(RawWriteDomainProposalRefV1) |
  Binding(RawBindingProposalRefV1) |
  EffectStream(RawEffectStreamProposalRefV1) |
  PinnedSyntheticOrigin(RawPinnedSyntheticOriginProposalRefV1) |
  GatedKey(RawGatedKeyProposalRefV1)

RuntimeCallInstanceWitness has no RawAnySourceProposalRefV1 variant; it is a
  proof-keyed comparison row and can be reached only by its
  SyntaxRuntimeCallInstanceKeyV1, never as a checked source-row replacement

RawValueOccurrenceUseProposalRefV1 =
  use_ref(ValueOccurrence, RawValueOccurrenceProposalV1.site == Use)
RawValueOccurrenceDefProposalRefV1 =
  def_ref(ValueOccurrence, RawValueOccurrenceProposalV1.site == Definition)
RawFoldValueOccurrenceUseProposalRefV1 =
  use_ref(FoldValueOccurrence, flow == Use)
RawFoldValueOccurrenceDefProposalRefV1 =
  def_ref(FoldValueOccurrence, flow == Definition)

RawControlSiteProposalV1 =
  { point: RawControlPointProposalRefV1, slot: u32 }
RawControlUseSiteProposalV1 =
  Slot(RawControlSiteProposalV1) | Edge(RawControlEdgeProposalRefV1)
RawFoldControlSiteProposalV1 =
  Slot { point: RawFoldPointProposalRefV1, slot: u32 } |
  Edge(RawFoldEdgeProposalRefV1)

RawPhaseValueUseProposalV1 =
  { value: RawPhaseNodeProposalRefV1,
    coercion: SyntaxPhaseCoercionOriginKeyV1 }
RawPhaseLoopBoundProposalV1 =
  Constant { value: SyntaxTypedValueOriginKeyV1,
             coercion: SyntaxPhaseCoercionOriginKeyV1 } |
  Runtime(RawPhaseValueUseProposalV1)

RawBitAccessProposalV1
  lsb: u64 / width: nonzero u64
RawPartSelectGeometryProposalV1 =
  Whole |
  Bit { dimension_ordinal: u32 } |
  Colon { dimension_ordinal: u32, low: u64, width: nonzero u64 } |
  PlusColon { dimension_ordinal: u32, width: nonzero u64 } |
  MinusColon { dimension_ordinal: u32, width: nonzero u64 } |
  Step { dimension_ordinal: u32, width: nonzero u64 }
RawKnownBoundsProposalV1 =
  Unknown | Known { low: u64, high_exclusive: u64, in_bounds: bool }

RawSourceCoercionProposalV1
  source_width: u64 / source_signed: bool /
  target_width: u64 / target_signed: bool /
  context: SelfDetermined | AssignmentValue | ExplicitCast |
           CommonExpressionOperand |
           ForFoldCounterOperand(CeloxSourceV0_20SignedI32) /
  width_action: Identity | Truncate | ZeroExtend | SignExtend

RawSourceComparisonProposalV1
  operator: SourceComparisonOperatorV1 /
  signed: bool /
  coercion: RawSourceCoercionProposalV1

RawSourceConditionSemanticsProposalV1 =
  IfReduction {
    semantics: CeloxSourceV0_20,
    source_domain: Bit | Logic,
    width: u64,
    rule: SourceIfReductionRuleV1
  } |
  TernaryBitMerge {
    semantics: CeloxSourceV0_20,
    condition_domain: Bit | Logic,
    condition_width: u64,
    then_coercion: RawSourceCoercionProposalV1,
    else_coercion: RawSourceCoercionProposalV1,
    result_coercion: RawSourceCoercionProposalV1,
    rule: SourceTernaryBitMergeRuleV1
  }

RawSourceCaseSemanticsProposalV1
  semantics: CeloxSourceV0_20 /
  operator: Case | CaseZ | Switch /
  selector_domain: Bit | Logic /
  selector_width: u64 / selector_signed: bool /
  first_match_rule: SourceCaseFirstMatchRuleV1 /
  default_rule: SourceCaseDefaultRuleV1 /
  xz_rule: SourceCaseXzRuleV1

RawForFoldTransitionSemanticsProposalV1
  semantics: CeloxSourceV0_20 /
  counter_rule: CeloxSourceV0_20SignedI32 /
  form: SingleForward | SingleReverse | ForwardExclusive |
        ForwardInclusive | ReverseExclusive | ReverseInclusive /
  empty_rule: Never | SignedGe | SignedGt | SignedLe | SignedLt /
  initial_rule: A | B | Sub32BOneAfterNonempty /
  header_rule: OneShotFirst | SignedLt | SignedLe | SignedGe /
  counter_step_rule: SingleCompleteWithoutUpdate |
    Update { op: SourceForStepAssignmentOpV1,
             operand_coercion: RawSourceCoercionProposalV1,
             result_coercion: RawSourceCoercionProposalV1 } /
  post_update_rule: AdvanceOrNormalRangeExit /
  continue_rule: SourceForContinueRuleV1

RawSemanticObjectProposalV1 {
  key: SyntaxSemanticObjectKeyV1
  object_width: u64
  declared_signed: bool
  declared_positive_type: Plain | Positive
  object_domain: Bit | Logic
  resolution: Ordinary | TriIntent(SyntaxTriIntentKeyV1)
  default_role: None | ExplicitClock | ImplicitClock |
                ExplicitReset | ImplicitReset
  dimensions: range(SemanticObjectDimension)
  strides: range(SemanticObjectStride)
}

RawInputProposalV1 {
  key: SyntaxCanonicalInputKeyV1
  object: SyntaxSemanticObjectKeyV1
  resolution: Memory | Environment |
              StaticComposite(SyntaxStaticCompositeProjectionKeyV1) |
              DynamicOverlay(RawDynamicAddressPlanProposalRefV1)
  selectors: range(InputAccessSelector)
  runtime_index_roles: range(InputRuntimeIndexRole)
  stride_prefix: range(InputStridePrefix)
  selected_width: u64
  result_signed: bool
  result_positive_type: Plain | Positive
  result_static_domain: Bit | Logic
  result_value_class: Evaluation | MaterializedStorage
  result_mask_class: AlwaysZero | MayCarryXZ
}

RawPhaseNodeProposalV1 =
  Input { input: SyntaxCanonicalInputKeyV1,
          runtime_indices: range(NodeInputIndex) } |
  Constant { value: SyntaxTypedValueOriginKeyV1 } |
  Coerce { operands: range(NodeOperand), exact_len: 1 } |
  Unary { op: PhaseUnaryOpV1,
          operands: range(NodeOperand), exact_len: 1 } |
  Binary { op: PhaseBinaryOpV1,
           operands: range(NodeOperand), exact_len: 2 } |
  Mux { operands: range(NodeOperand), exact_len: 3 } |
  ForFold { template: RawForFoldTemplateProposalRefV1,
            start: RawPhaseLoopBoundProposalV1,
            end: RawPhaseLoopBoundProposalV1,
            step: SyntaxTypedValueOriginKeyV1,
            states: range(NodeForStateRow),
            effects: range(NodeForEffectRow),
            result_state_ordinal: u32,
            continue_condition: RawPhaseValueUseProposalV1 } |
  Concat { parts: range(NodeConcatPartRow) } |
  Slice { value: RawPhaseNodeProposalRefV1,
          access: RawBitAccessProposalV1 }

RawControlUnitProposalV1 {
  root_region: RawPredicateRegionProposalRefV1
  entry: RawControlPointProposalRefV1
  exit: RawControlPointProposalRefV1
  roots: range(ControlUnitRoot)
}

RawPredicateRegionProposalV1 {
  unit: RawControlUnitProposalRefV1
  parent: optional RawPredicateRegionProposalRefV1
  entry: RawControlPointProposalRefV1
  exit: RawControlPointProposalRefV1
  owner: Root |
         GateTrue(RawGateProposalRefV1) |
         GateFalse(RawGateProposalRefV1) |
         DecisionArm(RawDecisionProposalRefV1, arm_ordinal: u32) |
         DecisionDefault(RawDecisionProposalRefV1)
  children: range(PredicateRegionChild)
}

RawControlPointProposalV1 {
  unit: RawControlUnitProposalRefV1
  region: RawPredicateRegionProposalRefV1
  kind: SourceControlPointKindV1
  ordered_actions: range(ControlPointAction)
  predecessor_edges: range(ControlPointPredecessorEdge)
  successor_edges: range(ControlPointSuccessorEdge)
}

RawControlEdgeProposalV1 {
  unit: RawControlUnitProposalRefV1
  predecessor: RawControlPointProposalRefV1
  successor: RawControlPointProposalRefV1
  kind: SourceControlEdgeKindV1
}

RawRootProposalV1 {
  unit: RawControlUnitProposalRefV1
  origin: SyntaxOccurrenceKeyV1
  environment: RawEnvironmentLineageRefV1
  semantic_specification: SourceRootSemanticSpecificationV1
  ordered_operands: range(RootOperand)
  disposition: Scheduled(RawActionProposalRefV1) | MetadataOnly
}

RawActionTargetProposalV1 =
  StaticTarget(SyntaxPreparedTargetKeyV1) |
  DynamicTarget { target: SyntaxPreparedTargetKeyV1,
                  plan: RawDynamicAddressPlanProposalRefV1 }
RawActionInputResolutionProposalV1 =
  Memory | Environment | StaticComposite |
  DynamicOverlay(RawDynamicAddressPlanProposalRefV1)
RawActionProposalKindV1 =
  ReadInput { result_slot: u32, input: SyntaxCanonicalInputKeyV1,
              resolution: RawActionInputResolutionProposalV1 } |
  CaptureValue { result_slot: u32, source_operand: u32 } |
  BindEnvironment { result_slot: u32, source_operand: u32,
                    binding: RawBindingProposalRefV1 } |
  EvaluatePinned { result_slot: u32,
                   ordered_operand_slots: range(ActionRuntimeArgumentOrdinal) } |
  StoreRoot { root: RawRootProposalRefV1,
              target: RawActionTargetProposalV1,
              value_operand: u32, observed_old_operand: optional u32,
              capture_enable_sites: range(ActionCaptureEnableSite),
              triggers: range(ActionTriggerRoot) } |
  RuntimeEvent { root: RawRootProposalRefV1,
                 observer: RawObserverProposalRefV1,
                 site: RawRuntimeEventSiteProposalRefV1,
                 predicate_operand: u32,
                 argument_operands: range(ActionRuntimeArgumentOrdinal),
                 enabled_value_operand: optional u32,
                 consume_enabled: bool, termination: bool } |
  InvokeRuntimeFunction {
    root: optional RawRootProposalRefV1,
    instance: SyntaxRuntimeCallInstanceKeyV1,
    operand_roles: range(ActionRuntimeCallOperandRole),
    result_roles: range(ActionRuntimeCallResultRole)
  } |
  ForFold { root: optional RawRootProposalRefV1, result_slot: u32,
            template: RawForFoldTemplateProposalRefV1 }

RawActionProposalV1 {
  unit: RawControlUnitProposalRefV1
  owner_point: RawControlPointProposalRefV1
  action_index: u32
  ordered_operands: range(ActionOperand)
  results: range(ActionResult)
  read_domains: range(ActionAccessReadDomain)
  write_domains: range(ActionAccessWriteDomain)
  read_bindings: range(ActionAccessReadBinding)
  write_bindings: range(ActionAccessWriteBinding)
  effect_publications: range(ActionAccessEffectPublication)
  kind: RawActionProposalKindV1
}

RawSourceUseOwnerProposalV1 =
  ValueOperand(RawValueOccurrenceProposalRefV1, operand_ordinal: u32) |
  ActionOperand(RawActionProposalRefV1, operand_ordinal: u32) |
  GateCondition(RawGateProposalRefV1) |
  GateResultOperand(RawGateResultMergeProposalRefV1,
                    Condition | Then | Else) |
  DecisionSelector(RawDecisionProposalRefV1) |
  DecisionPatternOperand(RawDecisionProposalRefV1, arm_ordinal: u32,
                         pattern_ordinal: u32, operand_ordinal: u32) |
  DecisionPatternPredicate(RawDecisionProposalRefV1, arm_ordinal: u32,
                           pattern_ordinal: u32) |
  DecisionArmPredicate(RawDecisionProposalRefV1, arm_ordinal: u32) |
  DecisionResultOperand(RawDecisionResultMergeProposalRefV1,
                        arm_ordinal: u32,
                        role: SourceDecisionResultOperandRoleV1)
RawSourceDefinitionOwnerProposalV1 =
  ActionResult(RawActionProposalRefV1, result_ordinal: u32) |
  GatedMuxResult(RawGatedMuxProposalRefV1) |
  PinnedSyntheticResult(RawPinnedSyntheticOriginProposalRefV1)

RawValueOccurrenceProposalV1 {
  semantic_node: RawPhaseNodeProposalRefV1
  site: Use {
          site: RawControlUseSiteProposalV1,
          semantic_use: SyntaxSemanticSlotKeyV1,
          owner: RawSourceUseOwnerProposalV1,
          role: SourceOccurrenceUseRoleV1,
          value_source: EvaluateHere |
            FixedValue { producer: SyntaxSemanticSlotKeyV1,
                         reason: DataSource | AddressSource | PreviousValue |
                                 ObserverTrigger | MergeArm | LoopCarried }
        } |
        Definition {
          site: RawControlSiteProposalV1,
          semantic_result: SyntaxSemanticSlotKeyV1,
          owner: RawSourceDefinitionOwnerProposalV1
        }
  ordered_operands: range(ValueOccurrenceOperand)
}

RawGateProposalV1 {
  unit: RawControlUnitProposalRefV1
  parent_region: RawPredicateRegionProposalRefV1
  condition: RawValueOccurrenceUseProposalRefV1
  header: RawControlPointProposalRefV1
  join: RawControlPointProposalRefV1
  continuation: RawControlPointProposalRefV1
  true_region: RawPredicateRegionProposalRefV1
  false_region: RawPredicateRegionProposalRefV1
  result_merges: range(GateResultMerge)
  origin: If | Ternary
  condition_semantics: RawSourceConditionSemanticsProposalV1
}

RawGateResultMergeProposalV1 {
  unit: RawControlUnitProposalRefV1
  gate: RawGateProposalRefV1
  merge_site: RawControlSiteProposalV1
  condition: RawValueOccurrenceUseProposalRefV1
  then_value: RawValueOccurrenceUseProposalRefV1
  else_value: RawValueOccurrenceUseProposalRefV1
  result: RawValueOccurrenceDefProposalRefV1
  mux: RawGatedMuxProposalRefV1
}

RawDecisionProposalV1 {
  unit: RawControlUnitProposalRefV1
  parent_region: RawPredicateRegionProposalRefV1
  selector: RawValueOccurrenceUseProposalRefV1
  dispatch_header: RawControlPointProposalRefV1
  join: RawControlPointProposalRefV1
  continuation: RawControlPointProposalRefV1
  ordered_arms: range(DecisionArmRow), nonempty: true
  default_region: RawPredicateRegionProposalRefV1
  source_semantics: RawSourceCaseSemanticsProposalV1
}

RawGatedMuxProposalV1 {
  key: RawGatedKeyProposalRefV1
  semantic_node: RawPhaseNodeProposalRefV1
  result: RawValueOccurrenceDefProposalRefV1
}

RawDecisionResultMergeProposalV1 {
  unit: RawControlUnitProposalRefV1
  decision: RawDecisionProposalRefV1
  merge_site: RawControlSiteProposalV1
  result: RawValueOccurrenceDefProposalRefV1
  default_value: RawValueOccurrenceUseProposalRefV1
  selected_arm_values: range(DecisionResultSelectedValue)
  ordered_steps: range(DecisionResultStepRow)
}

RawObserverProposalV1 {
  origin: SyntaxOccurrenceKeyV1
  environment: RawEnvironmentLineageRefV1
  kind: SourceObserverKindV1
  metadata: SourceObserverMetadataV1
  sensitivity_inputs: range(ObserverSensitivityInput)
  capture_inputs: range(ObserverCaptureInput)
  event_sites: range(ObserverEventSite)
}

RawObserverOccurrenceProposalV1 {
  observer: RawObserverProposalRefV1
  origin: SyntaxSemanticSlotKeyV1
  owner: Primary | Trigger
  group_ordinal: u32
  occurrence_ordinal: u32
}

RawRuntimeEventSiteProposalV1 {
  origin: SyntaxOccurrenceKeyV1
  owner_action: RawActionProposalRefV1
  predicate: optional RawValueOccurrenceUseProposalRefV1
  arguments: range(RuntimeEventArgument)
  emit_rule: SourceRuntimeEventEmitRuleV1
  termination: None | Continue | Finish | Fatal
  fatal_code: optional u32
}

RawRuntimeCallGraphBoundaryProposalV1 {
  entry: SyntaxSemanticSlotKeyV1
  setup: SyntaxSemanticSlotKeyV1
  program_entry: SyntaxSemanticSlotKeyV1
  program_exit: SyntaxSemanticSlotKeyV1
  copyout: SyntaxSemanticSlotKeyV1
  exit: SyntaxSemanticSlotKeyV1
  predecessor_coverage: range(RuntimeCallInstancePredecessor)
}

RawRuntimeCallInstanceProposalV1 {
  key: SyntaxRuntimeCallInstanceKeyV1
  call: SyntaxOccurrenceKeyV1
  caller_site: SyntaxSemanticSlotKeyV1
  actuals: range(RuntimeCallInstanceActual)
  setup_occurrences: range(RuntimeCallInstanceSetupOccurrence)
  graph: RawRuntimeCallGraphBoundaryProposalV1
  program_occurrences: range(RuntimeCallInstanceProgramOccurrence)
  writebacks: range(RuntimeCallInstanceWriteback)
  return_result: optional SyntaxSemanticSlotKeyV1
  read_domains: range(RuntimeCallInstanceAccessReadDomain)
  write_domains: range(RuntimeCallInstanceAccessWriteDomain)
  read_bindings: range(RuntimeCallInstanceAccessReadBinding)
  write_bindings: range(RuntimeCallInstanceAccessWriteBinding)
  effect_publications: range(RuntimeCallInstanceAccessEffectPublication)
}

RawDynamicAddressPlanProposalV1 {
  owner_action: RawActionProposalRefV1
  target: SyntaxPreparedTargetKeyV1
  input: SyntaxCanonicalInputKeyV1
  object: SyntaxSemanticObjectKeyV1
  object_type: SyntaxTypeUseKeyV1
  object_width: u64
  indices: range(DynamicAddressIndex)
  dimensions: range(DynamicAddressDimension)
  strides: range(DynamicAddressStride)
  part_select_geometry: RawPartSelectGeometryProposalV1
  selected_width: u64
  offset: u64
  address_known: bool
  bounds_when_known: RawKnownBoundsProposalV1
  access_guard: RawValueOccurrenceUseProposalRefV1
  access_semantics: CheckedRead | CheckedOverlayWrite
}

RawForFoldTemplateProposalV1 {
  origin: SyntaxOccurrenceKeyV1
  execution_lineage: SyntaxRuntimeSourceExecutionLineageWitnessRefV1
  local_scope: SyntaxRuntimeLocalScopeV1
  unit: RawControlUnitProposalRefV1
  owner_action: RawActionProposalRefV1
  counter: RawBindingProposalRefV1
  counter_type: SourceForCounterRuleV1
  range_inputs: range(ForFoldRangeInput)
  step: DefaultByRangeDirection |
        Explicit { use: RawValueOccurrenceUseProposalRefV1,
                   op: SourceForStepAssignmentOpV1 }
  transition_semantics: RawForFoldTransitionSemanticsProposalV1
  states: range(ForFoldStateRow)
  read_domains: range(ForFoldAccessReadDomain)
  write_domains: range(ForFoldAccessWriteDomain)
  read_bindings: range(ForFoldAccessReadBinding)
  write_bindings: range(ForFoldAccessWriteBinding)
  effect_publications: range(ForFoldAccessEffectPublication)
  regions: range(ForFoldRegionRow)
  points: range(ForFoldPointRow)
  edges: range(ForFoldEdgeRow)
  actions: range(ForFoldActionRow)
  value_occurrences: range(ForFoldValueOccurrenceRow)
  dynamic_plans: range(ForFoldDynamicAddressPlanRow)
  recurrences: range(ForFoldRecurrenceRow)
}

RawWriteDomainProposalV1 {
  origin: SyntaxSemanticObjectKeyV1
  owner: SyntaxSemanticSlotKeyV1
  normalized_type: SyntaxTypeUseKeyV1
  access: RawBitAccessProposalV1
}
RawBindingProposalV1 {
  origin: SyntaxSemanticObjectKeyV1
  owner: SyntaxSemanticSlotKeyV1
  normalized_type: SyntaxTypeUseKeyV1
  role: SourceBindingRoleV1
}
RawEffectStreamProposalV1 {
  origin: SyntaxOccurrenceKeyV1
  owner: SyntaxSemanticSlotKeyV1
  kind: SourceEffectStreamKindV1
  publication_rule: SourceEffectPublicationRuleV1
}

RawPinnedSyntheticOriginProposalV1 {
  key: SyntaxSemanticSlotKeyV1
  semantic_node: RawPhaseNodeProposalRefV1
  occurrence: RawValueOccurrenceProposalRefV1
  reason: SourcePinnedSyntheticReasonV1
}

RawGatedKeyProposalV1 {
  unit: RawControlUnitProposalRefV1
  owner: GateResult(RawGateResultMergeProposalRefV1) |
         DecisionStep(RawDecisionResultMergeProposalRefV1,
                      source_arm: u32)
  condition: RawValueOccurrenceUseProposalRefV1
  then_value: RawValueOccurrenceUseProposalRefV1
  else_value: RawValueOccurrenceUseProposalRefV1
  merge_site: RawControlSiteProposalV1
}

RawSourceProposalRowV1 =
  SemanticObjectWitness(RawSemanticObjectProposalV1) |
  InputWitness(RawInputProposalV1) |
  Node(RawPhaseNodeProposalV1) |
  ControlUnit(RawControlUnitProposalV1) |
  PredicateRegion(RawPredicateRegionProposalV1) |
  ControlPoint(RawControlPointProposalV1) |
  ControlEdge(RawControlEdgeProposalV1) |
  Root(RawRootProposalV1) |
  Action(RawActionProposalV1) |
  ValueOccurrence(RawValueOccurrenceProposalV1) |
  Gate(RawGateProposalV1) |
  GateResultMerge(RawGateResultMergeProposalV1) |
  Decision(RawDecisionProposalV1) |
  GatedMux(RawGatedMuxProposalV1) |
  DecisionResultMerge(RawDecisionResultMergeProposalV1) |
  Observer(RawObserverProposalV1) |
  ObserverOccurrence(RawObserverOccurrenceProposalV1) |
  RuntimeEventSite(RawRuntimeEventSiteProposalV1) |
  RuntimeCallInstanceWitness(RawRuntimeCallInstanceProposalV1) |
  DynamicAddressPlan(RawDynamicAddressPlanProposalV1) |
  ForFoldTemplate(RawForFoldTemplateProposalV1) |
  WriteDomain(RawWriteDomainProposalV1) |
  Binding(RawBindingProposalV1) |
  EffectStream(RawEffectStreamProposalV1) |
  PinnedSyntheticOrigin(RawPinnedSyntheticOriginProposalV1) |
  GatedKey(RawGatedKeyProposalV1)
```

This is the complete field list, not an illustrative Rust layout. The
generator emits a distinct concrete payload type for every declaration above
and fails its `CheckedSourceProposalFieldCoverageV1` const assertion when a
checked source row gains, loses, or changes a field. In particular,
`RawActionProposalKindV1::InvokeRuntimeFunction` is exactly the authoritative
`{ optional root, instance, operand_role_range, result_role_range }` shape. Its
actual/setup/program-occurrence/writeback relation is owned by the separately
keyed `RawRuntimeCallInstanceProposalV1`; the action contains no result-only
shortcut or inline program body.

Every variable member owns one of the following dedicated slices. This block is
the second half of the normative generator input. A `pool P(T)` declaration
simultaneously defines the concrete `RawPProposalPoolEntryV1` payload, the
typed `range(P)`, and the same-named `RawSourceProposalPoolEntryV1::P(T)`
variant. A bare `ref`, `u32`, or `scalar` entry still has its exact payload
shown; there is no unit, opaque, generic-ID, or generic-summary entry.

```text
SourceProposalPoolKindV1 =
  SemanticObjectDimension | SemanticObjectStride |
  InputAccessSelector | InputRuntimeIndexRole | InputStridePrefix |

  NodeInputIndex | NodeOperand | NodeForStateRow | NodeForEffectRow |
  NodeForEffectArgument | NodeConcatPartRow |

  ControlUnitRoot | PredicateRegionChild |
  ControlPointAction | ControlPointPredecessorEdge |
  ControlPointSuccessorEdge | RootOperand |
  ActionOperand | ActionResult | ActionCaptureEnableSite |
  ActionTriggerRoot | ActionRuntimeArgumentOrdinal |
  ActionRuntimeCallOperandRole | ActionRuntimeCallResultRole |
  ActionAccessReadDomain | ActionAccessWriteDomain |
  ActionAccessReadBinding | ActionAccessWriteBinding |
  ActionAccessEffectPublication | ValueOccurrenceOperand |

  RuntimeCallInstanceActual | RuntimeCallInstanceSetupOccurrence |
  RuntimeCallInstancePredecessor | RuntimeCallInstanceProgramOccurrence |
  RuntimeCallInstanceWriteback |
  RuntimeCallInstanceAccessReadDomain |
  RuntimeCallInstanceAccessWriteDomain |
  RuntimeCallInstanceAccessReadBinding |
  RuntimeCallInstanceAccessWriteBinding |
  RuntimeCallInstanceAccessEffectPublication |

  GateResultMerge | DecisionArmRow | DecisionPatternRow |
  DecisionPatternOperand | DecisionResultSelectedValue |
  DecisionResultStepRow |

  ObserverSensitivityInput | ObserverCaptureInput | ObserverEventSite |
  RuntimeEventArgument | DynamicAddressIndex |
  DynamicAddressDimension | DynamicAddressStride |

  ForFoldRangeInput | ForFoldStateRow |
  ForFoldAccessReadDomain | ForFoldAccessWriteDomain |
  ForFoldAccessReadBinding | ForFoldAccessWriteBinding |
  ForFoldAccessEffectPublication |
  ForFoldRegionRow | ForFoldPointRow | ForFoldEdgeRow |
  ForFoldActionRow | ForFoldValueOccurrenceRow |
  ForFoldDynamicAddressPlanRow | ForFoldRecurrenceRow |

  FoldRegionChild | FoldPointAction | FoldPointPredecessorEdge |
  FoldPointSuccessorEdge | FoldActionOperand | FoldActionResult |
  FoldActionCaptureEnableSite | FoldActionRuntimeArgumentOrdinal |
  FoldActionRuntimeCallOperandRole | FoldActionRuntimeCallResultRole |
  FoldActionAccessReadDomain | FoldActionAccessWriteDomain |
  FoldActionAccessReadBinding | FoldActionAccessWriteBinding |
  FoldActionAccessEffectPublication | FoldOccurrenceOperand |
  FoldDynamicAddressIndex | FoldDynamicAddressDimension |
  FoldDynamicAddressStride | FoldRecurrencePredecessorUse |
  FoldEffectRow | FoldEffectArgument

SourceProposalPoolKindV1::ALL = [all variants above in written order]
SourceProposalPoolKindV1::COUNT = 89

pool SemanticObjectDimension {
  dimension_ordinal: u32
  kind: Unpacked | Packed | Intrinsic
  extent: u64
}
pool SemanticObjectStride { dimension_ordinal: u32, stride: u64 }

pool InputAccessSelector =
  Member { syntax: SyntaxOccurrenceKeyV1, member: SyntaxEntityKeyV1,
           declaration_ordinal: u32, flat_offset: u64, width: u64 } |
  UnpackedIndex { syntax: SyntaxOccurrenceKeyV1, dimension_ordinal: u32,
                  constant_index: optional SyntaxTypedValueOriginKeyV1 } |
  PackedBit { syntax: SyntaxOccurrenceKeyV1, dimension_ordinal: u32,
              constant_index: optional SyntaxTypedValueOriginKeyV1 } |
  PackedPart { syntax: SyntaxOccurrenceKeyV1, dimension_ordinal: u32,
               kind: Colon | PlusColon | MinusColon | Step,
               anchor: SyntaxSemanticSlotKeyV1,
               width_or_bound: SyntaxSemanticSlotKeyV1 }
pool InputRuntimeIndexRole {
  source_ordinal: u32
  slot: SyntaxSemanticSlotKeyV1
  dimension_ordinal: u32
  role: Index | Anchor | Width | LowerBound | UpperBound
  extent: u64
  stride: u64
  coercion: SyntaxPhaseCoercionOriginKeyV1
}
pool InputStridePrefix { dimension_ordinal: u32, stride: u64 }

pool NodeInputIndex(RawPhaseNodeProposalRefV1)
pool NodeOperand(RawPhaseValueUseProposalV1)
pool NodeForStateRow {
  target: SyntaxSemanticObjectKeyV1
  access: RawBitAccessProposalV1
  initial: RawPhaseValueUseProposalV1
  update: RawPhaseValueUseProposalV1
}
pool NodeForEffectRow {
  site: RawRuntimeEventSiteProposalRefV1
  predicate: optional RawPhaseValueUseProposalV1
  emit_rule: SourceRuntimeEventEmitRuleV1
  arguments: range(NodeForEffectArgument)
  fatal_code: optional u32
}
pool NodeForEffectArgument(RawPhaseNodeProposalRefV1)
pool NodeConcatPartRow(RawPhaseValueUseProposalV1)

pool ControlUnitRoot(RawRootProposalRefV1)
pool PredicateRegionChild(RawPredicateRegionProposalRefV1)
pool ControlPointAction(RawActionProposalRefV1)
pool ControlPointPredecessorEdge(RawControlEdgeProposalRefV1)
pool ControlPointSuccessorEdge(RawControlEdgeProposalRefV1)
pool RootOperand(RawValueOccurrenceUseProposalRefV1)
pool ActionOperand(RawValueOccurrenceUseProposalRefV1)
pool ActionResult(RawValueOccurrenceDefProposalRefV1)
pool ActionCaptureEnableSite(RawControlSiteProposalV1)
pool ActionTriggerRoot(RawRootProposalRefV1)
pool ActionRuntimeArgumentOrdinal(u32)
pool ActionRuntimeCallOperandRole {
  role: generated encoding of SourceRuntimeFunctionInvokeOperandRoleV1
  operand_slot: u32
}
pool ActionRuntimeCallResultRole {
  role: generated encoding of SourceRuntimeFunctionInvokeResultRoleV1
  result_slot: u32
}

pool ActionAccessReadDomain(RawWriteDomainProposalRefV1)
pool ActionAccessWriteDomain(RawWriteDomainProposalRefV1)
pool ActionAccessReadBinding(RawBindingProposalRefV1)
pool ActionAccessWriteBinding(RawBindingProposalRefV1)
pool ActionAccessEffectPublication {
  stream: RawEffectStreamProposalRefV1
  publication_kind: SourceEffectPublicationKindV1
}
pool ValueOccurrenceOperand(RawValueOccurrenceUseProposalRefV1)

pool RuntimeCallInstanceActual =
  InputExpr {
    formal_ordinal: u32
    source: ExplicitArgument { argument: SyntaxOccurrenceKeyV1 } |
            DeclaredDefault { formal: SyntaxEntityKeyV1,
                              default_expression: SyntaxOccurrenceKeyV1 }
    use: SyntaxSemanticSlotKeyV1
    formal_binding_coercion: SyntaxPhaseCoercionOriginKeyV1
  } |
  OutputTarget {
    formal_ordinal: u32
    argument: SyntaxOccurrenceKeyV1
    target: SyntaxPreparedTargetKeyV1
    initial_value: SyntaxTypedValueOriginKeyV1
    formal_to_target_coercion: SyntaxPhaseCoercionOriginKeyV1
  } |
  InoutTarget {
    formal_ordinal: u32
    argument: SyntaxOccurrenceKeyV1
    target: SyntaxPreparedTargetKeyV1
    old_value_use: SyntaxSemanticSlotKeyV1
    target_to_formal_coercion: SyntaxPhaseCoercionOriginKeyV1
    formal_to_target_coercion: SyntaxPhaseCoercionOriginKeyV1
  }
pool RuntimeCallInstanceSetupOccurrence {
  source_ordinal: u32
  slot: SyntaxSemanticSlotKeyV1
}
pool RuntimeCallInstancePredecessor {
  point: SyntaxSemanticSlotKeyV1
  predecessor: SyntaxSemanticSlotKeyV1
  role: Entry | Setup | ProgramEntry | ProgramExit | Copyout | Exit
}
pool RuntimeCallInstanceProgramOccurrence {
  program_ordinal: u32
  occurrence: SyntaxRuntimeProgramOccurrenceKeyV1
}
pool RuntimeCallInstanceWriteback {
  formal_ordinal: u32
  program_exit: SyntaxRuntimeProgramOccurrenceKeyV1
  target: SyntaxPreparedTargetKeyV1
  target_coercion: SyntaxPhaseCoercionOriginKeyV1
  caller_slot: SyntaxSemanticSlotKeyV1
  write_domain: RawWriteDomainProposalRefV1
}
pool RuntimeCallInstanceAccessReadDomain(RawWriteDomainProposalRefV1)
pool RuntimeCallInstanceAccessWriteDomain(RawWriteDomainProposalRefV1)
pool RuntimeCallInstanceAccessReadBinding(RawBindingProposalRefV1)
pool RuntimeCallInstanceAccessWriteBinding(RawBindingProposalRefV1)
pool RuntimeCallInstanceAccessEffectPublication {
  stream: RawEffectStreamProposalRefV1
  publication_kind: SourceEffectPublicationKindV1
}

pool GateResultMerge(RawGateResultMergeProposalRefV1)
pool DecisionArmRow {
  ordinal: u32
  patterns: range(DecisionPatternRow)
  predicate: RawValueOccurrenceUseProposalRefV1
  region: RawPredicateRegionProposalRefV1
}
pool DecisionPatternRow =
  EqWildcard { operands: range(DecisionPatternOperand), exact_len: 1,
               coercion: SyntaxPhaseCoercionOriginKeyV1,
               predicate: RawValueOccurrenceUseProposalRefV1 } |
  Range { operands: range(DecisionPatternOperand), exact_len: 2,
          lower_comparison: RawSourceComparisonProposalV1,
          upper_comparison: RawSourceComparisonProposalV1,
          upper_inclusive: bool,
          predicate: RawValueOccurrenceUseProposalRefV1 }
pool DecisionPatternOperand {
  value: RawValueOccurrenceUseProposalRefV1
  source_domain: Bit | Logic
  width: u64
  signed: bool
  exact_constant: optional SyntaxTypedValueOriginKeyV1
}
pool DecisionResultSelectedValue(RawValueOccurrenceUseProposalRefV1)
pool DecisionResultStepRow {
  source_arm: u32
  predicate: RawValueOccurrenceUseProposalRefV1
  selected_value: RawValueOccurrenceUseProposalRefV1
  incoming_value: RawValueOccurrenceUseProposalRefV1
  result: RawValueOccurrenceDefProposalRefV1
  mux: RawGatedMuxProposalRefV1
}

pool ObserverSensitivityInput(SyntaxCanonicalInputKeyV1)
pool ObserverCaptureInput(SyntaxCanonicalInputKeyV1)
pool ObserverEventSite(RawRuntimeEventSiteProposalRefV1)
pool RuntimeEventArgument(RawValueOccurrenceUseProposalRefV1)
pool DynamicAddressIndex(RawValueOccurrenceUseProposalRefV1)
pool DynamicAddressDimension {
  dimension_ordinal: u32, kind: Unpacked | Packed | Intrinsic,
  extent: u64
}
pool DynamicAddressStride { dimension_ordinal: u32, stride: u64 }

pool ForFoldRangeInput {
  role: Singleton | Start | End | Step
  use: RawValueOccurrenceUseProposalRefV1
}
pool ForFoldStateRow {
  state_ordinal: u32
  target: SyntaxSemanticObjectKeyV1
  access: RawBitAccessProposalV1
  outer_initial: RawValueOccurrenceUseProposalRefV1
  result_slot: SyntaxSemanticSlotKeyV1
}
pool ForFoldAccessReadDomain(RawWriteDomainProposalRefV1)
pool ForFoldAccessWriteDomain(RawWriteDomainProposalRefV1)
pool ForFoldAccessReadBinding(RawBindingProposalRefV1)
pool ForFoldAccessWriteBinding(RawBindingProposalRefV1)
pool ForFoldAccessEffectPublication {
  stream: RawEffectStreamProposalRefV1
  publication_kind: SourceEffectPublicationKindV1
}

pool ForFoldRegionRow {
  parent: optional RawFoldRegionProposalRefV1
  entry: RawFoldPointProposalRefV1
  normal_exit: RawFoldPointProposalRefV1
  owner: LoopRoot | Body
  children: range(FoldRegionChild)
}
pool ForFoldPointRow {
  region: RawFoldRegionProposalRefV1
  kind: Entry | Header | BodyEntry | Body | ContinueLatch |
        TransitionDispatch | NormalExit
  actions: range(FoldPointAction)
  predecessor_edges: range(FoldPointPredecessorEdge)
  successor_edges: range(FoldPointSuccessorEdge)
}
pool ForFoldEdgeRow {
  predecessor: RawFoldPointProposalRefV1
  successor: RawFoldPointProposalRefV1
  kind: EntryHeader | HeaderBody | HeaderExit | BodyFlow |
        ContinueExit | ContinueDispatch | TransitionAdvance |
        TransitionRangeExit
  predicate: optional {
    use: RawFoldValueOccurrenceUseProposalRefV1,
    polarity_or_outcome: FoldEdgePredicateV1
  }
}

RawFoldActionInputResolutionProposalV1 =
  Memory | Environment | StaticComposite |
  DynamicOverlay(RawFoldDynamicAddressPlanProposalRefV1)
RawFoldActionTargetProposalV1 =
  StaticTarget(SyntaxPreparedTargetKeyV1) |
  DynamicTarget { target: SyntaxPreparedTargetKeyV1,
                  plan: RawFoldDynamicAddressPlanProposalRefV1 }
RawFoldActionProposalKindV1 =
  ReadInput { result_slot: u32, input: SyntaxCanonicalInputKeyV1,
              resolution: RawFoldActionInputResolutionProposalV1 } |
  CaptureValue { result_slot: u32, source_operand: u32 } |
  BindEnvironment { result_slot: u32, source_operand: u32,
                    binding: RawBindingProposalRefV1 } |
  EvaluatePinned { result_slot: u32,
                   ordered_operand_slots: range(FoldActionRuntimeArgumentOrdinal) } |
  StoreState { target: RawFoldActionTargetProposalV1,
               value_operand: u32, observed_old_operand: optional u32,
               capture_enable_sites: range(FoldActionCaptureEnableSite) } |
  PublishRuntimeEvent {
    stream: RawEffectStreamProposalRefV1,
    site: RawRuntimeEventSiteProposalRefV1,
    predicate_operand: u32,
    argument_operands: range(FoldActionRuntimeArgumentOrdinal),
    termination: bool
  } |
  InvokeRuntimeFunction {
    instance: SyntaxRuntimeCallInstanceKeyV1,
    operand_roles: range(FoldActionRuntimeCallOperandRole),
    result_roles: range(FoldActionRuntimeCallResultRole)
  }

pool ForFoldActionRow {
  owner_point: RawFoldPointProposalRefV1
  action_index: u32
  ordered_operands: range(FoldActionOperand)
  results: range(FoldActionResult)
  effects: range(FoldEffectRow)
  read_domains: range(FoldActionAccessReadDomain)
  write_domains: range(FoldActionAccessWriteDomain)
  read_bindings: range(FoldActionAccessReadBinding)
  write_bindings: range(FoldActionAccessWriteBinding)
  effect_publications: range(FoldActionAccessEffectPublication)
  kind: RawFoldActionProposalKindV1
}

RawFoldUseOwnerProposalV1 =
  ValueOperand(RawFoldValueOccurrenceProposalRefV1, operand_ordinal: u32) |
  ActionOperand(RawFoldActionProposalRefV1, operand_ordinal: u32) |
  HeaderCondition | ContinueCondition | TransitionOutcome |
  RecurrenceUpdate(Counter | State(state_ordinal: u32))
RawFoldDefinitionOwnerProposalV1 =
  OuterEntry(RawValueOccurrenceUseProposalRefV1) |
  HeaderParam(Counter | State(state_ordinal: u32)) |
  ExitParam(State(state_ordinal: u32)) |
  ActionResult(RawFoldActionProposalRefV1, result_ordinal: u32)
pool ForFoldValueOccurrenceRow {
  semantic_node: RawPhaseNodeProposalRefV1 |
                 FixedRuntimeLeaf(SourceFoldRuntimeLeafKindV1)
  flow: Use {
          semantic_use: SyntaxSemanticSlotKeyV1,
          site: RawFoldControlSiteProposalV1,
          owner: RawFoldUseOwnerProposalV1,
          role: SourceFoldOccurrenceUseRoleV1,
          value_source: EvaluateHere |
            FixedValue { producer: SyntaxSemanticSlotKeyV1,
                         reason: DataSource | AddressSource | PreviousValue |
                                 ObserverTrigger | MergeArm | LoopCarried }
        } |
        Definition {
          semantic_result: SyntaxSemanticSlotKeyV1,
          site: RawFoldControlSiteProposalV1,
          owner: RawFoldDefinitionOwnerProposalV1
        }
  ordered_operands: range(FoldOccurrenceOperand)
}
pool ForFoldDynamicAddressPlanRow {
  owner_action: RawFoldActionProposalRefV1
  target: SyntaxPreparedTargetKeyV1
  expected_slot: SyntaxSemanticSlotKeyV1
  input: SyntaxCanonicalInputKeyV1
  object: SyntaxSemanticObjectKeyV1
  object_type: SyntaxTypeUseKeyV1
  object_width: u64
  indices: range(FoldDynamicAddressIndex)
  dimensions: range(FoldDynamicAddressDimension)
  strides: range(FoldDynamicAddressStride)
  part_select_geometry: RawPartSelectGeometryProposalV1
  selected_width: u64
  offset: u64
  address_known: bool
  bounds_when_known: RawKnownBoundsProposalV1
  access_guard: RawFoldValueOccurrenceUseProposalRefV1
  access_semantics: CheckedRead | CheckedOverlayWrite
}
pool ForFoldRecurrenceRow {
  owner: Counter | State(state_ordinal: u32)
  outer_entry: RawFoldValueOccurrenceDefProposalRefV1
  header: RawFoldValueOccurrenceDefProposalRefV1
  update: RawFoldValueOccurrenceUseProposalRefV1
  advance_edge: RawFoldEdgeProposalRefV1
  exit_parameter: optional RawFoldValueOccurrenceDefProposalRefV1
  predecessor_uses: range(FoldRecurrencePredecessorUse)
}

pool FoldRegionChild(RawFoldRegionProposalRefV1)
pool FoldPointAction(RawFoldActionProposalRefV1)
pool FoldPointPredecessorEdge(RawFoldEdgeProposalRefV1)
pool FoldPointSuccessorEdge(RawFoldEdgeProposalRefV1)
pool FoldActionOperand(RawFoldValueOccurrenceUseProposalRefV1)
pool FoldActionResult(RawFoldValueOccurrenceDefProposalRefV1)
pool FoldActionCaptureEnableSite(RawFoldControlSiteProposalV1)
pool FoldActionRuntimeArgumentOrdinal(u32)
pool FoldActionRuntimeCallOperandRole {
  role: generated encoding of SourceRuntimeFunctionInvokeOperandRoleV1
  operand_slot: u32
}
pool FoldActionRuntimeCallResultRole {
  role: generated encoding of SourceRuntimeFunctionInvokeResultRoleV1
  result_slot: u32
}
pool FoldActionAccessReadDomain(RawWriteDomainProposalRefV1)
pool FoldActionAccessWriteDomain(RawWriteDomainProposalRefV1)
pool FoldActionAccessReadBinding(RawBindingProposalRefV1)
pool FoldActionAccessWriteBinding(RawBindingProposalRefV1)
pool FoldActionAccessEffectPublication {
  stream: RawEffectStreamProposalRefV1
  publication_kind: SourceEffectPublicationKindV1
}
pool FoldOccurrenceOperand(RawFoldValueOccurrenceUseProposalRefV1)
pool FoldDynamicAddressIndex(RawFoldValueOccurrenceUseProposalRefV1)
pool FoldDynamicAddressDimension {
  dimension_ordinal: u32, kind: Unpacked | Packed | Intrinsic,
  extent: u64
}
pool FoldDynamicAddressStride { dimension_ordinal: u32, stride: u64 }
pool FoldRecurrencePredecessorUse {
  edge: RawFoldEdgeProposalRefV1
  use: RawFoldValueOccurrenceUseProposalRefV1
}
pool FoldEffectRow {
  site: RawRuntimeEventSiteProposalRefV1
  predicate: optional RawFoldValueOccurrenceUseProposalRefV1
  emit_rule: SourceRuntimeEventEmitRuleV1
  arguments: range(FoldEffectArgument)
  fatal_code: optional u32
}
pool FoldEffectArgument(RawFoldValueOccurrenceUseProposalRefV1)

RawSourceProposalPoolEntryV1 = generated closed sum of every `pool` declaration
  above, in exactly SourceProposalPoolKindV1 order
```

`RuntimeCallInstanceActual::InputExpr.source` is the only input-actual source
sum. `ExplicitArgument` carries the exact retained call-argument syntax;
`DeclaredDefault` instead carries both the exact formal and its declared
default-expression syntax and can occur only for an omitted input. Output and
inout variants have no default arm. The instance row owns actual, setup,
program-occurrence, predecessor-coverage, and writeback ranges. The outer or
fold action owns only its invoke operand/result role ranges and combined access
summary. Those role entries are parallel ordinal annotations over the action's
existing operands/results and contain no second value ID.

The ownership map is exact. A semantic-object witness owns its dimension and
stride ranges; an input witness owns its selector, runtime-index-role, and
stride-prefix ranges. A node owns its input-index, operand, state, effect, and
concat-part ranges; each `NodeForEffectRow` owns its own argument range. A
control unit owns roots, a predicate region owns children, a control point owns
actions and its separate predecessor/successor ranges, a root owns operands,
an action owns operands/results/capture sites/triggers/runtime-argument
ordinals, its two runtime-call role ranges, and its five access-summary ranges.
A runtime-call-instance row owns its actual/setup/predecessor/program-occurrence/
writeback ranges and its five complete access-summary ranges. A value occurrence
owns its operand range. A gate owns gate-result-merge refs. A decision owns arm rows,
each arm row owns pattern rows, each pattern row owns pattern operands, and a
decision-result merge owns selected values and step rows. Observer, runtime
event, and dynamic-address rows own only their respectively named ranges.

A ForFold template owns range inputs, state rows, its five access-summary
ranges, and the region/point/edge/action/value-occurrence/dynamic-plan/
recurrence row ranges. Within that template, a fold region owns children; a
fold point owns actions and separate predecessor/successor edges; a fold action
owns operands/results/capture sites/runtime-argument ordinals, its two runtime-
call role ranges, effect rows, and its five access-summary ranges; a fold value
occurrence owns operands; a fold
dynamic plan owns indices/dimensions/strides; a recurrence row owns predecessor
uses; and an effect row owns arguments. Every listed field has exactly one
range and every range has exactly one owner. A kind which has no owner rows
must be empty. Adding a ranged checked field fails the exhaustive owner-field
match until a new dedicated pool kind is assigned.

```text
RawSourceProposalV1<'a>
  row_slices: exactly SourceProposalTableKindV1::COUNT borrowed slices in
              SourceProposalTableKindV1::ALL order
  pool_slices: exactly SourceProposalPoolKindV1::COUNT borrowed slices in
               SourceProposalPoolKindV1::ALL order
```

The logical row/entry discriminant must agree with the slice kind; a row cannot
be smuggled through another same-width slice. Row slices and then pool slices
are visited in enum order, and every owned range is a gap-free exact partition
of its one named slice.

Proposal rows may state an incorrect width, type, source site, coercion, access
summary, or recipe, but only as comparison data after independent derivation.
The common raw input has no ordinary/gated classification row, cache outcome,
interning key, or construction-request log. Detached preparation derives
positive `OrdinarySemantic` or `Gated(complete source key)` classification for
every node from the complete expected ordinary recipe relation and verified
complete gated-key registry.

### Detached input and live-builder construction identity

There are two front doors with intentionally different construction-state
inputs. The detached front door above receives no construction identity and
builds a temporary classification/index from the verified aggregate relation.
A live builder instead retains a private sidecar until its own finish:

```text
LiveConstructionWitnessV1<'a>
  requests: &'a [LiveNodeRequestRowV1]
  ordinary_cache: &'a [LiveOrdinaryCacheRowV1]
  gated_cache: &'a [LiveGatedCacheRowV1]

LiveNodeRequestRowV1
  canonical request serial: u64
  exact raw node recipe and ordered child references
  requested identity: OrdinarySemantic |
                      Gated(complete RawGatedKeyProposalV1)
  observed outcome: Inserted(raw node row) | Existing(raw node row)

LiveOrdinaryCacheRowV1
  exact ordinary structural key / raw node row

LiveGatedCacheRowV1
  complete raw gated key / exact structural key / raw node row
```

The sidecar is borrowed directly from `SourceArtifactBuilder`; it is not part
of `RawSourceAggregateV1`, a byte adapter, or any persistent wire. The live
verifier first derives typed semantics, the expected graph, every complete
gated key, and total node classification without consulting the sidecar. It
then replays every request in serial order, verifies `Inserted` versus
`Existing` against the prior prefix, requires every `Inserted` outcome to name
exactly the next append-order node row and every `Existing` outcome to name an
already inserted row, proves that every node has its unique inserting request,
and compares both final cache tables bijectively with the derived
ordinary/gated partitions. An ordinary cache row for a gated node,
an omitted request, or a wrong hit/insert target is
`CLASSIFY.CONSTRUCTION_REQUEST`; an omitted/duplicate/extra/wrong-partition
cache row is `CLASSIFY.CONSTRUCTION_CACHE`. Only after this comparison may
finish discard the request log and replace/drop the caches.

```text
try_prepare_source_aggregate(&RawSourceAggregateV1)
  -> Result<PreparedSourceAggregate, SourceAggregateErrorV1>

SourceArtifactBuilder::try_finish(self)
  -> Result<PreparedSourceAggregate,
            (SourceArtifactBuilder, SourceAggregateErrorV1)>
```

`SourceArtifactBuilder::try_finish` borrows an aggregate view and its
`LiveConstructionWitnessV1` from `self` while all verification output remains
separate. On failure it drops only that private output and returns the same
builder ownership with its proposal, request log, and caches unchanged. On
success it consumes the builder only after the sidecar comparison. A future
persistent wire follows the detached contract and never serializes or
reconstructs live cache history.

Canonical syntax traversal defines private row order and exact gap-free owned
ranges. The top-level verifier first performs an allocation-free scan of input
slice lengths, scalar tags, reference bounds, and range partitions. It then
uses `RawTopology` demand waves to reserve owner bitmaps/worklists and verify
unique ownership, canonical traversal, and orphan absence. Only after those
waves may it construct checked IDs or invoke the joint semantic scheduler.
Consequently an in-process proposal receives exactly the same distrust as a
decoded proposal; Rust type correctness is not semantic evidence.

The detached production entry point at this boundary is:

```text
try_prepare_source_aggregate(&RawSourceAggregateV1)
  -> Result<PreparedSourceAggregate, SourceAggregateErrorV1>
```

It borrows and never mutates its input. Success owns only independently checked
and staged output. Failure drops private work and leaves both producer state and
the raw aggregate unchanged. The separate live-builder entry point has the
sidecar and return-unchanged contract above; it does not invoke detached
preparation and skip the construction-identity comparison. An optional byte
adapter has the detached contract: it privately decodes borrowed bytes into
the logical slices, invokes this exact entry point, and can publish only the
resulting prepared/frozen aggregate—not decoded raw tables.

The following existing types are legacy/cache boundaries and are not inputs to
this decoder:

- `portable::StringTable` and `PortableVariable`;
- serde's `SLTNodeArenaWire` or standalone `SLTNodeArena::deserialize`;
- serde-deserialized checked control IDs;
- `SourceValueOccurrenceBoundary` by itself; and
- any legacy arena-only cache or optional provenance field.

No new verified record derives `Deserialize`. Compatibility is an explicit
version adapter into a legacy structural type, never relabeling into this
pipeline.

## Integer and scalar encoding

Every multibyte scalar uses little-endian byte order. Framing fields, IDs,
counts, widths, offsets, and lengths are unsigned. Wire data never contains
`usize`, a native pointer, an enum's Rust discriminant, or a Rust struct image.

| Meaning | Encoding |
| --- | --- |
| table-local and checked control/input/site IDs | `u32` |
| phase-node indices, counts, widths, offsets, and range lengths | `u64` |
| boolean | one byte, exactly `0` or `1` |
| closed enum | the schema-defined `u8`, `u16`, or `u32` tag |
| signed integer payload | fixed-width two's-complement field defined by its row schema |
| string or arbitrary-width integer bytes | a range into a canonical byte pool |

An enum tag not named by the exact aggregate schema is invalid. Reserved bytes
and flags must be zero. Conversion from `u64` to `usize` occurs only after the
value has been checked against both its table/pool bound and `usize::MAX`.
Conversion to a checked `u32` ID additionally requires that the complete dense
table length is representable by that ID namespace.

Strings are UTF-8 byte sequences. A schema's string table is strictly ordered
by raw UTF-8 bytes and contains no duplicate byte sequence. Other rows refer to
that table by checked `u32` string ID; they do not repeat or own string payloads.

An unsigned arbitrary-width integer uses minimal little-endian magnitude
bytes. Zero has length zero. A nonzero value's last byte is nonzero. Each
integer row owns one canonical byte-pool range, and every integer row is named
by exactly one scalar field in the decoded aggregate. This prevents several
large owned integers from being materialized from one small aliased byte span.
Semantic verification separately rejects payload or mask bits outside the
declared value width.

## Envelope header

The generic private envelope starts with this exact 40-byte header:

| Offset | Size | Field | Required value or interpretation |
| ---: | ---: | --- | --- |
| 0 | 8 | `magic` | ASCII `CELXRAW` followed by one zero byte |
| 8 | 2 | `framing_version` | little-endian `1` |
| 10 | 2 | `aggregate_kind` | exact kind requested by the internal schema descriptor |
| 12 | 4 | `schema_version` | exact version requested by that descriptor |
| 16 | 4 | `section_count` | number of 40-byte directory entries |
| 20 | 4 | `flags` | zero |
| 24 | 8 | `directory_bytes` | exactly `section_count * 40` |
| 32 | 8 | `payload_bytes` | exact number of bytes following the directory |

The header values are decoded field by field; the header is never transmuted to
a Rust type. The decoder receives one exact internal schema descriptor. It
does not accept a version range, guess an aggregate kind, or select the newest
known version. A kind or version mismatch is an ordinary structured error.

`aggregate_kind` assignments are reserved as follows for the eventual
aggregate schemas:

| Value | Aggregate |
| ---: | --- |
| 1 | source |
| 2 | occurrence |
| 3 | control value |

Reservation of kind `1` does not assign a source `schema_version`. A production
source descriptor must not be added until the complete `SourceWire` schema and
aggregate classification relation are specified. Private tests pass an
explicit test descriptor; its bytes are not a persistent artifact format.

The total input length must equal, using checked addition,
`40 + directory_bytes + payload_bytes`. Truncation and trailing bytes are both
noncanonical. Concatenated envelopes require an outer container and are not
accepted by this decoder.

## Section directory

The header is immediately followed by `section_count` entries of exactly 40
bytes each:

| Entry offset | Size | Field | Meaning |
| ---: | ---: | --- | --- |
| 0 | 4 | `tag` | schema-defined section tag |
| 4 | 2 | `kind` | `0` for fixed rows, `1` for a byte pool |
| 6 | 2 | `flags` | zero |
| 8 | 8 | `row_count` | number of rows, or bytes for a byte pool |
| 16 | 4 | `row_width` | exact encoded row width, or `1` for a byte pool |
| 20 | 4 | `reserved` | zero |
| 24 | 8 | `payload_offset` | offset relative to the start of envelope payload |
| 32 | 8 | `payload_len` | exact encoded byte length of the section |

The directory has one canonical representation:

1. Tags are strictly increasing. Unknown and duplicate tags are rejected.
2. The exact required/optional tag set and the exact `kind` and `row_width` of
   every tag come from the one selected schema descriptor. The descriptor also
   gives an optional section's canonical presence predicate; it may not permit
   both absent and present-empty encodings for the same value. An empty unknown
   section is still invalid.
3. `flags` and `reserved` are zero.
4. A fixed-row section satisfies
   `payload_len == row_count * row_width` using checked multiplication, and
   its schema-defined `row_width` is nonzero.
5. A byte pool has `kind == 1`, `row_width == 1`, and
   `payload_len == row_count`.
6. Sections partition the payload in directory order. The first offset is
   zero; every later offset equals the checked end of the preceding section;
   and the last checked end equals `payload_bytes`.
7. Padding, gaps, overlap, out-of-order physical sections, and bytes not owned
   by a section are invalid.

If the selected descriptor has no sections, `section_count`,
`directory_bytes`, and `payload_bytes` are all zero. A descriptor with required
sections cannot use that empty encoding.

ID, adjacency, operand, state, effect, range, and string-index pools are fixed
row sections with their schema-defined width, normally four or eight bytes.
Only uninterpreted UTF-8 and arbitrary-width magnitude storage uses byte-pool
sections.

This directory framing is generic. It does not define the complete list or
layout of source sections. Assigning those tags and row layouts is part of the
future complete `SourceWire` schema, not an incremental extension of a
partially accepted `SourceWireV1`.

## Flat table and pool canonicality

Recursive HIR and control shapes are encoded as flat rows with checked IDs.
Variable-length members are encoded as `{ start: u64, len: u64 }` ranges into
a section dedicated to that member role. A row never embeds a nested encoded
vector.

For each dedicated range pool, owner rows are visited in canonical table order
and their ranges must form an exact partition:

```text
first.start == 0
next.start == previous.start + previous.len
last.start + last.len == pool.row_count
```

With no owner rows, the corresponding pool must be empty. With owner rows and
an empty pool, every permitted empty range is at cursor zero.

Every addition is checked. This rule applies independently to node input
indices, concat operands, `ForFold` state rows, effect rows, effect arguments,
control predecessor/successor lists, occurrence operands, HIR children, and
all later source-provenance lists. A schema may require a range to be nonempty;
otherwise an empty range is canonical only at the current partition cursor.

Pool entries may repeat an ID when the language relation contains repeated
uses, but the repeated entry occupies repeated encoded bytes. Ranges may not
alias the same entries. Consequently one encoded entry cannot cause
input-dependent allocation amplification through many owner ranges.

String and arbitrary-width-integer byte ranges obey the same ordered exact-
partition rule in their respective row-table order. Other rows refer to the
resulting string/integer IDs rather than creating additional byte ranges.

Canonical source coordinates, table ordering, HIR child ordering, semantic
operand ordering, and provenance list ordering are semantic-schema rules. The
framing decoder validates scalar and range form; the selected aggregate
verifier validates those semantic orders independently from typed HIR.

## Two-pass decoder

Decoding is iterative and has two passes. It does not call a semantic verifier
during the first pass.

### Pass 1: validate and plan without owned allocation

The first pass:

1. reads the header and directory from borrowed bytes;
2. validates exact kind/version/tag/layout and the complete payload partition;
3. scans every fixed row for reserved bits, boolean/enum encodings, raw integer
   representability, checked table references, and canonical pool ranges;
4. validates UTF-8 and minimal arbitrary-width integer byte ranges in place;
5. computes with checked addition and multiplication the exact row counts,
   byte payloads, and owned staging sizes needed by pass 2; and
6. proves that every planned owned item has one corresponding encoded row or
   uniquely owned encoded byte range.

For each fixed table, the decoder proves at minimum that
`row_count <= payload_len / row_width` before converting the count or planning
storage. It separately checks `row_count * size_of::<RawRow>()` and the sum of
all planned staging allocations for `usize` overflow. There is no node, CFG,
depth, string, limb, or table-count policy limit: representability follows from
the actual encoded bytes and the host address space.

No `Vec::with_capacity`, `collect`, recursive descent, `BigUint` construction,
string copy, or `try_reserve` occurs in pass 1. An explicit worklist is used
where a schema-level flat relation still needs a traversal.

### Pass 2: fallible private materialization

Only after all pass-1 checks succeed does pass 2:

1. create empty private vectors;
2. call `try_reserve_exact` with the already checked counts for every vector;
3. decode fixed rows without changing a published object;
4. copy each aggregate-owned string/byte pool once; and
5. retain each validated arbitrary-width magnitude as one disjoint canonical
   range in that aggregate-owned raw byte pool until the joint typed-value
   verifier consumes it.

Failure of any reservation drops the private staging value. No partially
decoded table, checked ID, semantic fact, or artifact is returned. Production
code has no allocation retry, smaller fallback format, or semantic size
cutoff. A validated magnitude is decoded only into the aggregate's flat,
fallibly pre-reserved limb arena. Proof-path code never constructs
`num_bigint::BigUint`/`BigInt`, and a phase node names an independently
verified typed-value ID rather than owning payload/mask integers. Every limb,
scratch, row, and range allocation is therefore covered by the same structured
reservation-failure contract.

Tests use an injected `fail-at-N` reservation policy so every reserve site can
be failed deterministically. The production policy performs only the checked
`try_reserve_exact` calls; the test policy is not a runtime capacity setting.

## Unclassified source-node stage

The first node consumer is a private `UnclassifiedSourceNodeStage`. It is
constructed only after a typed-HIR verifier has independently produced the
exact semantic-object rows, exact input-access rows, and derived
`InputSemanticFacts<SourcePhase>` required by the raw node section. Object
identity and access identity are distinct: an input node names one exact
access, while a `ForFold` state target names the underlying semantic object.
Producer-supplied width, signedness, domain, dimension, access, or stride rows
are not semantic input facts.

Construction is ordered as follows:

1. Scan every child reference in every raw node without dereferencing it.
   Every child must exist and precede its owner. This scan covers all nested
   node-reference pools.
2. Check all raw input and runtime-site indices against the complete expected
   semantic tables, while they are still raw integers.
3. Convert one append-ordered node at a time to phase-typed IDs, checked ranges
   in pre-reserved flat payload pools, and a fixed
   `PhaseSLTNodeV1<SourcePhase>` descriptor. Constant rows name the exact
   `VerifiedSourceTypedValueId` already derived by the joint aggregate;
   they never construct or own an integer payload here.
4. Recompute every normative fact row—width, signedness, positive-type class,
   static domain, value class, mask class, and `lowerable`—plus access geometry
   and structural coercion rules from the verified input facts and checked
   prefix. Construct one private
   `PreparedPhaseSLTNodeFactsV1<SourcePhase>` containing the complete dense row
   table and the exact pending arena-owner identity which will receive the
   source artifact brand; no singular fact row is a replay output. The exhaustive
   variant transfer table is in
   [`source-semantic-inputs.md`](./source-semantic-inputs.md).
5. Compare each node recipe and coercion context with the independently
   derived complete expected source-value graph.

The stage preserves the encoded node rows and does not intern them. In
particular, it does not:

- build the ordinary AVL index;
- reject raw equality between an ordinary mux and a gated mux;
- merge equal gated muxes belonging to different complete gated keys;
- infer ordinary identity from absence in an incomplete gated table;
- prove all-node expected-graph reachability; or
- create `FrozenSLTNodeArena<SourcePhase>`.

Those operations require the complete expected graph and verified source
provenance. Aggregate classification must derive exactly one
`OrdinarySemantic` or `Gated(complete source key)` identity for every node,
prove disjoint total coverage, and only then rebuild the ordinary index while
excluding gated nodes. The current ordinary-only `replay_typed` path is not a
wire decoder and must not be called on this unclassified stage.

The stage type has no public constructor, serializer, planner view, lowering
view, or `commit`/`freeze` method. Only the eventual aggregate source verifier
may consume it.
Its node replay member is the whole private
`PreparedPhaseSLTNodeFactsV1<SourcePhase>` table:

```text
PreparedPhaseSLTNodeFactsV1<P>
  pending_arena_owner: exact nonforgeable identity of the staged node owner
  rows: complete dense [PhaseSLTNodeFactV1<P>]
  no FrozenSLTNodeArena<P>, public brand, retained view, or standalone commit
```

Borrowing
`PhaseSLTNodeFactV1<SourcePhase>` for one ordinal never drops, replaces, or
rebrands that owning table.

## Prepare, failure, and commit ownership

Byte decoding borrows `&[u8]`; every error therefore leaves the encoded input
unchanged. All decoded rows and node materialization live in private staging
owners which are dropped on failure.

Artifact identity uses one fallibly allocated, non-ZST live token, not a
serialized/global number or a durable address key:

```text
ArtifactBrandOwner
  private exactly-one BrandToken allocation created with try_reserve_exact
  not Clone/Copy/Serialize/Deserialize

BrandRef<'a>
  private &'a BrandToken

BrandedId<'a, P, K>
  BrandRef<'a> / compact private ID / phase and kind markers
```

`BrandRef` equality uses reference identity only while both owners are live.
The owner allocation is stable across aggregate moves, and Rust borrowing
prevents either token from being freed while a reference exists; allocator
address reuse therefore cannot make two live brands equal. No raw pointer,
`NonNull`, integer address, token address, global counter, random value, digest,
or split bit field is stored in a row/key or used as durable identity.

One `SourceConstructionSession` owns the brand, decoded input, staging pools,
and construction indices. A field-splitting editor borrows `BrandRef<'a>` and
may return `BrandedId<'a, ...>` handles. On every multi-handle operation it
checks brand equality before indexing, then stores only compact local IDs in
session rows. Neither the session nor any prepared/frozen part stores a
`BrandRef`, branded handle, or reference to its own owner. The borrow checker
therefore prevents `finish(self)` or moving/dropping the session while any
handle remains live; no self-referential aggregate is created.

`finish(self)` verifies all compact relations/capacity, drops transient
handles/indices, strips staging wrappers to unbranded prepared parts, and only
then moves the owner and parts together into `PreparedSourceAggregate`.
Failure returns the owned session/error so the top-level API can return every
unchanged input. Commit moves the same owner and unbranded parts into
`FrozenSourceArtifact` without allocation. Frozen APIs create new ephemeral
branded views borrowed from that owner; tables have no standalone constructor,
freeze, serializer, or public compact-ID constructor.

Brands are never proof-bearing wire fields. Deserialization rejects a legacy
brand field, verifies compact raw relations, creates a fresh owner, and reruns
the aggregate preparation. Decoding identical bytes twice therefore yields
distinct live brands; cross-artifact compact IDs compose only through an
explicit verified mapping relation, never equality by number or shape.

Cross-artifact use has one closed allocation-free API error, separate from
source preparation errors:

```text
ArtifactBrandErrorV1
  rule: BRAND.CROSS_ARTIFACT
  operation: FactsReplay | PhaseLookup | MappingInsert | MappingQuery |
             DerivedPlanCompose
  left:  { ArtifactPhaseTagV1, ArtifactHandleKindTagV1, compact local id }
  right: { ArtifactPhaseTagV1, ArtifactHandleKindTagV1, compact local id }

ArtifactPhaseTagV1 =
  Source | DraftOccurrence | Occurrence | ControlValue |
  SIR | MIR | Allocation

ArtifactHandleKindTagV1 =
  PhaseNode | VerifiedTypedValue | VerifiedBits | PhaseCoercion |
  SemanticObject | Input | ControlUnit | PredicateRegion |
  ControlPoint | ControlEdge | Root | Action | ValueOccurrence |
  Gate | GateResultMerge | Decision | DecisionResultMerge | GatedMux |
  Observer | ObserverOccurrence | RuntimeEventSite | DynamicAddressPlan |
  RuntimeExecutionLineage | RuntimeFunctionSpecialization |
  RuntimeFunctionProgram | RuntimeCallInstance | PreparedTarget |
  ForFoldTemplate | FoldRegion | FoldPoint | FoldEdge | FoldAction |
  FoldValueOccurrence | FoldDynamicAddressPlan | FoldCanonicalProducer |
  FoldInstValue | FoldMemoryToken | FoldEnvironmentToken | FoldEffectToken |
  WriteDomain | Binding | EffectStream | Mapping | DerivedPlan
```

This enum is exhaustive for handles which may cross into a multi-handle API.
Expected-graph IDs, raw proposal IDs, memo/dependency IDs, and other
construction-only compact IDs have no public `BrandedId` constructor and are
reachable only through a branded owning artifact/view, so they cannot be an
operand of `ArtifactBrandErrorV1`. Exposing any such ID as a multi-handle API
operand requires adding its explicit tag before that API is made visible; it
cannot reuse `DerivedPlan` or an `Other` catch-all.

It contains no token address, global brand number, string, or allocator data.
Every multi-handle operation compares the two borrowed `BrandRef`s before
dereferencing either compact ID; `BRAND.CROSS_ARTIFACT` therefore precedes
local-ID bounds, mapping, and semantic errors. Equal brands continue with the
ordinary operation's documented precedence. Phase/kind mismatches are normally
unrepresentable in Rust and are not folded into this runtime rule. This error is
not a `SourceAggregateErrorV1`: preparation owns one brand and cannot encounter a
cross-owner composition.

The aggregate ownership API has this shape; a byte adapter adds only private
decoded-row ownership around the same borrowed verifier call:

```text
try_prepare_source_aggregate(&RawSourceAggregateV1)
  -> Result<PreparedSourceAggregate, SourceAggregateErrorV1>

PreparedSourceAggregate::commit(self) -> FrozenSourceArtifact
```

Preparation must verify the typed HIR, expected graphs, source-node recipes,
source provenance, roots/actions/observers/runtime sites, ForFold transition
semantics, gated registries, ordinary/gated classification, canonical indices,
and all-node reachability as one aggregate relation. It also reserves exact
cache-free final storage before returning success.

Commit only moves staged owned rows into already reserved storage and drops
construction state. In that one infallible move it consumes the matched staged
node owner and `PreparedPhaseSLTNodeFactsV1<SourcePhase>`, creates the
`FrozenSLTNodeArena<SourcePhase>` and retained
`PhaseSLTNodeFactsV1<SourcePhase>` together under the same artifact brand and
arena identity, and exposes neither object early. It performs no allocation,
semantic check, ID conversion, map insertion, or fallible operation. A live
producer-side builder follows the same rule and is returned unchanged with its
error when preparation fails.

No intermediate value in this document can be committed. This prevents an
arena, semantic-input table, or narrow source-occurrence topology from being
mistaken for a planner-ready source artifact.

## Structured errors

Decode and preparation failures use one closed, allocation-free tagged sum:

```text
SourceAggregateErrorV1 =
  SourceLocal(SourceAggregateLocalErrorV1) |
  TypedConstant(TypedConstantErrorV1)

SourceAggregateLocalErrorV1
  rule: SourceAggregateRuleIdV1
  phase(): Header | Directory | SyntaxAdapter | EnvironmentLineage |
           ProducerWitness | ProposalTopology | TypedHIR |
           RuntimeExecutionLineage |
           NodeReplay |
           Provenance | Classification | Resource | Aggregate
  owner: None |
         ByteOffset(u64) |
         Section(tag) |
         SyntaxRow(PrivateRawSyntaxTableKindV1, u64) |
         SyntaxPoolEntry(PrivateRawSyntaxPoolKindV1, u64) |
         EnvironmentLineage(u64) |
         ExecutionLineageWitness(u64) |
         ExecutionLocalScope { lineage: u64,
                               scope: SyntaxRuntimeLocalScopeV1 } |
         ExecutionPoolEntry(SyntaxRuntimeSourceExecutionLineagePoolKindV1,
                            u64) |
         ProducerWitness(SyntaxAnalyzerWitnessKindV1, u64) |
         ProducerWitnessPoolEntry(SyntaxAnalyzerWitnessPoolKindV1, u64) |
         ProposalRow(SourceProposalTableKindV1, u64) |
         ProposalPoolEntry(SourceProposalPoolKindV1, u64) |
         LiveConstruction(LiveConstructionRowKindV1, u64) |
         RawNode(u64) |
         TypedOwner(SourceTypedOwnerKindV1, u64) |
         FoldTypedOwner { template: u64,
                          kind: SourceFoldTypedOwnerKindV1,
                          local: u64 } |
         ResourceDemand(SourceResourceDemandIdV1) |
         ReservationSite(SourceResourceSiteIdV1)
  context: None |
           ExpectedActual(u64, u64) |
           Pair(u64, u64) |
           MemberOrdinal { owner: u64, member: u64 } |
           Range { start: u64, len: u64, bound: u64 } |
           ReferenceKind { expected: SourceReferenceKindV1,
                           actual: SourceReferenceKindV1 } |
           Tag(u64) |
           Capacity { elements: u64, element_size: u64 }
```

All tags in that record are closed:

```text
SyntaxAnalyzerWitnessKindV1 = generated discriminant of the closed
  SyntaxAnalyzerWitnessRowV1 body sum stored by RawSyntaxAnalyzerWitnessV1.rows
SyntaxAnalyzerWitnessKindV1::ALL = generated in that body-sum order
SyntaxAnalyzerWitnessKindV1::COUNT = SyntaxAnalyzerWitnessKindV1::ALL.len()

LiveConstructionRowKindV1 = Request | OrdinaryCache | GatedCache

SourceReferenceKindV1 = generated discriminant of RawSourceReferenceV1
SourceReferenceKindV1::ALL = generated in RawSourceReferenceV1 body-sum order
SourceReferenceKindV1::COUNT = SourceReferenceKindV1::ALL.len()

SourceTypedOwnerKindV1 =
  ExpectedObject | ExpectedInput | ExpectedUse | ExpectedResult |
  ExpectedTargetHandle |
  ExpectedTypedValueOrigin | CanonicalTypedValue |
  ExpectedPhaseCoercionOrigin | CanonicalPhaseCoercion |
  ExpectedTypeUseInstance | CanonicalNormalizedType | ExpectedTypeMember |
  ExpectedTriIntent | ExpectedTriDriver | ExpectedTriDriverUpdate |
  ExpectedTriRead |
  ExpectedStaticCompositeProjection | ExpectedStaticCompositeStride |
  ExpectedControlUnit | ExpectedRegion | ExpectedPoint | ExpectedEdge |
  ExpectedRoot | ExpectedAction | ExpectedGate | ExpectedDecision |
  ExpectedObserver | ExpectedRuntimeSite | ExpectedDynamicPlan |
  ExpectedRuntimeCallInstance | ExpectedRuntimeCallGraph |
  ExpectedRuntimeCallActual | ExpectedRuntimeCallWriteback |
  RuntimeFunctionSpecialization | RuntimeFunctionProgram |
  ExpectedRuntimeProgramObject | ExpectedRuntimeProgramInput |
  ExpectedRuntimeProgramTarget |
  ExpectedForFold | PhaseNodeFact | VerifiedGatedKey

SourceFoldTypedOwnerKindV1 =
  ExpectedUse | ExpectedResult | ExpectedRegion | ExpectedPoint |
  ExpectedEdge | ExpectedAction | ExpectedValueOccurrence |
  ExpectedDynamicAddressPlan | ExpectedRecurrence | ExpectedEffect

SourceResourceWaveV1 =
  DecodeMaterialize | RawTopology | SyntaxFlatten | WitnessMap | JointSemantic |
  RuntimeExecutionLineage | ExpectedGraph | NodeReplay | Provenance |
  Classification | OutputCommit

SourceResourceSemanticsV1 = CeloxSourceV0_20

SourceResourceDemandIdV1
  semantics: SourceResourceSemanticsV1
  wave_serial: checked full-width u64
  wave: SourceResourceWaveV1
  resource: SourceResourceKindV1

SourceResourceSiteIdV1
  demand: SourceResourceDemandIdV1
  grow_ordinal_within_wave: checked u32

SourceResourceKindV1 =
  PrivateRawSyntaxRows(PrivateRawSyntaxTableKindV1) |
  PrivateRawSyntaxPoolEntries(PrivateRawSyntaxPoolKindV1) |
  PrivateRawMagnitudeRows | PrivateRawMagnitudeBytes |
  PrivateRawTraversalRows |
  EnvironmentLineageRows |
  RuntimeExecutionLineageWitnessRows |
  RuntimeExecutionLineageWitnessPoolEntries(
    SyntaxRuntimeSourceExecutionLineagePoolKindV1) |
  ProducerWitnessRows |
  ProducerWitnessPoolEntries(SyntaxAnalyzerWitnessPoolKindV1) |
  ProposalRows(SourceProposalTableKindV1) |
  ProposalPoolEntries(SourceProposalPoolKindV1) |
  RawOwnerBits(SourceOwnershipArenaKindV1) |
  RawReferenceScratchRows | RawOrderScratchRows |
  SyntaxKeyMapRows | WitnessKeyMapRows | ProposalKeyMapRows |
  ExpectedObjectRows | ExpectedInputRows | ExpectedTargetHandleRows |
  ExpectedValueUseRows | ExpectedValueResultRows |
  PhaseTypedValueOriginRows | PhaseCoercionOriginRows |
  SourcePhaseValueRows | SourcePhaseBitsRows |
  SourcePhaseBitPlaneWords | SourcePhaseCoercionRows |
  RuntimeFunctionRows(SourceRuntimeFunctionArenaKindV1) |
  ExpectedControlRows(SourceExpectedControlArenaKindV1) |
  ExpectedForFoldRows(SourceExpectedFoldArenaKindV1) |
  ExpectedTriRows(SourceExpectedTriArenaKindV1) |
  StaticCompositeRows(SourceStaticCompositeArenaKindV1) |
  PhaseNodeFactRows |
  VerifiedSourceRows(SourceProposalTableKindV1) |
  VerifiedSourcePoolEntries(SourceProposalPoolKindV1) |
  NodeClassificationRows | OrdinaryIndexNodes | PreparedRootRefs

SourceOwnershipArenaKindV1 =
  PrivateRawSyntaxRows(PrivateRawSyntaxTableKindV1) |
  PrivateRawSyntaxPoolEntries(PrivateRawSyntaxPoolKindV1) |
  PrivateRawMagnitudeRows | PrivateRawMagnitudeBytes |
  EnvironmentLineage | RuntimeExecutionLineageWitnessRows |
  RuntimeExecutionLineageWitnessPoolEntries(
    SyntaxRuntimeSourceExecutionLineagePoolKindV1) |
  ProducerWitnessRows |
  ProducerWitnessPoolEntries(SyntaxAnalyzerWitnessPoolKindV1) |
  ProposalRows(SourceProposalTableKindV1) |
  ProposalPoolEntries(SourceProposalPoolKindV1)

SourceExpectedControlArenaKindV1 =
  Unit | Region | Point | Edge | Root | Action | Gate | GateResultMerge |
  Decision | DecisionResultMerge | Observer | ObserverOccurrence |
  RuntimeEventSite | DynamicAddressPlan

SourceRuntimeFunctionAccessArenaKindV1 =
  ProgramReadDomain | ProgramWriteDomain |
  ProgramReadBinding | ProgramWriteBinding | ProgramEffectPublication |
  CallSetupReadDomain | CallTargetReadDomain |
  CallNestedReadDomain | CallNestedWriteDomain |
  CallNestedReadBinding | CallNestedWriteBinding |
  CallNestedEffectPublication | CallCopyoutWriteDomain |
  CallCombinedReadDomain | CallCombinedWriteDomain |
  CallCombinedReadBinding | CallCombinedWriteBinding |
  CallCombinedEffectPublication

SourceRuntimeFunctionInvokeDomainAccessRowV1 =
  SourceScoped(SourceWriteDomainId) |
  ProgramScoped(RuntimeFunctionProgramDomainRefV1)
SourceRuntimeFunctionInvokeBindingAccessRowV1 =
  SourceScoped(SourceBindingId) |
  ProgramScoped(RuntimeFunctionProgramBindingRefV1)
SourceRuntimeFunctionInvokeEffectAccessRowV1 =
  SourceScoped(SourceEffectStreamId, publication kind) |
  ProgramScoped(RuntimeFunctionProgramEffectRefV1, publication kind)

SourceRuntimeFunctionAccessArenaPayloadV1 =
  ProgramReadDomain(RuntimeFunctionProgramDomainRefV1) |
  ProgramWriteDomain(RuntimeFunctionProgramDomainRefV1) |
  ProgramReadBinding(RuntimeFunctionProgramBindingRefV1) |
  ProgramWriteBinding(RuntimeFunctionProgramBindingRefV1) |
  ProgramEffectPublication(RuntimeFunctionProgramEffectRefV1,
                           publication kind) |
  CallSetupReadDomain(SourceRuntimeFunctionInvokeDomainAccessRowV1) |
  CallTargetReadDomain(SourceRuntimeFunctionInvokeDomainAccessRowV1) |
  CallNestedReadDomain(SourceRuntimeFunctionInvokeDomainAccessRowV1) |
  CallNestedWriteDomain(SourceRuntimeFunctionInvokeDomainAccessRowV1) |
  CallNestedReadBinding(SourceRuntimeFunctionInvokeBindingAccessRowV1) |
  CallNestedWriteBinding(SourceRuntimeFunctionInvokeBindingAccessRowV1) |
  CallNestedEffectPublication(SourceRuntimeFunctionInvokeEffectAccessRowV1) |
  CallCopyoutWriteDomain(SourceRuntimeFunctionInvokeDomainAccessRowV1) |
  CallCombinedReadDomain(SourceRuntimeFunctionInvokeDomainAccessRowV1) |
  CallCombinedWriteDomain(SourceRuntimeFunctionInvokeDomainAccessRowV1) |
  CallCombinedReadBinding(SourceRuntimeFunctionInvokeBindingAccessRowV1) |
  CallCombinedWriteBinding(SourceRuntimeFunctionInvokeBindingAccessRowV1) |
  CallCombinedEffectPublication(SourceRuntimeFunctionInvokeEffectAccessRowV1)

For each `SourceRuntimeFunctionAccessArenaKindV1` variant, the corresponding
same-named payload variant above is its sole physical row type. A call row's
`SourceScoped`/`ProgramScoped` tag must equal its owning
`SourceRuntimeFunctionInvokeOwnerTargetV1` and access-summary tag. Program
`Frame` refs never appear in a source-scoped boundary row; only exact
`CapturedOuter` refs map there.

SourceRuntimeFunctionArenaKindV1 =
  Specialization | Program | Formal | Local | Object | Input |
  ProgramPreparedTargetHandle |
  Binding | Value | CallResult |
  ControlRegion | ControlPoint | ControlEdge |
  Action | ActionOperand | ActionResult |
  AccessSummaryRows(SourceRuntimeFunctionAccessArenaKindV1) |
  NestedCallSite | NestedCallActual | NestedCallWriteback |
  RetainedForFoldTemplate |
  CallInstance | CallGraph | CallActual | CallSetupOccurrence |
  CallProgramOccurrence | CallInvoke |
  CallInvokeOperandRole | CallInvokeResultRole |
  CallPredecessor | CallExit | CallWriteback | CallPreparedTargetHandle

SourceExpectedFoldArenaKindV1 =
  Template | Use | Result | Region | Point | Edge | Action |
  ValueOccurrence | PreparedTargetHandle | DynamicAddressPlan |
  State | Recurrence | Effect

SourceExpectedTriArenaKindV1 = Intent | Driver | DriverUpdate | Read

SourceStaticCompositeArenaKindV1 = Projection | Stride

PrivateRawSyntaxTableKindV1::ALL = generated authoritative body-sum order from
  source-semantic-inputs.md
PrivateRawSyntaxTableKindV1::COUNT = PrivateRawSyntaxTableKindV1::ALL.len()
PrivateRawSyntaxPoolKindV1::ALL = generated authoritative body-sum order from
  source-semantic-inputs.md
PrivateRawSyntaxPoolKindV1::COUNT = PrivateRawSyntaxPoolKindV1::ALL.len()
SyntaxRuntimeSourceExecutionLineagePoolKindV1::ALL = [FormalTypeContent]
SyntaxRuntimeSourceExecutionLineagePoolKindV1::COUNT = 1
SyntaxAnalyzerWitnessPoolKindV1::ALL = generated from the closed
  SyntaxAnalyzerWitnessPoolEntryV1 body sum in written pool-kind order
SyntaxAnalyzerWitnessPoolKindV1::COUNT =
  SyntaxAnalyzerWitnessPoolKindV1::ALL.len()

SourceOwnershipArenaKindV1::ALL = const expansion in written outer order;
  every payload variant expands through the authoritative body-sum `ALL` of
  its payload enum
SourceOwnershipArenaKindV1::COUNT = SourceOwnershipArenaKindV1::ALL.len()
SourceExpectedControlArenaKindV1::ALL =
  [Unit, Region, Point, Edge, Root, Action, Gate, GateResultMerge,
   Decision, DecisionResultMerge, Observer, ObserverOccurrence,
   RuntimeEventSite, DynamicAddressPlan]
SourceExpectedControlArenaKindV1::COUNT =
  SourceExpectedControlArenaKindV1::ALL.len()
SourceRuntimeFunctionAccessArenaKindV1::ALL =
  [ProgramReadDomain, ProgramWriteDomain,
   ProgramReadBinding, ProgramWriteBinding, ProgramEffectPublication,
   CallSetupReadDomain, CallTargetReadDomain,
   CallNestedReadDomain, CallNestedWriteDomain,
   CallNestedReadBinding, CallNestedWriteBinding,
   CallNestedEffectPublication, CallCopyoutWriteDomain,
   CallCombinedReadDomain, CallCombinedWriteDomain,
   CallCombinedReadBinding, CallCombinedWriteBinding,
   CallCombinedEffectPublication]
SourceRuntimeFunctionAccessArenaKindV1::COUNT = 18
SourceRuntimeFunctionArenaKindV1::ALL = const expansion in written outer order;
  AccessSummaryRows expands once for each exact
  SourceRuntimeFunctionAccessArenaKindV1::ALL entry
SourceRuntimeFunctionArenaKindV1::COUNT =
  SourceRuntimeFunctionArenaKindV1::ALL.len()
SourceExpectedFoldArenaKindV1::ALL =
  [Template, Use, Result, Region, Point, Edge, Action, ValueOccurrence,
   PreparedTargetHandle, DynamicAddressPlan, State, Recurrence, Effect]
SourceExpectedFoldArenaKindV1::COUNT =
  SourceExpectedFoldArenaKindV1::ALL.len()
SourceExpectedTriArenaKindV1::ALL = [Intent, Driver, DriverUpdate, Read]
SourceExpectedTriArenaKindV1::COUNT = SourceExpectedTriArenaKindV1::ALL.len()
SourceStaticCompositeArenaKindV1::ALL = [Projection, Stride]
SourceStaticCompositeArenaKindV1::COUNT =
  SourceStaticCompositeArenaKindV1::ALL.len()

SourceResourceKindV1::ALL = the stored const-expanded flat array obtained by
  visiting SourceResourceKindV1 in written outer order and replacing every
  payload variant at its position by one fully applied value for every entry
  of that payload enum's `ALL`; nested two-payload variants use left-major then
  right-minor written order
SourceResourceKindV1::COUNT = SourceResourceKindV1::ALL.len()
SourceResourceCountV1 = checked full-width u64 logical occupancy/delta
SourceResourcePlanV1 =
  [SourceResourceCountV1; SourceResourceKindV1::COUNT]
SourceResourceLayoutV1 =
  the owned tuple whose ordinal-i field is the exclusive physical source-local
  arena or reusable scratch buffer named by SourceResourceKindV1::ALL[i]

SourceAggregateResourceKindV1 =
  SourceLocal(SourceResourceKindV1) |
  TypedConstant(TypedConstantResourceKindV1)
SourceAggregateResourceKindV1::ALL =
  SourceLocal(each SourceResourceKindV1::ALL value in order), then
  TypedConstant(each TypedConstantResourceKindV1::ALL value in order)
SourceAggregateResourceKindV1::COUNT =
  SourceResourceKindV1::COUNT + TypedConstantResourceKindV1::COUNT
SourceAggregateResourcePlanV1 =
  [SourceResourceCountV1; SourceAggregateResourceKindV1::COUNT]
SourceAggregateResourceLayoutV1 =
  { source_local: SourceResourceLayoutV1,
    typed_constant: TypedConstantResourceLayoutV1 }
```

Error routing is a generated check-site property, never a constructor choice:

| Check-site class | Exact route |
| --- | --- |
| syntax-key/tag/range/order/pool ownership, duplicate producer key, or key-to-private-row mapping failure | `SourceLocal(SOURCE.AGGREGATE_WITNESS)` with `ProducerWitness`/`ProducerWitnessPoolEntry` |
| successfully mapped type/generic/constant expected row is missing | `TypedConstant(CONST.AGGREGATE_OUTPUT)` with the complete typed owner/context |
| successfully mapped type/generic/constant row is extra | `TypedConstant(CONST.AGGREGATE_ORPHAN)` with the complete typed owner/context |
| successfully mapped type/generic/constant content, dependency, coercion, specialization, value, or certificate summary disagrees | `TypedConstant(CONST.AGGREGATE_WITNESS)` with the complete typed owner/context |
| runtime execution-lineage witness topology/content | `SourceLocal(SOURCE.EXECUTION_*)` |
| source proposal, source-only witness, source expected graph/node/provenance/classification | the exact `SourceLocal` rule and owner for that source check site |
| typed resource count/plan/ID/storage failure | nested `TypedConstantErrorV1` with its `TypedConstantResourceDemandIdV1`/`TypedConstantResourceSiteIdV1` |
| source-local resource count/plan/ID/storage failure | `SourceLocal` with `SourceResourceDemandIdV1`/`SourceResourceSiteIdV1` |

`WITNESS_ROUTE_META_V1` is an exhaustive generated match over the same
`SyntaxAnalyzerWitnessRowV1` body sum which generates
`SyntaxAnalyzerWitnessKindV1`. Every current mapped type/generic/constant
variant is `TypedSemantic` after successful mapping; a source-only variant may
be `SourceLocal` only when its body-sum declaration explicitly carries that
metadata. Adding a row variant without route metadata fails const generation.
There is no default arm and no rule which retries a nested typed failure as a
source-local witness failure.

Precedence is structural framing/syntax/environment/witness topology and
mapping first, then the canonical joint typed-semantic scheduler, then runtime
execution-lineage comparison, then source proposal/expected-graph/node/
provenance/classification comparison. The first failure returns immediately.
A later outer check cannot replace, flatten, or compete with a nested typed
failure.

One fully applied `SourceResourceKindV1` value names exactly one physical
source-local growable arena or reusable scratch buffer, and every field of
`SourceResourceLayoutV1` has exactly one such value. The nested arena-kind enums
distinguish physical storage; they are not diagnostic group labels. The source-
local owner includes the one authoritative private raw syntax tables/pools,
magnitude rows/bytes, iterative syntax-flattener rows, raw owner bitmaps, and
raw reference/order scratch. The typed verifier receives checked borrowed
views of those arenas after outer topology succeeds and cannot allocate, grow,
clear, or retag them.

The source-local owner also exclusively owns `SourcePhaseValueRows`,
`SourcePhaseBitsRows`, `SourcePhaseBitPlaneWords`, and
`SourcePhaseCoercionRows`, respectively the physical
`VerifiedTypedValueArena<SourcePhase>` value rows, its bits descriptors, their
payload/mask word storage, and `VerifiedPhaseCoercionArena<SourcePhase>` rows.
They are not aliases of typed-constant `PersistentValueRows`,
`PersistentBitsRows`, `PersistentBitPlaneWords`, or `CoercionRows`.
Conversely, `TypedConstantResourceKindV1::SourceProjectionRows` is only the
typed root/content-to-source-ID relation and owns none of those source-phase
payload arenas.

`SourceAggregateResourceLayoutV1` is the disjoint physical union. No field may
be reachable through both `SourceLocal(kind)` and `TypedConstant(kind)`, and no
allocator-backed proof-path collection may be absent from both layouts. Its
plan is the exact concatenation of the source-local and typed plans in
`SourceAggregateResourceKindV1::ALL` order. Each source wave indexes
`SourceResourcePlanV1`; each typed task indexes its independent
`TypedConstantResourcePlanV1`; the aggregate driver may view both only through
the concatenated plan and cannot translate one resource tag into the other.

The runtime-function layout likewise has no coarse call cache. Specialization,
program, formal/local/object/input/value rows, program prepared-target rows,
program nested-call actual/writeback rows, control/action rows, call
instance/graph/actual/setup/program-occurrence/invoke/predecessor/exit/
writeback rows, call prepared-target rows, and both invoke-role pools are
distinct arenas. `ProgramPreparedTargetHandle` is in total physical bijection
with `RuntimeFunctionProgramPreparedTargetHandleRowV1`; nested actuals and
writebacks reference that same ID. `CallPreparedTargetHandle` is the disjoint
call-scoped projection and cannot substitute for it. Outer expected targets
and fold targets live only in `ExpectedTargetHandleRows` and
`ExpectedForFoldRows(PreparedTargetHandle)` respectively. No handle row is
reachable through two resource tags. `AccessSummaryRows(kind)` expands to the eighteen actual persistent
access pools in `SourceRuntimeFunctionAccessArenaKindV1::ALL`; impossible
component/kind pairs have neither a tag nor a zero-length placeholder arena.

An optional byte adapter's exact closed descriptor maps each validated section
tag directly to one source-local resource before `DecodeMaterialize`; there is
no generic decoded-row/byte arena or arbitrary numeric section tag. The mixed
producer-witness row slice is the one `ProducerWitnessRows` arena, while every
execution-lineage pool, witness pool, proposal table/pool, source runtime-
function/control/fold/Tri/static arena, and verified output table/pool has its
own fully applied value. `SyntaxRuntimeLocalScopeV1::ForFold` references the
already verified template resource and creates no execution-lineage or per-
iteration arena.

`ResourceDemand` is used only when checked demand/count/byte arithmetic or the
eventual dense ID namespace is unrepresentable before a grow is attempted. Its
`SourceResourceDemandIdV1` retains the exact semantics, checked full-width wave
serial, wave kind, and fully applied source-local resource. `ReservationSite`
is used only for failure of the one actual `try_reserve_exact` call and retains
that complete demand plus the checked grow ordinal. Neither may drop the wave
serial, substitute an aggregate/typed resource tag, or invent a site for a
pre-grow representability failure. Both use `Capacity`; substituting a table
row, generic allocator message, or one owner variant for the other is invalid.
`ProducerWitness` and `ProposalRow` locate a
structured row; their pool-entry variants locate the exact owned range entry.
`FoldTypedOwner.template` names the aggregate-global expected ForFold-template
row and `local` is the checked local ordinal in the named table; neither number
is flattened into the other. Tri and static-composite typed owners use their
aggregate-global canonical expected-row/pool-entry ordinal. A failure wholly
inside constant proof construction remains the nested `TypedConstantErrorV1`;
`ExpectedTypedValueOrigin`/`CanonicalTypedValue` are used only by the later
source proposal/node/provenance comparison against that completed output. The
corresponding phase-coercion owner pair follows the same origin-versus-interned-
content rule.
`ReferenceKind` is present only after both tags are valid closed tags but the
field requires a different family; an unknown numeric tag uses `Tag`.

The logical `rule`/`phase()`/`owner`/`context` view is exactly
`SourceAggregateLocalErrorV1`, the payload of `SourceLocal`; only
`rule`/`owner`/`context` are stored and `phase()` is derived from `RULE_META`.
It covers framing and source-owned preparation failures. It is not the
representation of an error returned by the independently verified
typed-constant relation.

`TypedConstant` stores the complete allocation-free
[`TypedConstantErrorV1`](./typed-constant-evaluation.md) inline, as required by
the [source semantic aggregate boundary](./source-semantic-inputs.md). Wrapping
it is an infallible move. The wrapper must not call `to_string`, `format!`, or
`Display`; flatten it into a `SourceAggregateRuleIdV1`; discard or rewrite its
phase/owner/context; copy source text; or allocate a `Box`. Equality and any
machine encoding retain the exact nested typed-constant rule, phase, owner, and
context. Its own stable rule namespace and deterministic precedence therefore
survive the aggregate boundary unchanged.

Neither variant contains a `String`, `Vec`, copied source excerpt, or allocator
error message. `Display` formats the selected variant lazily only after the
machine-readable error has crossed the verifier boundary. Diagnostic source
excerpts, if later desired, are looked up under a separate bounded diagnostic
policy and are not part of either error payload.

The initially reserved stable `SourceAggregateLocalErrorV1` rule IDs are:

| Rule ID | Meaning |
| --- | --- |
| `WIRE.HEADER_TRUNCATED` | the 40-byte header is incomplete |
| `WIRE.MAGIC` | the envelope magic differs |
| `WIRE.FRAMING_VERSION` | framing version is not exactly supported |
| `WIRE.AGGREGATE_KIND` | aggregate kind differs from the selected descriptor |
| `WIRE.SCHEMA_VERSION` | schema version differs from the selected descriptor |
| `WIRE.LENGTH_OVERFLOW` | checked encoded-length arithmetic failed |
| `WIRE.TOTAL_LENGTH` | truncation or trailing bytes violate exact length |
| `WIRE.DIRECTORY_SIZE` | directory byte length is not `count * 40` |
| `WIRE.SECTION_TAG` | a required tag is missing or an unknown tag is present |
| `WIRE.SECTION_ORDER` | section tags are not strictly increasing |
| `WIRE.SECTION_KIND` | section kind differs from the schema |
| `WIRE.SECTION_FLAGS_ZERO` | flags or reserved fields are nonzero |
| `WIRE.SECTION_ROW_WIDTH` | encoded row width differs from the schema |
| `WIRE.SECTION_LENGTH` | row count, width, and payload length disagree |
| `WIRE.SECTION_CONTIGUOUS` | sections do not exactly partition the payload |
| `WIRE.SCALAR_TAG` | a closed enum tag is unknown |
| `WIRE.SCALAR_BOOL` | a boolean is not zero or one |
| `WIRE.ID_REPRESENTABLE` | a raw integer cannot enter its eventual namespace |
| `WIRE.RANGE` | a checked pool range is outside its pool |
| `WIRE.POOL_CANONICAL` | pool ranges overlap, have a gap, alias, or lack coverage |
| `WIRE.UTF8` | a string payload is not UTF-8 |
| `WIRE.MAGNITUDE_CANONICAL` | an unsigned magnitude is not minimally encoded |
| `WIRE.STORAGE_AVAILABLE` | a planned private allocation failed |

The non-wire `SourceAggregateRuleIdV1` registry is also closed:

| Namespace | Stable IDs |
| --- | --- |
| Syntax adapter/topology | `SOURCE.SYNTAX_VERSION`, `SOURCE.SYNTAX_VARIANT`, `SOURCE.RAW_REFERENCE`, `SOURCE.RAW_RANGE`, `SOURCE.RAW_PARTITION`, `SOURCE.RAW_OWNER`, `SOURCE.RAW_ORDER`, `SOURCE.DENSE_ID_REPRESENTABLE` |
| Runtime source execution lineage | `SOURCE.EXECUTION_PARENT`, `SOURCE.EXECUTION_ORDER`, `SOURCE.EXECUTION_ROOT`, `SOURCE.EXECUTION_CALL`, `SOURCE.EXECUTION_SPECIALIZATION`, `SOURCE.EXECUTION_LOCAL_SCOPE`, `SOURCE.EXECUTION_BIJECTION`, `SOURCE.RUNTIME_FUNCTION_RECURSION` |
| Graph/node replay | `GRAPH.CHILD_EXISTS`, `GRAPH.CHILD_PRECEDES_OWNER`, `GRAPH.OPERAND_ARITY`, `GRAPH.OPERAND_ORDER`, `GRAPH.NODE_RECIPE`, `GRAPH.EXPECTED_BIJECTION`, `GRAPH.REACHABLE`, `GRAPH.ORDINARY_DUPLICATE` |
| Semantic object/input | `INPUT.OBJECT_BIJECTION`, `INPUT.TYPE`, `INPUT.ACCESS`, `INPUT.DIMENSION`, `INPUT.INDEX_ROLE`, `INPUT.PART_GEOMETRY`, `INPUT.SIGNEDNESS`, `INPUT.DOMAIN`, `INPUT.DEFAULT_ROLE`, `INPUT.COMPLETE` |
| Source coercion | `COERCION.ROLE`, `COERCION.WIDTH`, `COERCION.SIGN`, `COERCION.DOMAIN`, `COERCION.VALUE_CLASS`, `COERCION.RESULT` |
| Control/ForFold | `CONTROL.UNIT`, `CONTROL.REGION`, `CONTROL.POINT`, `CONTROL.EDGE`, `CONTROL.ACTION`, `CONTROL.ROOT`, `CONTROL.EXPECTED_BIJECTION`, `FOR_FOLD.RANGE`, `FOR_FOLD.STEP`, `FOR_FOLD.STATE`, `FOR_FOLD.EFFECT`, `FOR_FOLD.CONTROL`, `FOR_FOLD.RESULT` |
| Provenance/classification | `PROVENANCE.SITE`, `PROVENANCE.OPERAND`, `PROVENANCE.DEFINITION`, `PROVENANCE.PRODUCER`, `PROVENANCE.GATE`, `PROVENANCE.DECISION`, `PROVENANCE.OBSERVER`, `PROVENANCE.RUNTIME_SITE`, `CLASSIFY.GATED_KEY`, `CLASSIFY.GATED_BIJECTION`, `CLASSIFY.ORDINARY_BIJECTION`, `CLASSIFY.CONSTRUCTION_REQUEST`, `CLASSIFY.CONSTRUCTION_CACHE`, `CLASSIFY.TOTAL` |
| Aggregate/resource | `SOURCE.AGGREGATE_ORPHAN`, `SOURCE.AGGREGATE_OUTPUT`, `SOURCE.AGGREGATE_WITNESS`, `SOURCE.RESOURCE_COUNT_REPRESENTABLE`, `SOURCE.RESOURCE_PLAN_REPRESENTABLE`, `SOURCE.RESOURCE_ID_EXHAUSTED`, `SOURCE.RESOURCE_STORAGE_AVAILABLE` |

`PrivateRawSyntaxTableKindV1` and `PrivateRawSyntaxPoolKindV1` are the one
authoritative comprehensive registries in
[`source-semantic-inputs.md`](./source-semantic-inputs.md). This document does
not define a coarse alias or subset. Each parser-derived private row and range
field maps to exactly one entry in those registries, and
`SourceResourceKindV1::{PrivateRawSyntaxRows,PrivateRawSyntaxPoolEntries}` uses
those exact tags.
The more specific typed/constant error row kind remains inside the nested
`TypedConstantErrorV1`.

Like that error, `SourceAggregateLocalErrorV1` has a private `RULE_META`
constructor deriving `phase()` and the allowed owner/context shape. Resource
count/plan/ID rules permit only `ResourceDemand(SourceResourceDemandIdV1)`;
wire/source-local storage failure permits only
`ReservationSite(SourceResourceSiteIdV1)`. Typed resource failures never enter
this metadata table and remain nested typed errors. `SOURCE.AGGREGATE_WITNESS`
permits `ProducerWitness`/pool owners only for syntax-key topology and mapping,
and permits proposal or independently derived typed owners only for later
source-owned comparison against an already completed typed output. It cannot
represent mapped type/generic/constant semantic completeness or content
mismatch;
execution-lineage rules permit only `ExecutionLineageWitness`,
`ExecutionLocalScope`, `ExecutionPoolEntry`, or the expected runtime-call/fold
typed owner;
the two construction-classification rules permit only `LiveConstruction` or
the compared `RawNode`. Each rejecting branch is represented in one exhaustive
source check-site table, and namespace order in the table above is its
semantic-phase order, not lexicographic string order.

The generated discriminant fixtures compare `SyntaxAnalyzerWitnessKindV1::ALL`
one-for-one with `SyntaxAnalyzerWitnessRowV1` and
`SourceReferenceKindV1::ALL` one-for-one with `RawSourceReferenceV1`. A new body
variant without generated discriminant, route metadata, error-owner support,
and malformed-tag fixture is a compile failure; neither discriminant may be a
handwritten subset.

Node and semantic verification retain their more specific stable IDs, such as
`GRAPH.CHILD_EXISTS`, `GRAPH.CHILD_PRECEDES_OWNER`, `INPUT.*`, `COERCION.*`,
and `FOR_FOLD.*`, with a `RawNode` owner until aggregate typing succeeds.
Errors never select a compatibility adapter, legacy allocator, retry, partial
artifact, or correctness fallback.

## Required adversarial fixtures

The raw aggregate verifier fixtures cover malformed logical tables directly.
Before any optional persistent schema descriptor is added, the private byte
adapter must additionally cover:

- truncation at every byte offset of a valid nontrivial test envelope;
- trailing bytes and concatenated envelopes;
- wrong magic/kind/version, nonzero flags/reserved bytes, unknown/duplicate/
  out-of-order tags, and wrong row width;
- `u64::MAX` counts, count/width and offset/length overflow, and host-`usize`
  conversion failure;
- section gaps, overlaps, reversed physical order, and unowned payload bytes;
- pool range gaps, overlaps, aliases, noncanonical empty ranges, and incomplete
  coverage;
- invalid booleans/enums/UTF-8, duplicate or unsorted strings, leading-zero
  integer magnitudes, and width-exceeding value/mask bits;
- every producer-witness key variant with a wrong occurrence/entity kind,
  unchecked environment-lineage reference, duplicate syntax key, wrong
  cross-witness key, and every dedicated witness-pool ownership violation;
- one shared runtime-function body instantiated at two outer call sites, its
  same nested call beneath both parents, and equal type specializations which
  share a program but retain distinct root/runtime-call lineage rows;
- the same retained-ForFold template paired with each owning call lineage as a
  local scope, nested fold syntax, and arbitrarily many runtime iterations,
  none of which adds a ForFold/iteration lineage row;
- `Body`/`ForFold` local-scope substitution, a sibling/noncontained template
  syntax key, and any proposed iteration/backedge ordinal in identity;
- execution-lineage missing/extra/duplicate roots or calls, self/forward/wrong
  parents, reordered formal types, wrong generic/type specialization,
  analyzer/private-ID substitution, a recursive call cycle, and a finite
  producer prefix of that cycle;
- every proposal reference-family substitution, including attempts to encode
  an expected use/result as a proposal row, a typed constant by proof lineage
  without matching content, a runtime call instance/program as a source row or
  private ID, or a Tri intent without its exact modifier key;
- const/compile fixtures which expand all 26
  `SourceProposalTableKindV1::ALL` entries and all 89
  `SourceProposalPoolKindV1::ALL` entries, prove their ordinal/`COUNT` equality,
  construct exactly one row/entry payload per discriminant, and fail generation
  when a checked field or range lacks an explicit schema mapping;
- compile-fail fixtures for every fixed-kind proposal field, including a Root
  in an Action field, an Action in a ControlPoint edge field, a non-ForFold
  template in a fold-local reference, and use/definition row substitution;
- runtime-call fixtures separating explicit input arguments from
  `DeclaredDefault { formal, default_expression }`, rejecting a default for an
  output/inout or present input, and proving that actual/setup/program/
  predecessor/writeback ranges are owned only by the call-instance row while
  outer/fold actions own only their two role ranges;
- repeated input uses which canonicalize to one input, a forged non-first
  `SyntaxCanonicalInputKeyV1`, constant and derived typed-value origins with
  equal content sharing one value ID, and a derived origin which incorrectly
  attempts to require or reuse an analyzer constant witness;
- distinct coercion origins sharing canonical content, wrong coercion
  origin/slot roles, a static-composite key on a non-static input, and a member
  key whose spelling matches but declaration identity differs, plus distinct
  alias/type-use keys sharing one canonical normalized type;
- a gap/overlap/orphan/wrong-owner fixture for every
  `SourceProposalPoolKindV1::ALL` entry, including runtime-call-instance,
  nested decision, and ForFold row ranges;
- missing, self, and forward child IDs in every node-reference role;
- malformed accesses, input geometry, coercions, concat widths, canonical
  `ForFold` state rows, effect arguments, and arbitrary-width bounds/steps;
- equal raw muxes that later classify as ordinary/gated or as distinct gated
  owners, proving structural staging does not reject them prematurely;
- structurally valid but unexpected/unreachable nodes, proving only the later
  expected-graph relation rejects them;
- nonreciprocal/unsorted source adjacency and wrong occurrence operand arity,
  order, unit, or site when the complete provenance rows are added;
- live-builder missing/duplicate/reordered request rows, wrong hit/insert
  outcomes, ordinary/gated cross-placement, and missing/extra cache entries,
  while the same detached proposal succeeds without a live sidecar;
- const-exhaustive expansion of `SourceResourceKindV1::ALL`,
  `TypedConstantResourceKindV1::ALL`, and their ordered disjoint union
  `SourceAggregateResourceKindV1::ALL`, proving `COUNT`/plan/layout ordinal
  equality, one tag per physical field, no duplicate physical owner, and no
  allocator-backed layout field without a tag;
- one ownership fixture for every outer raw syntax/pool/magnitude/traversal/
  owner/ref/order arena, every source-phase value/bits/word/coercion arena, and
  every `SourceRuntimeFunctionAccessArenaKindV1::ALL` entry, proving none
  occurs in `TypedConstantResourceKindV1::ALL`;
- source resource count/plan/ID failures retaining exact semantics, nonzero
  full-width wave serial, wave, and resource in `SourceResourceDemandIdV1`, and
  each injected source reserve failure retaining that whole demand plus the
  exact grow ordinal in `SourceResourceSiteIdV1`; typed failures must retain the
  corresponding typed IDs inside the nested error instead;
- paired witness-routing fixtures: malformed key/range/order/ownership and
  unmappable keys return `SourceLocal(SOURCE.AGGREGATE_WITNESS)`, while a mapped
  missing/extra/content-mismatching type/generic/constant witness returns the
  exact nested `CONST.AGGREGATE_OUTPUT`/`ORPHAN`/`WITNESS`; no fixture may be
  accepted through both constructors or change route with traversal order;
- body-sum generation fixtures proving witness/reference `ALL` and `COUNT`,
  route metadata, and error-owner coverage change together with any new
  `SyntaxAnalyzerWitnessRowV1` or `RawSourceReferenceV1` variant;
- deterministic failure at every `try_reserve_exact` site, with no published
  output and an unchanged live builder/input owner;
- non-ZST exactly-one brand-token allocation, moves of prepared/frozen owners,
  same-artifact composition, cross-artifact rejection before indexing, and
  allocation-address reuse stress after prior owners are dropped;
- compile-fail lifetime fixtures where a branded handle outlives, is stored in,
  or prevents finishing/moving its session, and source/occurrence phase handles
  cannot be interchanged;
- serialization contains no brand, decoding identical bytes twice creates
  distinct brands, no issuer/global-counter state exists, and commit performs
  zero allocator calls; and
- iterative 100k/1M-node graphs plus large concat and `ForFold` pools.

Tests for a narrow fixture schema demonstrate decoder safety only. They are not
evidence that a real source artifact verifies, that lowering has switched, or
that the pinned Heliodor performance gate passes.

## Explicit non-goals

This boundary deliberately does not:

- assign or publish `SourceWireV1`; the production boundary is the explicitly
  versioned in-memory `RawSourceAggregateV1` above;
- define a partial accepted source language or silently omit unsupported HIR;
- expose an arena-only deserializer, freeze, planner, or lowering API;
- derive source input facts, coercions, truth semantics, or provenance from
  producer-supplied proof fields;
- classify a node as ordinary because it is absent from an incomplete gated
  registry;
- use serde or a legacy portable/cache type as current-wire verification;
- add a node, CFG, depth, payload, or iteration cutoff;
- retry allocation or fall back to a legacy representation;
- change symbolic evaluation, SIR/MIR lowering, regalloc, or backend output; or
- claim end-to-end performance from structural decoder measurements.

The first persistent source-wire descriptor is permitted only after every
source section and closed variant, all independently derived expected rows, the
ordinary/gated total classification, and the full aggregate prepare/commit
relation have been implemented together. It is not an implementation gate for
the raw aggregate verifier itself.
