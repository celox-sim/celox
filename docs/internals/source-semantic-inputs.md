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
VerylParserV0_20_1_UEscape1 parsed unit + exact tokens
  -> verifier-derived PrivateRawSemanticSyntaxV1 / RawTypedSourceHIR view
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

The syntax-flattening and first semantic arrow are aggregate-owned relations,
not a call which may trust a
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
(ExpectedTypedConstantExecutionOwnerV1 = SourceAggregate |
 FunctionTemplate(RawFunctionTemplateId),
RawConstExprOccurrenceId, VerifiedGenericEnvironmentId, proof role and typing
context) relation; it is not the raw occurrence index.

VerifiedRuntimeFunctionSpecializationId: checked dense ID for one exact
(RawFunctionTemplateId, verified generic environment, canonical formal
directions/types, optional return type) runtime program; it contains no input
value and is not a constant-function specialization ID.

ExpectedSourceRuntimeCallInstanceId: checked dense expected-graph ID for one
exact runtime call occurrence, execution-lineage/local-scope pair,
actual/target relation, and shared runtime specialization.

RuntimeSourceExecutionLineageId: checked dense persistent Root-or-RuntimeCall
lineage ID; it scopes instantiated function-body slots and never denotes a
runtime loop iteration.

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
  resolution_class: Memory | Environment |
                    StaticComposite(StaticCompositeProjectionRecipeId) |
                    DynamicOverlay(expected dynamic-plan ID)
  member_projection: ordered verified field IDs / checked flat offsets /
                     selected member type, or empty
  normalized access
  ordered runtime index roles
  selected_width
  result_signed
  result_positive_type
  result_static_domain: Bit | Logic
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

## Closed verifier-derived source HIR

The parsed AST is not itself a convenient proof table, but replacing it with
an analyzer `Module` would discard exactly the syntax ownership needed here.
The source aggregate therefore flattens the
`VerylParserV0_20_1_UEscape1` adapter tree into the
following private, borrowed, syntax-preserving relation. This relation is
derived by the verifier from parser nodes and token ranges; it is not a table
which the analyzer or SLT producer may populate. Every row retains its exact
`SyntaxOccurrenceKeyV1`, lexical scope, source coordinate, and owning row.
References below begin as untrusted full-width raw indices. The allocation-
free topology scan first verifies every endpoint, reference, owner, and
gap-free range without dereferencing one; only then may a checked private raw
ID/range view be constructed. Owned ranges are half-open ranges in one
specifically named pool.

Generic-environment path lineage uses one semantic-owned producer-facing role
sum. This `Root` is a generic-use path role; it is unrelated to the runtime
execution root role defined below:

```text
SyntaxEnvironmentLineageRoleV1 =
  Root |
  PathComponent {
    path: SyntaxOccurrenceKeyV1,
    component_ordinal: checked u32
  }
```

An environment-lineage row carries this role beside the exact optional
generic-use syntax for that row. `Root` names the surface owner's generic use.
`PathComponent` names exactly the indicated component of the retained qualified
path in canonical left-to-right component order, and the row's optional
generic-use syntax must be that component's own generic-use syntax rather than
the root's or the following component's. Missing generic syntax is explicit
`None`; an equal spelling or analyzer-resolved environment cannot substitute
for the `(path, component_ordinal, optional generic use)` identity. The physical
`RawEnvironmentLineageRowV1` comparison schema imports this enum as a generated
alias and must not redeclare another role sum.

```text
RawParsedUnitRowV1
  parsed-unit ordinal / exact file-resource lineage
  top_items: RawTopItemPool
  typed_hir_roots: RawTypedHIRRootPool
  runtime_event_anchors: RawRuntimeEventAnchorPool
  observer_anchors: RawObserverAnchorPool

RawTopItemRowV1 =
  Module(RawModuleId) | Interface(RawInterfaceId) |
  Package(RawPackageId) | Alias(RawTypeId) |
  Proto(RawProtoDeclarationId) | Function(RawFunctionTemplateId) |
  Import(RawImportId) | Bind(RawBindId) |
  Unsupported(RawUnsupportedTopItemKindV1)

RawModuleRowV1
  exact declared name / scope / generic-formal range / module type context
  ports: RawModulePortPool
  items: RawModuleItemPool

RawInterfaceRowV1
  exact declared name / scope / generic-formal range / module type context
  ports: RawInterfacePortPool
  items: RawInterfaceItemPool

RawPackageRowV1
  exact declared name / scope / generic-formal range
  proof_items: RawPackageItemPool

RawModulePortRowV1 / RawInterfacePortRowV1
  exact declaration ordinal
  direction: RawPortDirectionV1
  object: RawObjectId
  default: None | Expression(RawSourceExprId)

RawPortDirectionV1 =
  Input | Output | Inout |
  Modport(RawNameOccurrenceId) | Import(RawNameOccurrenceId)

RawModuleItemRowV1 =
  Object(RawObjectId) | StaticBinding(RawStaticBindingId) |
  Type(RawTypeId) | Function(RawFunctionTemplateId) |
  Instance(RawInstanceId) | ContinuousAssign(RawAssignmentId) |
  Process(RawProcessId) | Generate(RawGenerateId) |
  Import(RawImportId) | Alias(RawTypeId) | Bind(RawBindId) |
  Unsupported(RawUnsupportedUnitItemKindV1)

RawInterfaceItemRowV1 =
  Object(RawObjectId) | StaticBinding(RawStaticBindingId) |
  Type(RawTypeId) | Function(RawFunctionTemplateId) |
  Instance(RawInstanceId) | ContinuousAssign(RawAssignmentId) |
  Process(RawProcessId) | Generate(RawGenerateId) |
  Modport(RawModportId) | Import(RawImportId) | Alias(RawTypeId) |
  Bind(RawBindId) | Unsupported(RawUnsupportedUnitItemKindV1)

RawPackageItemRowV1 =
  StaticBinding(RawStaticBindingId) | Type(RawTypeId) |
  Function(RawFunctionTemplateId) | Import(RawImportId) |
  Alias(RawTypeId) | Unsupported(RawUnsupportedPackageItemKindV1)

RawProtoDeclarationRowV1 =
  Module { exact name / parameter_requirement: None | WithParameter,
           ports: RawProtoPortPool } |
  Interface { exact name / parameter_requirement: None | WithParameter,
              items: RawProtoInterfaceItemPool } |
  Package { exact name / items: RawProtoPackageItemPool }

RawProtoPortRowV1
  declaration ordinal / exact name / direction: RawPortDirectionV1
  type_use: RawTypeUseId

RawProtoInterfaceItemRowV1 =
  Object(RawObjectId with no initializer) |
  Const { exact name, type_use: RawTypeUseId } |
  Function(RawFunctionTemplateId classified Prototype) |
  Type { exact name, optional bound: RawTypeUseId } |
  Alias { kind: Module | Interface | Package,
          exact name, target: RawNameOccurrenceId } |
  Modport(RawModportId) | Import(RawImportId)

RawProtoPackageItemRowV1 =
  Const { exact name, type_use: RawTypeUseId } |
  Function(RawFunctionTemplateId classified Prototype) |
  Type { exact name, optional bound: RawTypeUseId } |
  Enum(RawTypeId) | StructUnion(RawTypeId) |
  Alias { kind: Module | Interface | Package,
          exact name, target: RawNameOccurrenceId } |
  Import(RawImportId)

RawBindRowV1
  exact source coordinate / owning scope
  target: RawNameOccurrenceId with expected bind-target namespace
  component: RawInstanceId classified BoundInstance

RawModportRowV1
  exact interface owner / declared name / source coordinate
  items: RawModportItemPool
  default: None | Input | Output |
           Same(RawModportDefaultNamePool) |
           Converse(RawModportDefaultNamePool)

RawModportItemRowV1
  declaration ordinal / exact member name / direction: RawPortDirectionV1

RawModportDefaultNameRowV1
  source ordinal / exact interface-member name occurrence
```

Prototype ports/items and modport/default-name pools are gap-free in parser
order like every other syntax-owned pool. `Prototype` and `BoundInstance` are
closed declaration classes checked against the exact owning syntax; they are
not analyzer flags. A proto row contributes only namespace/type compatibility
proofs, a bind row contributes only the later hierarchy relation, and a
modport row contributes only interface exposure. None creates an executable
source value root by itself.

`Package`, `Proto`, type, constant, alias, import, and function rows can be
proof prerequisites without being executable units. Only `Module` and the
closed executable portion of `Interface` can own a source control unit. A
`Bind` or `Modport` is retained for the later hierarchy/interface relation but
does not silently create a value root. Public/private spelling, attributes,
and grouped declaration syntax remain exact parser lineage and never affect
row identity through an analyzer hash map.

The declaration, process, statement, target, and expression rows are closed:

```text
RawMemberRowV1
  owner: Struct(RawTypeId) | Union(RawTypeId)
  exact declared-name occurrence / declaration ordinal / source coordinate
  type_use: RawTypeUseId

RawStaticBindingRowV1
  exact declaration / owner scope / source coordinate
  kind: Const | Gen | Parameter | GenericConst | LocalConst | LocalGen
  type_source: RawTypeSource
  initializer: RawConstExprOccurrenceId

RawFunctionTemplateRowV1
  exact declaration / declared name / lexical owner and source coordinate
  generic_formals: RawGenericFormalPool
  ports: RawFunctionPortPool
  return_type: None | Some(RawTypeUseId)
  kind: Definition {
          source_body: RawBlockId,
          constant_projection_root: RawConstBlockId
        } |
        Prototype

RawFunctionPortRowV1
  exact function owner / declaration ordinal / declared name
  direction: RawPortDirectionV1
  type_use: RawTypeUseId
  default: None | Expression(RawSourceExprId)

`RawFunctionTemplateRowV1` and `RawFunctionPortRowV1` are the only shared
function declaration/port rows. The constant projection rows defined in
[`typed-constant-evaluation.md`](./typed-constant-evaluation.md) carry a total
inverse to the same `source_body`, block items, statements, and expressions;
they are a restricted verifier view, not a second function declaration or
parser body. `Prototype` has neither body and cannot be invoked without an
independently specified linking relation in another language profile. V1 has no
such relation: its runtime capability below rejects the prototype row itself and
never borrows an equal-named body. `Modport`/`Import` and every syntactic default
remain in the raw relation even when a particular constant/runtime capability
profile later expands or rejects them.

RawProcessRowV1
  owner executable unit / source ordinal
  kind: AlwaysComb |
        AlwaysFf { events: RawAlwaysEventPool } |
        Initial | Final
  body: RawBlockId

RawAlwaysEventRowV1
  exact source ordinal
  kind: Clock | Reset
  edge: Posedge | Negedge | Level
  value: RawSourceExprId

RawBlockRowV1
  exact lexical scope / owner process, control arm, or retained ForFold
  items: RawBlockItemPool

RawBlockItemRowV1 =
  LocalObject(RawObjectId) |
  LocalStaticBinding(RawStaticBindingId) | Statement(RawStatementId) |
  Unsupported(RawUnsupportedBlockItemKindV1)

RawStatementRowV1 =
  Assignment(RawAssignmentId) |
  If { clauses: RawIfClausePool, else_block: optional RawBlockId } |
  IfReset { then_block: RawBlockId, else_block: optional RawBlockId } |
  Decision(RawDecisionId) | RetainedForFold(RawRetainedForFoldId) |
  RuntimeCall(RawRuntimeCallId) | UserCall(RawSourceCallId) |
  Return(optional RawSourceExprId) | Break | Null |
  Unsupported(RawUnsupportedStatementKindV1)

RawIfClauseRowV1
  exact `if`/`else if` source ordinal
  condition: RawSourceExprId / body: RawBlockId

RawDecisionRowV1
  kind: Case { selector: RawSourceExprId,
               comparison: Exact4State | Wildcard } |
        Switch
  arms: RawDecisionArmPool
  default: optional RawBlockId

RawDecisionArmRowV1
  declaration ordinal / patterns: RawDecisionPatternPool / body: RawBlockId

RawDecisionPatternRowV1 =
  Expression(RawSourceExprId) |
  Range { low: RawSourceExprId, high: RawSourceExprId,
          inclusive: bool } |
  InsideSet(RawSourceExprPool) |
  Unsupported(RawUnsupportedPatternKindV1)

RawAssignmentRowV1
  owner continuous declaration or exact procedural statement
  timing: Continuous | Blocking | Nonblocking
  operator: Set | Add | Sub | Mul | Div | Rem |
            BitAnd | BitOr | BitXor |
            LogicShiftLeft | LogicShiftRight |
            ArithShiftLeft | ArithShiftRight
  targets: RawAssignmentTargetPool
  rhs: RawSourceExprId

RawAssignmentTargetRowV1
  source ordinal / target: RawTargetId

RawTargetRowV1 =
  Access(RawAccessPathId) |
  Concat(RawTargetPool) |
  Unsupported(RawUnsupportedTargetKindV1)

RawAccessPathRowV1
  exact base name occurrence
  selectors: RawSelectorPool

RawSelectorRowV1 =
  Member(exact name occurrence) |
  Index(RawSourceExprId) |
  Colon { high: RawSourceExprId, low: RawSourceExprId } |
  PlusColon { anchor: RawSourceExprId, width: RawSourceExprId } |
  MinusColon { anchor: RawSourceExprId, width: RawSourceExprId } |
  Step { anchor: RawSourceExprId, width: RawSourceExprId }

RawSourceExprRowV1 =
  IntegralLiteral(exact RawConstExprOccurrenceId) |
  StringLiteral(exact RawConstExprOccurrenceId) |
  TypeValue(RawTypeUseId) |
  Reference(RawAccessPathId) |
  Unary(RawUnaryOp, RawSourceExprId) |
  Binary(RawBinaryOp, left RawSourceExprId, right RawSourceExprId) |
  Cast { target: RawCastTargetId, operand: RawSourceExprId } |
  Conditional { condition, then_value, else_value: RawSourceExprId } |
  Concat(RawConcatPartPool) |
  Select { base: RawSourceExprId, selectors: RawSelectorPool } |
  ArrayConstructor(RawArrayItemPool) |
  StructConstructor { type_use: RawTypeUseId,
                      fields: RawConstructorFieldPool,
                      default: optional RawSourceExprId } |
  DecisionExpression(RawExpressionDecisionId) |
  InsideOutside { target: RawSourceExprId,
                  patterns: RawDecisionPatternPool, negated: bool } |
  PureSystemCall(RawPureSystemCallId) |
  UserCall(RawSourceCallId) |
  Unsupported(RawUnsupportedExpressionKindV1)

RawSourceExprTagV1 =
  the exact payload-free discriminant of every variant above, with the
  retained RawUnsupportedExpressionKindV1 subtag and no numeric catch-all

RawConcatPartRowV1
  value: RawSourceExprId / optional repeat: RawConstExprOccurrenceId

RawArrayItemRowV1 =
  Value(RawSourceExprId) |
  Repeat { value: RawSourceExprId, count: RawConstExprOccurrenceId } |
  Default(RawSourceExprId)

RawConstructorFieldRowV1
  exact field name / declaration ordinal / value: RawSourceExprId

RawSourceCallRowV1
  owner: Expression(SyntaxOccurrenceKeyV1) |
         Statement(SyntaxOccurrenceKeyV1)
  exact callee name and generic use / arguments: RawCallArgumentPool

RawCallArgumentRowV1
  source ordinal / optional named-formal occurrence
  exact expression: RawSourceExprId

RawPureSystemCallRowV1
  function: RawPureSystemFunctionV1 / arguments: RawCallArgumentPool

RawExpressionDecisionRowV1
  kind: Case { selector: RawSourceExprId,
               comparison: Exact4State | Wildcard } | Switch
  arms: RawExpressionDecisionArmPool
  default: optional RawSourceExprId

RawExpressionDecisionArmRowV1
  declaration ordinal / patterns: RawDecisionPatternPool
  result: RawSourceExprId

RawPureSystemFunctionV1 =
  Bits | Size | Clog2 | Onehot | Signed | Unsigned
```

An `ArgumentItem` owns exactly that one source expression. It does not contain
a producer-classified input value plus a second output target. After resolving
the exact function template and formal direction, the verifier either uses the
expression as a value or derives the one assignable-target projection from the
same retained syntax. Failure of that closed projection is an output/inout
actual error. The unused projection creates no expected read or write.

An expression decision uses the same selector, arm, pattern, and default
ownership as `RawDecisionRowV1`, except each arm/default names one expression
instead of a block. It is a distinct closed row sum, so a statement arm cannot
be substituted by an equal-coordinate expression arm. Every general source
expression also has at most one verifier-derived projection to the exact
`RawConstExprOccurrenceId` relation when a type-only or value proof demands
it. Absence of that projection means runtime-only; an analyzer `Comptime` bit
cannot manufacture it.

Instances and elaborative generate syntax are retained without accepting an
already expanded analyzer body as proof:

```text
RawInstanceRowV1
  exact instance declaration / module or interface name / generic use
  inputs: RawInstanceInputPool / outputs: RawInstanceOutputPool

RawInstanceInputRowV1
  declared port ordinal / exact port name / value: RawSourceExprId

RawInstanceOutputRowV1
  declared port ordinal / exact port name / targets: RawInstanceOutputTargetPool

RawInstanceOutputTargetRowV1
  source ordinal / target: RawTargetId

RawGenerateRowV1 =
  If { condition: RawConstExprOccurrenceId,
       then_items: RawGenerateItemPool,
       else_items: RawGenerateItemPool } |
  For { binding: RawStaticBindingId, range: RawGenerateRangeId,
        body: RawGenerateItemPool } |
  Block(RawGenerateItemPool) |
  Unsupported(RawUnsupportedGenerateKindV1)

RawGenerateItemRowV1
  the exact `RawModuleItemRowV1` payload admitted in generate context

RawGenerateRangeRowV1 =
  Single { value: RawConstExprOccurrenceId, reverse: bool } |
  Between { start: RawConstExprOccurrenceId,
            end: RawConstExprOccurrenceId,
            direction: Forward | Reverse,
            end_kind: Exclusive | Inclusive,
            step: DefaultByDirection |
                  Explicit(RawConstExprOccurrenceId,
                           SourceForStepAssignmentOp) }
```

Generate conditions/ranges are elaborative constant proofs. Their verified
finite result fixes exact expansion lineage; there is no expansion-size policy
cap and no analyzer-produced truncated body. A procedural `for` is instead
always represented by the retained runtime row below. A producer must not
choose between the two based on cost.

```text
RawRetainedForFoldRowV1
  exact statement / owning process and block
  counter: RawObjectId with RawTypeSource::DerivedForCounter
  range: Single { value: RawSourceExprId, reverse: bool } |
         Between { start: RawSourceExprId, end: RawSourceExprId,
                   direction: Forward | Reverse,
                   end_kind: Exclusive | Inclusive }
  step: DefaultByRangeDirection |
        Explicit { value: RawConstExprOccurrenceId,
                   operator: SourceForStepAssignmentOp }
  body: RawBlockId

RawRuntimeCallRowV1
  exact statement occurrence
  kind: Display { arguments: RawCallArgumentPool } |
        Write { arguments: RawCallArgumentPool } |
        AssertContinue { predicate: RawSourceExprId,
                         arguments: RawCallArgumentPool } |
        AssertFatal { predicate: RawSourceExprId,
                      arguments: RawCallArgumentPool } |
        Finish

RawRuntimeEventAnchorRowV1
  exact RawRuntimeCallId / owning process, block, and statement ordinal

RawObserverAnchorRowV1
  exact RawRuntimeEventAnchorId
  owner: CombinationalProcess(RawProcessId) |
         RetainedForFold(RawRetainedForFoldId)
```

Every admitted runtime call owns exactly one event anchor. A call reached in a
combinational process or retained fold also owns exactly one observer anchor.
The anchors deliberately contain no guard, sensitivity, captured-value,
preceding-write, local-input, effect, or activation-group summary. The
complete expected control/value traversal derives those fields from ordered
reads, writes, predicates, arguments, and fold boundaries and then derives
`SourceObserver`, `SourceObserverOccurrence`, `SourceRuntimeEventSite`, and
fold-effect rows bidirectionally. Thus omitting a call from both an observer
cache and an SLT arena cannot pass. `$readmemh`, testbench methods, and every
other effectful system call are explicit unsupported tags in this profile,
not aliases of `Display` or opaque callbacks.

The adapter's unsupported sums are exhaustive for the
`VerylParserV0_20_1_UEscape1` profile:

```text
RawUnsupportedTopItemKindV1 = Embed | Include | Recovery
RawUnsupportedUnitItemKindV1 = Connect | UnsafeBlock | Embed | Recovery
RawUnsupportedPackageItemKindV1 = Embed | Recovery
RawUnsupportedBlockItemKindV1 = Recovery
RawUnsupportedStatementKindV1 = TestbenchMethod | Recovery
RawUnsupportedPatternKindV1 = OpenEndedRange | Recovery
RawUnsupportedTargetKindV1 = SystemVerilogPath | Recovery
RawUnsupportedExpressionKindV1 = FloatValue | DirectUnionConstructor |
  SystemVerilogExpression |
  RetainedRejectedSystemCall | Recovery
RawUnsupportedGenerateKindV1 = AnalyzerOnlyExpansion | Recovery
```

`Return` is admitted only in a verified function body and `Break` only in the
innermost retained fold body. `Initial`, `Final`, nonblocking assignment,
`AlwaysFf`, interface processes, and hierarchy connections are retained closed
tags, but a consumer profile which cannot execute one rejects its exact tag at
the root capability check. It never drops the row or rewrites it as
combinational assignment. An optional byte adapter rejects an unknown numeric
tag before any of these enums exists; none of the sums has an integer
catch-all.

Every parser-derived row and owned range above maps to one and only one table
or pool kind. This document is the authoritative registry for
`PrivateRawSemanticSyntaxV1`; the source-wire and typed-constant documents use
generated subsets of these same enums and must not redeclare shortened copies.
The complete discriminants are:

```text
PrivateRawSyntaxTableKindV1 =
  Coordinate | Spelling | Scope | Declaration | Import | PathComponent |
  NameOccurrence | BoundSymbol |
  ParsedUnit | TopItem | Module | Interface | Package |
  ModulePort | InterfacePort | ModuleItem | InterfaceItem | PackageItem |
  ProtoDeclaration | ProtoPort | ProtoInterfaceItem | ProtoPackageItem |
  Bind | Modport | ModportItem | ModportDefaultName |
  Generate | GenerateItem | GenerateRange |
  Instance | InstanceInput | InstanceOutput | InstanceOutputTarget |
  Type | TypeUse | TypeMember | TypeInference | TypeInferenceCandidate |
  Extent | EnumVariant | GenericFormal | GenericUse | GenericArgument |
  Modifier | Object | ObjectTypeContext | ModuleTypeContext | StaticBinding |
  ConstExpr | SourceExpr |
  IntegralLiteral | StringLiteral | FloatLiteral | CastTarget |
  FunctionUse | FunctionActual | SourceCall | PureSystemCall |
  SystemCall | SystemArgument | CallArgument |
  ConstConcatPart | SourceConcatPart |
  ConstArrayItem | SourceArrayItem |
  NamedField | SourceConstructorField |
  Select | Selector |
  ConstAssignmentTarget | SourceAssignmentTarget |
  SourceTarget | SourceAccessPath |
  ConstDecisionArm | SourceDecisionArm | ExpressionDecisionArm |
  DecisionPattern | ExpressionDecision |
  FunctionTemplate | FunctionPort |
  ConstBlock | ConstBlockItem | LocalDeclaration | ConstFor |
  ConstStatement | ConstIfClause | ConstControlArm | ConstRoot |
  Process | AlwaysEvent | SourceBlock | SourceBlockItem | SourceStatement |
  SourceIfClause | SourceDecision | SourceAssignment | RetainedForFold |
  RuntimeCall | RuntimeEventAnchor | ObserverAnchor | TypedHIRRoot

PrivateRawSyntaxPoolKindV1 =
  ScopeDeclaration | ScopeImport |
  ImportPathComponent | NamePathComponent |
  TopItem | ModulePort | ModuleItem | InterfacePort | InterfaceItem |
  PackageItem | ProtoPort | ProtoInterfaceItem | ProtoPackageItem |
  ModportItem | ModportDefaultName | GenerateItem |
  TypeModifier | TypeUnpackedExtent | TypePackedExtent | TypeMember |
  EnumVariant | GenericFormal | GenericArgument | TypeInferenceCandidate |
  FunctionPort | FunctionActual | SystemArgument |
  ConstConcatPart | ConstArrayItem | NamedField | Select |
  ConstDecisionArm | DecisionPattern |
  ConstBlockItem | ConstIfClause | ConstControlArm |
  ConstControlArmExpression | ConstAssignmentTarget | ConstRoot |
  SourceBlockItem | AlwaysEvent | SourceIfClause |
  SourceDecisionArm | ExpressionDecisionArm | SourceExpression |
  SourceConcatPart | SourceArrayItem | SourceConstructorField |
  CallArgument | Selector | SourceAssignmentTarget | SourceTarget |
  InstanceInput | InstanceOutput | InstanceOutputTarget |
  RuntimeEventAnchor | ObserverAnchor | TypedHIRRoot
```

The singular names denote separate physical pools, not a tagged shared vector.
For example, `RawSourceExprPool`, `RawTargetPool`, and both generate-body ranges
use `SourceExpression`, `SourceTarget`, and `GenerateItem` respectively;
`RawBlockItemPool` uses `SourceBlockItem`, while the constant-function block
uses `ConstBlockItem`. Module,
interface, package, prototype, modport, instance, decision, block, call,
constructor, type, generic, observer, and fold owners obey independent gap-free cursors
over the exact pool named by their field. A row may be referenced many times
but belongs to exactly one owner range. Empty ranges name the current cursor;
the final owner cursor equals the physical pool length. No unlisted range pool
or generic `Vec<RawId>` exists.

Analyzer/resolution/constant-value witness rows are not parser syntax and do
not occur in either enum. Their producer-facing ranges use the separate
`SyntaxAnalyzerWitnessPoolKindV1` registry and syntax-lineage keys. Arbitrary
magnitude rows/bytes likewise use the magnitude resource registry. A resource
kind is therefore a total one-to-one mapping to exactly one of: one table kind,
one syntax-pool kind, one witness-pool kind, or one magnitude arena; it cannot
give the same physical arena a second coarse source/constant tag.

Finally, the typed-HIR roots are a closed inverse of syntax owners:

```text
RawTypedHIRRootRowV1 =
  ExecutableUnit(Module | Interface, exact owner) |
  ProtoDeclaration(RawProtoDeclarationId) |
  ObjectDeclaration(RawObjectId) | StaticBinding(RawStaticBindingId) |
  TypeDeclaration(RawTypeId) | FunctionTemplate(RawFunctionTemplateId) |
  Process(RawProcessId) | ContinuousAssignment(RawAssignmentId) |
  InstanceConnection(RawInstanceId, declared port ordinal) |
  BindConnection(RawBindId) | ModportExposure(RawModportId) |
  RuntimeEvent(RawRuntimeEventAnchorId) |
  RetainedForFold(RawRetainedForFoldId)
```

Every admitted owner which can seed typing, storage, hierarchy glue, control,
an event, or a value has exactly one matching root and every root maps back to
that exact owner and coordinate. Unsupported rows have no root and cause a
structured error exactly when reached from an executable/proof root. Root
order is parsed-unit order, then module/interface/package declaration order,
then the fixed variant-child ordinals above. From these roots the verifier
iteratively derives blocks, predicates, decisions, assignment actions,
runtime observers/events, access paths and selector classifications, compact
static projections, dynamic-address plans, and retained ForFold templates.
It compares the resulting `ExpectedSourceValueGraph` bidirectionally with
phase/control proposals; no producer-supplied root, read/write summary,
dynamic flag, observer capture, or fold-state list participates in derivation.

The source-to-expected expansion is also closed:

| Verified raw owner | Required expected relation |
| --- | --- |
| executable module/interface | one control unit for each process or continuous root, plus declaration/object roots in canonical item order |
| `AlwaysComb`, `AlwaysFf`, `Initial`, `Final` | exact process entry/exit, source-ordered block traversal, event reads, and the profile-specific root capability |
| reference expression | one exact expected value use; its access path derives a semantic input read and every selector operand use |
| unary/binary/concat/conditional | operand uses in fixed tag order; conditional derives one three-valued truth split and ternary bit-merge gate, never procedural `IfReduction` |
| explicit cast | one operand use and one independently derived coercion/materialization occurrence with the exact retained cast target |
| array/struct constructor or select | source-ordered item/member/base/selector uses, exact normalized aggregate/member type, compact repeat/default or lane plan, and no analyzer-computed aggregate value |
| pure system call | exact ordered value actual uses and one closed system primitive; no opaque analyzer result |
| user call in expression or statement form | exact formal-direction actual projection, shared type-specialized runtime program, pinned call instance, optional result, and output/inout writebacks |
| statement `if`/`if_reset` | one ordered gate chain using procedural `IfReduction`; false and X/Z take the else/next-clause edge |
| statement or expression case/switch | one selector acquisition when present, source-ordered pattern uses, exact arm/default regions, and the decision-result merge only for expression form |
| simple assignment | RHS and target-selector uses followed by one static or dynamic write action for each canonical target |
| compound assignment | target selectors once, exact old-value read once, RHS once, closed binary operation, target materialization, then the same write action |
| instance connection | exact ordered input read or output-target write/glue root; direction and port identity are independently resolved |
| runtime call/event anchor | one runtime site and root action; an observer anchor additionally derives all predicate/argument occurrences, sensitivity reads, captured local versions, and ordered preceding writes |
| retained procedural `for` | the exact outer range/step uses and one nested `ExpectedSourceForFoldGraph`; its body derives parallel state, recurrence, dynamic-plan, and effect rows from the same statement traversal |

Before variant expansion, the verifier independently derives one closed
classification:

```text
ExpectedSourceExpressionExpansionV1 =
  SourceGraphStaticValue(ExpectedTypedConstantExprId,
                         VerifiedSourceTypedValueId) |
  RuntimeByTag(RawSourceExprTagV1) |
  ProofOnlyByTag(RawSourceExprTagV1)
```

`SourceGraphStaticValue` is selected before any expected-graph ID exists by the
controller-first raw-HIR classification in
[`typed-constant-evaluation.md`](./typed-constant-evaluation.md):

```text
SourceGraphStaticClassV1 = generated semantic-facing alias of the authoritative
  SourceGraphStaticClassV1 in typed-constant-evaluation.md
SourceGraphRuntimeDependencyV1 = generated semantic-facing alias of the
  authoritative SourceGraphRuntimeDependencyV1 in typed-constant-evaluation.md
```

Typing covers every child. Value classification first completes each guard and
then follows only its semantically activated edges in canonical order. A known
short-circuit or conditional/decision control suppresses the same value edges
as evaluation; an X/unknown/runtime control activates every semantically
possible edge. Array repeat values retain their specified eager evaluation,
whereas array/struct defaults and pattern/result edges use their exact closed
`RawGuardRoleV1`. Suppressed children remain completely typed but create no
source value occurrence and do not taint the parent. This relation is derived
from raw expression ownership, not from `ExpectedSourceValueGraph`, an SLT
node, or an analyzer `Comptime` flag.

Only `FullyStaticProjectable` packed-integral results can become a Phase
constant. A projectable occurrence is a frontier root exactly when no enclosing
projectable occurrence in the same activated executable value tree is fully
static. `FullyStaticNonprojectable` array/struct/string/type results may be
evaluated inside an eligible parent but cannot themselves publish a Constant;
their maximal projectable descendants remain eligible. For each selected root,
source expansion publishes one `Constant` node/result from the completed
StaticOutput value and does not descend into its child value occurrences or
actions. Proof-only string/type results use `ProofOnlyByTag` and never become
Phase constants.

Eligibility and maximality are derived from retained syntax, resolved
bindings, execution owner, activated guarded dependencies, and the typed
dependency relation. A mutable source object/input, function port, runtime
local/`Let`, loop counter/state, output/inout target, runtime event, or other
effect on an activated edge yields the first canonical `RuntimeDependent`
reason. Analyzer `Comptime` is compared only as a witness. Inside a shared runtime-function
program, the static proof identity is the function template plus verified
generic environment and syntax/context; it never contains caller input values
or call lineage. Any dependency on a formal, local/`Let`, loop/runtime state,
or environment input stays `RuntimeByTag` even when one particular call passes
a constant. Such call-site constant propagation is a later verified rewrite,
not a value-specialized runtime program or a forged source StaticOutput root.

After applying that precedence, expression transfer is exhaustive by tag:

| `RawSourceExprRowV1` variant | Exact expected transfer |
| --- | --- |
| `IntegralLiteral` | its maximal executable occurrence uses `SourceGraphStaticValue`; nested occurrences are retained only inside the owning larger static proof |
| `StringLiteral` | one typed-string proof-only result, admitted only by the closed constant binding/function/runtime-template consumers; no Phase value |
| `TypeValue` | one type-only result admitted only by a retained type-query/type context; no value evaluation or Phase value |
| `Reference` | resolve the exact object/binding/constant/function-local class; mutable/external storage derives one normalized Input read, while completed static bindings derive their exact constant origin |
| `Unary` | one coerced operand use and one `Unary` result; NAND/NOR/XNOR use the canonical two-node recipe rather than another tag |
| ordinary `Binary` | left then right coerced uses and one exact `Binary` result with both verified coercion IDs |
| logical `Binary` | one left truth use and a three-valued gate which demands/gates the right call/action fragment according to the closed `&&`/`||` table, then one logical result |
| `Cast` | one operand use and one first-class `Coerce` result; only the verified cast target decides materialization |
| `Conditional` | condition once, true/false call/action fragments under the three-valued split, exact arm coercions, and one ternary bit-merge result; it never uses procedural `IfReduction` |
| `Concat` | parts in source order, each repeat count as an exact static proof, checked total type, and one compact concat/repeat recipe; repetition count never expands operand rows |
| `Select` | base once and selectors left-to-right; reference bases derive normalized Input/static-composite/dynamic plans, while value bases derive exact member/slice/partial-lane results with packed-invalid X semantics |
| `ArrayConstructor` | item rows in source order, static repeat proofs, at most one default, canonical interval coverage, element materialization coercions, and one compact aggregate recipe/result |
| `StructConstructor` | named fields in source order, exact member-name inverse, at most one default, canonical member coverage/layout, member materializations, and one compact aggregate recipe/result |
| `DecisionExpression` | the statement decision selector/pattern order plus exactly one typed result per selected/deferred arm and one reverse three-valued merge; no duplicated selector evaluation |
| `InsideOutside` | target once, patterns in source order with exact pattern operands, closed short-circuit truth combination, and one final logical-not only for `outside` |
| `PureSystemCall` | arguments and value/type-only demand fixed by the closed system tag; `$bits`/`$size` create type-only edges, while admitted integral primitives create their explicit value uses and one result |
| `UserCall` | one `ExpectedSourceRuntimeCallInstanceV1` with the execution-lineage/local-scope pair, formal-direction actual relation, pinned shared-program invoke, optional result, and all output/inout effects |
| `Unsupported` | the exact closed unsupported rule; no expected result, node, action, or fallback |

Every value-producing admitted row has exactly one natural result occurrence;
each contextual consumer owns its separate use/coercion occurrence. Compact
concat/array/struct/static-composite descriptors retain ranges and checked
counts rather than one node per repeated element, defaulted member, or lane.
The inverse verifier covers proof-only results as well, so dropping an
apparently non-lowered string/type operand cannot make a producer proposal
complete.

For each `RawAccessPathRowV1`, name/member resolution and type normalization
run before access classification. The verified constant relation classifies
each selector as known, X/Z constant, or runtime. The verifier then derives
exactly one of a contiguous static access, the compact static-composite recipe,
or a `SourceDynamicAddressPlan` containing the ordered runtime indices,
identically-false constant-X guards, checked partial-lane intervals, and read
or write semantics. A compound target reuses that one derived target handle
for its old read and final write; it never evaluates a dynamic selector twice.
Every expected dynamic plan maps back to one exact access-path owner/role, and
every such access requiring a plan maps forward once. This inverse check is
what makes the raw selector rows sufficient; a separate analyzer `dynamic`
bit, address expression, or read/write summary would be redundant untrusted
input.

### Runtime user-function relation

Runtime user functions are not opaque expressions and are not constant-VM
calls. Runtime admission first derives a closed structural capability without
rewriting the shared function/port rows:

```text
RuntimeFunctionStructuralRejectReasonV1 =
  PrototypeTemplate |
  DirectImportPort(formal declaration ordinal) |
  ImportedModportMember(formal declaration ordinal,
                        exact RawModportId,
                        interface member declaration ordinal)

VerifiedRuntimeFormalOriginV1 =
  Direct {
    RawFunctionPortId / formal declaration ordinal,
    direction: Input | Output | Inout
  } |
  ModportMember {
    RawFunctionPortId / formal declaration ordinal,
    exact RawModportId / interface member declaration ordinal and path,
    effective direction: Input | Output | Inout,
    complete member type with the port's interface-array geometry
  }

VerifiedRuntimeModportExpansionV1
  RawFunctionPortId whose retained direction is Modport(exact name occurrence)
  exact resolved interface specialization / RawModportId
  canonical effective-member rows in interface member declaration order
  each row's explicit/default provenance and effective RawPortDirectionV1

VerifiedRuntimeFunctionCapabilityV1 =
  ExecutableDefinition {
    RawFunctionTemplateId whose kind is Definition,
    canonical flattened formal slots: VerifiedRuntimeFormalOriginPool,
    optional complete return type / exact body
  } |
  StructuralReject {
    RawFunctionTemplateId,
    first RuntimeFunctionStructuralRejectReasonV1
  }
```

Direct `Input`/`Output`/`Inout` ports each contribute their complete normalized
formal slots. A retained `Modport(name)` direction is not converted in
`RawFunctionPortRowV1`; the verifier resolves its exact interface specialization
and modport and derives `VerifiedRuntimeModportExpansionV1`. Explicit modport
items are unique and resolve to the exact interface member. The effective set is
then ordered by interface member declaration order, independently of analyzer
symbol IDs or maps:

- `None` adds no unspecified member;
- `Input` or `Output` adds every unspecified interface variable with that
  direction;
- `Same(names)` adds each unspecified variable and imported function exposed by
  the named other modports with its effective direction; and
- `Converse(names)` adds each unspecified variable exposed by the named other
  modports with `Input`/`Output` exchanged and `Inout` unchanged; imported
  functions are not exposed by this V0_20 converse rule.

Named default modports are visited in retained name-list order, but final rows
remain in interface declaration order. Every name must resolve to another
modport of the same interface specialization. The verifier expands the complete
default dependency graph with an explicit worklist; self/cycles, duplicate
explicit members, unknown/wrong-interface members, or two inherited effective
directions for one member are source-structure errors before runtime capability
is selected. A port array contributes the same checked array geometry to each
member type rather than allocating one formal row per lane. No generic,
member-count, extent, or expansion-size cutoff exists.

After a structurally valid expansion, an effective imported function member is
not silently omitted: the first such member yields
`ImportedModportMember`. A direct retained `Import` function port similarly
yields `DirectImportPort`. A `Prototype` template always yields
`PrototypeTemplate`; V1 does not resolve it to an equal-named definition or
borrow another body. Their stable rule IDs are respectively
`SOURCE.RUNTIME_FUNCTION_MODPORT_IMPORT`,
`SOURCE.RUNTIME_FUNCTION_IMPORT_PORT`, and
`SOURCE.RUNTIME_FUNCTION_PROTOTYPE`. These reasons are selected only when that
template is demanded as a runtime callee. `PrototypeTemplate` is tested before
any port expansion; a definition is scanned by formal then effective-member
declaration order. They are the entire runtime structural-reject sum. Other
malformed name/type/modport relations fail their independently named source
verification rule rather than being mislabeled as capability rejection.

Only `ExecutableDefinition` supplies the verified formal list. The one raw
argument expression is resolved against that list and derives exactly one
call-actual variant:

```text
ExpectedSourceTargetHandleRowV1
  id: ExpectedSourceTargetHandleId
  exact assignable ModuleSource target syntax / execution lineage / local scope
  semantic object / normalized access / complete target type
  ordered selector ExpectedSourceUseIds
  prepared exactly once; old-value reads and every writeback reuse this ID

ExpectedSourceTargetHandleRangeV1 = generated nominal checked range of the
  complete canonical ExpectedSourceTargetHandleRowV1 table

VerifiedRuntimeCallActualV1 =
  InputExpr {
    formal ordinal / explicit RawCallArgumentId or declared-input default,
    exact ExpectedSourceUseId,
    formal-binding materializing coercion
  } |
  OutputTarget {
    formal ordinal / exact RawCallArgumentId,
    once-prepared ExpectedSourceTargetHandleId,
    initial formal value: recursive materialized type default,
    formal-to-target materializing coercion
  } |
  InoutTarget {
    formal ordinal / exact RawCallArgumentId,
    once-prepared ExpectedSourceTargetHandleId,
    exact old-value ExpectedSourceUseId,
    target-to-formal copy-in materializing coercion,
    formal-to-same-target copy-out materializing coercion
  }
```

Positional actuals map by formal declaration ordinal; named actuals map by the
exact formal name. Mixing named and positional syntax, duplicate/unknown
names, extra actuals, a missing nondefault input, or a missing output/inout
actual rejects. Only a missing input with an exact declared default can create
the default variant. For output/inout, the verifier derives an assignable
target from the same `RawCallArgumentRowV1.exact expression`; it does not trust
an analyzer `AssignDestination` or accept another target syntax row. Input
uses create no target, output uses create no input read, and inout uses the one
prepared target for both its old read and later write.

The executable body is shared by type specialization, never by input value:

```text
VerifiedRuntimeFunctionSpecializationKeyV1
  RawFunctionTemplateId / VerifiedGenericEnvironmentId
  canonical formal list of (direction, complete verified type)
  optional complete verified return type
  no input value, memo key, call-site ID, or analyzer specialization ID

RuntimeFunctionProgramFormalObjectIdV1
  specialization: VerifiedRuntimeFunctionSpecializationId /
  checked dense u32 ordinal in that specialization's formal table
RuntimeFunctionProgramLocalObjectIdV1
  specialization: VerifiedRuntimeFunctionSpecializationId /
  checked dense u32 ordinal in that specialization's local-object table
RuntimeFunctionProgramInputIdV1
  specialization: VerifiedRuntimeFunctionSpecializationId /
  checked dense u32 ordinal in that specialization's input table
RuntimeFunctionProgramPreparedTargetHandleIdV1
  specialization: VerifiedRuntimeFunctionSpecializationId /
  checked dense u32 ordinal in that specialization's prepared-target table
RuntimeFunctionProgramBindingIdV1
  specialization: VerifiedRuntimeFunctionSpecializationId /
  checked dense u32 ordinal in that specialization's binding table
RuntimeFunctionProgramValueIdV1
  specialization: VerifiedRuntimeFunctionSpecializationId /
  checked dense u32 ordinal in that specialization's value table
RuntimeFunctionProgramRegionIdV1
  specialization: VerifiedRuntimeFunctionSpecializationId /
  checked dense u32 ordinal in that specialization's region table
RuntimeFunctionProgramPointIdV1
  specialization: VerifiedRuntimeFunctionSpecializationId /
  checked dense u32 ordinal in that specialization's point table
RuntimeFunctionProgramEdgeIdV1
  specialization: VerifiedRuntimeFunctionSpecializationId /
  checked dense u32 ordinal in that specialization's edge table
RuntimeFunctionProgramActionIdV1
  specialization: VerifiedRuntimeFunctionSpecializationId /
  checked dense u32 ordinal in that specialization's action table
RuntimeFunctionProgramDynamicAddressPlanIdV1
  specialization: VerifiedRuntimeFunctionSpecializationId /
  checked dense u32 ordinal in that specialization's dynamic-plan table
RuntimeFunctionProgramEffectStreamIdV1
  specialization: VerifiedRuntimeFunctionSpecializationId /
  checked dense u32 ordinal in that specialization's effect-stream table
RuntimeFunctionProgramRuntimeEventSiteIdV1
  specialization: VerifiedRuntimeFunctionSpecializationId /
  checked dense u32 ordinal in that specialization's runtime-site table
RuntimeFunctionProgramNestedCallInstanceIdV1
  specialization: VerifiedRuntimeFunctionSpecializationId /
  checked dense u32 ordinal in that specialization's nested-call-site table
RuntimeFunctionProgramSiteIdV1
  specialization: VerifiedRuntimeFunctionSpecializationId /
  checked dense u32 ordinal in that specialization's use/control-site table

RuntimeFunctionProgramActionId = generated public alias of the exact
  RuntimeFunctionProgramActionIdV1 nominal newtype; it is not a second ID

RuntimeFunctionProgramTableRangeV1<K>
  specialization: VerifiedRuntimeFunctionSpecializationId /
  checked physical start u32 / checked length u32 / nominal row kind K
  the generated concrete instantiation for each ID kind above is a distinct
    range type; logical ID ordinal i addresses only physical row start + i in
    that specialization's corresponding row table

RuntimeFunctionProgramFormalObjectRangeV1 = generated nominal
  RuntimeFunctionProgramTableRangeV1<RuntimeFunctionProgramFormalObjectIdV1>
RuntimeFunctionProgramLocalObjectRangeV1 = generated nominal
  RuntimeFunctionProgramTableRangeV1<RuntimeFunctionProgramLocalObjectIdV1>
RuntimeFunctionProgramInputRangeV1 = generated nominal
  RuntimeFunctionProgramTableRangeV1<RuntimeFunctionProgramInputIdV1>
RuntimeFunctionProgramPreparedTargetHandleRangeV1 = generated nominal
  RuntimeFunctionProgramTableRangeV1<
    RuntimeFunctionProgramPreparedTargetHandleIdV1>
RuntimeFunctionProgramBindingRangeV1 = generated nominal
  RuntimeFunctionProgramTableRangeV1<RuntimeFunctionProgramBindingIdV1>
RuntimeFunctionProgramValueRangeV1 = generated nominal
  RuntimeFunctionProgramTableRangeV1<RuntimeFunctionProgramValueIdV1>
RuntimeFunctionProgramRegionRangeV1 = generated nominal
  RuntimeFunctionProgramTableRangeV1<RuntimeFunctionProgramRegionIdV1>
RuntimeFunctionProgramPointRangeV1 = generated nominal
  RuntimeFunctionProgramTableRangeV1<RuntimeFunctionProgramPointIdV1>
RuntimeFunctionProgramEdgeRangeV1 = generated nominal
  RuntimeFunctionProgramTableRangeV1<RuntimeFunctionProgramEdgeIdV1>
RuntimeFunctionProgramActionRangeV1 = generated nominal
  RuntimeFunctionProgramTableRangeV1<RuntimeFunctionProgramActionIdV1>
RuntimeFunctionProgramDynamicAddressPlanRangeV1 = generated nominal
  RuntimeFunctionProgramTableRangeV1<
    RuntimeFunctionProgramDynamicAddressPlanIdV1>
RuntimeFunctionProgramEffectStreamRangeV1 = generated nominal
  RuntimeFunctionProgramTableRangeV1<RuntimeFunctionProgramEffectStreamIdV1>
RuntimeFunctionProgramRuntimeEventSiteRangeV1 = generated nominal
  RuntimeFunctionProgramTableRangeV1<
    RuntimeFunctionProgramRuntimeEventSiteIdV1>
RuntimeFunctionProgramNestedCallInstanceRangeV1 = generated nominal
  RuntimeFunctionProgramTableRangeV1<
    RuntimeFunctionProgramNestedCallInstanceIdV1>
RuntimeFunctionProgramSiteRangeV1 = generated nominal
  RuntimeFunctionProgramTableRangeV1<RuntimeFunctionProgramSiteIdV1>

RuntimeFunctionProgramObjectRefV1 =
  Formal(RuntimeFunctionProgramFormalObjectIdV1) |
  Local(RuntimeFunctionProgramLocalObjectIdV1)

RuntimeFunctionProgramInputOriginV1 =
  FrameObject(RuntimeFunctionProgramObjectRefV1, exact normalized access) |
  CapturedOuter {
    object: SourceSemanticObjectId,
    input: SourceInputId,
    exact capture/boundary proof
  }

RuntimeFunctionProgramBindingOriginV1 =
  FrameObject(RuntimeFunctionProgramObjectRefV1, exact lexical binding) |
  CapturedOuter(SourceBindingId, exact capture/boundary proof)

RuntimeFunctionProgramEffectStreamOriginV1 =
  ProgramLocal(exact raw runtime-effect occurrence) |
  CapturedOuter(SourceEffectStreamId, exact capture/boundary proof)

RuntimeFunctionProgramDomainRefV1 =
  Frame {
    object: RuntimeFunctionProgramObjectRefV1,
    access: exact normalized bit/domain access
  } |
  CapturedOuter {
    domain: SourceWriteDomainId,
    exact capture/boundary proof
  }

RuntimeFunctionProgramBindingRefV1 =
  Frame {
    binding: RuntimeFunctionProgramBindingIdV1
  } |
  CapturedOuter {
    binding: SourceBindingId,
    exact capture/boundary proof
  }

RuntimeFunctionProgramEffectRefV1 =
  Frame {
    stream: RuntimeFunctionProgramEffectStreamIdV1
  } |
  CapturedOuter {
    stream: SourceEffectStreamId,
    exact capture/boundary proof
  }

RuntimeFunctionProgramSemanticAccessSummaryV1
  read_domains: checked canonical range [RuntimeFunctionProgramDomainRefV1]
  write_domains: checked canonical range [RuntimeFunctionProgramDomainRefV1]
  read_bindings: checked canonical range [RuntimeFunctionProgramBindingRefV1]
  write_bindings: checked canonical range [RuntimeFunctionProgramBindingRefV1]
  effect_publications: checked canonical range
    [(RuntimeFunctionProgramEffectRefV1, publication kind)]

RuntimeFunctionProgramFormalObjectRowV1
  id: RuntimeFunctionProgramFormalObjectIdV1
  flattened formal ordinal / exact VerifiedRuntimeFormalOriginV1
  direction: Input | Output | Inout / complete verified type
  entry_value: RuntimeFunctionProgramValueIdV1
  exit_value: None for Input |
              Some(RuntimeFunctionProgramValueIdV1) for Output/Inout

RuntimeFunctionProgramLocalObjectRowV1
  id: RuntimeFunctionProgramLocalObjectIdV1
  exact raw local declaration / lexical block and block-entry generation
  Var | Let | return storage | retained-fold state
  complete verified type / exact initializer-or-default relation

RuntimeFunctionProgramInputRowV1
  id: RuntimeFunctionProgramInputIdV1
  origin: RuntimeFunctionProgramInputOriginV1
  first canonical read: RuntimeFunctionProgramSiteIdV1
  complete verified type / object width / access geometry and domain

RuntimeFunctionProgramPreparedTargetHandleRowV1
  id: RuntimeFunctionProgramPreparedTargetHandleIdV1
  exact assignable target syntax / lexical execution owner
  object: RuntimeFunctionProgramObjectRefV1 / normalized access and target type
  ordered selector values: RuntimeFunctionProgramValueIdV1
  prepared exactly once before any old-value read or writeback

RuntimeFunctionProgramBindingRowV1
  id: RuntimeFunctionProgramBindingIdV1
  origin: RuntimeFunctionProgramBindingOriginV1
  exact lexical generation / publication and read semantics

RuntimeFunctionProgramValueRowV1
  id: RuntimeFunctionProgramValueIdV1
  origin: FormalEntry(RuntimeFunctionProgramFormalObjectIdV1) |
          LocalDefinition(RuntimeFunctionProgramLocalObjectIdV1) |
          PointParameter(RuntimeFunctionProgramPointIdV1, parameter ordinal) |
          ActionResult(RuntimeFunctionProgramActionIdV1, result ordinal) |
          Constant(VerifiedSourceTypedValueId)
  complete verified type and Phase value facts /
  ordered program-relative producer IDs and exact semantic owner/site

RuntimeFunctionProgramRegionRowV1
  id: RuntimeFunctionProgramRegionIdV1
  owner: ProgramRoot |
         Nested(RuntimeFunctionProgramRegionIdV1, exact raw control owner)
  entry / exit: RuntimeFunctionProgramPointIdV1
  checked ordered child-region/point/edge ranges of this specialization

RuntimeFunctionProgramPointRowV1
  id: RuntimeFunctionProgramPointIdV1
  region: RuntimeFunctionProgramRegionIdV1
  Entry | NormalExit | ReturnExit | Ordinary | FoldBoundary
  checked ordered RuntimeFunctionProgramActionIdV1 range /
  checked predecessor/successor RuntimeFunctionProgramEdgeIdV1 ranges

RuntimeFunctionProgramEdgeRowV1
  id: RuntimeFunctionProgramEdgeIdV1
  predecessor / successor: RuntimeFunctionProgramPointIdV1
  exact edge kind /
  optional predicate RuntimeFunctionProgramValueIdV1 with exact outcome

RuntimeFunctionProgramDynamicAddressPlanRowV1
  id: RuntimeFunctionProgramDynamicAddressPlanIdV1
  owner: RuntimeFunctionProgramActionIdV1
  target: RuntimeFunctionProgramPreparedTargetHandleIdV1
  input: RuntimeFunctionProgramInputIdV1
  object: RuntimeFunctionProgramObjectRefV1 / complete type and object width
  checked ordered RuntimeFunctionProgramValueIdV1 index/guard range /
  dimensions, strides, part-select geometry, selected width, offset,
    address-known, bounds-when-known, and read/overlay-write semantics

RuntimeFunctionProgramEffectStreamRowV1
  id: RuntimeFunctionProgramEffectStreamIdV1
  origin: RuntimeFunctionProgramEffectStreamOriginV1
  exact publication order and effect semantics

RuntimeFunctionProgramRuntimeEventSiteRowV1
  id: RuntimeFunctionProgramRuntimeEventSiteIdV1
  owner: RuntimeFunctionProgramActionIdV1
  stream: RuntimeFunctionProgramEffectStreamIdV1
  optional predicate / ordered arguments: RuntimeFunctionProgramValueIdV1
  exact continuation/termination/fatal semantics

RuntimeFunctionProgramNestedCallActualV1 =
  InputExpr {
    formal_ordinal: u32,
    value: RuntimeFunctionProgramValueIdV1,
    formal_binding_coercion: exact program-relative coercion
  } |
  OutputTarget {
    formal_ordinal: u32,
    target: RuntimeFunctionProgramPreparedTargetHandleIdV1,
    initial_value: RuntimeFunctionProgramValueIdV1,
    formal_to_target_coercion: exact program-relative coercion
  } |
  InoutTarget {
    formal_ordinal: u32,
    target: RuntimeFunctionProgramPreparedTargetHandleIdV1,
    old_value: RuntimeFunctionProgramValueIdV1,
    target_to_formal_coercion: exact program-relative coercion,
    formal_to_target_coercion: exact program-relative coercion
  }

RuntimeFunctionProgramNestedCallWritebackV1
  flattened output/inout formal ordinal
  result: RuntimeFunctionProgramValueIdV1
  target: RuntimeFunctionProgramPreparedTargetHandleIdV1
  target_coercion: exact program-relative coercion
  caller_slot: RuntimeFunctionProgramSiteIdV1
  target is exactly the same once-prepared handle in the matching actual

RuntimeFunctionProgramNestedCallActualRangeV1 = generated nominal checked
  range in the specialization-owned nested-call-actual pool
RuntimeFunctionProgramNestedCallWritebackRangeV1 = generated nominal checked
  range in the specialization-owned nested-call-writeback pool

RuntimeFunctionProgramNestedCallInstanceRowV1
  id: RuntimeFunctionProgramNestedCallInstanceIdV1
  exact raw nested-call occurrence / owner RuntimeFunctionProgramActionIdV1
  callee: VerifiedRuntimeFunctionSpecializationId
  actuals: RuntimeFunctionProgramNestedCallActualRangeV1
  results: checked ordered RuntimeFunctionProgramValueIdV1 range
  writebacks: RuntimeFunctionProgramNestedCallWritebackRangeV1
  exact authoritative invoke operand/result-role ranges and program boundary
  this is a shared program-relative call site, never an
    ExpectedSourceRuntimeCallInstanceId execution instance

RuntimeFunctionProgramSiteRowV1
  id: RuntimeFunctionProgramSiteIdV1
  owner: PointSlot(RuntimeFunctionProgramPointIdV1, slot ordinal) |
         Edge(RuntimeFunctionProgramEdgeIdV1)
  exact raw/program semantic use and optional RuntimeFunctionProgramValueIdV1

RuntimeFunctionProgramInputResolutionV1 =
  Memory | Environment | StaticComposite |
  DynamicOverlay(RuntimeFunctionProgramDynamicAddressPlanIdV1)

RuntimeFunctionProgramActionKindV1 =
  NestedActionSemanticKind<
    Input = RuntimeFunctionProgramInputIdV1,
    Binding = RuntimeFunctionProgramBindingIdV1,
    Target = RuntimeFunctionProgramPreparedTargetHandleIdV1,
    DynamicPlan = RuntimeFunctionProgramDynamicAddressPlanIdV1,
    EffectStream = RuntimeFunctionProgramEffectStreamIdV1,
    RuntimeSite = RuntimeFunctionProgramRuntimeEventSiteIdV1,
    RuntimeCall = RuntimeFunctionProgramNestedCallInstanceIdV1,
    Site = RuntimeFunctionProgramSiteIdV1,
    Resolution = RuntimeFunctionProgramInputResolutionV1>

RuntimeFunctionProgramActionV1
  id: RuntimeFunctionProgramActionIdV1
  owner point: RuntimeFunctionProgramPointIdV1 / exact action slot
  checked ordered RuntimeFunctionProgramValueIdV1 operands/results
  semantic_accesses: RuntimeFunctionProgramSemanticAccessSummaryV1
  kind: RuntimeFunctionProgramActionKindV1

VerifiedRuntimeFunctionProgramV1
  specialization: VerifiedRuntimeFunctionSpecializationId
  exact verified signature / entry / normal exit / return exits
  formal_objects: RuntimeFunctionProgramFormalObjectRangeV1
  local_objects: RuntimeFunctionProgramLocalObjectRangeV1
  inputs: RuntimeFunctionProgramInputRangeV1
  prepared_targets: RuntimeFunctionProgramPreparedTargetHandleRangeV1
  bindings: RuntimeFunctionProgramBindingRangeV1
  values: RuntimeFunctionProgramValueRangeV1
  regions: RuntimeFunctionProgramRegionRangeV1
  points: RuntimeFunctionProgramPointRangeV1
  edges: RuntimeFunctionProgramEdgeRangeV1
  actions: RuntimeFunctionProgramActionRangeV1
  dynamic_plans: RuntimeFunctionProgramDynamicAddressPlanRangeV1
  effect_streams: RuntimeFunctionProgramEffectStreamRangeV1
  runtime_sites: RuntimeFunctionProgramRuntimeEventSiteRangeV1
  nested_call_sites: RuntimeFunctionProgramNestedCallInstanceRangeV1
  sites: RuntimeFunctionProgramSiteRangeV1
  retained-ForFold-template range with exact explicit source-template boundary
  program_read_domains: checked canonical range
    [RuntimeFunctionProgramDomainRefV1]
  program_write_domains: checked canonical range
    [RuntimeFunctionProgramDomainRefV1]
  program_read_bindings: checked canonical range
    [RuntimeFunctionProgramBindingRefV1]
  program_write_bindings: checked canonical range
    [RuntimeFunctionProgramBindingRefV1]
  program_effect_publications: checked canonical range
    [(RuntimeFunctionProgramEffectRefV1, publication kind)]
  one RuntimeFunctionProgramValueIdV1 exit value for every output/inout formal
    and optional return

ExpectedSourceRuntimeCallInstanceV1
  exact RawSourceCallId / expression-result or statement owner/site
  RuntimeSourceExecutionLineageId of its RuntimeCall child row
  caller local_scope: RuntimeSourceLocalScopeV1
  VerifiedRuntimeFunctionSpecializationId
  actuals: VerifiedRuntimeCallActualPool in formal declaration order
  call-setup occurrences in raw argument source order
  expected: ExpectedSourceRuntimeCallGraphV1
  call-scoped program lineage/frame view
  optional return result / output-inout writeback rows
  exact read/write/binding/effect summary

ExpectedSourceRuntimeCallGraphV1
  entry / setup / program-entry / program-exit / copyout / exit points
  exact source-ordered setup actions and program boundary operands/results
  writebacks: ExpectedRuntimeFunctionWritebackPool
  one optional return result and complete predecessor coverage

ExpectedRuntimeFunctionWritebackRowV1
  flattened output/inout formal ordinal / program exit value
  exact once-prepared target handle / materializing target coercion
  copyout action at the formal-order slot / exact write access summary

SourceRuntimeFunctionInvokeOperandRoleV1 =
  InputActual(flattened formal ordinal) |
  InoutOldValue(flattened formal ordinal) |
  TargetSelector(flattened output/inout formal ordinal,
                 selector operand ordinal) |
  DeclaredInputDefault(flattened formal ordinal)

SourceRuntimeFunctionInvokeResultRoleV1 =
  Return | OutputValue(flattened formal ordinal) |
  InoutValue(flattened formal ordinal)

SourceScopedRuntimeFunctionInvokeAccessV1
  call_setup_read_domains: checked canonical range [SourceWriteDomainId]
  call_target_read_domains: checked canonical range [SourceWriteDomainId]
  call_nested_read_domains: checked canonical range [SourceWriteDomainId]
  call_nested_write_domains: checked canonical range [SourceWriteDomainId]
  call_nested_read_bindings: checked canonical range [SourceBindingId]
  call_nested_write_bindings: checked canonical range [SourceBindingId]
  call_nested_effect_publications:
    checked canonical range [(SourceEffectStreamId, publication kind)]
  call_copyout_write_domains: checked canonical range [SourceWriteDomainId]
  call_combined_read_domains: checked canonical range [SourceWriteDomainId]
  call_combined_write_domains: checked canonical range [SourceWriteDomainId]
  call_combined_read_bindings: checked canonical range [SourceBindingId]
  call_combined_write_bindings: checked canonical range [SourceBindingId]
  call_combined_effect_publications:
    checked canonical range [(SourceEffectStreamId, publication kind)]

ProgramScopedRuntimeFunctionInvokeAccessV1
  call_setup_read_domains: checked canonical range
    [RuntimeFunctionProgramDomainRefV1]
  call_target_read_domains: checked canonical range
    [RuntimeFunctionProgramDomainRefV1]
  call_nested_read_domains: checked canonical range
    [RuntimeFunctionProgramDomainRefV1]
  call_nested_write_domains: checked canonical range
    [RuntimeFunctionProgramDomainRefV1]
  call_nested_read_bindings: checked canonical range
    [RuntimeFunctionProgramBindingRefV1]
  call_nested_write_bindings: checked canonical range
    [RuntimeFunctionProgramBindingRefV1]
  call_nested_effect_publications: checked canonical range
    [(RuntimeFunctionProgramEffectRefV1, publication kind)]
  call_copyout_write_domains: checked canonical range
    [RuntimeFunctionProgramDomainRefV1]
  call_combined_read_domains: checked canonical range
    [RuntimeFunctionProgramDomainRefV1]
  call_combined_write_domains: checked canonical range
    [RuntimeFunctionProgramDomainRefV1]
  call_combined_read_bindings: checked canonical range
    [RuntimeFunctionProgramBindingRefV1]
  call_combined_write_bindings: checked canonical range
    [RuntimeFunctionProgramBindingRefV1]
  call_combined_effect_publications: checked canonical range
    [(RuntimeFunctionProgramEffectRefV1, publication kind)]

SourceRuntimeFunctionInvokeAccessSummaryV1 =
  SourceScoped(SourceScopedRuntimeFunctionInvokeAccessV1) |
  ProgramScoped(ProgramScopedRuntimeFunctionInvokeAccessV1)

SourceRuntimeFunctionInvokeOwnerTargetV1 =
  Outer {
    action: SourceControlActionId,
    instance: ExpectedSourceRuntimeCallInstanceId,
    writebacks: ExpectedRuntimeFunctionWritebackPool
  } |
  RuntimeProgram {
    specialization: VerifiedRuntimeFunctionSpecializationId,
    action: RuntimeFunctionProgramActionId,
    site: RuntimeFunctionProgramNestedCallInstanceIdV1,
    writebacks: RuntimeFunctionProgramNestedCallWritebackRangeV1
  } |
  ForFold {
    template: SourceForFoldTemplateId,
    action: SourceFoldActionId,
    instance: ExpectedSourceRuntimeCallInstanceId,
    writebacks: ExpectedRuntimeFunctionWritebackPool
  }

SourceRuntimeFunctionInvokeV1
  owner_target: SourceRuntimeFunctionInvokeOwnerTargetV1
  operand_roles: SourceRuntimeFunctionInvokeOperandRolePool
  result_roles: SourceRuntimeFunctionInvokeResultRolePool
  expected nested call graph / exact program boundary mapping
  accesses: SourceRuntimeFunctionInvokeAccessSummaryV1
```

Every program ID above is a distinct generated nominal newtype whose identity
contains both the named `VerifiedRuntimeFunctionSpecializationId` and its
kind-local ordinal. The verifier checks the specialization owner before the
ordinal and never strips that owner to compare integer payloads. For every
kind `K`, `RuntimeFunctionProgramTableRangeV1<K>` covers the complete logical
`[0, len)` table of that one program. Its physical row at `start + i` has
exactly the ID `(specialization, i)`, and row-to-ID and ID-to-row are total
inverses. `start` and checked `start + len` must equal that specialization's
reserved table interval. All owner-local variable ranges use generated
field-specific range types with the same specialization brand; their checked
subranges exactly partition the named physical pool in canonical owner/slot
order. A range with
another specialization, row kind, pool kind, gap, overlap, duplicate, missing
row, or trailing entry rejects before any ID is dereferenced.

The flattened `VerifiedRuntimeFormalOriginV1` list and formal rows are a total
bijection in formal ordinal. The independently replayed admitted function body
likewise maps every required local object, canonical input, prepared target,
lexical binding, value, region, point, edge, action, dynamic plan, effect
stream, runtime site, nested call site/actual/writeback, and use/control site
to exactly one corresponding program row,
and every row maps back to that exact raw-HIR/program owner. Region child and
point/action/edge ranges cover their tables exactly; each action belongs to one
point/slot; each dynamic plan and runtime site belongs to its exact action; and
`InvokeRuntimeFunction` actions and
`RuntimeFunctionProgramNestedCallInstanceRowV1` rows are bijective. These are
normative tables, not optional producer caches.
Prepared-target rows and the nested-call actual/writeback pools likewise have
total physical inverses: every program target syntax owns one canonical handle,
every dynamic plan names that handle, and each output/inout actual and
formal-order writeback names the identical handle. No unused target, orphan
actual/writeback, or equal-numbered call-scoped handle is admitted.

None of the program IDs is representation-compatible with another program-ID
kind or with `SourceSemanticObjectId`, `SourceInputId`, `SourceBindingId`,
`SourceValueOccurrenceId`, `SourcePredicateRegionId`, `SourceControlPointId`,
`SourceControlEdgeId`, `SourceControlActionId`, `SourceDynamicAddressPlanId`,
`SourceEffectStreamId`, `SourceRuntimeEventSiteId`,
`ExpectedSourceRuntimeCallInstanceId`, `SourceControlSite`,
`SourceForFoldTemplateId`, `SourceFoldPredicateRegionId`, `SourceFoldPointId`,
`SourceFoldEdgeId`, `SourceFoldActionId`, `SourceFoldValueOccurrenceId`, or
`SourceFoldDynamicAddressPlanId`. The explicit `CapturedOuter` input/binding/
effect origins and retained-ForFold boundary are the only fields which may
contain a mutable source/fold semantic ID. An outer value enters the program
only through `RuntimeFunctionProgramInputRowV1` and a `ReadInput` action result;
there is no direct captured-outer value origin. Immutable exact syntax, type,
formal-origin, and `VerifiedSourceTypedValueId` constant-proof payloads may
appear only in their named proof/origin fields. Every such payload remains in
its original nominal type and never converts into a program ID. Every embedded
program-owned ID or range must carry the row's exact specialization owner.
In particular, `RuntimeFunctionProgramPreparedTargetHandleIdV1` is not
representation-compatible with `ExpectedSourceTargetHandleId` or
`SourceFoldPreparedTargetHandleId`; its dynamic-plan inverse and every nested
output/inout actual/writeback carry the same program specialization.

`RuntimeFunctionProgramNestedCallInstanceIdV1` identifies a shared structural
call site, not one execution-lineage instance. For each instantiated compound
view `(parent RuntimeSourceExecutionLineageId, RuntimeSourceLocalScopeV1,
specialization, nested-call-site ID)`, the verifier derives exactly one
`ExpectedSourceRuntimeCallInstanceId`; its `RuntimeCall` lineage row has that
parent and the callee specialization recorded by the site row. The inverse
recovers the same compound view. Thus this mapping is a bijection per parent
lineage/local scope, while the immutable program row remains shared and is
never polluted with a caller ID or input value.

`SourceRuntimeFunctionInvokeOwnerTargetV1` is the only source-side dependent
invoke owner/target sum.
`Outer` names an ordinary `SourceControlAction`; `RuntimeProgram` names one
program-relative action in the exact shared specialization; `ForFold` names one
action in the exact source fold template. The owner action must have the closed
`InvokeRuntimeFunction` variant, the same role ranges and independently replayed
summary. An outer/fold variant carries its occurrence-scoped
`ExpectedSourceRuntimeCallInstanceId` directly; a runtime-program variant
carries only its shared structural
`RuntimeFunctionProgramNestedCallInstanceIdV1`. Conversely every outer/fold
invoke action owns exactly one `SourceRuntimeFunctionInvokeV1`, and every
instantiated runtime-program invoke view owns exactly one; every non-invoke
action/view owns none. The three owner variants and their inverse indices are
disjoint; equal integer ordinals or equal call bits cannot cross a namespace.
For `RuntimeProgram`, the separately written `specialization` field must equal
the specialization owner embedded in `RuntimeFunctionProgramActionIdV1`.
`Outer` and `ForFold` require
`SourceRuntimeFunctionInvokeAccessSummaryV1::SourceScoped`; `RuntimeProgram`
requires `ProgramScoped`. This tag agreement is generated as an exhaustive
owner/access match with no default arm. Program-frame domain, binding, and
effect refs remain program-scoped. At the enclosing call boundary, only a
`CapturedOuter` ref maps to its exact source-scoped ID; every `Frame` ref is
consumed by the program frame and is absent from the external combined
summary. Both directions of that filter/map are verified, so a local frame
effect cannot leak out and a captured access cannot disappear.
For `Outer` and `ForFold`, the five `call_combined_*` ranges are exactly the
owning action's `SourceSemanticAccessSummary`. For `RuntimeProgram`, they are
exactly the owning `RuntimeFunctionProgramActionV1.semantic_accesses`. Setup,
target, nested, and copyout ranges independently recompute those combined
ranges; neither action summary nor invoke summary is accepted as an oracle for
the other.

The role pools are parallel bijections to the owning action's existing
`ordered_operands` and `results`; every role stores the checked slot ordinal and
no second value ID. Operand order is explicit raw-argument setup order followed
by declared defaults in formal order. Result order is optional `Return` first,
then every output/inout exit value in flattened formal/member declaration
order. Each writeback names exactly the matching output/inout result role and
the handle prepared by its target-selector roles. Missing, extra, duplicate,
reordered, or cross-call roles fail even when all value types match.
`Return` has the verified return type after return materialization;
`OutputValue`/`InoutValue` have their exact materialized formal types. Their
width/signed/positive/domain/mask facts are checked against the shared program
exit and cannot be copied from the caller target.

The closed outer action variant is therefore:

```text
ActionSemanticKind::InvokeRuntimeFunction {
  optional root: SourceRootId,
  instance: ExpectedSourceRuntimeCallInstanceId,
  operand_role_range,
  result_role_range
}
```

Its combined semantic summary is independently recomputed as the ordered union
of setup reads, program environment/object reads and writes, bindings/effects,
and copyout target writes. The nested graph boundary proves that its incoming
tokens equal the outer action's read/binding/effect inputs and its exit tokens
equal the outer action's published summary. Return and every copyout publish
atomically at completion; a planner cannot retain the return while dropping or
moving an effect/write.

A runtime call inside a shared function program uses the same shape in that
program's scoped action namespace, with no outer `SourceRootId`. A call inside
a retained ForFold uses fold-scoped operands/results, targets, dynamic plans,
and effects and is covered by the fold boundary inverse. These nested variants
use respectively `SourceRuntimeFunctionInvokeOwnerTargetV1::RuntimeProgram` and
`::ForFold`; they cannot name outer action IDs, and their execution-lineage parent is the owning
root/runtime-call row. Their independent `RuntimeSourceLocalScopeV1` is `Body`
for a body call and `ForFold(the exact owning template)` for a fold call.
Runtime calls are pinned `Whole` actions for scheduling and atomization;
result bit projections may be derived after completion, but the invoke
action/program/effects are never cloned per result slice.

The runtime verifier may share the already completed function template,
signature, normalized port types, and static/generic prerequisites with the
constant verifier. `VerifiedRuntimeFunctionProgramV1`, its specialization ID,
call-instance IDs, and runtime values are nevertheless distinct from the
constant evaluator's program, memo, frame, trace, and
`VerifiedConstantFunctionSpecializationId`. Runtime specialization equality compares
only the complete type key above. Two calls with different input bits reuse
that program; equal input bits do not merge call instances or output targets.
The parser-derived `FunctionTemplate`/`FunctionPort`/function-body rows in the
authoritative syntax registry are owned once. Names such as `ConstBlock` in
the constant evaluator denote its restricted semantic projection of that
shared function syntax, not a second parser body or permission to omit
output/inout/runtime statements from the runtime projection.

Call setup visits explicit argument syntax in source order. An input evaluates
once and materializes into its formal; output/inout target selectors are
evaluated and the target handle is prepared once; inout then reads and
materializes that same target once. Missing declared input defaults follow in
formal declaration order. Each default is typed under the callee template's
verified generic environment and evaluated per call against a read-only view of
already staged preceding formals; self/later-formal dependency rejects. A
formal-dependent default is a call-scoped runtime fragment, not a template
static root or a value-specialized program. The default expression is always
the one `RawFunctionPortRowV1.default` source expression. Its constant-function
projection and runtime `DeclaredInputDefault` setup occurrence map back to that
same syntax/typed-HIR owner and verified type/coercion relation; neither
capability owns a copied default AST or analyzer-cached value. A default which
is independent of formals may reuse its one completed static value proof, but
the runtime call still owns the per-call setup/materialization occurrence. Only
after setup completes does the shared program run. On every normal/return exit,
output and inout values copy out in formal
declaration order, using the retained handles and target coercions; therefore
overlapping legal targets have one deterministic declaration-order writeback,
not analyzer `HashMap` order. A statement-form call discards only the optional
return result after all body effects and copy-out actions. An expression-form
call requires a return type but still retains any output/inout effects.

Direct function ports and expanded interface-modport members first produce one
canonical flattened formal-slot list in formal/member declaration order. The
`VerifiedRuntimeCallActualV1` sum is per flattened slot; one raw argument may
therefore own a contiguous actual range, but every slot still derives from
that argument's one expression and exact appended member projection. Setup and
writeback order use this flattened list where formal order is required.

Runtime source execution has its own persistent lineage namespace:

```text
RuntimeSourceRootRoleV1 =
  AggregateCatalog | ControlUnit | Observer | StaticInitializer

RuntimeSourceLocalScopeV1 =
  Body | ForFold(template: SourceForFoldTemplateId)

SyntaxRuntimeLocalScopeV1 =
  Body | ForFold(template: SyntaxOccurrenceKeyV1)

RuntimeSourceExecutionLineageRowV1 =
  Root { exact executable-unit/root syntax,
         role: RuntimeSourceRootRoleV1 } |
  RuntimeCall { parent: RuntimeSourceExecutionLineageId,
                exact call SyntaxOccurrenceKeyV1,
                specialization: VerifiedRuntimeFunctionSpecializationId }

SyntaxRuntimeSourceExecutionLineageWitnessRowV1 =
  Root { exact executable-unit/root SyntaxOccurrenceKeyV1,
         role: RuntimeSourceRootRoleV1 } |
  RuntimeCall { parent witness row,
                exact call SyntaxOccurrenceKeyV1,
                SyntaxRuntimeFunctionSpecializationKeyV1 }

SyntaxRuntimeFunctionSpecializationKeyV1
  function-template SyntaxOccurrenceKeyV1 / generic-environment lineage key
  canonical formal direction/type-content keys / optional return-type key
```

The four root roles are exhaustive and role identity participates in lineage
content. `AggregateCatalog` owns a catalog-level executable/static prerequisite,
`ControlUnit` owns an ordinary executable source control root, `Observer` owns
the separately scheduled observer execution root, and `StaticInitializer` owns
the exact admitted initializer root. Each role must point to the independently
derived typed-HIR root of that class; equal syntax under a different role is not
the same lineage. The producer-facing witness imports
`RuntimeSourceRootRoleV1` as a generated alias rather than spelling the four
variants again.

Execution lineage and local scope are independent axes. A root or runtime-call
lineage identifies the instantiated call path; `RuntimeSourceLocalScopeV1`
identifies either its ordinary body or one retained fold template. The
producer-facing comparison uses the content-equivalent
`SyntaxRuntimeLocalScopeV1`, whose `ForFold` payload is the template's exact
syntax key. Mapping that key to the one verified `SourceForFoldTemplateId`
produces `RuntimeSourceLocalScopeV1`; no other scope variant, implicit default,
or iteration identity exists. The source-wire schema imports both named enums
from this semantic definition and cannot redeclare a competing scope or root
role.

Private lineage rows are allocated by canonical expected traversal; every
parent ID is smaller than its child, equal complete rows are interned by
content, and no lineage-depth cap exists. Producer-facing lineage witness rows
use producer-local checked references only after a first pass proves every
parent precedes its child, every root/step syntax key exists, ranges are
gap-free, and the row set is canonical. The verifier maps their complete
syntax-keyed specialization content to private verified specialization IDs and
then compares both lineage sets bidirectionally. A producer never writes a
private lineage or specialization ID. These comparison rows are producer
proposal/witness resources, not parser-derived
`PrivateRawSyntaxTableKindV1`/`PrivateRawSyntaxPoolKindV1` entries.

Every function-program value/control/action slot is program-relative. An
instantiated reference is the checked compound
`(RuntimeSourceExecutionLineageId, RuntimeSourceLocalScopeV1,
VerifiedRuntimeFunctionSpecializationId, program-relative slot ID)`; it does
not allocate a copied slot row. Every use,
result, canonical producer, formal/local object, lexical binding, dynamic plan,
observer/effect, and source provenance key interpreted through that view
carries its exact execution-lineage/local-scope pair. A local identity is
therefore at least
`(execution lineage, RuntimeSourceLocalScopeV1, raw local declaration,
lexical block generation)`, and a formal identity is
`(execution lineage, Body, flattened formal ordinal)`; a use of that formal
inside a retained fold retains the formal's `Body` declaration identity while
its use slot carries `ForFold(template)`. Two outer
calls of the same specialization, or the same nested call site reached under
two parent calls, cannot collide merely because their raw body syntax and
program slot are equal.

Formal/local frame objects use checked runtime-program/call-scoped object and
input namespaces; they are not interchangeable with module
`SourceSemanticObjectId`/`SourceInputId`. Only an exact captured outer read or
actual/copyout target names the module source namespace, through an explicit
boundary row. This prevents a function local from aliasing an equal-shaped
module variable while still allowing its internal dynamic accesses and
retained folds to use the same normalized access rules.

The producer comparison form uses the mapped
`SyntaxRuntimeSourceExecutionLineageWitnessRowV1` reference. In particular,
the old conceptual `SyntaxSemanticSlotKeyV1.scope = Outer | ForFold` is not a
closed key: it is replaced by the pair
`SyntaxSemanticSlotKeyV1.execution_lineage = Root | RuntimeCall` and
`local_scope: SyntaxRuntimeLocalScopeV1`. A retained fold does not
create one lineage per runtime iteration; its program-local IDs remain scoped
by its template under the current root/call lineage. The source-wire schema
must use this generated definition rather than redeclare a competing scope.
Program sharing stores the relative body once; adding lineage to instantiated
slots does not clone that program.

Program rows are relative recipes. A call instance supplies its lineage/frame
overlay to those rows, so local/formal state and effects never alias another
call while no body-slot mapping table is copied. Every internal
program read, write, nested call, fold action, runtime event, exit value, and
caller writeback maps both ways to the call-scoped
`ExpectedSourceValueGraph`. Omitting the call from a phase/control proposal,
keeping only its return expression, or keeping its writes without the return
therefore fails. The pinned invoke action, rather than an SLT producer's
inlined body or analyzer value cache, owns the optional result and all effects.
The exact `ActionSemanticKind::InvokeRuntimeFunction` shape above is analogous
to the existing nested ForFold boundary; its expected nested graph and semantic
summary expose every setup/program/copyout read, write, binding, and effect.
Persistent storage is
`Theta(unique runtime type specializations + shared program rows + call
instances + lineage rows + actual/target/writeback rows)`. It does not copy a
callee body for each caller, caller path, input value, or result slice.

Runtime recursion is a structural capability boundary. Before specialization,
an iterative pass builds the direct runtime-call graph over raw function
templates from every verified function body and rejects the first canonical
self-edge or nontrivial SCC as `SOURCE.RUNTIME_FUNCTION_RECURSION`. Rejecting
at template level also prevents a generic recursive call from manufacturing an
unbounded sequence of unequal type specializations. The remaining DAG is
specialized and programs are completed in reverse topological order with an
explicit worklist; there is no call-depth, body-size, or compilation-time cap.
Every procedural function loop becomes the same language-versioned retained
ForFold transition/program relation in the function-program namespace; it is
not unrolled by a size heuristic and does not use a termination certificate.

A constant proof may still invoke an admitted input-only function through the
separate finite small-step certificate, where concrete input values belong to
the memo key and a terminating recursive invocation may be proved. That
certificate cannot prove a runtime call, replace its type-only shared program,
authorize output/inout, or turn a retained runtime loop into a VM trace.

Source-to-occurrence mapping preserves this boundary exactly:

```text
OccurrenceRuntimeRootOwnerV1 =
  AggregateCatalog {
    instance: SourceInstanceId,
    exact catalog-root SyntaxOccurrenceKeyV1
  } |
  ControlUnit { root: ControlRootRef } |
  Observer { occurrence: ObserverOccurrenceId } |
  StaticInitializer {
    instance: SourceInstanceId,
    exact initializer SyntaxOccurrenceKeyV1
  }

OccurrenceRuntimeExecutionLineageRowV1
  origin: SourceRef<RuntimeSourceExecutionLineageId>
  kind: Root {
          role: RuntimeSourceRootRoleV1,
          owner: OccurrenceRuntimeRootOwnerV1
        } |
        RuntimeCall {
          parent: OccurrenceRuntimeExecutionLineageId,
          call: OccurrenceRuntimeCallInstanceId
        }

OccurrenceRuntimeLocalScopeV1 =
  Body | ForFold(template: ForFoldTemplateId)

SourceToOccurrenceRuntimeLocalScopeRowV1
  source instance / source and occurrence execution-lineage IDs
  source: RuntimeSourceLocalScopeV1
  occurrence: OccurrenceRuntimeLocalScopeV1

OccurrenceRuntimeProgramActionCoordinateV1
  execution lineage: OccurrenceRuntimeExecutionLineageId
  local_scope: OccurrenceRuntimeLocalScopeV1
  specialization: VerifiedRuntimeFunctionSpecializationId
  action: RuntimeFunctionProgramActionId

OccurrenceRuntimeFunctionInvokeOwnerTargetV1 =
  Outer {
    action: ControlActionId,
    instance: OccurrenceRuntimeCallInstanceId
  } |
  RuntimeProgram {
    coordinate: OccurrenceRuntimeProgramActionCoordinateV1,
    site: RuntimeFunctionProgramNestedCallInstanceIdV1,
    instance: OccurrenceRuntimeCallInstanceId
  } |
  ForFold {
    template: ForFoldTemplateId,
    action: FoldActionId,
    instance: OccurrenceRuntimeCallInstanceId
  }

OccurrenceRuntimeFunctionInvokeV1
  owner_target: OccurrenceRuntimeFunctionInvokeOwnerTargetV1
  mapped operand/result role pools and formal-order writebacks
  exact mapped program boundary and independently replayed SemanticAccessSummary

OccurrenceRuntimeProgramCallInstantiationV1
  site: RuntimeFunctionProgramNestedCallInstanceIdV1
  execution_lineage: OccurrenceRuntimeExecutionLineageId
  local_scope: OccurrenceRuntimeLocalScopeV1
  instance: OccurrenceRuntimeCallInstanceId
  total bijection from (site, execution_lineage, local_scope) to instance

OccurrenceRuntimeFunctionCallInstanceV1
  id: OccurrenceRuntimeCallInstanceId
  owner invoke: OccurrenceRuntimeFunctionInvokeOwnerTargetV1
  origin: SourceOccurrence(SourceInstanceId,
                           ExpectedSourceRuntimeCallInstanceId) |
          ProgramInstantiation(OccurrenceRuntimeProgramCallInstantiationV1)
  mapped runtime specialization/program
  mapped execution lineage / OccurrenceRuntimeLocalScopeV1
  mapped actual objects, inputs, target handles, dynamic plans, and coercions
  mapped SourceRuntimeFunctionInvokeV1 operand/result roles and writebacks
  independently replayed occurrence read/write/binding/effect summary

SourceToOccurrenceRuntimeFunctionRelationV1
  total maps for call instances, execution lineages and four-role root owners,
    SourceToOccurrenceRuntimeLocalScopeRowV1, program slots and nested actions,
    formal/local objects, inputs/results, target handles, dynamic plans,
    coercions, events, and retained-ForFold rows
  total maps from every source-scoped outer/ForFold
    SourceRuntimeFunctionInvokeV1 to exactly one
    OccurrenceRuntimeFunctionInvokeV1 per SourceInstanceId, including owner
    variants, operand/result roles, result order, and formal-order writebacks
  total maps from every shared RuntimeProgram invoke site and each reachable
    OccurrenceRuntimeProgramCallInstantiationV1 to exactly one occurrence
    invoke with the mapped program-relative roles/writebacks
  exact inverse origin rows and boundary token/value equality for every map;
    no occurrence call/invoke/root/scope/program row lacks one source owner
  total `OccurrenceRuntimeProgramCallInstantiationV1` map for every reachable
    shared program site under every execution-lineage/local-scope pair
```

For each `(SourceInstanceId, source execution lineage, source local scope)` the
scope relation is a total bijection. `Body` maps only to `Body`.
`ForFold(SourceForFoldTemplateId)` maps only to
`ForFold(the exact ForFoldTemplateId selected by that instance's already
verified fold-template expansion)`, and the inverse recovers the same source
template. It is the tag/semantics which are unchanged; the checked template ID
namespace is always remapped. No iteration creates a scope row.

Root roles are likewise preserved exactly. `AggregateCatalog` and
`StaticInitializer` retain the source-instance-qualified syntax owner without
inventing a scheduled root; `ControlUnit` maps to the exact Whole
`ControlRootRef`; `Observer` maps to the exact scheduled
`ObserverOccurrenceId`. The `OccurrenceRuntimeRootOwnerV1` variant must equal
the source `RuntimeSourceRootRoleV1`, every source root-lineage row has exactly
one mapped occurrence row for its source instance, and every occurrence root
owner maps back. A catalog/initializer owner cannot masquerade as an executable
control root merely because its syntax or value is equal.

The occurrence invoke owner has the same inverse rule as the source owner:
every outer, runtime-program, or fold action whose kind is
`InvokeRuntimeFunction` owns exactly one occurrence invoke row, and non-invoke
actions own none. Source and occurrence owner variants must match under the
action/program/fold mapping; mapping an outer invoke into a program/fold owner,
or vice versa, rejects before token resolution.

Every outer/ForFold `SourceRuntimeFunctionInvokeV1` maps to exactly one
`OccurrenceRuntimeFunctionInvokeV1` for its source instance. A shared
RuntimeProgram invoke instead maps once for each reachable
`OccurrenceRuntimeProgramCallInstantiationV1`; its inverse is the pair of the
one structural source-program invoke and that exact instantiation row. Every
mapped invoke belongs to exactly one of those disjoint inverses. Operand/result role ordinals,
formal/member order, optional return, target preparation, program boundary,
and copyout order are unchanged; only phase/object/input/control namespaces and
independently verified instance type substitutions change. Two source
instances or two call lineages cannot merge their call instances even when the
program and all current bits are equal. Complete mapped programs may share
immutable structural content only after those origin mappings remain total.

The occurrence action inventory carries the corresponding closed
`InvokeRuntimeFunction` variant through draft-to-final ID mapping and token/
dynamic/Tri resolution. Any mapped Tri intent used by setup, program access, or
copyout must have its complete occurrence TriNet map before the call can
freeze. The unit transaction stages the call instance, action, lineage maps,
program boundary, target/writeback rows, and semantic summaries atomically;
failure publishes none. Downstream control, scheduling, atomization, and
lowering consume this resolved invoke as one pinned Whole action and may not
replace it with a return-only node or an unverified inline expansion.

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
        F32 | F64 | String |
        Clock/ClockPosedge/ClockNegedge |
        Reset/ResetAsyncHigh/ResetAsyncLow/ResetSyncHigh/ResetSyncLow |
        Alias(target RawTypeUseId) |
        Enum(RawEnumBaseRule, RawEnumEncoding, canonical variant range) |
        Struct(canonical RawMemberRange) | Union(canonical RawMemberRange) |
        Unsupported(RawUnsupportedTypeDeclarationKindV1)

RawUnsupportedTypeDeclarationKindV1 =
  SystemVerilog | Interface | Module | Package | Proto | Modport |
  Function | Void | Recovery

RawTypeUseOwnerRoleV1 =
  ModulePort | InterfacePort |
  FunctionPort | FunctionReturn |
  ProtoPort | ProtoConst | ProtoTypeBound |
  ObjectDeclaration | LocalDeclaration | StaticBinding |
  AliasTarget | StructMember | UnionMember | EnumBase |
  CastTarget | TypeValue | StructConstructorTarget |
  GenericConstBound |
  GenericArgumentFixedType | GenericArgumentIdentifierTypeProjection

RawTypeUseRow
  exact surface type-syntax occurrence
  owner: RawTypeUseOwnerRoleV1 / exact owning raw row and role-local ordinal
  core: Builtin(RawBuiltinTypeV1) |
        UserName(RawNameOccurrenceId with expected TypeOrGenericType namespace)
  generic use: RawGenericUseId
  modifiers: canonical RawModifierRange
  unpacked extents: canonical RawUnpackedExtentRange
  packed extents: canonical RawPackedExtentRange

RawTypeSource =
  Explicit(RawTypeUseId) |
  Inferred(RawTypeInferenceId) |
  DerivedForCounter(CeloxSourceV0_20SignedI32)

RawTypeInferenceRow
  exact declaration and lexical scope
  source: Initializer(RawConstExprOccurrenceId) |
          Assignments(canonical RawTypeInferenceCandidateRange)

RawTypeInferenceCandidateRow
  source ordinal / exact assignment occurrence and target owner
  operator/select-path classification / exact right expression

RawResolvedTypeWitnessRow
  exact declaration / proposed analyzer resolved type
  comparison witness only

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
  kind: Type |
        Inst(exact retained proto-interface path/bound) |
        Proto(exact retained fixed/named proto bound) |
        Const(bound: RawTypeUseId)
  default: None | Some(RawGenericArgumentId)

RawGenericUseRow
  exact surface owner: TypeUse(RawTypeUseId) |
    FunctionCall | ModuleInstance | InterfaceInstance | PackageUse |
    ProtoBound
  exact source coordinate / parent lexical scope
  canonical RawGenericArgumentRange; a nongeneric use owns the canonical
    empty range

RawGenericArgumentRow
  exact owner: RawGenericUseId at source ordinal |
               declared default of one RawGenericFormalId
  syntax: FixedTypeSyntax(RawTypeUseId) |
          Identifier { name: RawNameOccurrenceId,
                       type_projection: RawTypeUseId,
                       const_projection: RawConstExprOccurrenceId } |
          NumberSyntax(RawConstExprOccurrenceId) |
          BooleanSyntax(RawConstExprOccurrenceId)

RawGenericEnvironmentWitnessRow
  analyzer specialization identity / exact surface generic use
  optional parent witness / canonical selected-binding witness range /
  canonical extent-resolution and enum-resolution witness ranges
  comparison witness only; it is never used to choose a verified environment

RawGenericBindingWitnessRow
  owner witness / exact formal ID / declaration ordinal
  selection: Explicit(RawGenericArgumentId) | DeclaredDefault
  analyzer resolved argument witness:
    Type(exact type identity) | Inst(exact instance identity) |
    Proto(exact proto identity) | Const(exact typed value witness)

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
  type_source: RawTypeSource
  initializer: None | Expression(RawSourceExprId)
  module_context: RawObjectTypeContextId

RawObjectTypeContextRow
  exact object / module identity
  owner scope: ModuleTop | InterfaceTop | ProcessBlock |
               FunctionBody | ForFoldBody
  declaration role: Port(direction) | Variable | Let | InterfaceMember |
                    Binding | ForFoldBinding | FunctionArgument |
                    FunctionLocal
  exact concrete-inout classification

RawModuleTypeContext
  exact module identity
  candidate traversal: DerivedCanonicalVerylV0_20
  mapped analyzer default-clock/default-reset object witnesses, if any
  exact port/storage role required by source TriIntent and default-clock/reset
  rules
```

`RawTypeUseOwnerRoleV1` is exhaustive; there is no `Declaration`, `Other`, or
coordinate-only fallback. `ModulePort`/`InterfacePort` own the explicit type of
the corresponding port object. `FunctionPort` covers the one shared
`RawFunctionPortRowV1` for either a definition or prototype, while
`FunctionReturn` is owned by its exact `RawFunctionTemplateRowV1`. `ProtoPort`,
`ProtoConst`, and `ProtoTypeBound` distinguish the three type-bearing proto
positions. `ObjectDeclaration` covers a module/interface storage object and
`LocalDeclaration` covers the exact process/function block declaration; their
`RawObjectTypeContextRow` must agree with the role. `StaticBinding` owns an
explicit type on a parameter/const/gen/local static binding.

`AliasTarget`, `StructMember`, `UnionMember`, and `EnumBase` point back to the
exact type/member declaration row. `CastTarget`, `TypeValue`, and
`StructConstructorTarget` point to the exact expression projection which owns
that type syntax. `GenericConstBound` belongs only to a `Const` generic formal.
The two generic-argument variants distinguish an actual/default written as
fixed type syntax from the verifier-derived type projection of identifier
syntax; the unused constant/instance/proto projection owns no type use. Every
role has one exact inverse owner and every admitted surface type syntax has one
role. A role swap fails even if the spelling, normalized type, owner coordinate,
and generic environment are equal.

An inferred declaration has no surface type syntax and therefore creates no
`RawTypeUseRow` and cannot use `RawTypeUseOwnerRoleV1::Other` or
`InferredReplay`. Its producer comparison key is the separate closed form:

```text
SyntaxInferredTypeReplayKeyV1
  owner: exact declaration SyntaxOccurrenceKeyV1
  environment: exact environment-lineage reference
```

It maps to the declaration's `RawTypeInferenceId` and independently replayed
type, never to a fabricated empty type-use row. The producer-facing
`SyntaxTypeUseKeyV1` imports `RawTypeUseOwnerRoleV1` as its generated role alias;
inferred-type witnesses use `SyntaxInferredTypeReplayKeyV1` instead.

`DerivedCanonicalVerylV0_20` owns no stored range or candidate pool. The
verifier walks the already owned module/interface port and item pools in their
canonical order, selects exactly the eligible object/type-use rows, and derives
the clock and reset candidate sequences and final explicit/implicit choices.
The optional analyzer objects are syntax-keyed witnesses mapped only after
that traversal; they are compared with, and never seed, the result. Therefore
no `ClockCandidate`/`ResetCandidate` syntax pool kind is missing from the
authoritative registry.

Every `Raw*WitnessRow` in that illustration is a verifier-derived mapped
mirror, not a producer-facing row. The producer supplies only the closed
syntax-keyed `SyntaxAnalyzerWitnessRowV1` relation described at the staging
boundary: its owner and every referenced syntax occurrence are
`SyntaxOccurrenceKeyV1`/closed witness-pool entries. After the complete raw
syntax topology and occurrence-lineage scan succeeds, the verifier builds the
one-to-one occurrence-key-to-private-ID map, translates the witness into the
`RawResolvedTypeWitnessRow`, `RawExtentResolutionWitnessRow`,
`RawEnumResolutionWitnessRow`, `RawEnumVariantValueWitnessRow`,
`RawGenericEnvironmentWitnessRow`, and `RawGenericBindingWitnessRow` views
above, and then compares those mirrors with independently derived semantics.
Missing, duplicate, extra, ambiguously keyed, or unmappable witness rows fail
before semantic lookup. A producer can never name a private `RawTypeId`,
`RawExtentId`, `RawEnumVariantId`, or private witness ID, even if its integer
happens to be in range. Mapped witness mirrors belong to witness resources,
not `PrivateRawSyntaxTableKindV1` or `PrivateRawSyntaxPoolKindV1`.

`RawBuiltinTypeV1` is exactly the builtin subset listed by `RawTypeRow.kind`
from `Bit` through the reset kinds, including `F32`, `F64`, and `String`; it has
no user/generic variant. A `UserName` is resolved independently and then yields
either a named `RawTypeId` or the in-scope `RawGenericFormalId` of kind `Type`.
The analyzer-proposed target remains only a resolution witness.

Generic argument syntax is intentionally not preclassified by the analyzer.
After the generic owner and formal list resolve, `FixedTypeSyntax` is admitted
only for a `Type` formal. `Identifier` is resolved under the formal's expected
namespace and may become Type, Inst, Proto, or a Const reference. Its name row
uses `DeferredGenericFormalKind` and cannot be looked up until that formal is
known. The type/reference-expression projections are both verifier-derived
views of the same exact identifier syntax; exactly the projection selected by
the formal enters the semantic relation and the other creates no proof/use.
Number and
Boolean syntax are admitted only for `Const` and become exact constant-expression
proofs. Any other formal/syntax pairing, ambiguity, or kind mismatch rejects;
the adapter cannot relabel one syntax row to the kind it expects.

`Explicit` is the only variant which owns surface type syntax.
`Inferred` owns the complete candidate relation, not an analyzer-resolved type.
For an initializer declaration and for assignment-inferred `var`, the verifier
uses the closed atomic-expression, first-candidate, exact-conflict rules in
`typed-constant-evaluation.md` over the complete source-ordered syntax range;
missing, extra, reordered, noninferable, use-before-inference, or conflicting
candidates reject. The same relation applies outside constant functions.
`DerivedForCounter` is used only by a retained `for` binding and derives the
exact signed two-state `i32` rule without inventing a type-use occurrence.
`RawResolvedTypeWitnessRow` is compared only after derivation.

`F32`, `F64`, and `String` are closed raw tags, not spellings of the
`unsupported` tag. Their presence at the raw boundary does not make them
executable. The joint relation derives one closed class for every verified type
use:

```text
VerifiedTypeUseClass =
  Executable(VerifiedNormalizedType) |
  NonExecutableProof(VerifiedProofOnlyType)

VerifiedProofOnlyType =
  F32 { fixed storage width 32, persistent type-only shape } |
  F64 { fixed storage width 64, persistent type-only shape } |
  String { no fixed packed-storage width }
```

A direct proof-only `F32`/`F64` use rejects an explicit packed extent and every
`Signed`/`Tri`/`Default` modifier, retains ordinary unpacked extents, and uses
its fixed terminal only for the closed `$bits` and `$size` type queries. The
V0_20 proof-only `String` use is scalar and rejects modifiers and extents; its
constant values are governed by the typed-constant relation and it has no
implicit bit-vector width. An alias may retain a proof-only target and its
classification, but no alias, modifier, extent, member insertion, or equal
storage width may turn it into `Executable`. Only the `Executable` variant may
produce a `VerifiedNormalizedType`, semantic object, input, or SLT constant.

The raw syntax classes above are deliberately orthogonal to the four semantic
generic kinds. Only after the owner/formal resolves does the verifier derive
the disjoint `VerifiedGenericArgument = Type | Inst | Proto | Const` sum. A
fixed type maps only to `Type`; number/Boolean syntax maps only to `Const`; an
identifier is resolved under the formal-selected namespace and uses exactly its
type or constant projection when needed. `Inst` and `Proto` retain the same
identifier syntax but resolve in their distinct namespaces. An analyzer-
selected symbol/kind remains comparison evidence and cannot relabel the raw
syntax.

Every explicit/default argument must have exactly the kind of its formal. A
`Const` formal's bound `RawTypeUseId` is owned by that formal and is normalized
in its declaration/preceding-formal environment. Its selected actual receives
an `ExpectedTypedConstantExprId` in the exact use environment and is checked
against that bound by the closed generic-constant coercion rule. Equal shape,
bit content, or analyzer specialization identity cannot allow a `Type`,
`Inst`, `Proto`, or `Const` argument to enter another variant.

The private `RawTypedSourceHIR<'a>` view borrows token spelling/magnitude bytes
from the parsed-source owner; the verifier boundary
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
must begin at the current `TypeUnpackedExtent` cursor and each packed range at
the independent current `TypePackedExtent` cursor; each end becomes its own
next cursor and an empty range names that pool's current cursor. The final two
cursors must equal their respective pool lengths.
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
actual are checked from retained source rows. `Type`, `Inst`, `Proto`, and
`Const` have distinct verified argument variants and compatibility relations;
an instance identifier cannot masquerade as an equal-shaped type, and a
constant with equal bits cannot masquerade as any of the other three kinds.
Every constant actual, including number, boolean, and constant-identifier
syntax, is a raw constant-expression occurrence and receives its own typed
proof against the formal's retained bound.

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

The joint dependency relation has exactly these closed node kinds:

```text
TypeUseInstance | GenericEnvironment | ConstBinding | ConstExprProof |
EnumVariantReplay(enum, declaration ordinal) | EnumFinalize(enum) |
FunctionSignature | FunctionSpecialization
```

Every type prerequisite, eager value prerequisite, and guarded value use is
independently derived from source ownership. The exact edge classes and demand
rules are specified by
[Typed constant evaluation and finite execution certificates](./typed-constant-evaluation.md).
An iterative SCC pass rejects every self-edge or nontrivial SCC in the static
type/eager subgraph, including a width/enum/generic constant which eagerly
depends back on itself. A short-circuited operand, unselected conditional arm,
ordered decision result, or function execution is instead a guarded value
use: it is fully type checked, but becomes a value dependency only when the
closed evaluator demands it. A concrete demanded guarded cycle is rejected by
the evaluator's iterative visiting-state relation; a dead guarded edge is not
misclassified as an eager cycle.

Enum bindings are replayed and published strictly in declaration order. An
implicit variant at ordinal zero derives the encoding's closed initial value;
every implicit `EnumVariantReplay(enum, ordinal)` with nonzero ordinal has one
eager predecessor edge to `EnumVariantReplay(enum, ordinal - 1)`. An explicit
variant has one eager edge to its `ConstExprProof`. A reference in that proof
to a previously published variant of the same explicit-fixed-base enum adds an
eager edge to that earlier `EnumVariantReplay`. A reference to the current
variant or any later variant is rejected as a forward enum reference even if
another graph path would otherwise be acyclic. The worklist may type an
explicit recipe before its turn, but it cannot complete or publish replay
ordinal `n` before all lower ordinals have completed. Completion publishes the
recipe binding only for use by later same-enum replay; the externally visible
final member binding is published only by `EnumFinalize`. A producer-supplied
dependency order cannot change this sequence.

`EnumFinalize(enum)` depends on every variant replay and alone derives an
inferred width, final lossless coercions, encoding recurrence, and uniqueness.
Consequently, in `CeloxSourceV0_20`, an explicit recipe of an inferred-base
enum may not reference even an earlier variant of that same enum: the
reference's packed width would depend on `EnumFinalize`, while finalization
would depend on the recipe. That circular-width form is rejected as a
dependency cycle rather than evaluated at an analyzer-guessed width. A same-
enum prior-variant recipe reference is admitted only for `ExplicitFixed`,
whose width is independently known. A future profile may admit the inferred-
base form only by defining a separately versioned symbolic-width constraint
relation.

Constant-function call/backedge execution edges are not static definition
edges: recursive concrete calls and loops use the separate finite execution-
trace relation, so this SCC rule is not a recursion or iteration cap.
Completed SCC condensation order fixes the static prerequisite order and the
closed demand machine fixes guarded evaluation order; analyzer dependency
order is not an oracle.

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
modport, type-valued, and void kinds are rejected outright. `F32`, `F64`, and
`String` instead normalize only to the closed `NonExecutableProof` variant
defined above. Requiring an executable type from that variant is a structured
type-kind error; it never falls back to `Bit`.

This is the executable-type boundary. The joint constant relation uses the
proof-only float shape for both `$bits` and `$size` of `f32`/`f64`, and uses
the proof-only string marker for the limited typed-string values specified by
[Typed constant evaluation](./typed-constant-evaluation.md). None can become a
`VerifiedNormalizedType`, semantic object, input, or SLT constant. No float
value is evaluated under V0_20, and `String` acquires no invented packed width.

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

Same-enum references obey the joint replay rule above. An explicit recipe may
refer only to an earlier, already replayed member, and only when the enum uses
`ExplicitFixed`; current and later members are forward-reference errors. For
`Omitted`, `InferBit`, or `InferLogic`, even a prior-member recipe reference is
the rejected circular-width dependency, never a request to reuse an analyzer
member width.

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
signed. This is exactly the two-stage
`InferredEnumRecipeStageV1::{SelfDeterminedRecipe, FinalBaseReplay}` relation
defined in [Typed constant evaluation](./typed-constant-evaluation.md): the
first stage rejects all-bits/context-filled recipes, same-enum references,
user calls, and every operation which needs the unknown enum width; recurrence
then supplies exact unbounded recipes, `EnumFinalize` derives width, and the
second stage applies final lossless coercion without reevaluating an expression
or discovering a dependency. No step truncates to a provisional width.
`ExplicitFixed` skips this special stage. `Omitted`, `InferBit`, and
`InferLogic` must use it; an analyzer-width context cannot replace either
stage.

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

A proof-only direct `F32` or `F64` uses the same persistent shape machinery
with fixed `Packed(32)` or `Packed(64)` terminal solely for type queries; its
class remains `NonExecutableProof`. A proof-only `String` has no
`VerifiedTypeShape` terminal. Alias normalization propagates the proof-only
class together with the target shape/marker and cannot erase that boundary.

Each demanded `(RawTypeUseId, VerifiedGenericEnvironmentId)`
`TypeUseInstance` evaluates and copies its environment-qualified extent range
once into the verified extent arena and creates at most one unpacked and one
packed segment. The same surface use demanded in two unequal generic
environments is two instances; equal raw extent syntax is not permission to
reuse a result whose constant environment differs. Exact completed instance
content may be interned only after the two environment-qualified proofs compare
equal. An alias instance with an empty own range shares the target-instance
head; otherwise its segment points to that target head. Alias target order is therefore
`own unpacked ++ target unpacked`, then
`own packed ++ target packed ++ target terminal`. In Veryl 0.20 an explicit
outer alias width precedes the target width, so aliasing a bare `Bit` may
legitimately retain both `Packed(outer)` and the target's selectable
`Packed(1)`.

Only a semantic object or other required root materializes dimensions: walk
the unpacked chain outer-to-target, then the packed chain outer-to-target,
append the one terminal dimension, and derive suffix strides from right to
left. The final suffix product must equal the independently checked summary.
Construction is
`Theta(demanded TypeUseInstances + evaluated extent instances + shape segments)`
plus the size of materialized root dimensions and canonical content-index
comparisons. It is not bounded merely by surface type rows or raw extents,
because demanded specializations are real semantic work. Copying every target
vector into every alias, and therefore `Theta(depth^2)` behavior in either
alias depth or specialization count, is forbidden.

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
materialization, plus runtime-function specialization/program/call/lineage,
actual-role, target-handle, and writeback pools. Failure returns one
allocation-free structured error and
leaves all externally visible lengths, mappings, brands, and owners unchanged.
No `String`, formatting allocation, panic, partial commit, retry with a smaller
representation, or fallback path is permitted.

### Lossless aggregate error embedding

The typed-constant verifier's allocation-free
[`TypedConstantErrorV1`](./typed-constant-evaluation.md) is already a complete
machine-readable failure. The enclosing source preparation must preserve it as
a nested value rather than format it or translate its stable fields into a
less-specific aggregate rule:

```text
SourceAggregateErrorV1 =
  SourceLocal(SourceAggregateLocalErrorV1) |
  TypedConstant(TypedConstantErrorV1)

SourceAggregateLocalErrorV1
  rule: SourceAggregateRuleIdV1
  phase(): closed source framing/type/node/provenance/aggregate phase derived
           only from immutable RULE_META_V1[rule]
  owner: closed allocation-free source owner
  context: closed allocation-free source context
```

The flat framing record illustrated in `source-wire-format.md` is the
`SourceAggregateLocalErrorV1` payload of `SourceLocal`, not permission to flatten
or rename a nested verifier error. The eventual aggregate API and schema must
expose the tagged sum above.

`SourceAggregateErrorV1::TypedConstant` stores the nested error inline. Its
construction is an infallible, allocation-free move; it uses no `String`,
`format!`, `Box`, copied source text, erased numeric context, or replacement
`SourceAggregateRuleIdV1`. Equality, optional machine encoding, and tests observe
the exact nested `rule`, `phase`, `owner`, and `context`. `Display` may delegate
to lazy formatting only after the error has crossed the verifier boundary.

The nested variant retains the typed-constant verifier's deterministic error
precedence. The outer aggregate may add no competing semantic check after that
failure and may not retry through a source-local fallback. Returning either
variant drops private preparation and leaves the caller's externally visible
input, owners, brands, mappings, and published lengths unchanged.

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
  static_domain: Bit | Logic
  value_class: Evaluation | MaterializedStorage
  positive_type: Plain | Positive

PhaseTypedValueOriginRow<P>
  value: VerifiedTypedValueId<P>
  origin: exact ExpectedTypedConstantExprId or closed derived-value origin
```

Bits above `width` are zero. Static domain and current evaluation content are
separate facts. A `MaterializedStorage` Bit value has an empty X/Z mask, while
an `Evaluation` value may retain X/Z even with static Bit domain until a real
assignment/cast/store boundary. Mask zero means a known data bit; mask one with
payload zero means X and mask one with payload one means Z. Identity/select/
concat preserve X versus Z, while an operation whose closed truth table
produces an unknown uses the specified canonical X result. Mathematical enum/extent/cost arithmetic
uses separately typed `VerifiedNatural` and `VerifiedSignedMagnitude` views on
the same word substrate; a fixed-width Logic payload is never reinterpreted as
one of them merely because its mask is zero.

Value content and proof origin are separate relations. Equal typed bit content
may share one value row, while every constant proof/node role still owns its
exact `PhaseTypedValueOriginRow`; numeric value-ID equality never proves
provenance. Content identity includes `value_class` and static domain. Input/
storage cells are materialized, but the value produced by reading one copies
the independently derived access result class below. A whole, member-only, or
unpacked-only read whose every invalid alternative is a recursive storage
default remains `MaterializedStorage`. An access which can synthesize packed
invalid lanes produces `Evaluation`, even when the selected static domain is
Bit. Operator literals/results are evaluation values unless their exact
verified value row names a real materialization. A store or other two-state
materialization creates a distinct checked value row after the source
coercion. This prevents a phase optimization from clearing X at every Bit-
typed operator or from carrying X through an actual Bit store.

### Closed phase node and fact transfer relation

Phase nodes and their variable-size payloads contain only checked IDs/ranges.
`PhaseSLTNodeV1<P>` is exactly this sum:

```text
PhaseValueUseV1<P>
  value: PhaseNodeId<P>
  coercion: VerifiedPhaseCoercionId<P>

PhaseSLTNodeV1<P> =
  Input { input: PhaseInputId<P>,
          runtime_indices: PhaseNodeIdPool<P> } |
  Constant { value: VerifiedTypedValueId<P> } |
  Coerce { operand: PhaseValueUseV1<P> } |
  Unary { op: PhaseUnaryOpV1, operand: PhaseValueUseV1<P> } |
  Binary { lhs: PhaseValueUseV1<P>, op: PhaseBinaryOpV1,
           rhs: PhaseValueUseV1<P> } |
  Mux { condition: PhaseValueUseV1<P>,
        then_value: PhaseValueUseV1<P>,
        else_value: PhaseValueUseV1<P> } |
  ForFold { template: PhaseForFoldTemplateId<P>,
            start: PhaseLoopBoundV1<P>, end: PhaseLoopBoundV1<P>,
            step: VerifiedTypedValueId<P>,
            states: PhaseForFoldStatePool<P>,
            effects: PhaseForFoldEffectPool<P>,
            result_state_ordinal: checked u32,
            continue_condition: PhaseValueUseV1<P> } |
  Concat { parts: PhaseConcatPartPool<P> } |
  Slice { value: PhaseNodeId<P>, access: nonempty BitAccess }

PhaseUnaryOpV1 =
  Ident | Minus | BitNot | LogicNot | ReduceAnd | ReduceOr | ReduceXor

PhaseBinaryOpV1 =
  Add | Sub | Mul | Div | Rem | And | Or | Xor |
  Shl | Shr | Sar |
  Eq | Ne | EqWildcard | NeWildcard |
  LtU | LtS | LeU | LeS | GtU | GtS | GeU | GeS |
  LogicAnd | LogicOr

PhaseLoopBoundV1<P> =
  Constant { value: VerifiedTypedValueId<P>,
             coercion: VerifiedPhaseCoercionId<P> } |
  Runtime(PhaseValueUseV1<P>)

PhaseConcatPartRowV1<P>
  value: PhaseValueUseV1<P>

PhaseForFoldStateRowV1<P>
  target: PhaseObjectAtom<P>
  initial: PhaseValueUseV1<P> with materializing target coercion
  update: PhaseValueUseV1<P> with the same materializing target type

PhaseForFoldEffectRowV1<P>
  exact runtime site / optional predicate PhaseValueUseV1<P>
  emit rule / arguments: PhaseNodeIdPool<P> / optional fatal code
```

`Coerce` is the value occurrence for an explicit cast or other independently
required first-class coercion result; assignment/store edge coercions remain
owned by their action. `Nand`, `Nor`, reduction XNOR, binary XNOR, and
arithmetic-left shift have
canonical verifier-derived recipes using the listed nodes (`ReduceAnd/Or/Xor`
or `Xor` followed by `LogicNot`/`BitNot`, and `Shl` after the exact arithmetic-
left typing check). Runtime power has no `PhaseBinaryOpV1` tag in this profile;
an occurrence which was not completed by the static constant relation is
`LOWER.CAPABILITY.RUNTIME_POWER`, not a numeric tag cast to another operator.
No recovery/unknown node variant exists.

The one retained fact row is:

```text
PhaseSLTNodeFactV1<P>
  width: nonzero usize
  signed: bool
  positive_type: Plain | Positive
  static_domain: Bit | Logic
  value_class: Evaluation | MaterializedStorage
  mask_class: AlwaysZero | MayCarryXZ
  lowerable: bool

PhaseSLTNodeFactsV1<P>
  exact owning phase/artifact brand and FrozenSLTNodeArena<P> identity
  rows: dense [PhaseSLTNodeFactV1<P>]
  exactly one row at the same ordinal as every PhaseSLTNodeV1<P>, and no extra
```

`PhaseSLTNodeFactsV1<P>` is the only retained fact-table artifact. It is
constructed and replayed as one aggregate with its same-phase node arena and
cannot be independently forged, paired with another arena, truncated, or
extended. A singular `PhaseSLTNodeFactV1<P>` is only a checked row view borrowed
from that owning table.
Before aggregate commit, node replay owns only the private complete dense
`PreparedPhaseSLTNodeFactsV1<P>` described by the source staging contract. It
has the pending arena-owner identity but no frozen arena or public brand.
Commit consumes that prepared table and its matched staged arena and creates
`FrozenSLTNodeArena<P>` plus retained `PhaseSLTNodeFactsV1<P>` together; neither
is a valid retained artifact before that point.

`AlwaysZero` is a sound, locally checkable proof that no execution of this node
can contain X/Z. `MayCarryXZ` is its conservative complement (“not proved
always zero”), not a claim that some execution necessarily contains X/Z. V1
computes the strongest proof in its closed local abstraction: child facts,
canonical node identity, exact coercions, and canonical constant content may
be inspected, but it performs no global Boolean theorem proving. This makes
the result deterministic and prevents a producer from strengthening it.

Coerce, unary, and binary child facts are the uncoerced natural facts named by
`PhaseValueUseV1.value`. The expected typed value occurrence independently
derives each operand target and exact coercion ID: unary result-context
propagation derives its one operand use; binary common-width/common-signed/
domain typing derives both uses. Replay compares both stored coercion IDs with
that relation and applies them before the value/mask rule. Result signedness is
derived from the raw natural operand signedness as specified by the operator,
while extension uses the independently derived common target signedness. A
bare child ID or producer common-width flag cannot recover this distinction.
`Input.runtime_indices` is the intentional exception to storing a
`PhaseValueUseV1` inline: its exact positional operand coercion is already
owned once by the referenced `InputAccessFact` runtime-role row and replay
checks each child against that row. The ForFold step/operator coercion is
similarly owned once by the verified template. Neither may be repeated with a
producer-selected second coercion in the node payload. ForFold effect argument
IDs are likewise positional projections of their exact verified runtime-site
argument/coercion rows; the optional effect predicate remains a
`PhaseValueUseV1` because its condition coercion is part of the effect node.

Transfer runs only after the allocation-free edge scan proves every child is a
strictly earlier same-phase ID. It then requires the exact Input child count
and order; nonzero child/operand widths; equal derived binary common target
types; equal wildcard-comparison operand widths; a width-one verified
condition use for `Mux` and every predicate; nonempty concat parts with
self-determined part coercions; an ordered in-bounds nonempty `Slice`; and the
complete ForFold state/effect/result inverse. Failure publishes no fact row and
does not continue with a conservative guessed type.

Let `J(Bit, Bit) = Bit` and every other domain join be `Logic`. `Z(x)` means
`x.mask_class == AlwaysZero`. A non-materializing coercion changes the width,
signedness, positive class, and static domain required by its verified target
but preserves possible X/Z through extension/truncation, including when that
target domain is Bit. A materializing coercion first applies the same extension
basis and then, only for a Bit target, maps known one to one and 0/X/Z to zero;
its result is `MaterializedStorage` and `AlwaysZero`. A materialized Logic
target preserves the mask and is `AlwaysZero` only when its source was proved
so. Every coercion kind, basis, target fact, and materialization flag is
independently derived from its expected value-use role. Arithmetic unary,
binary, mux-arm, condition, concat-part, and selector-normalization roles are
non-materializing;
`Coerce` uses the explicit cast/coercion role and may be materializing;
ForFold state initialization/update and real assignment/store boundaries use
the exact materializing role, and an explicit cast materializes exactly when
its verified target rule requires it (in V0_20, the two-state Bit boundary).
A producer cannot mark an ordinary operand as materializing to clear its mask.

The following transfer table is exhaustive. “common” means the exact
contextual typing rule in the typed-constant result table, not `max` applied by
the producer. “all child lowerable” includes every runtime index, bound,
state-initial/state-update, effect predicate/argument, and continue child. In
the table, `R(u)` is the uncoerced natural fact of a `PhaseValueUseV1` and
`C(u)` is its fact after the independently verified operand coercion. Every
width/domain/mask rule uses `C(u)` unless it explicitly says raw natural
signedness; merely loading the fact of `u.value` and ignoring `u.coercion` is a
verification failure.

| Variant | width / signed / positive | static domain / value class | `mask_class` |
| --- | --- | --- | --- |
| `Input` | copy `selected_width`, `result_signed`, and `result_positive_type` from the exact `InputAccessFact` | copy `result_static_domain` and independently derived `result_value_class` | copy the derived input mask rule below |
| `Constant` | copy the exact verified value width, signedness, and positive class; width must be nonzero | copy its static domain and value class | `AlwaysZero` iff its canonical X/Z mask plane is empty |
| `Coerce` | copy `C(operand)` width/signed/positive | copy `C(operand)` domain and value class | copy the exact coerced mask proof; a materializing Bit coercion is `AlwaysZero` |
| `Unary Ident` | `C(operand)` width/signed/positive | `C(operand)` domain; `Evaluation` | `C(operand)` mask |
| `Unary Minus/BitNot` | `C(operand)` width/signed; `Plain` | `C(operand)` domain; `Evaluation` | `AlwaysZero` for an `AlwaysZero` coerced operand or when exact coerced-constant evaluation has an empty result mask; otherwise `MayCarryXZ` |
| `Unary LogicNot/Reduce*` | width one, unsigned, `Plain` | `C(operand)` domain; `Evaluation` | the same exact-coerced-constant exception; otherwise `AlwaysZero` only for an `AlwaysZero` coerced operand |
| arithmetic/bitwise `Binary` | common coercion-target width; signed iff both `R(lhs)` and `R(rhs)` are signed; `Plain` | join of `C(lhs)`/`C(rhs)` domains; `Evaluation` | exact coerced constant operands are evaluated by the closed value rule; otherwise use the per-op mask rules below |
| shift `Binary` | `C(lhs)` contextual width and `R(lhs)` signedness; `Plain` | `C(lhs)` domain; `Evaluation` | exact coerced constant operands are evaluated; otherwise use the per-op mask rules below |
| relational/equality/logical `Binary` | width one, unsigned, `Plain` | join of `C(lhs)`/`C(rhs)` domains; `Evaluation` | exact coerced constant operands are evaluated; otherwise use the per-op mask rules below |
| `Mux` | exact common coerced-arm width; signed iff both raw natural arms are signed; `Positive` only when both verified coerced-arm target types are the same Positive type, otherwise `Plain` | join of coerced-arm domains; `Evaluation` | `AlwaysZero` iff both coerced arms are `AlwaysZero` **and** (`C(condition)` is `AlwaysZero` **or** the arms have the same canonical node/coercion identity **or** they are equal canonical known-two-state constants by content); otherwise `MayCarryXZ` |
| `ForFold` | copy the independently verified selected result-state access width/signed/positive | selected state target domain; `MaterializedStorage` | `AlwaysZero` iff the target is Bit, or both the selected state's initial and update values are `AlwaysZero` after their materializing target coercions; otherwise `MayCarryXZ` |
| `Concat` | checked nonzero sum of nonempty coerced self-determined part widths; unsigned, `Plain` | join of coerced part domains; `Evaluation` | `AlwaysZero` iff every coerced part is `AlwaysZero` |
| `Slice` | checked `msb-lsb+1`; unsigned, `Plain` | child domain; `Evaluation` | `AlwaysZero` for an `AlwaysZero` child or when an exact constant child has an empty mask in the selected range; otherwise `MayCarryXZ` |

The nonconstant per-op mask rules are:

- `Div`/`Rem` are `AlwaysZero` only when both coerced operands are `AlwaysZero` and
  the coerced RHS is a canonical known-nonzero constant.
- `And` is also `AlwaysZero` when either coerced operand is a canonical known
  all-zero constant; `Or` is also `AlwaysZero` when either is canonical known
  all-one at the common width. Other arithmetic/bitwise tags require both
  coerced operands `AlwaysZero`.
- `Shl`/`Shr` are also `AlwaysZero` for a canonical known count at least the
  result width; otherwise every shift requires both coerced operands `AlwaysZero`.
  `Sar` does not use that exception because an unknown sign bit remains X.
- `LogicAnd` is also `AlwaysZero` with a canonical known-false operand and
  `LogicOr` with a canonical known-true operand. Other logical, relational,
  and ordinary-equality results require both coerced operands `AlwaysZero` in
  this local abstraction. Wildcard equality has one additional proof: a
  canonical RHS pattern whose every coerced bit is X/Z ignores every position
  and therefore has a known result; otherwise it too requires both operands
  `AlwaysZero`.

For `Div` and `Rem`, a dynamic divisor remains `MayCarryXZ` even when both
operand static domains are Bit: runtime zero produces the specified all-X
evaluation result. A constant zero divisor also produces all-X and is not a
verification error. The known-nonzero exception above is established only by
canonical typed-value content after the exact common coercion; a producer
nonzero flag is not evidence. Other arithmetic with an X/Z-bearing coerced
operand and an unknown coerced shift count likewise retains transient X in
`Evaluation` even when the static result domain is Bit.

For `Mux`, `IfReduction` is not used. A known coerced condition selects one coerced
arm, while an X/Z condition performs the closed bit merge. Consequently two
known but unequal Bit arms can synthesize X under an unknown condition. That X
survives in a Bit-domain `Evaluation` fact until a later actual Bit
materialization. Merely seeing `static_domain = Bit` must not derive
`AlwaysZero`. The identical-arm exception applies only after both coerced arms
are proved `AlwaysZero`; two references to the same X/Z-bearing value remain
`MayCarryXZ` because X/X and Z/Z merge to canonical X.

ForFold header/continue X uses `IfReduction` to choose a control edge; it does
not merge a predicate bit into state content. Every zero-iteration result comes
from the selected initial value and every iterated result from the selected
materialized update. Therefore the table's two `AlwaysZero` state proofs are
closed and no counter/transition rule can inject X into an otherwise known
Logic state. A Bit target is independently `AlwaysZero` because each state
materialization clears X/Z.

An input access derives `result_value_class` and `result_mask_class` exactly as
follows:

1. A static in-bounds whole/member/unpacked/packed projection, and a dynamic
   access whose every possible invalid dimension is
   `UnpackedStorageDefault`, yields `MaterializedStorage`; the recursive
   unpacked default is itself a materialized storage value.
2. Any `ConstantUnknownXZ` or runtime packed selector, or any indexed packed
   part select whose compact interval plan can contain an invalid lane, yields
   `Evaluation`. Its invalid lanes are X even when the selected object is Bit.
3. A `MaterializedStorage` Bit result is `AlwaysZero`. A Logic result is
   `MayCarryXZ`. An `Evaluation` result is `MayCarryXZ` when a packed invalid/X
   path exists or its selected Logic storage may carry X/Z; otherwise it is
   `AlwaysZero`.

Runtime index child mask alone does not prove bounds: a known two-state index
can still be out of range. Conversely, an unpacked-invalid Bit read remains a
materialized zero default. These rules are replayed from selector provenance,
dimension kind, exact constant proof, and the at-most-three-interval packed
plan; they are not a producer-supplied `zero_mask` bit.

Lowerability is versioned separately from semantic validity:

```text
LoweringCapabilityV1 = Supported | Unsupported(PhaseLoweringReasonV1)

PhaseLoweringReasonV1 =
  RuntimePower | NonExecutableType | UnsupportedRuntimeEffect |
  BackendPrimitiveUnavailable(PhasePrimitiveV1)

PhasePrimitiveV1 =
  MemoryRead | EnvironmentRead | StaticCompositeProjection |
  DynamicLaneRead | DynamicLaneWrite | ConstantValue | Coerce |
  Unary(PhaseUnaryOpV1) | Binary(PhaseBinaryOpV1) |
  TernaryMerge | ForFoldTransition | ForFoldEffect |
  Concat | Slice |
  RuntimeEvent(PhaseRuntimeEventPrimitiveV1)

PhaseRuntimeEventPrimitiveV1 =
  Display | Write | AssertContinue | AssertFatal | Finish
```

| Node or referenced plan | `LoweringCapabilityV1` in the V1 inventory |
| --- | --- |
| `Input::Memory`, `Input::Environment` | `Supported` after exact input plan verification |
| `Input::StaticComposite` | `Supported` only with the compact strided recipe below |
| `Input::DynamicOverlay` | `Supported` only with the checked address-known/bounds/partial-lane plan |
| `Constant`; `Coerce`; each listed `PhaseUnaryOpV1` | `Supported` |
| `Add/Sub/Mul/And/Or/Xor`; `Eq/Ne/EqWildcard/NeWildcard`; every `Lt/Le/Gt/Ge` signed/unsigned tag; `LogicAnd/LogicOr` | `Supported` |
| `Div/Rem` | `Supported` with the closed four-state zero-divisor primitive |
| `Shl/Shr/Sar` | `Supported` with arbitrary-width count comparison before host conversion |
| `Mux` | `Supported` with the closed three-valued bit merge |
| `ForFold` | `Supported` only with its complete verified transition/effect template |
| `Concat`; in-bounds `Slice` | `Supported` |
| each `PhaseRuntimeEventPrimitiveV1` | `Supported` only at its exact verified outer/fold effect site |

For each variant, `fact.lowerable` is true exactly when its table entry is
`Supported`, every referenced semantic plan/template is complete, and all
children are lowerable. Width magnitude, node count, loop trip count, and
compile time are not capability predicates. An out-of-bounds static `Slice`,
zero-width concat/part, malformed packed plan, or missing template is a
structured verifier error rather than `lowerable = false`. A genuinely absent
backend primitive deterministically records the closed unsupported reason and
prevents freeze/lowering; it never selects another operator, caps the graph,
or falls back to legacy SLT.

`PhaseSLTLoopBound::Constant`, the ForFold step, and `Constant` all name
`VerifiedTypedValueId<P>` values in the same phase aggregate. The ForFold
state/effect and concat/input-index pools are separately owned, dense,
gap-free ranges; an argument or state ID cannot enter another pool merely
because its integer is in bounds.

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
one contiguous flat range. That form is one `InputAccess` with
`resolution_class = StaticComposite(recipe)`, but it is neither one contiguous
memory interval nor an expanded concat. The recipe is compact:

```text
StaticCompositeProjectionRecipeV1
  object / checked static base before the first unconsumed packed dimension
  outer_dimensions: StaticCompositeStridePool
  projected_member_path: verified field-ID range
  composed_member_lsb / lane_width
  lane_count / checked result_width = lane_count * lane_width
  packed_order: CanonicalVerylPackedOrderV0_20
  exact selected type / signedness / positive class / domain

StaticCompositeStrideRowV1
  normalized packed-dimension ordinal / nonzero extent / source stride

StaticCompositePackedOrderV1 =
  CanonicalVerylPackedOrderV0_20
```

For extents `E[0..k)` and source strides `S[0..k)`, let
`Q[i] = product(E[i+1..k])`, `lane_count = product(E)`, and for
`0 <= q < lane_count` let `digit(i,q) = (q / Q[i]) mod E[i]`. The one symbolic
projection relation is:

```text
source_lsb(q) = static_base + composed_member_lsb
                + sum(digit(i,q) * S[i])
destination_lsb(q) = q * lane_width
copy exactly lane_width bits
```

`CanonicalVerylPackedOrderV0_20` is not an assumed host orientation. The
normalized V0_20 dimension relation proves that every accepted packed extent
is indexed from zero, every increment adds its positive suffix stride, the
leftmost remaining dimension is slowest varying, and the packed-member layout
places the last declared member at the least-significant offset. Therefore
`q = 0` names the least flat source/destination lane and increasing `q`
increases flat LSB. The packed-order tag, all stride rows, and the member path
are part of recipe identity. A future language profile with descending ranges,
reversed lanes, or a different member orientation must add another closed
`StaticCompositePackedOrder` variant and formulas; it cannot reuse this V1
recipe because the resulting intervals happen to have equal widths.

All products, sums, endpoints, and the final object/result bounds are checked
before publication. The verifier proves from normalized suffix strides that
the symbolic source intervals are disjoint and occur in increasing flat-LSB
order; it does not enumerate them. Nested member selection only extends the
verified field path and checked `composed_member_lsb` within one lane. The
expected value graph owns one static-composite recipe and one value occurrence,
not `lane_count` Input/Concat rows. Lowering uses the same mixed-radix recipe
as a counted projection primitive; it may not unroll by lane count, impose a
lane cap, or forge a contiguous load. Storage and verification are
`Theta(unconsumed dimensions + member-path length)` regardless of lane count.
Recipe identity is the complete packed-order/stride/member/type content, not
its checked numeric ID; canonical IDs are allocated only after content equality
and first expected-use order have been proved.

## Exact access normalization

Let a verified semantic object have dimensions `D[0..N)`, strides `S[0..N)`,
and `U` leading unpacked dimensions. Access normalization consumes dimensions
from zero upward. Each HIR index is tied to exactly one dimension and retains
whether that dimension is unpacked or packed.

Each index expression is classified by the verified constant evaluator into
exactly one of three forms:

- `KnownTwoStateConstant` is converted only after its arbitrary-width value is proved
  representable as a mathematical integer and in `0 <= index < extent`. Its checked `index * stride` contribution
  is added to `static_base`; it creates no runtime index child.
- `ConstantUnknownXZ` retains the exact constant width/payload/mask proof but
  creates no runtime child and is never converted to `usize`. It makes
  `address_known` identically false. A packed selector produces all-X
  `Evaluation` content regardless of static domain; an unpacked selector
  produces the recursively materialized element default (Bit zero, Logic X).
  A write is a no-op. Bounds and offset arithmetic are not evaluated on that
  false path.
- `RuntimeValue` creates exactly one ordered index role and later exactly one
  phase-node child. The role records the expected HIR operand, source width,
  source signedness/domain, normalization coercion, extent, and stride.

The legacy `eval_constexpr` helper is not an oracle here because it discards
the X/Z mask. A constant is `KnownTwoStateConstant` only when its verified
mask is zero. These classifications and their constant proof rows remain in
the expected input specification even though the compact phase fact contains
no child for either constant form.

Runtime index arithmetic and bounds comparison remain in their original
normalized arbitrary-width signed/magnitude domain. `address_known` inspects
the actual evaluation mask for both static Bit and Logic operands; static domain
alone never proves knownness. Conversion to `usize` or a machine pointer occurs
only on the verified known and in-bounds path.

Every ordinary index, static or runtime, has the exact bound
`0 <= normalized_index < extent`. A runtime index contributes
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

NormalizedIndexRole
  operand/source width/signedness/static domain and evaluation-mask use
  dimension / extent / stride
  invalid_read_semantics: PackedEvaluationX | UnpackedStorageDefault

NormalizedPartSelect
  Colon { low, elements, dimension, stride, AllLanesStaticValid } |
  PlusColon { anchor role, elements, dimension, stride,
              PackedLaneIntervalPlan(low = anchor) } |
  MinusColon { anchor role, elements, dimension, stride,
               PackedLaneIntervalPlan(low = anchor - (elements - 1)) } |
  Step { anchor role, elements, dimension, stride,
         PackedLaneIntervalPlan(low = anchor * elements) }

PackedLaneIntervalPlan
  result lane k maps to source element low + k, for 0 <= k < elements
  when anchor known:
    valid lane interval = [max(0, -low), min(elements, extent - low))
    with empty/clamped endpoints computed in arbitrary-width signed arithmetic
  when anchor X/Z: no known-valid lane and the complete read result is X
  read: valid interval from object, lower/upper invalid intervals X
  write: update only valid interval; invalid intervals are no-ops
```

`anchor role` is a `KnownTwoStateConstant` folded into `static_base`, a
`ConstantUnknownXZ` proof which makes the access guard false, or one ordered
`RuntimeValue` role. The three forms have one result-type rule.

Invalid-read behavior is retained per consumed dimension and applied
left-to-right. An invalid unpacked role substitutes the recursive materialized
element default and later selectors operate on that value; an invalid packed
role produces the appropriately shaped all-X evaluation result. The combined
`access_guard` only controls whether a direct machine address may be formed; its
false path must replay these per-role semantics and may not collapse them to one
final static-domain test.

The packed lane plan is symbolic and has at most three intervals; it never
allocates one validity bit or action per element. `bounds_when_known` for an
indexed part select means the valid interval covers `0..elements`. A false
whole-range guard with a known anchor still executes the retained partial-lane
read/write plan rather than replacing the whole value with X.

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
0 <= low <= high < extent
elements = high - low + 1
static_base += low * stride
selected_width = elements * stride
```

All subtraction, addition, and multiplication is checked.

### Plus-colon

`[anchor +: elements]` requires static nonzero `elements <= extent` and uses:

```text
low = anchor
bounds = 0 <= anchor && anchor + elements <= extent
offset contribution = anchor * stride
selected_width = elements * stride
```

### Minus-colon

`[anchor -: elements]` requires static nonzero `elements <= extent` and uses:

```text
low = anchor - (elements - 1)
bounds = 0 <= anchor && anchor < extent && anchor + 1 >= elements
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
bounds = 0 <= anchor && low + elements <= extent
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

The result static domain is independently derived from the exact selected
semantic type and is either `Bit` or `Logic`. It is not derived from an index
type and does not by itself prove an empty evaluation mask. For node facts the
input row supplies the exact result derived by the closed transfer relation
above:

```text
Input.width         = input.selected_width
Input.signed        = input.result_signed
Input.positive_type = input.result_positive_type
Input.static_domain = input.result_static_domain
Input.value_class   = input.result_value_class
Input.mask_class    = input.result_mask_class
```

`MaterializedStorage/AlwaysZero` is derived for a Bit storage read when every
possible invalid selector is unpacked and therefore also returns a materialized
Bit default. A Logic storage read remains `MaterializedStorage/MayCarryXZ`.
Any runtime/unknown packed selector or partial-lane packed plan derives
`Evaluation/MayCarryXZ`; a packed-invalid read can therefore be X even with
static Bit domain. Index children contribute to lowerability, dependency,
actual address-known, bounds, and mask behavior, but never change the selected
static domain. An actual Bit store/cast later performs materialization and
clears X/Z.

## Phase input representation and private facts

The canonical new node shape is:

```text
PhaseSLTNodeV1::Input {
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
InputSemanticFactsPart<P>
  objects: [SemanticObjectFact<P>]
  inputs: [InputAccessFact<P>]

InputSemanticFactsView<'a, P>
  private BrandRef<'a>
  borrowed &'a InputSemanticFactsPart<P>

SemanticObjectFact<P>
  object
  object_width / declared_signed / declared_positive_type / object_domain
  exact PhaseObjectResolution<P> / default_role
  canonical dimensions with extent and stride

InputAccessFact<P>
  input / object
  compact normalized access and ordered runtime role geometry
  optional verified selected-member type projection
  selected_width / result_signed / result_positive_type / result_static_domain
  result_value_class / result_mask_class
```

Only the aggregate semantic verifier can construct the unbranded part. It has no public row
constructor, standalone verifier, serializer, deserializer, wire form, or
freeze method. The construction session and prepared/frozen artifact store one
`ArtifactBrandOwner` beside unbranded compact phase/fact parts, never inside
them. An editor or frozen API borrows that owner to create ephemeral branded
phase and `InputSemanticFactsView` handles; multi-view operations compare those
borrowed brands before indexing. Facts from another artifact or phase therefore
fail before node replay without a self-reference or durable brand field.

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

### Raw typed-HIR closure fixtures

- a generated adapter census maps every
  `VerylParserV0_20_1_UEscape1` node variant to its exact primary row plus the
  closed zero-or-one row of each semantic projection kind permitted above (for
  example SourceExpr and a demanded ConstExpr projection), or one explicit
  unsupported sum;
  adding a parser variant or projection without updating that exhaustive
  emission vector fails compilation;
- every `PrivateRawSyntaxPoolKindV1` positive fixture is followed by missing,
  overlapping, duplicated-owner, gap, orphan, reordered, and wrong-pool-tag
  variants; witness and magnitude arenas cannot be relabeled as syntax pools;
- environment lineage distinguishes `SyntaxEnvironmentLineageRoleV1::Root`
  from each canonical `PathComponent`; wrong component ordinal, path, optional
  component generic-use syntax, parent, or row order fails even when the final
  verified generic environment is equal;
- module/interface/package/proto port and item ordering, generate nesting,
  bind target/component ownership, modport item/default-name ownership, and
  exactly matching typed-HIR roots are permuted independently;
- every process kind, assignment timing/operator, statement/control variant,
  target form, expression variant, selector, decision pattern, runtime event,
  observer anchor, and retained-ForFold range/step form has one positive row
  and wrong-owner/equal-shaped-ID substitution failures;
- `Connect`, unsafe/embed/include, testbench method, runtime float/direct-union/
  SystemVerilog expression, unsupported effectful system call, analyzer-only
  generate expansion, and every recovery variant return their exact closed
  unsupported rule rather than disappearing from traversal;
- one source reference, target dynamic selector, runtime call, observer
  capture, or fold-body effect omitted from both a phase proposal and its
  producer summary still fails the independently derived expected inverse; and
- syntax-keyed analyzer witnesses map only after lineage verification; a
  producer private raw ID, ambiguous occurrence key, missing mapped mirror, or
  witness row inserted into a syntax table fails before semantic replay.

### Runtime-function and static-expression fixtures

- two calls with the same template/generic/type key and different input bits
  share one runtime program but retain distinct call instances, result/effect
  occurrences, target handles, and execution lineages;
- the same nested raw call site reached below two parent call lineages cannot
  collide in ExpectedUse/Result, formal/local object, canonical producer,
  dynamic-plan, event, or provenance keys;
- all four `RuntimeSourceRootRoleV1` variants require their exact independently
  derived root owner. Under one root/call lineage, otherwise equal `Body` and
  `ForFold(template)` object/use/slot keys remain distinct, while entering a
  fold creates no new execution-lineage row and two iterations create neither a
  new lineage nor a new local scope;
- positional/named mapping, mixed arguments, defaults, flattened modport
  members, wrong direction, nonassignable output/inout expressions, and equal-
  shaped formal/actual substitutions are checked from the one raw expression;
- one definition with a retained input default is invoked once through the
  admitted constant VM and once as a runtime call: both consumers name the same
  `RawFunctionPortRowV1.default`, typed-HIR expression, generic environment,
  verified default type, and coercion; the constant call uses its certificate
  while the runtime call emits one `DeclaredInputDefault` setup occurrence, and
  a copied default row/proof fails the inverse;
- for `f(a: input T, b: input T = a + one)` called at two runtime sites with
  different `a` values, each call evaluates `b` once from its own staged `a`
  and obtains the corresponding different value, while both calls retain one
  type-specialized runtime program; `a + one` remains the one template-owned
  runtime fragment and cannot become a template static root, call-value
  specialization, memo key, or duplicated per-call program row;
- a valid function `Modport` formal expands explicit and defaulted interface
  members in canonical interface declaration order with exact effective
  directions and array geometry; analyzer member-map permutation is inert.
  A direct `Import`, an imported effective member (including one inherited by
  `Same`), and a `Prototype` callee fail respectively with the three exact
  `RuntimeFunctionStructuralRejectReasonV1` variants instead of losing a member
  or borrowing another body;
- output has no old-value read, input has no target, and inout prepares one
  dynamic target and reads/writes that same handle; selector side effects or
  reads cannot execute twice;
- overlapping output/inout targets copy out in formal/member declaration order
  under randomized analyzer-map insertion order, and reversed writeback fails;
- direct and mutual runtime recursion, including a generic changing-type
  recursion, fail the canonical template SCC rule; a very deep acyclic call
  DAG succeeds iteratively without a depth cap;
- a runtime-function procedural loop derives its call-scoped retained ForFold,
  while a constant-function invocation of the same raw template uses a
  distinct finite VM trace; neither ID/completion proof substitutes for the
  other;
- a maximal fully static executable subtree becomes one
  `SourceGraphStaticValue`/Phase Constant while all descendants remain typed;
  missing or extra descendant source-value nodes fail the exact expansion;
- `0 && runtime_read` produces one maximal known-false
  `SourceGraphStaticValue`/`Constant` and no expected use, input, action, or
  producer row for the typed-but-suppressed runtime read;
- a known-true conditional with a static selected arm and a runtime-dependent
  unselected arm produces one maximal Constant with the selected content and no
  expected row for the suppressed arm;
- a fully static X-controlled conditional with distinct static arms activates
  both proof edges, applies the closed ternary bit merge, and produces one
  maximal Constant with the exact merged X/Z mask; its typed proof descendants
  do not become additional source-value rows; and
- the corresponding conditional with a runtime controller and two static arms
  cannot collapse: its exact expected expansion contains the controller Input
  use, one maximal Constant occurrence for each arm, both gated arm uses, and
  one ternary `Mux` result. Dropping either possible arm or claiming one outer
  `SourceGraphStaticValue` fails the expected inverse;
- a wrong analyzer `Comptime` bit cannot change maximal-static classification,
  and X/Z/divide-by-zero static output retains its exact evaluation mask; and
- a constant actual passed to a runtime function does not value-specialize its
  program or make formal/local-dependent expressions static; only a later
  independently verified rewrite may propagate it.

### Type and object fixtures

- every `RawTypeUseOwnerRoleV1` variant has one positive exact-owner fixture;
  swapping equal-shaped module/interface/function/proto ports, return/local,
  alias/member/enum/cast/constructor/generic owners fails, and an inferred
  declaration is accepted only through `SyntaxInferredTypeReplayKeyV1` without
  manufacturing a `RawTypeUseRow`;
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
- direct and aliased proof-only `f32`/`f64` shapes are accepted by both
  `$bits` and `$size` with their fixed 32/64-bit terminal, while a float value
  use or executable-object projection is rejected;
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
- an `ExplicitFixed` recipe may reference an earlier same-enum member, while a
  current/later member reference is rejected; the same prior-member recipe on
  `Omitted`, `InferBit`, or `InferLogic` is rejected as a circular-width
  dependency;
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
- an unconsumed outer packed array's `s.member` uses one exact compact
  `StaticComposite` strided recipe rather than lane expansion or a forged
  contiguous range; a huge lane count keeps constant recipe row count.

### Domain and signedness fixtures

- signed whole-object and static/dynamic unpacked-only reads remain signed;
- named member projection uses the selected member's signedness/domain rather
  than the aggregate object's, before any further packed select;
- any static/dynamic packed bit/part select is unsigned;
- equal flat ranges reached once through unpacked provenance and once through
  packed provenance have their independently derived signedness;
- materialized Bit read with only statically valid or unpacked-invalid selectors
  derives `AlwaysZero` mask class;
- a packed selector with X/Z or runtime-invalid lanes derives `MayCarryXZ` and
  produces X evaluation content even for static Bit result domain;
- an invalid unpacked selector returns the recursive materialized element
  default (Bit zero, Logic X), while a later packed-invalid selector can still
  produce X; and
- every unknown/out-of-range path follows the packed/unpacked and partial-lane
  plan without forming an out-of-object pointer.

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
  root-materialization, and runtime-function program/call/lineage/actual/
  target/writeback reservation site;
- no semantic length or mapping change after any failed derivation/replay;
- iterative deeply nested type and access graphs without host recursion;
- a deep alias chain with at least one own extent per alias proving linear
  verified extent/segment storage and forbidding quadratic copied vectors; and
- 100k/1M input-access derivation/replay measurements including large
  multidimensional and part-select tables, plus deep shared-program runtime
  call DAGs with many call instances but no program-body cloning.

Existing positive regression tests for packed-bit stride, hierarchical dynamic
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
   static-composite, runtime-function program/call lineage, and `ForFold`
   variant, including maximal `SourceGraphStaticValue` collapse;
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

- [Veryl formal syntax, pinned documentation commit](https://github.com/veryl-lang/doc/blob/c3b01bb33092a81df0fa51dd2d49496faa163116/book/src/07_appendix/01_formal_syntax.md)
- [Builtin types, pinned documentation commit](https://github.com/veryl-lang/doc/blob/c3b01bb33092a81df0fa51dd2d49496faa163116/book/src/05_language_reference/03_data_type/01_builtin_type.md)
- [User-defined types, pinned documentation commit](https://github.com/veryl-lang/doc/blob/c3b01bb33092a81df0fa51dd2d49496faa163116/book/src/05_language_reference/03_data_type/02_user_defined_type.md)
- [Arrays, pinned documentation commit](https://github.com/veryl-lang/doc/blob/c3b01bb33092a81df0fa51dd2d49496faa163116/book/src/05_language_reference/03_data_type/03_array.md)
- [Clock / Reset, pinned documentation commit](https://github.com/veryl-lang/doc/blob/c3b01bb33092a81df0fa51dd2d49496faa163116/book/src/05_language_reference/03_data_type/04_clock_reset.md)
- [Bit select, pinned documentation commit](https://github.com/veryl-lang/doc/blob/c3b01bb33092a81df0fa51dd2d49496faa163116/book/src/05_language_reference/04_expression/06_bit_select.md)
- [Veryl 0.20.1 parser grammar](https://github.com/veryl-lang/veryl/blob/dfa101b1fd02484ec616f115366e86ee63c39c14/crates/parser/veryl.par)
- [Veryl corrected reflected-Gray recurrence](https://github.com/veryl-lang/veryl/commit/95a14877823a4b9214729ab48152a09ab94b8412)
- [Veryl duplicate enum-value validation](https://github.com/veryl-lang/veryl/commit/22a722a0a6ef483bf3ea54464d83068e38d2fbef)
