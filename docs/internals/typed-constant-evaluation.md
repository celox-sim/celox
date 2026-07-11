# Typed constant evaluation and finite execution certificates

This document is the normative `CeloxSourceV0_20` profile for the typed
constant relation used by the verifier-first source pipeline. It completes the
`ExpectedTypedConstantExpr` boundary introduced by
[Source semantic objects and input accesses](./source-semantic-inputs.md) and
uses the fallible framing and ownership rules in
[Private source-wire framing and staging](./source-wire-format.md).

It is a design and implementation contract. The current analyzer evaluator,
resolved `Comptime` values, enum caches, `num_bigint` values, and the private
`source_semantic.rs` checkpoint are comparison inputs only. None is a semantic
oracle. This relation must be implemented and verified before that checkpoint
may be connected to source-node production or committed as a completed
frontend.

The words *must*, *must not*, *required*, and *exactly* below are normative.

## Scope and relationship to the other verifiers

The complete ownership chain is:

```text
VerylParserV0_20_1_UEscape1 parsed syntax/exact tokens +
  analyzer witnesses/certificates
  -> verifier-owned flat RawTypedSourceHIR/constant/function staging
  -> structural and ownership verification
  -> joint type-use/generic/static-dependency/enum-binding worklist
  -> contextual expression typing and closed coercion relation
  -> typed constant evaluation + maximal executable-HIR source-static frontier +
     enum completion + finite function-trace replay
  -> analyzer/cache witness comparison
  -> complete ExpectedTypedConstantExpr relation
  -> enum/type replay and complete VerifiedTypedSourceHIR
  -> source TriIntent and ExpectedSourceValueGraph
```

These arrows are one prepared aggregate. No intermediate result can be frozen,
serialized as a verified artifact, or used to construct SLT nodes. In
particular:

- type normalization supplies independently verified shapes and member types;
- constant evaluation supplies exact typed values for extents, generic actuals,
  parameters, enum recipes, static selects, admitted constant functions, and
  maximal fully static ordinary source expressions;
- an explicit enum recipe consumes its prerequisites and publishes that
  variant's binding to the same private worklist; an implicit recipe consumes
  its predecessor, and final width/encoding/uniqueness replay runs only after
  every variant binding exists;
- `TriIntent` and occurrence `TriNet` do not participate in constant typing;
  and
- an integral result reaches a `PhaseSLTNodeV1::Constant` only after the complete
  aggregate has finished and the result has been materialized into the same
  phase's branded typed-value arena.

There is no compatibility mode which trusts a producer when this verifier
disagrees. A disagreement is resolved by checking this specification and then
fixing either the verifier rule or the producer/analyzer adapter. The verifier
is not weakened merely to admit existing IR.

## Closed semantics version and admitted roots

Every aggregate names exactly one closed
`TypedConstantSemantics::CeloxSourceV0_20`. This profile follows Veryl V0_20
for the executable type subset admitted by `source-semantic-inputs.md`; it does
not claim that Celox already implements every legal Veryl data type. A future
change to literal sizing,
floating point, string operations, wildcard matching, aggregate construction,
or constant-function execution requires a new semantics tag and fixtures. A
wire cannot select individual rules.

V0_20 admits proof roots with these result classes:

```text
PackedIntegral
TypedString
TypeValue
FixedAggregate(Array or Struct)
```

Only `PackedIntegral`, including the verified packed projection of a fixed
packed aggregate, may become an executable SLT constant. `TypedString` is
admitted only for a typed string literal/parameter and the closed comparisons
specified below. A floating-point value or floating-point intermediate in a
proof root is rejected at typing, including one whose final comparison or cast
would be integral. A type-only `$bits` or `$size` query may inspect the fixed
32/64-bit shape of `f32`/`f64` without constructing or evaluating a float. Supporting
float values later requires a separately versioned, deterministic software
IEEE-754 specification; host `f32`, `f64`, `powf`, or platform `libm` is not
acceptable evidence. This is an explicit capability rejection at the source
profile boundary, not permission to reinterpret a valid float as integral or
to trust an analyzer approximation.

Unsupported syntax is a structured typing error, not an analyzer fallback.
An unused non-executable source declaration need not become a proof root, but
every expression reachable by type checking an admitted root is retained.
All arms are type checked even when value evaluation will short-circuit them.

## Canonical raw relation

The verifier-owned private raw staging is flat and syntax preserving. It retains source occurrence,
owner, identifier spelling/path, lexical scope, namespace role, exact generic
environment witness, and operand order. It does not use a producer-selected
symbol target, result type, coercion, or evaluation order as semantic input.

```text
RawConstExprRow
  exact occurrence / owner scope / source coordinate
  kind:
    IntegralLiteral(RawIntegralLiteralId) |
    StringLiteral(RawStringLiteralId) |
    FloatLiteral(closed unsupported literal row) |
    Reference(RawNameOccurrenceId) |
    TypeValue(RawTypeUseId) |
    TypeOf(RawConstExprOccurrenceId) |
    BoundSymbol(RawBoundSymbolId) |
    Unary(RawUnaryOp, operand) |
    Binary(RawBinaryOp, left, right) |
    Cast(RawCastTargetId, operand) |
    Conditional(condition, then_value, else_value) |
    Concat(RawConcatPartRange) |
    ArrayConstructor(RawArrayItemRange) |
    StructConstructor(RawTypeUseId, RawNamedFieldRange,
                      optional default expression) |
    DirectUnionConstructor(closed unsupported retained row) |
    Select(base, RawSelectRange) |
    CaseExpression(selector, RawDecisionArmRange, default) |
    SwitchExpression(RawDecisionArmRange, default) |
    InsideOutside(target, RawDecisionPatternRange, negated) |
    SystemCall(RawSystemCallId) |
    UserCall(RawFunctionUseId) |
    Invalid(RawUnsupportedExpressionKindV1)

RawUnaryOp =
  Plus | Minus | BitNot | LogicNot |
  ReduceAnd | ReduceNand | ReduceOr | ReduceNor |
  ReduceXor | ReduceXnor

RawBinaryOp =
  Pow | Div | Rem | Mul | Add | Sub |
  ArithShiftL | ArithShiftR | LogicShiftL | LogicShiftR |
  Less | LessEq | Greater | GreaterEq |
  Eq | EqWildcard | Ne | NeWildcard |
  LogicAnd | LogicOr |
  BitAnd | BitOr | BitXor | BitXnor

RawRejectedSystemFunctionKindV1 =
  Onehot0 | Countones | Countbits | Isunknown | Dimensions |
  UnpackedDimensions | Left | Right | Low | High

RawSystemFunction = Bits | Size | Clog2 | Onehot | Signed | Unsigned |
                    Readmemh | Display | Write | Assert | AssertContinue |
                    Finish |
                    RetainedRejectedStandard(RawRejectedSystemFunctionKindV1) |
                    Unrecognized
```

`RawConstExprTagV1` is exactly the payload-free discriminant of the listed
`RawConstExprRow.kind` variants, including `FloatLiteral`,
`DirectUnionConstructor`, and `Invalid`; it has no catch-all numeric value.
Malformed numeric tags exist only at an optional byte adapter and fail before
this enum is constructed.

```text
ConstDerivationRuleV1 =
  StaticRoot(TypedConstantRootRoleV1) |
  RuntimeFragment(RawConstExprTagV1) |
  TypeOnly(RawConstExprTagV1)
```

The row's `evaluation_class` selects exactly one matching derivation variant;
the expression/root tag and verified context determine it uniquely.
`StaticRoot(SourceGraphStaticValue)` is selected only by the independently
derived maximal source-static frontier below. A producer `is_const`/`Comptime`
tag cannot select this derivation rule.

Argument cardinality and argument roles are derived from the closed tag.
`Bits`, `Size`, `Clog2`, `Onehot`, `Signed`, and `Unsigned` have exactly one
input-expression argument. `Readmemh` has one input-expression followed by one
assignment-target argument. `Finish` has none. `Display` and `Write` have zero
or more input expressions. Each `Assert` variant has one condition followed by
zero or more input expressions. A different cardinality or role is rejected
before typing. The effectful forms are still retained losslessly and type
checked before the constant-machine admission rule rejects them. `BitNand` and
`BitNor` are reduction operators only; a producer cannot relabel them as binary
operations. `TypeOf` is type-only and does not evaluate its operand.

Only `Bits`, `Size`, `Clog2`, `Onehot`, `Signed`, and `Unsigned` are admitted
pure tags in this profile. Other valid Veryl/SystemVerilog functions—including
`$onehot0`, `$countones`, `$countbits`, `$isunknown`, `$dimensions`,
`$unpacked_dimensions`, `$left`, `$right`, `$low`, and `$high`—resolve from
their exact spelling to `RetainedRejectedStandard` and fail
`CONST.TYPE_SYSTEM_CALL`. An adapter cannot classify them as an admitted tag or
as an opaque user call. Adding any of them requires a versioned type/value rule.
Any other valid dollar-identifier spelling remains in its owning
`RawSystemCallRow`, is classified as `Unrecognized`, and is rejected by the same
closed typing boundary; it is data attached to a known variant, not an unknown
enum discriminant.

The literal rows retain syntax rather than an analyzer value:

```text
RawIntegralLiteralRow
  exact original-token RawStringSpellingId
  proposed kind: UnsizedDecimal | SizedBased | WidthlessBased |
                 AllBitsZero | AllBitsOne | AllBitsX | AllBitsZ | Boolean

RawStringLiteralRow
  exact RawStringSpellingId including quotes and escape spelling

RawScopeKindV1 =
  Project | Package | Module | Interface | Function | Block |
  Enum | Struct | Union

RawScopeRow
  exact syntax occurrence / RawScopeKindV1
  optional lexical parent RawScopeOccurrenceId
  canonical declaration and import ranges

RawNamespaceClassV1 =
  Parameter | Constant | GenericConstant | EnumVariant | Function |
  Port | Local | LoopBinding | Type | Member | Package | Instance | Proto

RawDeclarationTargetV1 =
  Parameter(RawStaticBindingId) | Constant(RawStaticBindingId) |
  GenericConstant(RawGenericFormalId) | EnumVariant(RawEnumVariantId) |
  Function(RawFunctionTemplateId) | Port(RawFunctionPortId) |
  Local(RawLocalDeclarationId) | LoopBinding(RawForId) |
  Type(RawTypeId) | Member(RawTypeMemberId) |
  Package(RawPackageId) | Instance(RawInstanceId) |
  Proto(RawProtoDeclarationId)

RawDeclarationRow
  source ordinal / exact spelling / closed namespace class
  exact visibility interval in its owning scope
  target: RawDeclarationTargetV1

RawImportRow
  source ordinal / exact path-component range / Explicit | Wildcard
  exact visibility interval

RawPathComponentRow
  source ordinal / exact spelling / optional RawGenericUseId

RawNameOccurrenceRow
  nonempty canonical RawPathComponentRange
  exact lexical RawScopeOccurrenceId /
  expected namespace: Concrete(RawNamespaceClassV1) |
                      DeferredGenericFormalKind(RawGenericArgumentId) /
  source coordinate

RawAnalyzerResolutionWitnessRow
  exact RawNameOccurrenceId /
  proposed: Unresolved(RawAnalyzerUnresolvedReasonV1) |
            Resolved(RawDeclarationTargetV1)
  comparison witness only; it never selects the semantic target

RawAnalyzerUnresolvedReasonV1 =
  NotFound | Ambiguous | WrongNamespace | InvalidGenericArity | Recovery

RawBoundSymbolRow
  kind: Msb | Lsb
  exact owning select/type-query occurrence and dimension ordinal
  retained base expression or type-use occurrence

RawCastTargetRow =
  BuiltinTypeUse(RawTypeUseId) |
  UserName(RawNameOccurrenceId with expected TypeOrConstantWidth namespace) |
  WidthLiteral(RawIntegralLiteralId)

RawFunctionUseRow
  exact call occurrence / RawNameOccurrenceId / RawFunctionActualRange

RawFunctionActualRow
  source ordinal / Positional | Named(RawNameOccurrenceId) / expression

RawSystemCallRow
  exact system-name occurrence / proposed RawSystemFunction /
  RawSystemArgumentRange

RawSystemArgumentRow =
  InputExpr(RawConstExprOccurrenceId) |
  OutputTarget(RawAssignmentTargetId)
```

The verifier reparses the original token bytes and derives kind, explicit
width, apostrophe, signed marker, radix, underscore-free digits, and canonical
digit bytes. It revalidates underscore positions, per-radix characters, the
decimal X/Z prohibition, X/Z placement, and every checked count. A proposed
kind is a witness only. Unsized/all-bits literal sizing is derived from its
exact contextual-typing rule; the adapter may not replace it with a
resolved analyzer value. `RawStringSpellingId` refers to
`RawSourceAggregateV1`'s strictly byte-sorted, deduplicated UTF-8 spelling
table; a literal
occurrence owns only that reference. Decoded value bytes live in a separate
constant-value byte pool whose ranges are occurrence/value owned. String
escapes are decoded exactly once. There is no Unicode normalization, lossy
UTF-8 replacement, locale comparison, or host `String` on the proof path.

Variable-size syntax uses disjoint canonical pools:

```text
RawConcatPartRow
  source ordinal / expression / optional repeat-count expression

RawArrayItemRow
  source ordinal / Value(expression) |
                   Repeat(expression, count expression) |
                   Default(expression)

RawNamedFieldRow
  source ordinal / exact member-name occurrence / expression

RawSelectRow
  source ordinal / Member(exact member-name occurrence) |
                   Index(expression) |
                   PartSelect(lower, upper or width, exact direction tag)

RawAssignmentTargetRow
  exact target occurrence / owning statement and source coordinate
  base RawNameOccurrenceId / canonical RawSelectRange

RawDecisionArmRow
  source ordinal / canonical pattern range / result expression

RawDecisionPatternRow
  source ordinal / Equality(expression) |
                   Range(lower, upper, inclusive-upper) |
                   Boolean(expression)
```

Structural cardinality is semantic, not inferred from a later failure: a
struct constructor has at least one explicit field; a case/switch expression
has at least one non-default arm and exactly one required default; an
inside/outside pattern range is nonempty; and each case/switch arm condition
range is nonempty. Empty raw ranges in those roles are `CONST.TYPE_ARITY`.

Scope parents must be earlier checked IDs, form one rooted lexical forest, and
own gap-free declaration/import ranges. A declaration's spelling, namespace,
target kind, owner, source ordinal, and visibility interval are re-derived from
the retained syntax; a producer cannot insert a symbol-table-only declaration.
Ports and generic formals are visible throughout their declared body; ordinary
local declarations and loop bindings are visible only in their exact retained
post-declaration/body interval. Package/module/type declarations use their
source-defined whole-scope visibility. Duplicate declarations in one namespace
and overlapping incompatible visibility intervals reject before lookup.

The independent resolver starts at the occurrence's checked lexical scope. For
an unqualified first component it forms exactly three candidate tiers at each
scope, in this order: eligible direct declarations, eligible explicit imports
in source order, and eligible wildcard imports in source order. It selects the
first **nonempty** tier and does not inspect a lower-priority tier or an outer
scope after that. One candidate occurrence in that tier resolves the component;
two or more are ambiguous, even when they propose the same final declaration.
Only when all three tiers are empty does lookup continue at the lexical parent.
Visibility and expected-namespace filtering happen before a tier is tested for
emptiness; target-kind admission happens on the selected candidate and cannot
make lookup fall through to a lower tier. An import path is itself resolved by
the same relation. Tiers are prepared lazily: resolving an import, detecting an
import cycle, or allocating its active-stack row occurs only after every higher
tier in that scope was proved empty. Thus an invalid unused wildcard import
cannot preempt a valid direct declaration. Import resolution proceeds
with an explicit active-import stack, so a cycle or ambiguity cannot be hidden
in an analyzer import cache. Each later qualified component is looked up only
in the exact namespace/member scope opened by the preceding verified target.
Every component's optional generic use is verified at that component before
the next lookup. Visibility, namespace class, target-kind admission, and final
path exhaustion are mandatory; textual suffix matching and mangled analyzer
names are forbidden.

Resolver fixtures cover a direct declaration shadowing broken/lower-priority
imports, an explicit import shadowing wildcard imports, ambiguity between two
occurrences in the selected tier (including two paths to the same target), and
all-three-empty continuation to the parent scope. They also prove that a
wrong-kind selected candidate errors instead of falling through.

This resolver derives parameter, constant, enum-variant, generic-constant,
function, port, local, loop-binding, type, member, package, instance, and proto
targets without consulting an analyzer ID. The same relation
maps named call arguments to ports and named constructor fields to members;
positional arguments map by declaration ordinal. Missing, extra, duplicate, or
mixed positional/named actuals are rejected by the `CeloxSourceV0_20` call
rule: one call is either entirely positional or entirely named. The pinned
grammar can retain a mix, and the pinned analyzer diagnoses it but later turns
`Arguments::Mixed` into an empty argument list. The adapter must retain every
raw actual so that the verifier reports the source-rule failure; neither the
lossy downstream list nor its resulting arity error is an oracle.
Every proposed analyzer resolution is compared bidirectionally only after the
expected target table is complete. There may be no missing or extra witness.

Calls evaluate actual expressions in retained source ordinal, then place the
completed values into independently resolved port slots. This order, rather
than a producer `HashMap` traversal or port order, is the V0_20 constant-machine
order.

The verifier-owned private raw semantic staging additionally owns flat tables for roots, binding
declarations, generic environments and uses, function templates and
specialization witnesses, ports, local declarations, blocks, statements, assignment
targets, invocation certificates, trace steps, literal bytes, arbitrary-bit
bytes, and all adjacency/range pools. Nested `Vec`, `String`, `BigUint`, AST
pointer identity, and a 24-bit packed index are forbidden.

There is no raw semantic specialization table. Any producer specialization ID
appears only in `RawConstantExprWitnessRow`; verified specializations are
created solely by concrete demand and compared later as witnesses.

Every occurrence row and owned pool element has exactly one owner. Referential
string-table and name-spelling IDs may repeat and never own their bytes. All
other owned ranges are
gap-free, nonoverlapping, and in source ordinal; referential tables may be
permuted only when every reference is relocated and canonical verified output
is unchanged. Raw indices remain raw until the complete range/reference/owner
scan succeeds.

## Proof identity, roots, and environments

One raw occurrence may be typed more than once. Its proof identity is:

```text
ExpectedTypedConstantExecutionOwnerV1 =
  SourceAggregate | FunctionTemplate(RawFunctionTemplateId)

ExpectedTypedConstantExprKey
  execution_owner: ExpectedTypedConstantExecutionOwnerV1
  raw: RawConstExprOccurrenceId
  environment: VerifiedGenericEnvironmentId
  role: TypedConstantProofRoleV1
  context: VerifiedExpressionContext

ExpectedTypedConstantExprId
  checked dense proof ID assigned on first discovery in canonical root order

ExpectedConstantRootId
  checked dense root ID assigned in the same canonical root discovery;
  it is not an ExpectedSourceValueGraph ID

VerifiedConstantFunctionSpecializationId
  checked dense ID for one exact (RawFunctionTemplateId,
  VerifiedGenericEnvironmentId, canonical input/return type key) constant-VM
  program; concrete input values belong only to MemoKey
```

```text
TypedConstantRootRoleV1 =
  TypeExtent(raw type use, Unpacked | Packed, dimension ordinal) |
  ConstInitializer(raw declaration) |
  ParameterInitializer(raw declaration) |
  GenericConstActual(raw generic use, formal ordinal) |
  GenericConstDefault(raw generic formal) |
  EnumVariantRecipe(raw enum variant) |
  CastWidth(raw cast target) |
  ConcatRepeat(raw concat part) |
  SelectGeometry(raw select, ColonLower | ColonUpper | IndexedWidth) |
  SourceGraphStaticValue |
  StaticLocalInitializer(raw local declaration)

TypedConstantProofRoleV1 =
  Root(TypedConstantRootRoleV1) |
  ExprOperand(owner raw expression, RawOperandRoleV1, source ordinal) |
  StatementOperand(owner raw statement, RawOperandRoleV1, source ordinal) |
  ConstructorMember(owner raw expression, member/item ordinal,
                    Value | Count | Default) |
  FunctionPort(owner raw call, formal ordinal,
               ExplicitActual | DeclaredDefault | Return) |
  Local(owner raw local declaration, Initializer | Read) |
  Loop(owner raw for, Singleton | Start | End | Step | Condition | Update)

RawExpressionContextV1 =
  SelfDetermined |
  AssignmentTo(raw type use, RawContextOwnerV1) |
  ExplicitCastTo(raw cast target) |
  CommonOperand(owner raw expression, RawContextOperationV1) |
  Condition(RawContextOwnerV1) |
  TypeOnly(RawContextOwnerV1) |
  LosslessTo(raw type use, RawContextOwnerV1)

RawContextOwnerV1 =
  Root(TypedConstantRootRoleV1) |
  Expression(raw expression, RawOperandRoleV1, source ordinal) |
  Statement(raw statement, RawOperandRoleV1, source ordinal) |
  Local(raw local declaration) | FunctionPort(raw function template, ordinal)

RawContextOperationV1 =
  Unary(RawUnaryOp) | Binary(RawBinaryOp) | Conditional | Concat |
  ArrayConstructor | StructConstructor | Select | Pattern | Decision |
  PureSystem(RawSystemFunction) | UserCall | Assignment(RawAssignmentOperator)
```

The raw context contains only syntax/raw references and comparison witnesses;
the verifier independently maps it to the closed `VerifiedExpressionContext`.
Equal raw expressions in different roles do not alias unless their complete
keys—including the source aggregate or exact function-template execution
owner—compare
equal. A source coordinate, analyzer expression ID, or equal
result value is not proof identity. No “other owner” or opaque context variant
exists in V1.

Generic environments are derived in declaration order exactly as specified by
the source semantic-object relation. Type, instance, proto, and constant
arguments occupy distinct variants. A constant binding stores an
`ExpectedTypedConstantExprId` and verified value content, not an analyzer hash.
Default arguments are evaluated in the language-defined parent/preceding-formal
environment. Aggregate actuals share persistent value nodes and are never
expanded per specialization.

The complete root table is derived bidirectionally from fixed typed-HIR root
owners plus the independently derived source-static frontier below. A missing,
duplicate, extra, wrong-role, wrong-environment, nonmaximal source-static, or
orphan expression proof is invalid.

### Source-graph static-value frontier

Ordinary executable source expressions need an independently verified constant
producer whenever the expected source graph will emit a
`PhaseSLTNodeV1::Constant`. The verifier discovers those producers from the raw
executable-HIR ownership tree after name/type/static-binding prerequisites are
known, but **before** assigning any `ExpectedSourceValueGraph` node, use, or
result ID. Analyzer `Comptime`, an existing SLT constant, producer CSE, and
absence of an input node are not eligibility evidence.

The controller-first, demand-sensitive classification is closed. This document
is the sole authoritative owner of `SourceGraphStaticClassV1` and
`SourceGraphRuntimeDependencyV1`; every source/wire document imports generated
aliases and must not repeat either discriminant list:

```text
SourceGraphStaticClassV1 =
  FullyStaticProjectable |
  FullyStaticNonprojectable |
  RuntimeDependent(first SourceGraphRuntimeDependencyV1)

SourceGraphRuntimeDependencyV1 =
  SemanticObjectRead | EnvironmentOrMemoryRead | MutableBinding |
  FunctionPort | FunctionLocalOrLet | LoopBinding |
  RuntimeCallOrExternalState | RuntimeControlResult | EffectfulOperation

VerifiedSourceGraphStaticValueRow
  root: ExpectedConstantRootId with role SourceGraphStaticValue
  proof: ExpectedTypedConstantExprId
  execution owner / verified generic environment /
    exact raw executable-expression occurrence and final context
  value: VerifiedConstValueId
  source_projection: VerifiedSourceTypedValueId
```

Typing visits every expression child in the same closed operand order; value
classification visits only the independently activated edges in that order.
Literals, immutable parameter/constant/generic/enum references, and
type-only results are static when their independently verified prerequisites
are ready. A pure unary/binary/conditional/decision/constructor/select/system
form is fully static only when every **activated** value dependency is fully
static; type-only prerequisites do not create a runtime dependency. Every
guarded operand is still typed. When its controlling expression has a completed
independently verified static value, the classifier applies the same
short-circuit, conditional-X merge, ordered decision, and pattern truth rules as
evaluation and activates exactly the edges those rules may demand. Thus
`0 && runtime_read` and a known-true conditional do not acquire a value edge to
their suppressed operands, while an X condition activates every arm required by
the closed merge rule. A suppressed port/local proof remains a complete
`RuntimeFragment`; it is neither executed nor allowed to taint the parent's
static class. The classifier always completes the controlling proof before
classifying its guarded successors. If that proof is runtime-dependent and
therefore cannot produce a static controlling value, classification activates
every guarded edge which is semantically possible. An admitted pure user call is static only
when every actual is static and the callee's verified capability relation proves
no external mutable/environment/effect dependency. Its concrete finite
execution still uses the ordinary certificate VM.

A source semantic-object/input read, mutable binding, runtime control result,
or nonadmitted/effectful call makes the containing expression
`RuntimeDependent`; the first reason is the first canonical **activated**
offending value edge and is recomputed, not proposed. Within a runtime function template, every
expression depending transitively on an input port, `Var`, `Let`, loop binding,
assignment state, or runtime call state remains `RuntimeFragment` even when one
caller supplies a constant actual. Expressions in that template which are
independent of all such state may be static roots, but their identity is exactly
`FunctionTemplate(raw template) + VerifiedGenericEnvironmentId`. Call argument
values are never part of proof/root/program identity: the independently derived
input/return types select the logical function specialization, while concrete
values belong only to `MemoKey`. Caller-specific constant propagation is a
later verified source-graph rewrite, not source typing.
The template frontier is derived once per exact `(raw template, verified generic
environment)` and every expected source-graph expansion of that template must
map the same raw occurrence to that same verified projection; it cannot clone a
new root per call or merge roots across generic environments.

`FullyStaticProjectable` additionally requires a final executable packed-
integral projection accepted by the source typed-value relation. Fixed
aggregates and type/string values may be `FullyStaticNonprojectable` and may be
evaluated inside a projectable parent, but cannot directly claim one
`PhaseSLTNodeV1::Constant` or a `SourceGraphStaticValue` root. Their maximal
projectable descendants remain candidates unless another fully static
projectable occurrence encloses them. A projectable occurrence becomes a
`SourceGraphStaticValue` root exactly when no enclosing projectable occurrence
in the same executable value tree is fully static. Thus `a + (1 + 2)` creates
one root for `(1 + 2)`, while `(1 + 2) + 3` creates only the outer root. The
frontier is computed top-down from the completed demand-sensitive classes in
canonical raw owner/operand order.
All fixed and frontier roots then receive dense IDs by
`(canonical raw owner traversal, TypedConstantRootRoleV1 tag order as written,
role-local ordinal, verified generic-environment key)`. No analyzer root list,
expected-graph traversal, or producer node order participates.

Every selected frontier occurrence receives
`TypedConstantRootRoleV1::SourceGraphStaticValue`,
`evaluation_class = StaticOutput`, and one
`ExpectedConstantRootRow`. Proofs evaluated only as descendants of that root
remain `RuntimeFragment` and publish no value even when their own operands are
static; a distinct type-forming/static-binding root role remains independently
owned and is not suppressed. Root evaluation applies the exact final source
context/coercion, promotes the constant result into the typed verifier's
persistent value arena, and derives the canonical packed source-projection
content. It never writes or grows a source-phase arena. Only after all such
roots and witness comparisons succeed does the outer source session reserve and
intern that content in its `VerifiedSourceTypedValueArena`; an outer reservation
failure remains an outer source-resource error. On success the typed verifier
records only the checked root/content/`VerifiedSourceTypedValueId` relation in
`VerifiedSourceGraphStaticValueRow`. `ExpectedSourceValueGraph` traversal then
maps the exact raw owner/environment/context lineage to that row and consumes
`source_projection` in its expected `Constant` recipe. The root never contains
or depends on a later expected-graph ID.

The raw witness relation is bidirectional over this independently derived
frontier. Every selected root has exactly one `RawConstantExprWitnessRow` with
a proposed value and `PureNoCertificate | Certificate` summary as appropriate;
an unselected descendant/runtime occurrence must propose `None`. A missing,
extra, split, merged, or value/type/coercion/trace-disagreeing frontier witness
is rejected. Analyzer `Comptime` may be retained as one comparison field only
after evaluation; it cannot create, remove, widen, or merge a frontier root.

The completed expected relation is concrete rather than an informal property:

```text
ExpectedTypedConstantExprRow
  key / checked dense proof ID
  natural VerifiedConstType / final VerifiedConstType
  ordered VerifiedConstOperandRange
  TypePrerequisiteRange / EagerValuePrerequisiteRange /
    GuardedValueUseRange
  evaluation_class: TypeOnly | StaticOutput | RuntimeFragment
  completion: Typed | StaticOutputReady(VerifiedConstValueId)
  ConstDerivationRuleV1

VerifiedConstOperandRow
  owner proof / source operand ordinal and RawOperandRoleV1 / operand proof
  natural-to-operand-context coercion / operand-to-result coercion if required

ExpectedConstantRootRow
  exact typed-HIR owner and root role / proof / final expected type and value

RawConstantExprWitnessRow
  raw occurrence / raw environment-use witness /
  TypedConstantProofRoleV1 / RawExpressionContextV1
  proposed natural/final RawProposedConstTypeV1
  canonical RawProposedCoercionRange / RawProposedDependencyRange
  proposed RawProposedConstValueV1
  proposed RawSpecializationWitnessV1 / RawTraceSummaryV1

RawProposedConstTypeV1 =
  Integral(width raw magnitude, signed witness, Bit | Logic) |
  PackedAggregate(raw type use, projected width/sign/domain witnesses) |
  FixedArray(raw type use, canonical extent witness range) |
  TypedString | TypeValue(raw type use) | Unit

RawProposedConstValueV1 =
  None | Root(RawProposedValueNodeId)

RawProposedValueNodeRow
  owner: ConstantExprWitness(raw witness row) |
         ArrayElement(raw proposed interval row) |
         StructMember(raw proposed member-value row)
  kind:
    Integral(payload RawArbitraryBitsId, mask RawArbitraryBitsId,
             width/sign/domain/value-class witnesses) |
    String(canonical RawProposedStringByteRange) |
    Array(raw type-use witness, canonical RawProposedArrayExtentRange,
          canonical RawProposedArrayIntervalRange,
          root: raw proposed interval row) |
    Struct(raw type-use witness,
           canonical RawProposedStructMemberValueRange) |
    TypeValue(raw type-use witness) | Unit

RawProposedArrayExtentRow
  owner array value node / dimension ordinal / RawArbitraryBitsId extent

RawProposedStringByteRow
  owner string value node / byte ordinal / exact u8

RawProposedArrayIntervalRow
  owner array value node / canonical child-first ordinal /
  start RawArbitraryBitsId / length RawArbitraryBitsId /
  value RawProposedValueNodeId /
  left: None | Some(raw proposed interval row) /
  right: None | Some(raw proposed interval row)

RawProposedStructMemberValueRow
  owner struct value node / declaration ordinal /
  exact member-name occurrence / value RawProposedValueNodeId

RawProposedCoercionRow
  source proof role / target role / proposed closed coercion tag /
  source and target type witnesses
RawGuardRoleV1 =
  LogicalRight | ConditionalThen | ConditionalElse |
  ArrayDefault(tail/member context ordinal) |
  StructDefault(member declaration ordinal) |
  DecisionPattern(arm ordinal, pattern ordinal) |
  DecisionArmResult(arm ordinal) | DecisionDefault |
  InsidePattern(pattern ordinal) | FunctionExecution
RawProposedDependencyRow
  source proof role / target raw occurrence and raw environment witness /
  TypePrerequisite | EagerValuePrerequisite | GuardedValueUse(RawGuardRoleV1)
RawSpecializationWitnessV1 =
  None | Analyzer(raw function template, analyzer specialization identity,
                  raw generic-environment witness)
RawTraceSummaryV1 =
  PureNoCertificate | Certificate(raw invocation-certificate row,
                                  proposed row count)
```

Every `Raw*Witness`/`RawProposed*` row in this block is a verifier-derived
private mapped mirror of the syntax-lineage-keyed producer relation in
[`source-wire-format.md`](./source-wire-format.md). The producer never supplies
the private occurrence, environment, type-use, value-node, specialization, or
certificate IDs shown here. Only after syntax/environment lineage and the
producer witness forest are structurally valid does the verifier map their
closed keys and owned pool entries into these private IDs; missing, duplicate,
extra, or unmappable rows fail before semantic comparison.

One non-`None` witness owns exactly one root proposed-value node. An array node
owns its extent and interval ranges; each interval row owns exactly its one
value node. A struct node owns declaration-ordered member rows and each member
row owns exactly its one value node. A string node owns its byte range. These
ranges and node references form one finite rooted forest: no alias, cycle,
duplicate owner, orphan node/entry, gap, or noncanonical empty range is valid.
Array interval rows are child-first, have positive length, are disjoint, have a
strictly start-ordered in-order traversal, exactly cover the independently
derived flattened fixed-array element count, and their named root reaches every
row once. Every admitted fixed-array extent is nonzero, so the interval range
is nonempty. All arbitrary counts remain
canonical raw magnitudes until verified, so this witness topology does not
perform a host-size conversion. The verifier derives the expected value first
and compares this forest by content; producer node shape and IDs never become
persistent value identity.

The root `value` field is an output slot filled only by the verifier's pure
evaluation or accepted synthetic-root replay. It is not an input to replay and
has no producer counterpart.

`TypeOnly` and `RuntimeFragment` publish no value. A runtime fragment may execute
zero, one, or many times in one or several function invocations and may produce
different ephemeral values each time; `YieldProof` returns that instance to its
caller but never writes the proof row or promotes it. A `StaticOutput` is
exactly an `ExpectedConstantRootRow`: type extent, constant/parameter binding,
generic constant actual/default, enum recipe, cast width, concat repeat count,
colon-select bounds, indexed-select width, eligible static-local initializer,
or one maximal `SourceGraphStaticValue` frontier occurrence. It alone may
transition from `Typed` to `StaticOutputReady`. A descendant of a source-graph
static root is a `RuntimeFragment` in this storage sense even when its value is
deterministic: it executes inside the owning root program and does not publish a
second output.
Array repeat counts/defaults and indexed-select anchors are runtime fragments
under an already fixed expected type. An
unselected guarded fragment remains a complete `Typed` row. `Visiting` is
private scheduler/VM state, not a publishable row state. The verifier
constructs the expected rows first, then compares witness rows
bidirectionally by raw occurrence, environment use, role, and context. A
producer-proposed coercion, dependency, specialization, or value appears only
in the witness table. Missing, duplicate, extra, or orphan expected/output/
witness rows are rejected.

A witness for `TypeOnly | RuntimeFragment` must propose
`RawProposedConstValueV1::None`; only a `StaticOutput` witness may propose a
value or trace summary. Runtime values are checked against the independently
derived exact type/coercion at every execution, but there is intentionally no
single analyzer value with which all loop/invocation instances could agree.

## Joint dependencies and contextual typing

Type normalization and constant evaluation are one joint worklist with closed
node kinds:

```text
TypeUseInstance | GenericEnvironment | ConstBinding | ConstExprProof |
EnumVariantReplay(enum, declaration ordinal) | EnumFinalize(enum) |
FunctionSignature | FunctionSpecialization | StaticBinding | Program
```

Edges have one of three independently derived kinds:

```text
TypePrerequisite
EagerValuePrerequisite
GuardedValueUse(guarding expression and exact RawGuardRoleV1)
```

The expression dependency table is exhaustive:

| Form | Type prerequisites | Value demand order |
| --- | --- | --- |
| literal, type value | declared/context type only | none for a type value; literal itself when demanded |
| parameter/generic/local-constant/enum reference | resolved binding type | referenced binding eager when this proof is demanded |
| function input/mutable local/loop-binding reference | resolved frame-slot type | no graph edge; one derived VM load when executed |
| `TypeOf`, `$bits`, `$size`, bound symbol | referenced child/base shape only | child/base is never value-demanded |
| unary, ordinary binary, cast operand | every expression operand; a builtin/user type target adds its normalized type edge, a user constant-width target adds its binding type edge, and a literal-width target is reparsed during typing | unary operand eager; ordinary binary left then right eager; a user constant-width target is eager before the cast operand, and otherwise the cast operand is eager after its target type is complete |
| `&&`, `\|\|` | left and right | left eager, right guarded by the closed short-circuit state |
| conditional | condition and both arms | condition eager; then/else guarded; unknown visits then before else |
| concat/repeat | every part and repeat count | for each source item, its value is eager exactly once and then its optional count is eager; count zero omits only the constructed contribution |
| array constructor | item/count/default and all tail assignment contexts | items are visited in source order; an ordinary value is eager once, and a repeated value is eager once before its count even when that count is zero; default remains guarded by `fill > 0` and is visited once per distinct missing member/tail context |
| struct constructor | type and every explicit/default member context | explicit fields eager in source order; default guarded for each missing member in declaration order; no missing member leaves it type-only |
| select | base shape/value and every index/bound proof | base eager, then indices/bounds eager left-to-right |
| case expression | selector, all patterns, results, default | selector eager; arm conditions source-order guarded; one arm's comma-separated patterns short-circuit OR; selected result guarded; unknown visits current result before the remaining decision |
| switch expression | all conditions, results, default | conditions use the same ordered guarded OR/decision rule as case |
| `inside`/`outside` | target and nonempty ordered patterns | target eager once; patterns guarded source-order by logical OR; `outside` negates the completed result once |
| pure system function | tag-specific operand contexts | admitted value arguments eager in source order; type queries are type-only |
| user call | resolved signature and every actual/formal context | actuals eager in retained source order; the call body is a guarded execution use |

Every raw expression tag has exactly the rows named by this table. A zero-repeat
value is evaluated once by the eager rule; a zero-fill default remains type
checked but undemanded. No adapter may turn a
guarded edge into an eager SCC edge or omit a type edge because a value path is
dead. Thus a skipped `1 || (1 / 0)` arm is typed but does not divide, and a
syntactically recursive dead arm is not falsely rejected as an eager value
cycle.

Type formation is strictly elaborative in `CeloxSourceV0_20`. A concat repeat
count and a colon/indexed part-select width may change an exact result type, so
their proofs may read only parameters, generic constants, enum constants, and
lexically visible `Const` bindings classified `ElaborativeTypeForming`, which
transitively have only those dependencies and no user call. A function input port, mutable local, `Let`, loop binding,
assignment target, control-dependent result, or user-function result is not a
type-forming constant even when the enclosing function is invoked from a
constant root; using one here rejects as `CONST.TYPE_CONTEXT`. The same rule
applies to every function-local `Const`: it is static for exactly one raw
function template and verified generic environment, and may never depend on
function actual values or VM state. Such a binding is classified independently
as:

```text
VerifiedStaticBindingClassV1 =
  ElaborativeTypeForming | ValueOnlyStatic
```

`ElaborativeTypeForming` has only the elaborative dependencies above.
`ValueOnlyStatic` may additionally call an admitted pure user function whose
actuals are themselves static values, and therefore may require the synthetic
root/certificate described below, but no type-forming proof may reference it.
The class is the transitive dependency result, not a producer annotation.
It is derived for every parameter, generic constant, source/local `Const`, and
enum binding—not only function locals—so a value-only binding cannot launder a
call result into an extent, cast width, repeat width, select geometry, enum
width recipe, or generic constant used by a type.

```text
TypeFormingProofRoleV1 =
  TypeExtent | CastWidth | ConcatRepeat |
  SelectColonLower | SelectColonUpper | SelectIndexedWidth |
  InferredEnumWidthRecipe | TypeUsedGenericConst
```

Every proof in this closed role set accepts references only to
`ElaborativeTypeForming` bindings. `ArrayRepeat` is deliberately absent because
it checks/fills an already fixed array type at VM execution time.

This follows the pinned Veryl repeat check, which requires `is_const`; function
ports are created as non-const variables. It also avoids inventing dependent
runtime types absent from the source language. Array item repeat/default fill
counts do not form the fixed expected array type and remain ordinary runtime
values under the guarded construction rule. A function specialization is
therefore keyed only by template, verified generic environment, and its
canonical input/return *types*; all input values belong only to `MemoKey`.

The joint demand scheduler's explicit `Unseen | Visiting | Ready` stack rejects
the first canonical active backedge over type-prerequisite and eager-value
tasks. It does not reject a recursive constant-function call graph or a guarded
expression edge which is never executed; activating a guarded edge creates its
task and therefore exposes a concrete cycle if one exists. Function loops and
recursion use the finite execution certificate below.

Enum replay participates at variant granularity. The first implicit variant
uses the encoding's closed initial recipe and has no predecessor edge; each
later implicit variant has one eager edge to its predecessor. An explicit
variant has one eager edge to its constant proof; a reference in that proof to
an earlier variant has an eager
edge to that earlier `EnumVariantReplay`. A reference to the current or a later
variant is `CONST.DEP_FORWARD_ENUM`. `EnumFinalize` depends on every variant and
alone derives inferred width, final lossless coercions, encoding recurrence,
and uniqueness.

For an inferred-base enum, a reference to an earlier variant inside another
recipe would make that reference's packed width depend on `EnumFinalize`, while
the final width depends on the recipe. V0_20 has no source rule that resolves
that circular width. This profile therefore rejects that exact form as
`CONST.DEP_CYCLE`; it is not evaluated at a guessed analyzer width. The same
reference is admitted when the enum has an explicit fixed base. Supporting the
inferred-base form later requires a separately versioned symbolic-width
constraint relation and cannot be added as an evaluator heuristic.

Every explicit recipe of an inferred-base enum uses a dedicated two-stage
relation, not the ordinary final-context pass:

```text
InferredEnumRecipeStageV1 = SelfDeterminedRecipe | FinalBaseReplay
```

`SelfDeterminedRecipe` derives/evaluates a complete natural type without any
enum-base width. It admits sized/widthless/baseless integral and Boolean
literals; references to already complete elaborative bindings outside this
enum; admitted unary/binary/conditional/concat/select operations whose children
all obtain complete natural types; fixed casts; and admitted pure system
functions. It rejects all-bits literals (`'0/'1/'x/'z`), strings, type values,
unpacked aggregates, same-enum references, a cast/operation needing the unknown
enum width, and every user call or VM-dependent value. Thus no contextual fill
literal or width-inheriting expression can silently borrow a provisional base.

The recurrence consumes those exact mathematical/payload-mask values and then
`EnumFinalize` derives the maximum minimum width. `FinalBaseReplay` constructs
the final inferred base and applies the ordinary lossless coercion plus encoding
predicate to every recipe in declaration order. No recipe is reevaluated and no
new dependency is discovered in the second stage. `ExplicitFixed` enums skip
this special stage and use their known final context from the start.

No graph walk, type walk, aggregate walk, expression evaluation, or function
call uses the host call stack. `Enter`/`Finish` work frames and checked dense
state tables are used throughout. There is no nesting-depth, expression-count,
recursion-depth, loop-iteration, or trace-length policy cap.

Typing has two explicit passes:

1. derive every operand's self-determined/natural type bottom-up; and
2. propagate the exact enclosing context top-down, then derive every operand
   coercion and final result type.

The top-down pass may revisit a shared raw expression under a distinct proof
key, but derives each exact `(execution owner, raw, environment, role, context)`
once. It is
not a repeated fixed-point search. A producer-supplied type, signedness,
domain, width, `is_const`, or resolved shape is compared only after the
expected relation is complete.

`VerifiedExpressionContext` is one closed variant:

```text
SelfDetermined
AssignmentTo(VerifiedTypeUseInstanceId)
ExplicitCastTo(VerifiedCastTargetId)
CommonOperand(width, signedness, domain, operator rule)
Condition
TypeOnly
LosslessTo(VerifiedTypeUseInstanceId, exact owner rule)
```

Assignment and explicit-cast widening uses source signedness. Common-operand
widening uses the derived common signedness, so one unsigned operand forces
zero extension. Truncation is independent of the extension basis. The
positive-type rules and exact lossless coercion are those in
`source-semantic-inputs.md`; they are replayed here rather than copied from an
analyzer flag.

## Verified values and flat storage

The evaluator owns a richer internal value arena; executable phase constants
are a checked projection of it:

```text
VerifiedConstType =
  Source(VerifiedTypeUseInstanceId) |
  SystemIntegral { width, signed, static_domain, positive_type } |
  TypedString |
  TypeValue

VerifiedConstValueRow
  ty: VerifiedConstType
  kind: Integral(VerifiedIntegralValueId) |
        String(VerifiedStringValueId) |
        Type(VerifiedTypeUseInstanceId) |
        Array(VerifiedArrayValueId) |
        Struct(VerifiedStructValueId)

VerifiedDerivedConstValueOriginRow
  value: VerifiedConstValueId
  closed operator/function/aggregate step origin

VerifiedIntegralValueRow
  payload: VerifiedBitsId<ConstantProofPhase>
  xz_mask: VerifiedBitsId<ConstantProofPhase>
  width: nonzero checked bit width
  signed
  static_domain: Bit | Logic
  value_class: Evaluation | MaterializedStorage
  positive_type: Plain | Positive

VerifiedStringValueRow
  disjoint canonical range in the flat decoded-byte pool

VerifiedTypedValueRow<P>
  PackedIntegral { payload: VerifiedBitsId<P>, xz_mask: VerifiedBitsId<P>,
                   width, signed, static_domain,
                   value_class: Evaluation | MaterializedStorage,
                   positive_type }

PhaseTypedValueOriginRow<P>
  value: VerifiedTypedValueId<P>
  origin: exact ExpectedTypedConstantExprId or closed derived-value origin
```

The type variants are selected by a closed rule. A declaration/reference,
constructor, member/select which retains a declared nominal type, assignment
materialization, or explicit data-type cast uses `Source`. Literals and
ordinary unary/binary/comparison/system-function results use
`SystemIntegral`; a width cast also uses `SystemIntegral`. An explicit packed
projection uses `SystemIntegral` plus its separate exact source origin. A
same-source-type aggregate/enum conditional and an identity operation named by
the result table retain `Source`. `TypedString` and `TypeValue` are disjoint and
never encoded as a zero-width integral.

`SystemIntegral` admits only a nonzero width, a closed static Bit/Logic domain,
and a legal positive class. A `Source` value must independently normalize to an
admitted fixed type. `Integral` is compatible with a source primitive/enum or
an exact packed union reinterpretation; `Array` with a fixed array source;
`Struct` with that exact struct source; `String` only with `TypedString`; and
`Type` only with `TypeValue`. A proof-only float source owns no value row.
Every other type/kind pair is `CONST.TYPE_KIND`, even if its flat width happens
to match.

For integral values, bits above `width` are zero. The static Bit/Logic domain is
not the same fact as current four-state evaluation content. A
`MaterializedStorage` Bit value has an empty X/Z mask; an `Evaluation` value may
carry X/Z even when its static domain is Bit. Mask zero is a known bit; mask one
with payload zero is X and mask one with payload one is Z. Identity, select,
and concat preserve X versus Z. Operators which synthesize an unknown produce
canonical X, never Z. Value content identity includes `value_class` and static
domain, so an optimizer cannot silently substitute an unmaterialized value for
a storage boundary.

Only persistent completed values are content-addressed: evaluated proof roots
and bindings, memo arguments/results, and values selected for publication. VM
stack temporaries and superseded local versions are not globally interned.

The persistent content index is a flat fallible crit-bit/Patricia trie. A key
is a canonical sequence of fixed-width atoms: closed type metadata followed by
integral limbs or string bytes; aggregate/tree/rope keys contain already
canonical child IDs, exact lengths, and tags. A lookup follows stored
discriminating atom/bit positions and performs one full exact comparison at the
candidate leaf. Insertion performs one full scan to find the first differing
position, then inserts one checked branch and leaf in canonical discovery
order. Thus a long common prefix is not rescanned once per comparison level;
one lookup is `O(key atoms + trie branch depth)`. Equality never follows a hash,
raw pointer, numeric ID coincidence, or producer ID. Proof origin remains in
the separate result/origin relation.

During construction an editor temporarily borrows the aggregate's private
brand capability to validate and create checked IDs. Prepared/frozen rows store
only compact IDs and ranges, never a `BrandRef`; the owner and all unbranded
parts move together on commit. `ConstantProofPhase` is a private checked ID
namespace, not a separately publishable artifact phase. The final checked
projection derives canonical source-value content but does not create a source-
phase ID or origin row. After the complete typed proof and witness comparison
succeeds, the outer source session interns that content, creates the source-
phase `VerifiedTypedValueId<P>` and origin row, and returns the ID for the typed
verifier's checked `SourceProjectionRows` relation.

Arrays use a persistent interval tree with exact lengths:

```text
VerifiedArrayValueRow
  type / exact outer extent / tail shape / root VerifiedArrayTreeId

VerifiedArrayTreeRow =
  Uniform { exact length, value: VerifiedConstValueId } |
  Branch { exact length, left, right }

VerifiedStructValueRow
  exact source struct type / declaration-order member-value range
```

Branch splits are at the canonical midpoint. Equal uniform children merge by
content. A constructor first produces sorted, disjoint source interval runs and
bulk-builds the canonical tree in `O(items + produced tree nodes)`; it never
performs one root-to-leaf insertion per item. Struct values retain one
declaration-order range of member value IDs. Bit concatenations bulk-build a
balanced persistent rope with `Leaf`, `Repeat { child, exact count }`, and
`Join`; materialization to limb planes happens only at a boundary which
requires packed bits.

Tree and rope IDs are child-before-parent and therefore acyclic. Every branch
length equals the checked sum of its children and uses the exact canonical
midpoint; every child has the same element/tail type. `Repeat` checks
`child_width * count`, `Join` checks the exact width sum, and neither admits a
zero-width node. A zero contribution is omitted from the value tree/rope while
remaining in the proof/provenance interval table. These invariants are replayed
bidirectionally before a root is accepted.

All exact counts are verifier-owned arbitrary-width naturals until they have
been compared with the independently derived extent/width. Conversion to a
host index occurs only after representability and bound checks. Zero is legal
for a repeat contribution, but no executable integral value may have final
width zero. No repeat/default/array extent is expanded into one row per element
or bit.

## Literal typing and closed coercion order

Literal parsing first produces an exact mathematical/pattern value, then the
contextual type pass fixes its width. It never parses through a host integer.

- A base-less decimal literal is signed and two-state. Its self-determined
  width is `max(32, magnitude_bit_length + 1)` for a nonzero positive spelling
  and 32 for zero, so the sign bit can represent the exact nonnegative value.
  The literal itself does not reduce modulo 32 bits; unary minus is a separate
  operator. This follows the emitted SystemVerilog integer rule and makes the
  pinned analyzer's unconditional 32-bit truncation a witness mismatch.
- A based literal with an explicit width has that exact nonzero width. Digits
  whose exact payload/X/Z pattern needs more bits than that width are rejected
  as `CONST.TYPE_LITERAL`; V0_20 does not silently truncate an oversized
  literal. A fitting pattern is padded with known zero; when the leading
  lexical digit is X or Z, that state instead fills the added high bits. The
  `s` marker sets the completed literal's signedness but does not sign-fill
  known digits inside the literal. All of this precedes outer coercion.
- A widthless based literal receives the width which V0_20 must write into the
  translated code. For binary/octal/hex, a known leading digit contributes its
  significant bit length, except that a lexical leading zero contributes one
  bit; each following digit contributes 1/3/4 bits. A leading X or Z contributes
  the full radix digit width. Decimal uses the exact magnitude rule, with one
  bit for zero. Thus `'b001`, `'o001`, and `'h001` have widths 3, 7, and 9,
  while `'h0` has width one. The optional `s` marker changes only signedness;
  the adapter must also repair the pinned emitter path which currently assumes
  the first post-apostrophe byte is always the radix. Decimal leading zeros do
  not contribute radix-digit width: `'d001` has magnitude one and width one.
  The pinned emitter's generic leading-zero calculation currently emits width
  seven for that spelling and is another emitter bug, not this rule.
- Unsized fill literals `'0`, `'1`, `'x`, and `'z` are unsigned and have no
  admitted self-determined width in this profile. A context-determined use
  fills the required width with that state; this is a literal rule, not
  ordinary sign extension. They are rejected in a concat part, repeat count,
  shift count, logical/condition operand, or other self-determined position.
  An explicit sized all-bits literal has exactly its stated nonzero width.
- A known literal is Bit; a literal containing X or Z is Logic. X and Z remain
  distinct in its payload/mask representation. Translation must preserve the
  derived two-state domain at every Bit result boundary; it may not rely on an
  untyped SystemVerilog literal or ternary expression which can retain X/Z.
  The emitter therefore uses an exact-width two-state destination/cast or a
  verified folded value wherever the surrounding SystemVerilog context would
  otherwise be four-state.
- `true` and `false` are respectively known one and zero with natural type
  unsigned one-bit Bit.

The based-digit alphabet is closed:

| Radix | Admitted digits after separator removal |
| --- | --- |
| binary | `0`, `1`, `x`, `X`, `z`, `Z` |
| octal | `0`--`7`, `x`, `X`, `z`, `Z` |
| decimal | `0`--`9` only |
| hexadecimal | `0`--`9`, `a`--`f`, `A`--`F`, `x`, `X`, `z`, `Z` |

An underscore occurs only between two nonempty digit groups; leading,
trailing, doubled, or apostrophe/radix-adjacent underscores are invalid. At
least one digit is required. Upper/lower X/Z and hex spelling are semantically
equal but remain visible in the raw spelling witness.

Width, digit count, required limbs, and radix conversion are checked and
fallibly reserved before mutation. There is no maximum literal-width policy.
An inability to represent the exact required storage on the host is a resource
or representability error, never silent truncation.

For every operation, the order is fixed:

1. derive the natural result and operand-context types;
2. apply width/sign/static-domain operand coercions in four-state evaluation
   form, without materializing two-state storage;
3. execute the closed value rule at that width in four-state scratch form;
4. record an `Evaluation` result with its derived static Bit/Logic domain while
   retaining any X/Z content; and
5. only at an actual assignment/materialization boundary, apply the target
   coercion and create `MaterializedStorage`.

Materialization boundaries are declaration/parameter/`Let` initialization,
mutable local/object store, function-actual binding, function return, aggregate
element/member storage, an explicit cast to a two-state data type, and final
storage publication. Common-operand coercion, contextual width propagation, a
width cast, and an intermediate operator result are not boundaries. At a Bit
boundary, known one maps to one and 0/X/Z maps to zero after the extension basis
from step 2 has already been applied.

The verifier derives one closed operand class before consulting an operator:

| Operand class | Members | Admitted operations |
| --- | --- | --- |
| `NumericIntegral` | primitive Bit/Logic/bool/fixed/positive integer or enum with no unpacked dimension and no structural aggregate boundary | every integral row in the result table, constructors/selects which produce the class, and the pure integral system functions |
| `PackedStructural` | a completely packed array proof or source struct/union, excluding a control-family terminal | canonical integral projection, then the same integral operations; same-type constructor/member semantics and conditional reconstruction retain the structural result type |
| `ControlBit` | a bare clock/reset-family value with exactly its intrinsic one bit and no explicit dimension | identity, assignment/cast to an admitted one-bit target, `~`, `!`, bitwise/equality/logical operations, and same-type conditional; arithmetic, relational, shift, power, and concatenation are rejected |
| `ControlAggregate` | a clock/reset-family terminal with at least one explicit fixed dimension and no unpacked dimension | construction/reference/select, assignment, function argument/return, same-type conditional, vector bitwise operations, and whole-value ordinary/wildcard equality with scalar width-one result; `!`, `&&`, `\|\|`, arithmetic, relational, shift, power, and concat are rejected |
| `FixedUnpacked` | a non-string, non-float fixed value with at least one unpacked dimension | construction, reference, assignment, function argument/return, and point/member select only; whole-value operators and conditional merge are rejected in this profile |
| `TypedString` | source `string` | the closed string comparisons only |
| `TypeValue` | a retained type expression | type-query/type-context use only |
| `ProofOnlyFloat` | `f32`/`f64` shape | `$bits` and `$size` type queries only; no value may be constructed |

These predicates are a partition, not an ordered guess: type values, scalar
strings, and float shapes use their dedicated tags; presence of an unpacked
dimension selects only `FixedUnpacked`; a control terminal selects exactly
`ControlBit` or `ControlAggregate`; the remaining completely packed source is
`PackedStructural` exactly when its normalized value representation owns an
array/struct/union boundary, and otherwise its primitive/enum terminal is
`NumericIntegral`. Alias normalization is completed before this test but exact
enum/aggregate nominal identity remains in `Source`.

An operand in no row is `CONST.TYPE_OPERAND`. Projection is explicit in the
operand/coercion table; a producer cannot call an aggregate "integral" without
the verified projection. `$clog2`, `$onehot`, `$signed`, and `$unsigned` admit
`NumericIntegral` and projected `PackedStructural`, but not strings, unpacked
values, type values, either control class, or proof-only floats. `$signed` and
`$unsigned` preserve bits/width/static domain but produce the `Plain` positive class.

## Result typing table

The following table is complete for admitted integral operators. `join-domain`
is Logic when either operand is Logic and Bit otherwise. `common-width` is the
maximum natural operand width and any enclosing context width which the
operator is defined to inherit. `common-signed` is true only when every
participating operand is signed.

| Form | Operand context | Result |
| --- | --- | --- |
| unary `+`, unary `-` | operand/result context width | operand width, signedness, and domain |
| unary `~` | operand/result context width | operand width, signedness, and domain |
| reductions `& ~& \| ~\| ^ ~^` | self-determined operand | unsigned width 1, operand domain |
| logical `!` | self-determined width-one operand | unsigned width 1, operand domain |
| `+ - * / %`, bitwise binary | common-width/common-signed | common-width, common-signed, join-domain |
| `**` | left takes result context; right self-determined | left width/signedness, join-domain |
| shifts | left takes result context; count self-determined | left width/signedness/domain |
| relational/equality/wildcard equality | common-width/common-signed operands | unsigned width 1, join-domain |
| `&&`, `\|\|` | both self-determined width-one operands | unsigned width 1, join-domain |
| conditional | arms use common result context; condition is width one | common arm width, signed only if both arms signed, arm join-domain |
| concatenation/repetition | parts self-determined | checked sum width, unsigned, joined domain |
| explicit cast | source evaluated under the cast's closed rule | exact target type |

Relational and equality results are unsigned even when comparison is signed.
The right shift count never makes the left unsigned. `<<<` has the same bit
movement as `<<`, while `>>>` sign-fills; V0_20 rejects an arithmetic shift
whose left operand is not signed. A concatenation does not inherit an outer
signedness or use an unsized operand without a self-determined width.

The producer may retain a proposed coercion row, but the verifier derives the
row from this table and requires exact equality. It never accepts a
self-consistent but differently sized expression tree.

A verified cast target is independently derived from the retained syntax.
`BuiltinTypeUse` produces the exact normalized target type and applies
explicit-cast coercion, including the source-signed extension rule. `UserName`
is deliberately unresolved at the raw boundary: the independent resolver may
select either an admitted type declaration, which makes it a type cast, or a
parameter/generic/local/source constant, whose eager verified value makes it a
width cast. Any other target, ambiguity between the two namespaces, X/Z,
nonintegral, zero, or negative width rejects. This retains legal forms such as
`0 as WIDTH` without trusting an analyzer decision that the same identifier
spelling is a type. `WidthLiteral` must likewise be a known two-state positive
integer literal, is evaluated as an exact natural before host conversion, and
sets only the packed result width. A width cast
preserves the source signedness/domain; a packed aggregate first uses the one
canonical projection below. An enum therefore retains base signedness, an
all-packed array retains normalized whole-value signedness, and struct/union
projections are unsigned. Widening then uses that projected/source signedness,
narrowing drops high bits, and width
zero or an unrepresentable width is rejected. The analyzer's evaluated cast
target is comparison-only.

The `Positive` class is also closed. A reference or whole/unpacked-only select
of an already verified `p*`/alias value retains its declared class; parentheses
and an exact identity retain it. Every arithmetic, unary, bitwise, reduction,
logical, comparison, shift, power, conditional common type, concatenation,
packed select, `$signed`/`$unsigned`, and width cast produces `Plain` before an
outer context. A type cast or assignment to a Positive target succeeds only
through the source-profile positive rule: a constant result must be known
two-state and mathematically greater than zero. The resulting target value is
then marked `Positive`. No operation uses that marker as a runtime nonzero
optimization fact.

## Four-state truth and logical short circuit

In `CeloxSourceV0_20`, every operand of `!`, `&&`, `||`, procedural `if`,
expression conditional, and `switch` is required to have integral width one.
The pinned analyzer's width-one check is therefore a language prerequisite,
not an evaluation oracle. A future source profile may admit arbitrary-width
truth reduction, but it requires a distinct semantics tag. For the admitted
one-bit value define:

```text
known_one(v) = any bit with xz_mask = 0 and payload = 1
has_xz(v)    = any bit with xz_mask = 1

truth(v) = True     if known_one(v)
           Unknown  else if has_xz(v)
           False    otherwise
```

Known one is true, known zero is false, and X/Z is unknown. X and Z are not
distinguished by truth conversion.

```text
A && B
  A=False    -> False without value-evaluating B
  A=True     -> truth(B)
  A=Unknown  -> False if truth(B)=False, otherwise Unknown

A || B
  A=True     -> True without value-evaluating B
  A=False    -> truth(B)
  A=Unknown  -> True if truth(B)=True, otherwise Unknown
```

The skipped operand is still fully type checked. It does not execute a call,
read a value, divide by zero, allocate value scratch, or activate a guarded
dependency. Logical `!` maps true to zero, false to one, and unknown to X.

A procedural constant-function `if` first verifies the width-one condition and
takes the then edge only for `True`;
`False` and `Unknown` take the else edge. An expression conditional instead
uses the ternary merge below. These are distinct closed rules.

## Bitwise, reduction, and conditional rules

Bitwise operations first coerce both operands to the common type. At each bit,
Z is an unknown input just like X:

- AND produces zero if either input is known zero, one if both are known one,
  and X otherwise;
- OR produces one if either input is known one, zero if both are known zero,
  and X otherwise;
- XOR/XNOR produces X if either input is X/Z and otherwise the corresponding
  known Boolean value; and
- NOT produces X for X/Z and complements a known bit.

Reduction AND/OR use the same controlling-zero/controlling-one rules over all
bits. Reduction XOR/XNOR is X if any bit is X/Z. NAND and NOR complement the
corresponding reduction after the reduction is complete. An empty reduction
cannot arise because executable integral width is nonzero.

For a conditional expression, a known condition value-evaluates only the
selected arm. An unknown condition evaluates both arms, coerces both to the
derived common result type, and merges each bit:

```text
known 0 with known 0 -> 0
known 1 with known 1 -> 1
every other pair     -> X
```

In particular, X/X and Z/Z merge to X rather than preserving the input state.
The static result domain is derived from the arms, not the condition. When both
arms are Bit, the unknown-condition merge is computed in four-state scratch
and the `Evaluation` result retains the synthesized X bits with static Bit
domain. A later Bit storage boundary converts them to zero; a parent operator
sees them as X.

## Arithmetic, comparison, shift, and power rules

Known integral addition, subtraction, multiplication, and unary negation are
performed modulo `2^result_width`. Signedness controls interpretation for
division, remainder, relational comparison, and sign extension; it does not
change the stored bit pattern. Signed division truncates toward zero and the
remainder has the dividend's sign. The implementation uses fallible limb
algorithms, never `BigUint`/`BigInt` operators on the proof path.

If an arithmetic operand contains X/Z, the arithmetic result is all-X.
Division or remainder by known zero has the same all-X result. It remains X in
an `Evaluation` result for either static domain and is converted to zero only by
a later Bit materialization boundary. It is not an exception, panic, or reason
to skip the surrounding expression. In particular, the IEEE example
`int n=8, zero=0; int r=n/zero+n;` propagates X through the outer addition and
only the final `int` store converts it to zero; folding the division to zero
would incorrectly produce eight.

Relational comparison returns X if either coerced operand contains X/Z;
otherwise it compares the unsigned magnitudes or signed two's-complement
integers selected by the common signedness.

Ordinary equality first looks for a position where both coerced bits are known
and different. Such a definite mismatch produces known false for `==` and
known true for `!=`. If there is no definite mismatch but either operand has an
X/Z bit, the relation is ambiguous and the result is X. Otherwise all bits
match. Thus `2'bx0 == 2'b01` is false, while `2'bx0 == 2'b10` is X. This is the
IEEE ambiguous-relation rule; the pinned analyzer's definite-mismatch ordering
is correct and is covered by fixtures.

Wildcard equality first removes every position whose right/pattern bit is X or
Z. A definite known mismatch in any remaining position produces known false
for `==?` (known true for `!=?`). Otherwise, if a remaining left bit is X/Z,
the result is X; otherwise all remaining bits match. X/Z on the left is never a
wildcard. Procedural exact case equality is a separate four-state predicate
described below.

For shifts, the count is a self-determined unsigned bit pattern regardless of
its declared signedness. An X/Z count produces an all-X evaluation result.
The exact arbitrary-width count is compared with the result width before any
host conversion. A count at least the width produces zero for left/logical
right shifts and sign fill for arithmetic right shift; an unknown sign bit
fills X. No `usize` truncation or shift-count cap is permitted.

For integral power, an X/Z base or exponent produces all-X. A nonnegative
exponent uses modular exponentiation by squaring at the result width, so work
is logarithmic in the exponent value rather than one multiplication per unit.
A negative signed exponent has this exact result before width/domain coercion:

```text
base =  1                       -> 1
base = -1                       -> -1 for odd exponent, 1 for even exponent
base =  0                       -> X
every other known integral base -> 0
```

No implementation-defined overflow, host exponent conversion, or evaluation
timeout participates in these rules.

## Type queries, bound symbols, and system functions

`$bits` and `$size` are type-only operations: their argument is fully typed
but never value-evaluated. `$bits` computes the exact checked product of all
fixed storage dimensions. Structs use the sum of member widths, unions the one
common member width, enums the verified base width, and arrays every unpacked
and packed dimension.

`$size` uses a separate query-dimension list:

```text
expanded outer unpacked dimensions
  ++ explicit packed-array dimensions
  ++ terminal storage dimension (Packed or Intrinsic)
```

The slowest-varying dimension is first. Thus `$size(logic<8>[4])` is 4,
`$size(logic<10,20>)` is 10, and `$size(logic<8>)` is 8. The internal
`Intrinsic` storage width of a direct struct, union, or enum is its terminal
query dimension, so `$size` of that direct packed composite is its verified
packed width. An unavailable first dimension is X. Aliases use the expanded
target shape. The query-dimension relation is derived independently from the
normalized storage shape and compared with analyzer witnesses.

The proof-only `f32` and `f64` types contribute exact terminal dimensions
`Packed(32)` and `Packed(64)` to both query relations. This admits `$bits` and `$size` of their
fixed shapes without admitting a floating value or operation.

Both queries first compute an exact arbitrary-width natural, then produce the
SystemVerilog `integer` result:

```text
width = 32, signed = true, domain = Logic
payload = exact natural modulo 2^32, mask = 0
```

An X query result is 32 signed Logic X. A result greater than `i32::MAX` is not
an evaluator error or a reason to add a cap; any later extent/positive/lossless
context interprets the resulting 32-bit value normally.

`msb` and `lsb` are retained as `RawBoundSymbolRow` values tied to one exact
base expression/type-use occurrence and dimension. They do not consult an
ambient analyzer cursor. `lsb` is zero and `msb` is `extent - 1`, returned as
signed 32-bit Logic after modulo conversion, matching the emitted unsized
integer/`$bits` expression. The pinned analyzer's unsigned Bit<32> value is a
comparison mismatch to repair. A missing, zero, or unresolved dimension is a
structured bound-symbol error.

The remaining pure system functions are:

- `$clog2(v)`: evaluate the self-determined arbitrary-width integral bit
  pattern as an unsigned magnitude. X/Z yields signed 32-bit Logic X; zero
  yields zero; otherwise the known result is the exact bit length of `v - 1`,
  returned modulo `2^32` as signed 32-bit Logic.
- `$onehot(v)`: evaluate the arbitrary-width integral bit stream without width
  truncation and count bits whose exact four-state value is known one. X and Z
  are not high bits. The result is known unsigned one-bit Bit and is one iff
  that count is exactly one. The pinned analyzer's all-X result for any X/Z
  operand is a witness mismatch to fix, not the source semantic rule.
- `$signed(v)` and `$unsigned(v)`: evaluate `v`, preserve width/domain/value,
  set the result signedness, and reset the positive-type class to `Plain` as
  required by the closed positive rule.

`$readmemh`, `$display`, `$write`, `$assert`, `$assert_continue`, `$finish`,
and unknown/SystemVerilog escape calls have effects or no closed pure result
and are rejected in the constant machine. They are never silently ignored.

## Packed aggregate projection

Packed enum, struct, union, and all-packed array values have one canonical
integral projection. An enum projects its verified base bit pattern and retains
the enum base signedness. A struct projects members in declaration order, first
member at the most-significant side and last member at the least-significant
side; its projection is unsigned and uses the profile's already verified
uniform Bit/Logic member domain. An all-packed array projects logical elements
in normalized dimension order, preserves the normalized whole-array declared
signedness, and uses the verified uniform element Bit/Logic domain. A union is an unsigned packed bit pattern of
the one verified common width and has no runtime selected-member tag.

Projection builds/shares a bit rope from already typed member/element values;
it neither copies all bits nor loses the per-member constructor proof. The
inverse view of an exact-width packed value uses the independently verified
member/dimension offsets and creates persistent slices. Struct/packed-array
constructors, type casts, assignments, comparisons, concatenation operands,
condition arms, and member selects all invoke this same projection relation.
Unpacked dimensions have no integral projection.

For `==`, `!=`, wildcard equality, and integral casts/operators admitted on a
packed aggregate, both operands are first projected and then use the ordinary
integral rule. A conditional whose arms are equivalent completely packed
aggregates merges their canonical projections under an unknown condition and
reconstructs the same packed type through verified slices. An unpacked
aggregate conditional is rejected by the operand-class table rather than
expanded across its extent. Non-equivalent aggregate arms are a typing error. The final
`PhaseTypedValueOriginRow` ties a materialized packed value back to the exact
aggregate proof; value-ID equality alone is not provenance.

## Concatenation and repetition

Concatenation parts are ordered source-left-to-right from most-significant to
least-significant bits. Each part is self-determined, integral, and nonzero
width. A repeat count must be constant, known two-state, and mathematically
nonnegative. Zero repeat is legal as an empty contribution. The final checked
sum of `part_width * count` must be nonzero and representable by the verified
width type. Each source item's value is evaluated exactly once before its
optional repeat count; count zero suppresses only the rope contribution and
does not suppress the value evaluation or any call/error it performs.

A base-less decimal part is legal in this source profile because its exact
self-determined signed width was derived above. SystemVerilog forbids an
unsized constant number directly inside a concatenation, so emission must write
the verifier-derived exact-sized signed literal (for example the semantic
equivalent of `32'sd12`) rather than pass through `{12}`. The same obligation
applies to any otherwise-unsized part admitted by a future source rule.

Construction creates a persistent bit rope, not the expanded bit vector:

```text
VerifiedBitRopeRow =
  Leaf(VerifiedConstValueId) |
  Repeat { child, exact nonnegative count } |
  Join { checked width, left, right }
```

Joins use one deterministic weight-balanced construction from source order.
Adjacent equal leaves do not lose their distinct source-part provenance; only
the value rope may share them. Result signedness is unsigned. The result is
Logic if any nonempty contribution is Logic and Bit otherwise. Exact limb
planes are allocated only when a consumer needs materialized packed bits.
Work and storage for construction are proportional to syntax parts, not repeat
count or result width.

## Array and struct constructors

An array constructor is legal only under one exact fixed-array expected type,
and its verified shape retains whether each dimension is unpacked or packed.
One ordinary item produces one value of the outer dimension's complete tail
shape; repetition repeats that tail value, not its flattened leaves. Counts
are exact known nonnegative integers. A repeated item evaluates its tail value
once and then its count, including when the count is zero. The explicit count is the checked sum of
ordinary items and repeats. A value with any unpacked dimension remains an
`Array` proof value; an all-packed value additionally receives the canonical
integral projection and may become an SLT constant.

There may be at most one `default` item. It is not positional: after all
explicit source items retain their source-order output intervals, default
fills the trailing `extent - explicit_count` elements. Without default,
`explicit_count` must equal the outer extent. With default it must not exceed
the extent. A zero repeat and a default whose fill count is zero remain present
in the constructor proof even though they create no value-tree interval.

Each item is typed in the tail-shape assignment context. Nested arrays use
explicit constructor work frames. The value is the canonical persistent
midpoint interval tree described above; a huge repeat or default-fill remains
a uniform node. Bulk construction is `O(items + produced tree nodes)`; the
number of produced canonical midpoint nodes is reported separately and may be
`O(items * log extent)` for adversarial interval boundaries. One uniform fill
uses `O(1)` value storage. `Theta(extent)` expansion is forbidden.

A struct constructor names one exact verified struct type. Explicit field
names must exist and be unique. Without default, every member appears exactly
once. With one default, it supplies every missing member. Final values and
packing are in declaration order, independent of source field order. The
default expression is typed separately in each missing member's assignment
context, because an unsized literal may acquire a different type per member;
the raw AST is shared, every field retains a distinct proof role, and only
equal completed value content may be shared.

V0_20 rejects a direct union constructor. Treating a union as a struct and
concatenating several fields is invalid, while selecting an arbitrary single
member would add an unapproved source-language rule. Ordinary assignment of a
same-width packed integral expression to a verified union remains legal; a
later union-member select reinterprets the same packed bits and does not read
a runtime tag.

## Constant selects

Selects are verified left-to-right against the independently normalized shape.
A member occurrence must resolve to the exact struct/union member. Every index
is evaluated in its self-determined integral type. A known two-state value is
converted to a mathematical integer only after sign and representability
checks; in-bounds always means `0 <= index < extent`. X/Z is detected from the
evaluation mask, never inferred from the static Bit/Logic domain, and is never
converted to a host index.

A source-static known negative/out-of-bounds index is a structured bounds
error. “Source-static” means its activated value-dependency relation has no
frame/local/loop/runtime input and the proof is completed while verifying the
access under the frontier rule above; every suppressed guarded operand remains
typed but is not an activated input. It never means merely that one concrete
function invocation later supplies a value. At VM/runtime, a negative, out-of-bounds, or
X/Z packed bit/array index returns an all-X `Evaluation` value of the selected
packed width regardless of static domain; a later Bit materialization may turn
that X into zero. The same invalid index on an unpacked array read returns the
recursive default of its element storage type—zero for materialized two-state
leaves, X for four-state leaves—and an invalid unpacked or packed write is a
no-op. This distinction is explicit; a producer's conversion failure or
`unwrap_or(0)` is never an index value.

Colon part-select bounds must be known two-state constants because they define
the result width, and must satisfy `0 <= low <= high < extent`. Plus/minus/step
select widths must be known, nonzero, and no larger than the dimension; their
anchor is evaluated without materialization. A source-static invalid anchor is
a bounds error; a VM/runtime X/Z anchor yields an all-X packed `Evaluation`
result of the already fixed width. A known runtime anchor uses the exact
per-lane relation: in-range packed lanes are read, out-of-range lanes are X,
and out-of-range write lanes are ignored. Fully in-bounds anchors replay the exact checked bounds and offset formulas in
`source-semantic-inputs.md`. A range select is the final select in the access.

Array point selection follows the persistent interval tree in
`O(log extent)`. Nested select traversal uses an explicit worklist. It does not
materialize an array, reinterpret an X/Z index as `usize`, or rely on an
analyzer-computed offset.

## Case, switch, inside, and outside

The raw decision tables remain ordered syntax; no selector value domain or
range is expanded. A selector is evaluated exactly once. Every condition,
pattern, and result is typed, but guarded value evaluation follows the rules
below.

A singleton Veryl case pattern uses the wildcard-equality rule above, with
X/Z wildcard only on the pattern/right operand. A range pattern is
`lower <= selector && selector < upper`, or `<= upper` for an inclusive range,
using the closed relational and logical rules. Multiple patterns in one arm
are ordered logical alternatives.

Each source expression in a `switch` condition must have an integral
width-one result; several conditions attached to one arm are combined in
source order by logical OR. This preserves the emitted comparison with
`1'b1` and prevents a wide nonzero value from being ambiguously treated as
either truth reduction or numeric equality. Ordinary `if` and the logical
operators use the same width-one prerequisite.

Case and switch *expressions* are an ordered nested-conditional relation with a
required default:

```text
case selector { p0: v0, p1: v1, default: vd }
  == (selector ==? p0) ? v0 :
     (selector ==? p1) ? v1 : vd

switch { c0: v0, c1: v1, default: vd }
  == (c0 == 1'b1) ? v0 :
     (c1 == 1'b1) ? v1 : vd

case selector { p0, p1: v, default: vd }
  == ((selector ==? p0) || (selector ==? p1)) ? v : vd
```

Comma-separated patterns on one arm first form that one source-ordered,
short-circuit OR; the arm result occurs once. The pinned analyzer already forms
the OR, while the pinned emitter duplicates the result in nested ternaries.
Those differ for an unknown first pattern followed by a true later pattern, so
the emitter form is a bug to repair, not an alternative witness.

The verifier does not duplicate evaluation of the retained selector. The
pinned analyzer/emitter currently clone it into comparisons and must be fixed;
a fixture uses a terminating constant-function selector so call count and trace
order expose duplication. A true
condition evaluates only its result; false continues. An unknown condition
evaluates the current result and the remaining decision, then applies the
conditional bit merge. It must not jump directly to default. All result arms
use the one independently derived common result context.

Procedural `case` and `switch` inside a constant function use ordered
first-match control instead. A condition selects its body only when it is
known true; false or unknown continues to the next condition/arm, then default
or fall-through. This matches a `case (1'b1)` control decision and is not the
expression merge rule. Exact plain SystemVerilog case equality, if introduced
by a future retained syntax tag, requires a distinct rule rather than being
guessed from a lowered analyzer node.

`inside target { patterns }` evaluates `target` once and combines singleton
wildcard/range results with the logical-OR rule, including controlling true and
unknown propagation. `outside` applies logical NOT once to the complete inside
result; it does not invert each pattern. Work is proportional to patterns and
limb comparisons, never to the selector value range.

This deterministic source order is stronger than SystemVerilog `inside`, whose
set-expression evaluation order is not fixed. The emitter must therefore bind
the selector once and lower every admitted inside/outside or case-inside pattern
to the verified left-to-right wildcard/range comparison and short-circuit
control sequence. Direct `inside`/`case inside` emission is forbidden when a
pattern can execute a call or otherwise make order observable; the pinned
default pass-through is an emitter bug.

## Typed strings

A string proof value is its canonical decoded byte range. V0_20 admits typed
string literals, string constant bindings/parameters, and `==`, `!=`, `<`,
`<=`, `>`, `>=` between strings. Equality is byte-for-byte; ordering is
unsigned-byte lexicographic; the result is known unsigned one-bit Bit.

The accepted token grammar has unescaped Unicode scalars other than quote,
backslash, and U+0000--U+001F, plus exactly `\"`, `\\`, `\/`, `\b`, `\f`,
`\n`, `\r`, `\t`, and `\u` followed by four hexadecimal digits. The first
eight escapes produce their corresponding byte/scalar. `\uXXXX` denotes a
UTF-16 code unit: a high surrogate must be immediately followed by a second
`\uXXXX` low surrogate and the pair is combined; a lone, low-first, or
misordered surrogate is `CONST.RAW_ENCODING`. Every decoded Unicode scalar is
encoded as canonical UTF-8. An unescaped source scalar's original UTF-8 bytes
are retained. There is no normalization.

Decoded U+0000 from `\u0000` is retained as byte zero. No rule removes zero
bytes: integral-to-string and SystemVerilog packed-string assignment are not
admitted in this profile. The pinned parser's escape alternative is grouped so
that it accepts bare `uXXXX` while failing to recognize the intended
`\uXXXX`, and the pinned analyzer preserves invalid surrogate spellings; both
are producer bugs. Production parsing therefore uses the closed adapter version
`VerylParserV0_20_1_UEscape1`: it is the generated Veryl-0.20.1 parser and AST
with exactly one scanner correction, grouping the string escape alternative as
backslash followed by `u` and exactly four hexadecimal digits. It changes no
other token, grammar production, AST field/list order, token ordinal, source
span, or preprocessing coordinate. The adapter retains the original token
bytes; it neither rewrites `\uXXXX` into another spelling nor accepts a lone
backslash, a short/long hexadecimal sequence, or a nonhexadecimal digit.

`VerylParserV0_20_1_UEscape1` is part of the source-adapter version, not a new
constant-semantics version. An unpatched parser failure on a legal `\uXXXX`
literal cannot be reclassified as unsupported syntax or replaced by an analyzer
token. Conversely, an AST/token pair not produced by this exact adapter version
fails the source-adapter-version check before raw string rows are constructed.
After that structural check the verifier still reparses the retained original
token independently, including surrogate pairing and UTF-8 encoding; successful
scanner tokenization is not semantic evidence.

Adapter fixtures parse an ordinary BMP escape, `\u0000`, and one valid
high/low-surrogate pair while preserving exact token bytes and coordinates.
They reject truncated/nonhex escapes and prove that the unpatched 0.20.1
scanner's failure on the same legal tokens cannot enter the aggregate as an
`Invalid` expression or a missing literal row. Independent verifier fixtures
then reject lone or misordered surrogates with `CONST.RAW_ENCODING`.

These are Veryl source-string semantics, not SystemVerilog literal semantics.
The pinned emitter's raw token pass-through is invalid: SystemVerilog has a
different escape alphabet, treats a literal-literal comparison as an integral
comparison, and its `string` conversion cannot preserve embedded zero bytes.
In this profile every `TypedString` consumer is a verified constant operation,
so it is folded before executable lowering and no `TypedString` value may
cross the SystemVerilog emission boundary. A future executable-string profile
must lower an explicit `(length, byte range)` representation and implement its
operations on that representation; emitting a raw or canonically re-escaped
SystemVerilog `string` is not an equivalent fallback.

One raw string literal also has a closed packed-byte contextual form. It is
`TypedString` when explicitly assigned/bound/passed to `TypedString`, or
compared with another `TypedString`/uncontexted string
literal. It is `PackedByteLiteral` only when an exact fixed packed integral
assignment/formal, an integral operator's non-string peer, or a concatenation
part supplies that context. The adapter retains one raw literal occurrence;
the verifier, not an analyzer type tag, selects exactly one form.

`PackedByteLiteral` uses the same decoded bytes. In source order the first byte
is the most-significant byte and the last is least-significant, producing a
known unsigned static-Bit evaluation pattern of natural width `8 * byte_count`.
For zero decoded bytes the packed-expression exception below supplies one NUL
byte and width eight.
Ordinary unsigned integral assignment right-justifies it: widening adds known
zero high bits and narrowing discards high bits, after which the actual target
domain is materialized. A nonempty packed byte literal is a legal
self-determined concat part and may participate in an ordinary integral
comparison/operator with an integral peer. Two uncontexted string literals in a
comparison remain `TypedString`, so their comparison is byte-lexicographic, not
SystemVerilog literal-integer comparison. A mixed `TypedString` binding and an
integral operand is rejected; only the literal has this contextual projection.

An empty decoded literal is a valid zero-byte `TypedString`. Its
`PackedByteLiteral` projection is instead the one-byte ASCII NUL pattern
`8'h00`, matching the packed-expression rule; it is therefore legal in a
self-determined integral/operator or concat position. Emission of a packed byte
literal uses its verifier-derived exact-sized bit pattern (for example,
`"abc"` becomes the semantic equivalent of `24'h616263` and `""` of `8'h00`), never the
raw SystemVerilog string token. This preserves UTF-8 and embedded zero bytes.

String truth conversion, integral/string casts, concatenation, replication,
indexing, methods, `$bits`, and `$size` are rejected until a later complete
semantics version specifies them. These rejections concern a `TypedString`
value; the contextual literal projection above is already an integral value,
not string concatenation or a string-to-integral cast. Storage has no policy
length cap and every byte-pool allocation is fallible.

The only string coercion is exact `TypedString -> TypedString` identity for a
constant binding/parameter, `Let`, function input actual, or function return.
It preserves the complete decoded byte range including embedded zero bytes and
has no width, sign, domain, padding, truncation, or terminator step. Assignment
between string and any other class, an explicit string/integral cast, or an
executable string destination is `CONST.COERCE_DOMAIN`.

## Constant-function admission

A user function becomes a constant-function specialization only when reached
from an admitted proof root with concrete verified type/generic arguments and
typed constant input values. This proves one concrete invocation; it does not
claim that the function terminates or is constant for every possible input.

The raw function relation retains one shared syntax template plus separate
verified specializations. The template and port rows are exactly the
authoritative rows in
[`source-semantic-inputs.md`](./source-semantic-inputs.md); this document does
not define a second shortened port schema:

```text
RawFunctionTemplateRowV1
  exact declaration/owner/generic/port/return fields
  Definition { source_body: RawBlockId,
               constant_projection_root: RawConstBlockId } |
  Prototype

RawFunctionPortRowV1
  exact owner/declaration ordinal/name/type use
  direction: RawPortDirectionV1
  default: None | Expression(RawSourceExprId)

RawConstBlockRow
  source: exact RawBlockId in the owning function definition
  exact lexical scope / optional parent block / canonical RawBlockItemRange

RawBlockItemRow =
  exact source RawBlockItemId /
    LocalDeclaration(RawLocalDeclarationId) |
    Statement(RawConstStatementId) |
    NestedBlock(RawConstBlockId) |
    UnsupportedBlockItem(RawUnsupportedBlockItemKindV1)

RawLocalDeclarationRow =
  Var { name occurrence, optional type use } |
  Let { name occurrence, optional type use, initializer } |
  Const { name occurrence, optional type use, initializer } |
  Gen { retained name/type/expression; closed unsupported in this profile }

RawAssignmentOperator =
  Set | Add | Sub | Mul | Div | Rem | BitAnd | BitOr | BitXor |
  LogicShiftL | LogicShiftR | ArithShiftL | ArithShiftR |
  DiamondUnsupported

RawForRow
  loop-name occurrence
  range: Single(expression) |
         Between(start expression, end expression, inclusive flag)
  reverse flag
  step: Default | Explicit(RawAssignmentOperator, expression)
  body RawConstBlockId

RawConstStatementRow =
  exact source RawStatementId /
  Assign(RawAssignmentTargetId, RawAssignmentOperator, expression) |
  ConcatAssign(retained target range and expression; closed unsupported) |
  If(ordered RawIfClauseRange, optional else block) |
  IfReset(retained clauses; closed unsupported) |
  Case(selector, ordered RawControlArmRange) |
  Switch(ordered RawControlArmRange) |
  For(RawForId) |
  Break | Return(expression) | ExpressionCall(expression) |
  EffectfulOrUnsupported(RawUnsupportedConstStatementKindV1)

RawUnsupportedConstStatementKindV1 =
  RuntimeEvent | TestbenchMethod | NonblockingAssignment |
  SourceUnsupported(RawUnsupportedStatementKindV1)

RawIfClauseRow
  source ordinal / condition expression / exact RawConstBlockId body

RawControlBody =
  Statement(RawConstStatementId) | Block(RawConstBlockId)

RawControlArmRow =
  Case { source ordinal, nonempty RawDecisionPatternRange, RawControlBody } |
  Switch { source ordinal, nonempty ordered expression range, RawControlBody } |
  Default { source ordinal, RawControlBody }

VerifiedStaticBindingRow =
  Value { owner: Source(verified environment) |
                FunctionTemplate(RawFunctionTemplateId,
                                 VerifiedGenericEnvironmentId),
          declaration, verified type, persistent value,
          VerifiedStaticBindingClassV1,
          canonical VerifiedStaticDependencyRange } |
  Type { owner, declaration, normalized type value,
         class: ElaborativeTypeForming,
         canonical VerifiedStaticDependencyRange }

VerifiedStaticDependencyRow
  owner binding / direct dependency binding or
  StaticForbiddenDependencyRoleV1 /
  source ordinal / derived dependency class

StaticForbiddenDependencyRoleV1 =
  FunctionPort | MutableLocal | LetBinding | LoopBinding |
  RuntimeCallState | ExternalMutableRead | OutputOrInoutTarget |
  RuntimeEffect | LaterStaticBinding

VerifiedConstantFunctionSpecializationRow
  template / verified generic environment
  canonical input and return types
  derived local layout / derived flat small-step program
```

For a definition, the constant projection covers the complete shared source
body exactly once in canonical syntax order. Every projected block/item/
statement/target/expression row stores its exact source-row inverse and every
source row has exactly one projected admitted-or-unsupported row. Owner, scope,
source coordinate, operand order, and function port/default references must
agree both ways. A prototype has no projection. Thus `RawConstBlockId` is a
restricted view of one `RawBlockId`; it is never a second parser body that can
omit a runtime-only statement or change a default.

Every block/item/declaration/statement row has one canonical owner, and a
nested block's lexical parent must be the block whose item names it. An omitted
`Var` type is inferred independently from assignment syntax, never from an
analyzer resolved-type row. Canonical syntax traversal finds every assignment
whose resolved base is that declaration and classifies it by this closed state
machine:

```text
InferredVarAssignmentClassV1 =
  DefiningWholeSetAtomic | ConsistencyWholeSetAtomic |
  PreDefinitionNoninferable | PreDefinitionReadOrSelectedTarget |
  PostInferenceOrdinary
```

Only simple `Set` to the whole object with no member/index/part select can be a
defining/consistency candidate. The first such assignment must have an atomic
inferable right side and fixes the type. Any earlier non-atomic whole `Set` is
`PreDefinitionNoninferable`; any read, compound assignment, or selected target
before definition is `PreDefinitionReadOrSelectedTarget`; both reject as
`CONST.FUNCTION_LOCAL` rather than being skipped. Later atomic whole `Set` rows
are consistency candidates and must derive the same complete natural type.
Every other later assignment/read is `PostInferenceOrdinary` and uses ordinary
assignment/operand compatibility against the fixed type; it is not a second
inference candidate.

Atomic forms are a variable/reference or function call, a
sized or widthless based/real/Boolean literal, or parentheses containing one;
a base-less decimal, all-bit, string, operator, cast, concat, aggregate,
conditional, or compound update cannot initiate inference. A string initializer
therefore requires an explicit `string` or fixed packed integral target in this
profile. Absence of a defining candidate, use-before-inference, or a conflicting
consistency candidate is `CONST.FUNCTION_LOCAL`. Omitted `Let`/`Const` types use the same
closed inference relation on their initializer. Proposed resolved types are
comparison witnesses only.

`Var` initializes a mutable slot to the recursively derived source/SystemVerilog
type default on every block entry: zero for Bit/two-state storage, X for
Logic/four-state storage, empty bytes for string, and the corresponding
aggregate default. `Let` evaluates its initializer on every entry to its
declaring block and binds a fresh immutable slot; re-entering a loop body
therefore creates a fresh initialized binding. Assigning an immutable slot
rejects.

A local `Const` is not a VM local. Its initializer is an eager static
dependency and is evaluated exactly once per exact `(raw function template,
verified generic environment)`, independently of caller actual values and even
when its lexical block is not entered by the replayed invocation. It may
depend on that template/environment's generic/type environment, source constants,
lexically visible earlier static constants, and admitted pure user calls;
dependence on an input port, `Var`, `Let`, loop counter, control state, or a
later static binding is `CONST.FUNCTION_CONST_ONLY`. A user-call initializer is
an independent synthetic constant-binding root. The joint demand scheduler may
derive its callee program and replay its own finite certificate before any
instruction loads the binding; it is not hidden inside the enclosing invocation
trace and program construction does not require its value merely to encode a
typed static-binding load. A value initializer creates a persistent `Value`
binding; a type initializer creates a `Type` binding and never occupies a VM
value slot. The independently derived binding is compared with the analyzer
comptime witness.

The same `VerifiedStaticBindingRow` owns parameters, generic constants, enum
bindings, source `Const`, and function-local `Const`. Its dependency range is
direct and canonical rather than an expanded transitive set. Scheduler order
derives the class bottom-up: all direct binding dependencies already have a
class, and any forbidden operation or `ValueOnlyStatic` dependency makes the
owner `ValueOnlyStatic`. A type-forming reference requires the checked target
row to be `ElaborativeTypeForming`. This is the retained transitive eligibility
proof without quadratic copied dependency sets.

Procedural case/switch arms remain in
source order; zero or more arms and at most one `Default` are admitted, and a
second retained default is rejected rather than overwritten. Local
`Gen`, concatenation assignment, `if_reset`, diamond assignment, and every
recovery tag are retained but explicitly outside the constant-machine
capability boundary.

Only concrete input ports are admitted. A missing input actual may use only its
exact retained `RawFunctionPortRowV1.default`; that `RawSourceExprId` is typed
under the callee template's verified generic environment and evaluated per
invocation against a read-only view of already staged preceding formals. A
self/later-formal dependency rejects. Explicit actuals execute in source order;
missing defaults then execute in formal declaration order and are never
replaced by an analyzer cached value. A formal-dependent default is a
`RuntimeFragment`, not a template static root, and its current value does not
enter program/specialization identity. A missing nondefault input rejects. Output/inout,
modport/import/ref ports, writes outside the
current frame's locals, reads of mutable external state, timing/event controls,
static mutable locals, random/DPI/SystemVerilog calls, effectful system calls,
and recovery/unknown statements are rejected before execution. Calls must
target another admitted pure function specialization. All local definitions,
uses, assignment targets, loop bindings, and return paths are typed and
coerced independently.

Mutable locals are private to one invocation frame and lexical block-entry
generation. An assignment updates only
that frame after its right side has evaluated successfully. A return value is
assignment-coerced to the verified return type. Falling off a value-returning
function or executing `break` outside its exact loop is invalid. The verifier
never mutates analyzer state or shares a local
store between recursive frames.

A local target is resolved once to `(local slot, verified select path)`. Simple
assignment evaluates the right side first, then evaluates every dynamic target
index/anchor exactly once in retained path order, freezes one checked target
handle, and stores through it. Compound assignment evaluates/fixes those target
components first, reads the selected old value exactly once, evaluates the
right side, applies the corresponding closed operator, assignment-coerces the
result, and stores through the same handle. It never re-evaluates a target
expression for the write. This deterministic profile rule closes an order which
SystemVerilog leaves unspecified; the pinned compound lowering's duplicate
index evaluation is a producer bug. Array updates path-copy the persistent interval tree in
`O(log extent)`; struct/member and packed-bit updates create canonical
persistent replacement/bit-rope nodes and never clone the complete aggregate.
An X/Z target index performs no write, while a statically known out-of-bounds
target is rejected by the same select verifier as a read.

A pure helper with no return type may fall off its final block and returns an
internal `Unit` control result; `Unit` is not a constant proof value and cannot
appear where an expression value is required. A value-returning specialization
must execute an exact typed return. The VM's closed call result is
`Value(VerifiedConstValueId) | Unit`; either may be memoized, but only `Value`
may be pushed for an expression call. A statement call discards either result.

The loop binding has the source-defined signed 32-bit two-state `i32` type.
Range expressions are evaluated once, in source order, on loop entry; the
initial binding is assignment-coerced to `i32`, and updates use 32-bit wrapping
assignment semantics. `Single(a)` runs once with `i = a` in either direction,
then completes through a one-shot state without applying a counter update. An
explicit singleton step is still typed and evaluated once on entry but is not
executed.
For forward `a..b`, the initial value is `a` and the entry test is `i < b`;
`a..=b` uses `i <= b`. The default update is `i = i + 1`. For `rev a..b`, the
empty case `b <= a` exits without forming `b - 1`; otherwise the initial value
is `b - 1` and the test is `i >= a`. `rev a..=b` starts at `b`, tests
`i >= a`, and is empty when `b < a`. The default reverse update is
`i = i - 1`.

This range-object rule follows the source half-open/closed interval meaning.
The pinned emitter currently evaluates a reverse-exclusive initializer before
the empty test and re-evaluates the end expression in the generated condition;
it also turns a singleton into an inclusive counter loop which can repeat after
wrap or a nonprogressing update. The pinned analyzer instead represents an
operator-less range as empty `a..a`. These mismatches must be fixed rather than
copied into the verifier.

An explicit `step op= expression` replaces only the default update. V0_20
requires it to be a completed typed constant proof, evaluated once on loop
entry and reused. The admitted exact tags are `+=`, `-=`, `*=`, `/=`, `%=`,
`&=`, `|=`, `^=`, `<<=`, `>>=`, `<<<=`, and `>>>=`. Each update reads the
current counter, applies the corresponding integral rule, and assignment-
coerces/wraps the result to `i32`. The step need not be positive or host-sized;
zero/nonprogress and wrapping are ordinary transitions because the body may
later `break`. Termination is established only by the finite concrete trace.
Range values are never converted into a host range and there is no iteration
limit.

`break` transfers to the continuation of the innermost active loop in the
current frame. `return` discards that frame's remaining loop/control
continuations and returns only to its caller. Both targets are derived from the
verified control stack and must match the trace label; a producer cannot name
an outer loop or caller continuation directly.

## Independently derived small-step program

The verifier deterministically lowers every demanded constant proof root to a
synthetic `VerifiedConstantExecutionRoot` program and every reachable function
specialization to a flat subprogram. The synthetic program evaluates the whole
root, not merely its first callee: `f() + g()` and `$clog2(f())` therefore have
one root execution containing nested/sibling calls in source evaluation order.
These programs are internal derived state, not producer IR.

The flat program schema is closed:

```text
VerifiedConstProgramPointRow
  owner / ProgramOpV1 / ProgramControlV1
  exact input and output VerifiedConstType ranges
  exact frame-local/temp/target/call-buffer effects
  RawProgramPointLabel

ProgramOpV1 =
  PushLiteral | PushBoundSymbol | LoadBinding | LoadLocal | LoadStagedFormal |
  CloneTop | Drop | SaveTemp | LoadTemp | ReleaseTemp |
  ApplyCoercion | ApplyUnary | ApplyBinary | ApplyConditionalMerge |
  ApplyPattern | BuildConcat | ApplySelect |
  ApplyPureSystem |
  InvokeProof | YieldProof |
  BeginDecision | LoadDecisionSelector | DeferDecisionMerge | FinishDecision |
  BeginAggregate | StageAggregateValue | TestArrayDefault |
  FinishAggregate |
  PrepareTarget | LoadTarget | StoreTarget |
  BeginCallActuals | StageActual |
  EnterBlock | LeaveBlock | InitVar | BindLet |
  PrepareLoop | EvalLoopCondition | ApplyLoopUpdate | CompleteSingleton |
  ReleaseLoop |
  PushTruth | Noop | Branch | Jump | Call | ReturnValue | ReturnUnit

ProgramControlV1 =
  Linear(next point) |
  Jump(target point) |
  Branch2 { false point, true point } |
  Branch3 { false point, true point, unknown point } |
  EvalCall { child proof entry, continuation point } |
  EvalReturn |
  Call { continuation point, PushValue | DiscardResult } |
  Return |
  RootComplete
```

The algebraic payload and control pairing is normative; “ID” below always
means the independently checked full-width ID in the named derived table:

| `ProgramOpV1` | Exact payload | Only admitted control / private state effect |
| --- | --- | --- |
| `PushLiteral` | owner proof, retained literal ID, natural result type | `Linear`/root epilogue; pushes the reparsed value |
| `PushBoundSymbol` | owner proof, raw bound-symbol row, verified selected dimension and `Msb | Lsb` rule, natural result type | `Linear`/root epilogue; pushes the independently derived bound value |
| `LoadBinding` | owner proof, verified static binding ID, exact type | `Linear`; clones one persistent value ref |
| `LoadLocal` | owner proof, local slot ID, required block generation, exact type | `Linear`; clones the initialized slot ref |
| `LoadStagedFormal` | declared-default proof, call-buffer ID, earlier formal ordinal and exact materialized type | `Linear`; clones only an already staged preceding formal from that invocation |
| `CloneTop`, `Drop` | expected exact top type | `Linear`; stack effect only |
| `SaveTemp`, `LoadTemp`, `ReleaseTemp` | derived temp ID, exact type, definition/use ordinal | `Linear`; move, clone, or release that one temp ref |
| `ApplyCoercion` | checked coercion ID, source/final type, exact context/role | `Linear`; replaces the top ref |
| `ApplyUnary` | closed unary tag, operand/result types | `Linear`; replaces one ref |
| `ApplyBinary` | closed binary tag, left/right/result types | `Linear`; consumes two refs left-before-right |
| `ApplyConditionalMerge` | condition and equal arm types, closed merge rule | `Linear`; consumes condition/then/else |
| `ApplyPattern` | pattern ID/tag, ordered explicit operand descriptor, result type | `Linear`; no implicit evaluation/coercion |
| `BuildConcat` | concat ID, ordered value/count descriptor, exact result type/width | `Linear`; consumes the staged stack suffix and builds a rope |
| `ApplySelect` | select ID, ordered base/index/bound descriptor, checked lane plan, result type | `Linear`; no implicit child evaluation |
| `ApplyPureSystem` | closed admitted system tag, ordered explicit argument descriptor, result type | `Linear`; no opaque callback |
| `InvokeProof` | invocation-site ID, child proof ID and natural result type | `EvalCall`; pushes exact caller stack base/resume point and enters the unique child entry |
| `YieldProof` | current proof ID and natural result type | `EvalReturn`; requires one child-segment value and resumes the stored site |
| `BeginDecision` | decision ID, `Case(selector type) \| Switch`, result type | `Linear`; case consumes/stores one selector, switch consumes none |
| `LoadDecisionSelector` | case decision ID and exact selector type | `Linear`; clones the selector ref already stored by `BeginDecision` |
| `DeferDecisionMerge` | decision ID, arm ordinal, guard/result types | `Linear`; moves already computed refs into its continuation |
| `FinishDecision` | decision ID, exact result type and closed reverse-merge plan | `Linear`; consumes the completed continuation and pushes one result |
| `BeginAggregate` | constructor ID, `Array(expected shape) \| Struct(expected type)` | `Linear`; creates one empty builder |
| `StageAggregateValue` | builder ID, item/member/default/count role and ordinal, exact type | `Linear`; moves one top ref into the builder |
| `TestArrayDefault` | builder ID, outer extent, optional default proof ID | `Linear`; validates count and pushes the known fill-zero/positive condition for the following `Branch` |
| `FinishAggregate` | builder ID, canonical interval/member plan, exact result type | `Linear`; consumes builder and pushes one value |
| `PrepareTarget` | assignment ID, base slot, ordered selector descriptor, selected type | `Linear`; consumes explicit selector refs and creates one handle |
| `LoadTarget` | assignment ID, same target-handle ID and exact selected type | `Linear`; clones selected old value without reevaluating selectors |
| `StoreTarget` | assignment ID, same handle ID, materialized selected type | `Linear`; consumes value/handle and performs one store |
| `BeginCallActuals` | call-site ID, exact resolved signature and empty formal bitmap | `Linear`; creates one call buffer before evaluating any explicit actual |
| `StageActual` | call-site ID, `ExplicitArgument(source ordinal) \| DeclaredDefault(function-port ID)`, resolved formal ordinal/type | `Linear`; moves one materialized value into one empty formal cell |
| `EnterBlock`, `LeaveBlock` | block ID, parent/generation, exact local/temp range | `Linear`; initializes or releases the derived lexical state |
| `InitVar`, `BindLet` | declaration ID, local slot, generation, exact type/default or initializer role | `Linear`; initializes exactly once in that generation |
| `PrepareLoop` | loop ID, closed range form, exact `[singleton_i32] \| [start_i32, end_i32]` plus optional coerced-step descriptor, counter/step rules | `Linear`; consumes those entry operands in source order and creates loop state |
| `EvalLoopCondition` | loop ID, header/one-shot rule | `Linear`; pushes one known width-one condition |
| `ApplyLoopUpdate`, `CompleteSingleton`, `ReleaseLoop` | loop ID and exact update/completion/release rule | `Linear`; mutates or releases only that loop state |
| `PushTruth` | exact known width-one Bit value | `Linear`; pushes that value |
| `Noop` | synthetic-join owner and role | `Linear`/root epilogue; no state effect |
| `Branch` | branch-site ID, `IfReduction \| Truth3`, exact condition type | corresponding `Branch2` or `Branch3`; consumes condition |
| `Jump` | edge role and cleanup/backedge/join owner | `Jump`; no expression/state effect |
| `Call` | user-call site/signature, exact staged formal range, result role/type | `Call`; consumes the complete buffer and enters pending memo lookup |
| `ReturnValue`, `ReturnUnit` | return site, exact declared return type or Unit | `Return`; enters `PendingPromotion` |

For a nonroot `PushLiteral` through `FinishAggregate`, `Linear` eventually
reaches that fragment's unique `YieldProof`; the root epilogue is the only
ordinary operation allowed to use `RootComplete`. `Branch2/3` therefore pair
with `Branch` (not a nonexistent same-named op). `ProgramControlV1` stores only
checked point IDs derived after all fragments and call sites are enumerated.
Every table row above has a single source owner and no variant permits an extra
payload field or alternate state transition.

Every op payload is the exact checked row it names: proof/binding/local/temp ID,
closed operator/coercion/pattern/system tag, aggregate operand-descriptor range,
select/target descriptor, block/loop/call site, or return type. There is no
opaque callback or “evaluate this AST” instruction. `RootComplete` is a
successor sentinel, not an addressable point; arriving at it requires exactly
one final root value. `Branch2/3` occurs only with `Branch`; `Call`, `Return`,
and `Jump` occur only with their same-named op. Every other op has `Linear` or,
for the root epilogue, `RootComplete`. `EvalCall` occurs only with
`InvokeProof`, and `EvalReturn` only
with `YieldProof`. They are expression-proof control, not user-function calls.

The value-stack effects are exact. Each notation describes the affected top
suffix; an unmentioned lower prefix is preserved. A fresh proof-evaluation
segment has no such prefix of its own:

| Ops | Stack effect |
| --- | --- |
| `PushLiteral`, `PushBoundSymbol`, `LoadBinding`, `LoadLocal`, `LoadStagedFormal`, `LoadTemp`, `LoadDecisionSelector`, `PushTruth` | `[] -> [V]` of the derived exact type |
| `CloneTop` | `[V] -> [V, V]`, incrementing the value use count |
| `Drop`, `SaveTemp`, `StageActual`, `BindLet`, `StoreTarget` | `[V] -> []` |
| `ReleaseTemp`, `EnterBlock`, `LeaveBlock`, `InitVar`, `ApplyLoopUpdate`, `CompleteSingleton`, `ReleaseLoop`, `Noop`, `Jump`, `BeginAggregate`, `BeginCallActuals` | `[] -> []` |
| `ApplyCoercion`, `ApplyUnary`, `ApplySelect` with no dynamic operand | `[V] -> [V]` |
| `ApplyBinary` | `[left, right] -> [result]` |
| `ApplyConditionalMerge` | `[condition, then, else] -> [result]` |
| `ApplyPattern`, aggregate builders, dynamic `ApplySelect`, `ApplyPureSystem`, `PrepareTarget` | pop the exact typed operand-descriptor range and push the one declared result, except `PrepareTarget`, which stores one target handle and pushes nothing |
| `BeginDecision`, `DeferDecisionMerge`, `FinishDecision` | move the exact selector/guard/result references between value and decision-continuation storage as declared by their closed payload; `FinishDecision` leaves exactly one result |
| `LoadTarget` | `[] -> [selected old value]` from the already prepared target handle |
| `EvalLoopCondition` | `[] -> [unsigned width-one evaluation value]` |
| `Branch` | `[width-one condition] -> []` and chooses only the derived truth edge |
| `Call` | consumes the complete staged-actual buffer; a memo hit or later return commit produces one `Value` only for `PushValue` |
| `InvokeProof` | preserves the caller segment, pushes one evaluation continuation, and enters the named child proof with a fresh empty value-stack segment |
| `YieldProof` | requires exactly one child value, pops the evaluation continuation, restores the caller segment, and appends that value there |
| `StageAggregateValue` | `[V] -> []`, moving the typed value/count/default result into the exact current aggregate-builder role |
| `TestArrayDefault` | `[] -> [unsigned width-one known value]`; it first validates the completed explicit count, then is one only when a present default has nonzero fill |
| `FinishAggregate` | `[] -> [array or struct value]`, consuming and closing the one current aggregate builder without expanding uniform intervals |
| `PrepareLoop` | pops the exact singleton-or-start/end operands and optional explicit-step operand declared by its loop descriptor, then initializes loop storage |
| `ReturnValue` | `[materialized return value] -> PendingPromotion` |
| `ReturnUnit` | `[] -> PendingPromotion` |

An aggregate/select/system descriptor records each operand stack position,
role, natural/final type, explicit coercion point, and source ordinal; its arity
is not a bare count trusted from the producer. Program temps,
evaluation/decision continuations, aggregate builders, target handles, and
call-actual buffers are frame-private checked rows with derived
single-definition/use intervals. All incoming edges of a join have identical
stack height, exact types, temp liveness, block generations, target state, and
evaluation/decision/aggregate/loop continuation shape. A child proof entry
always has an empty isolated stack segment and its `YieldProof` always has one
value, so sharing a child between several call sites does not require their
parent stack shapes to match. These facts are verified before replay.

### Complete lowering templates

Each executable exact `ExpectedTypedConstantExpr` owns one program fragment
irrespective of how many control paths evaluate it. A proof retained only for
typing on a source-static root's independently suppressed guarded edge owns no
executable point or `InvokeProof` site. In the following table `L(x, r)` emits
one `InvokeProof` call site for child proof `x` and continuation role `r`; it
does not copy or recursively inline the child's points. `C(r)` emits the one
independently derived natural-to-role/result coercion, `M(r)` emits an actual
assignment/materialization coercion, and `T(r)` is the unique derived frame temp
for role `r`. Every listed sequence is source-left-to-right. Each non-root
fragment ends in one `YieldProof`; the root ends in `RootComplete`. The
implementation uses iterative `Enter`/`Finish` work frames but must produce
exactly this finite shared relation.

A proof fragment yields its natural result. Each invocation site owns the
explicit `C` required by that proof's exact context, and the root owns one
`C(RootFinal)` epilogue before `RootComplete`. When one child is needed on two
control paths, those paths have distinct constant-size `InvokeProof`/`C` call
sites and continuation roles, but both target the same child fragment. Thus no
child subtree is copied and total program points are linear in raw syntax plus
derived control edges.

For `SourceGraphStaticValue` only, the verified activated-guard relation is an
input to this point enumeration. A completed known short-circuit/conditional/
decision control instantiates the corresponding table row as its exact linear
selected sequence and omits every suppressed child call site; a completed X
control instantiates all merge-required sequences in their specified order. The
controller itself is still evaluated exactly once and its independently proved
value is checked by the same operator/decision rule. If the controller is
runtime-dependent, the enclosing expression is not a source-static root and the
ordinary unspecialized template applies in its runtime owner. This specialization
adds no opcode and is forbidden for a function body merely because one caller's
actual happens to be constant.

| Raw form | Required program template |
| --- | --- |
| literal/reference/bound symbol | `PushLiteral`; exactly one `LoadBinding`/`LoadLocal`; or `PushBoundSymbol`. A type-only value/`TypeOf` emits no program. |
| unary / ordinary binary | `L(operand, Operand), C(Operand), ApplyUnary`; or `L(left, Left), C(Left), L(right, Right), C(Right), ApplyBinary`. |
| type/width cast | finish the target type/value dependency first, then `L(value, CastOperand), C(CastOperand)`; only the explicit data-type cast's `C` may materialize. |
| `A && B`, `A \|\| B` | `L(A, Left), C(Left), CloneTop, Branch(Truth3)`. The controlling known edge does `Drop, PushTruth`; the other known and unknown edges use distinct call sites targeting the one shared B fragment, then `C(Right), ApplyBinary`; each ends in one `Jump` to the common join. Thus the retained copy of A participates in the unknown truth table. |
| conditional | `L(condition, Condition), C(Condition), CloneTop, Branch(Truth3)`; true/false edges `Drop` the retained condition and use path-specific call sites targeting the one shared then/else fragment followed by its exact result coercion; the unknown edge keeps the condition, calls those same shared fragments in then-before-else order from its own call sites, applies both exact result coercions, and emits `ApplyConditionalMerge`; all three jump to one exact-type join. |
| concat / repeated concat | for every item emit `L(value, Value), C(Value)` once and then `L(count, Count), C(Count)` when present, followed by one `BuildConcat` over the exact descriptor range. Zero count changes only construction. |
| array constructor | `BeginAggregate`; visit explicit items in source order with `L(value, ItemValue), M(Tail), StageAggregateValue`, followed by `L(count, RepeatCount), C(Count), StageAggregateValue` for a repeat. Emit `TestArrayDefault, Branch(IfReduction)`; the false edge consumes the builder in `FinishAggregate(NoDefaultFill)`, while the true edge invokes the default tail proof, materializes/stages it, and consumes the builder in `FinishAggregate(WithDefault)`. Both exact-type values jump to the fragment's one yield join. Thus no live builder crosses the join and fill zero never evaluates the default. |
| struct constructor | `BeginAggregate`; invoke/materialize/stage explicit fields in source order. For each independently derived missing member in declaration order, invoke that member-context default proof and stage it; no point exists for a nonmissing member's guarded proof. `FinishAggregate` builds declaration order once. |
| select | `L(base, Base), C(Base)`, then every dynamic index/bound left-to-right with its exact coercion, then one `ApplySelect` carrying the verified packed/unpacked and partial-lane plan. |
| pure system call | lower admitted value arguments in source order; type-query arguments emit no value program; finish with one `ApplyPureSystem`. |
| user call | emit `BeginCallActuals`; invoke explicit actuals in retained source order, `M(Formal)`, and `StageActual(ExplicitArgument, formal ordinal)`. Then visit each missing input default in formal declaration order under the callee template's verified generic environment and this invocation's staged-preceding-formal view, materialize it, and emit `StageActual(DeclaredDefault, formal ordinal)`. After the exact buffer is complete emit one `Call`. Named syntax changes mapping but never explicit evaluation order. |
| inside/outside | `L(target, PatternTarget), C(Target), SaveTemp`; invoke/coerce each pattern expression explicitly, then use `LoadTemp, ApplyPattern`, combining by the same ordered logical-short-circuit template; release the temp and apply one explicit final logical-not only for outside. |
| case/switch expression | Case begins `L(selector, Selector), C(Selector), BeginDecision`; switch begins `BeginDecision` with no selector. Every case pattern starts with `LoadDecisionSelector` and then explicitly invokes/coerces only that pattern's other operands before `ApplyPattern`; switch conditions have no selector load. The ordered short-circuit template uses `Branch(Truth3)`. True invokes the shared arm-result proof and `C(Result)` before finishing; false advances; unknown invokes that same result/coercion, records `DeferDecisionMerge`, and advances. Default is invoked with `C(Result)` once on each path which reaches it; `FinishDecision` unwinds deferred merges in reverse. No child or suffix program is duplicated. |
| block / local declaration | `EnterBlock`; each `Var` emits `InitVar`, each `Let` emits `L(initializer, LocalInitializer), M(Local), BindLet`, and static `Const` emits no runtime point; normal exit emits `LeaveBlock`. |
| simple assignment | `L(RHS, AssignmentRhs)` first; then invoke/coerce every dynamic target selector once, `PrepareTarget`, `M(Target)`, `StoreTarget`. |
| compound assignment | invoke/coerce target selectors once, `PrepareTarget, LoadTarget`, then `L(RHS, Right), C(Right), ApplyBinary, M(Target), StoreTarget` using the same handle; `LoadTarget` is already in the operator's exact left type. |
| procedural `if` | `L(condition, Condition), C(Condition), Branch(IfReduction)`; only known true selects then, while false/unknown select the next clause/else; each nonterminating arm jumps through its canonical cleanup edge to the one join. |
| procedural case/switch | invoke/coerce and store a case selector once; explicitly invoke/coerce patterns/conditions in source order. `Branch(IfReduction)` selects a body only for known true and sends false/unknown to the next test; default/fallthrough joins once. |
| `for` | For a singleton emit `L(value, LoopSingleton), M(CounterI32)`; for a bounded form emit `L(start, LoopStart), M(CounterI32), L(end, LoopEnd), M(CounterI32)`. An explicit step then emits `L(step, LoopStep), C(StepOperatorOperand)`; a default step adds no value. `PrepareLoop` consumes exactly that suffix once. Each iteration emits `EvalLoopCondition, Branch(IfReduction)`, body, `ApplyLoopUpdate` for bounded forms or `CompleteSingleton`, and one `Jump` back to the header. Header-false, completed-singleton, and post-update range-exit edges enter `PreReleaseExit`, execute exactly one `ReleaseLoop`, and reach `PostReleaseExit`. |
| `break` / return | `break` enters the derived cleanup chain described below; return enters the analogous all-frame-block cleanup after `L(value, Return), M(Return)`, then emits `ReturnValue`. A void fallthrough emits the unique synthetic `ReturnUnit`. |
| statement call | use the user-call template with `DiscardResult`; an ordinary expression statement emits its expression then `Drop`. |

Every `break` and return owns one canonical allocation-free cleanup chain.
Starting at the innermost active lexical block, it emits `ReleaseTemp` for each
derived statement/control temp in reverse definition order, then `LeaveBlock`;
on crossing a loop owner it emits `ReleaseLoop`. `break` stops after releasing
the blocks/state owned by its innermost loop and jumps directly to that loop's
`PostReleaseExit`, never its `PreReleaseExit` or normal `ReleaseLoop` point.
Return continues through every active block/loop in the function and then
reaches `ReturnValue | ReturnUnit`. A return value, when present, is the
preserved lower stack prefix throughout this chain. No target handle,
aggregate builder, staged call buffer, or expression/decision continuation may
be live at a statement abrupt edge. Normal exits use the same suffix of cleanup
points, so the final join has identical block generations, locals, temps, and
loop state. Cleanup points use the abrupt statement as lineage anchor plus the
crossed block/loop ordinal; they are not anonymous `Jump`s.

`InvokeProof` executes no child operation itself: it isolates the caller stack,
records an exact continuation, and transfers to the already existing child
fragment. `YieldProof` only restores that continuation. Therefore a nested
unknown conditional/case produces linear program storage in syntax points even
though one finite execution may visit several arms. `BeginDecision`/`DeferDecisionMerge` do not evaluate expressions: they only
retain already computed references. `FinishDecision` applies the closed merge
to those references without calls or allocation. Empty bodies and joins use a
single `Noop` only when there is no real point to own the required edge. No
template may hide a user call, selector evaluation, coercion, assignment, or
loop transition inside another instruction. Operand descriptors name stack
roles and verify the already explicit `C`/`M` points; they never perform an
additional implicit coercion.

### Closed raw lineage

```text
SyntaxRootRoleV1 =
  TypeExtent(type-use SyntaxOccurrenceKeyV1, Unpacked | Packed, ordinal) |
  ConstInitializer(declaration SyntaxOccurrenceKeyV1) |
  ParameterInitializer(declaration SyntaxOccurrenceKeyV1) |
  GenericConstActual(generic-use SyntaxOccurrenceKeyV1, formal ordinal) |
  GenericConstDefault(formal SyntaxOccurrenceKeyV1) |
  EnumVariantRecipe(variant SyntaxOccurrenceKeyV1) |
  CastWidth(cast SyntaxOccurrenceKeyV1) |
  ConcatRepeat(part-owner SyntaxOccurrenceKeyV1, part ordinal) |
  SelectGeometry(select-owner SyntaxOccurrenceKeyV1, select ordinal,
                 ColonLower | ColonUpper | IndexedWidth) |
  SourceGraphStaticValue |
  StaticLocalInitializer(local SyntaxOccurrenceKeyV1)

SyntaxExpressionContextV1 =
  SelfDetermined |
  AssignmentTo(type-use SyntaxOccurrenceKeyV1, RawOperandRoleV1) |
  ExplicitCastTo(cast SyntaxOccurrenceKeyV1) |
  CommonOperand(owner SyntaxOccurrenceKeyV1, RawContextOperationV1) |
  Condition(owner SyntaxOccurrenceKeyV1, expression | statement) |
  TypeOnly(owner SyntaxOccurrenceKeyV1, SyntaxContextOwnerKindV1) |
  LosslessTo(type-use SyntaxOccurrenceKeyV1,
             owner SyntaxOccurrenceKeyV1, SyntaxContextOwnerKindV1)

SyntaxContextOwnerKindV1 =
  Root | ExpressionOperand | StatementOperand | Local | FunctionPort

SyntaxExecutionOwnerV1 =
  SourceAggregate |
  FunctionTemplate(template SyntaxOccurrenceKeyV1)

SyntaxProofRoleV1 =
  Root(SyntaxRootRoleV1) |
  ExprOperand(owner SyntaxOccurrenceKeyV1, RawOperandRoleV1, source ordinal) |
  StatementOperand(owner SyntaxOccurrenceKeyV1, RawOperandRoleV1,
                   source ordinal) |
  ConstructorMember(owner SyntaxOccurrenceKeyV1, member/item ordinal,
                    Value | Count | Default) |
  FunctionPort(owner SyntaxOccurrenceKeyV1, formal ordinal,
               ExplicitActual | DeclaredDefault | Return) |
  Local(owner SyntaxOccurrenceKeyV1, Initializer | Read) |
  Loop(owner SyntaxOccurrenceKeyV1,
       Singleton | Start | End | Step | Condition | Update)

SyntaxProofLineageKeyV1
  SyntaxExecutionOwnerV1 / expression SyntaxOccurrenceKeyV1 /
  raw environment-lineage row / SyntaxProofRoleV1 /
  SyntaxExpressionContextV1

RawRootLabel
  SyntaxProofLineageKeyV1 whose role is Root

RawProgramOwnerLabel =
  Root(RawRootLabel) |
  Function(template SyntaxOccurrenceKeyV1, raw environment-lineage row)

RawLineageAnchor =
  Proof(SyntaxProofLineageKeyV1) | Statement(SyntaxOccurrenceKeyV1) |
  Block(SyntaxOccurrenceKeyV1) | Local(SyntaxOccurrenceKeyV1) |
  For(SyntaxOccurrenceKeyV1) | Function(SyntaxOccurrenceKeyV1) |
  Root(RawRootLabel)

RawOperandRoleV1 =
  UnaryOperand | BinaryLeft | BinaryRight |
  LogicalLeft | LogicalRight | CastValue | Condition |
  ConditionalThen | ConditionalElse |
  ConcatValue | ConcatCount |
  ArrayValue | ArrayCount | ArrayDefault |
  StructField | StructDefault |
  SelectBase | SelectIndex | SelectLower | SelectUpperOrWidth |
  PatternTarget | PatternValue | PatternLower | PatternUpper |
  CaseSelector | CasePattern | SwitchCondition |
  DecisionResult | DecisionDefault |
  SystemArgument | CallActual | CallDeclaredDefault | LocalInitializer |
  AssignmentRhs | AssignmentTargetIndex | LoopSingleton |
  LoopStart | LoopEnd | LoopStep | ReturnValue

EvaluationPathV1 =
  Direct | ShortCircuitKnown | ShortCircuitUnknown |
  ConditionalKnownTrue | ConditionalKnownFalse |
  ConditionalUnknownThen | ConditionalUnknownElse |
  DecisionKnown | DecisionUnknown | DecisionDefault |
  ProceduralControl | LoopEntry

ProgramRoleV1 =
  RootExpression | ImplicitUnitReturn |
  InvokeProof(RawOperandRoleV1, EvaluationPathV1) |
  ResumeProof(RawOperandRoleV1, EvaluationPathV1) |
  ExprFinish(closed RawConstExprTagV1) |
  Coercion(RawOperandRoleV1, EvaluationPathV1) |
  Guard(ShortCircuit | Conditional | Decision, Prepare | Test | Join) |
  Aggregate(Begin | Value | Count | DefaultTest | Default | Finish) |
  Select(Base | Index | Bound | Apply) |
  Pattern(SelectorLoad | Test | Combine) |
  SystemArgument | SystemApply |
  Call(Begin | StageExplicit | StageDefault | LoadDefaultFormal |
       Invoke | Resume) |
  Block(Enter | Leave) | Local(Initialize | Bind | Load) |
  Assignment(PrepareTarget | LoadOld | Rhs | Apply | Store) |
  Loop(Prepare | Header | Body | Update | Backedge |
       PreReleaseExit | Release | PostReleaseExit) |
  Cleanup(Break | Return, Block | Loop | Temp) |
  Return(Value | Unit) | SyntheticJoin

RawProgramPointLabel
  RawProgramOwnerLabel / RawLineageAnchor / ProgramRoleV1 /
  checked role-local ordinal
```

The role-local ordinal is zero unless the same anchor/role owns an ordered
operand, arm, pattern, field, selector, actual, cleanup owner, or loop edge;
then it is that retained source/declaration/nesting ordinal. The closed
`EvaluationPathV1` distinguishes the constant-size call/coercion sites which
target one shared child fragment. Root entry/accept use `RawRootLabel`, not forged
program points. An implicit unit return anchors the function's root block and
uses `ImplicitUnitReturn`; an empty/synthetic join anchors its owning expression
or statement and uses `SyntheticJoin`. Every point therefore has one stable
raw-lineage tuple. Lowering assigns dense checked point IDs by canonical
program-owner order, raw owner traversal, role tag, and ordinal. A certificate
proposes only these syntax-lineage labels and never a verified proof, specialization,
program, temp, or point ID.

The lowering itself is iterative and its output relation is checked
bidirectionally:

- every typed expression/statement role owns exactly the required program
  points and edges;
- every program point has exactly one owner;
- successors, local slots, stack effects, call sites, and return continuations
  are independently recomputed; and
- unreachable syntax remains type checked but owns no executable point unless
  reachable under the structural control graph.

The VM uses explicit fallibly reserved frame, local, expression-value, and
continuation stacks. Program and stack indices are checked full-width IDs; no
field is packed into 24 bits. The host call stack is never used.

VM-only integral/string/aggregate values live in a separate pre-reserved
ephemeral slot/node arena. Stack entries, locals, and frame results hold checked
references with verifier-owned use counts. Popping a stack value, replacing a
local, leaving a lexical block, or popping a frame releases the old reference;
unreachable path-copy nodes are returned to a deterministic LIFO free list at
the end of that transition. A later transition may reuse those slots. No
superseded local version is retained merely because the aggregate is
persistent. Only an evaluated proof output or a completed memo argument/result
is promoted by exact content into the persistent arena. Promotion finishes
before the ephemeral source is released.

## Finite execution certificate

A producer supplies only a finite sequence of labels witnessing the concrete
small-step path. It supplies no value, branch truth, arithmetic result, local
state, or next program instruction.

```text
RawInvocationCertificateRow
  RawRootLabel / exact RawTraceStepRange { start: u64, len: u64 }

RawTraceStep =
  EnterRoot { new-frame serial, RawRootLabel } |
  Linear { current-frame serial, RawProgramPointLabel } |
  Jump { current-frame serial, point: RawProgramPointLabel,
         successor: RawProgramPointLabel } |
  Branch { current-frame serial, point: RawProgramPointLabel,
           successor: RawProgramPointLabel } |
  InvokeProof { current-frame serial,
                invoke_point: RawProgramPointLabel,
                child_entry: RawProgramPointLabel } |
  YieldProof { current-frame serial,
               yield_point: RawProgramPointLabel,
               resume_point: RawProgramPointLabel } |
  Call { current-frame serial, new-frame serial,
         call_point: RawProgramPointLabel,
         callee_use: SyntaxOccurrenceKeyV1 } |
  MemoCall { current-frame serial,
             call_point: RawProgramPointLabel,
             callee_use: SyntaxOccurrenceKeyV1 } |
  Return { current-frame serial,
           return_point: RawProgramPointLabel, Value | Unit } |
  Accept { completed-root-frame serial, RawRootLabel }
```

The verifier maps the raw root occurrence/environment/role/context to the one
expected proof, derives its synthetic program, resolves each callee use and
generic environment independently, and only then compares labels. A raw
certificate cannot name an `ExpectedTypedConstantExprId`, a verified
specialization, or verified argument proof. Exactly one certificate is consumed
for each root whose actually demanded path executes at least one user-call
program point, whether that point is a memo miss (`Call`) or hit (`MemoCall`). A
certificate for a root with only dead/skipped calls, a duplicate certificate,
or a demanded call point without a certificate is rejected. Nested and sibling
calls stay in that same root trace; they do not acquire child certificates.

Certificate necessity is discovered without pre-executing a second semantic
machine. If the canonical raw root key has a certificate, replay starts in
labelled mode and sets `executed_user_call` on either `Call` or `MemoCall`; an
otherwise successful `Accept` with the flag false reports the extra/dead
certificate. If no certificate exists, the same derived synthetic VM runs once
in unlabelled probe mode. It uses identical semantic/demand waves but stops at
the first actually reached user-call point—before lookup or call execution—and
reports the missing certificate. Reaching pure root completion with no such
point accepts and promotes the result. Probe mode manufactures no labels,
values, or alternate fallback semantics.

Trace rows are fixed-width, canonical, and may be streamed from the verified
raw range. They need not be copied into a `Vec`. One VM transition consumes
exactly one row:

1. `EnterRoot` is valid only in the empty initial state and pushes synthetic
   root frame zero for the independently derived whole-root program.
2. `Linear` executes the uniquely derived non-control instruction at the named
   current point and advances to its unique successor.
3. `InvokeProof` verifies the derived child entry, pushes the evaluation
   continuation, and enters the child's fresh stack segment. `YieldProof`
   requires the unique child exit and one natural result, then verifies the
   stored resume point and restores the caller segment. Neither creates a user
   function frame or performs a coercion.
4. `Jump` executes no expression, requires the proposed successor to equal the
   uniquely derived unconditional edge, and transfers to it. Joins, `break`,
   and unconditional loop backedges use this row.
5. `Branch` consumes the already computed width-one condition from the value
   stack and requires the proposed successor to equal the derived edge. It
   evaluates no expression. Loop entries, exits, and backedges are ordinary
   typed control edges with retained loop lineage; only a conditional edge is
   a `Branch`.
6. On a verifier-derived memo miss, `Call` consumes already evaluated and
   coerced actuals in mapped port order, records the exact continuation, and
   pushes one fresh frame for the independently derived specialization.
   Argument expressions, including nested calls, have preceding transitions.
7. On a verifier-derived hit, `MemoCall` consumes the same ready actuals,
   verifies the exact key, pushes or discards the cached `Value | Unit` as the
   call-site role requires, advances to the continuation, and creates no frame.
8. `Return(Value)` consumes an already evaluated and return-coerced value;
   `Return(Unit)` is valid only at the derived void fallthrough/return point.
   After semantic and label checks it consumes that trace row and enters
   `PendingPromotion` with the current frame, original actuals, result, and
   continuation still intact. Return expressions and nested calls are preceding
   transitions, never hidden in the return row. The internal promotion waves
   consume no trace row; only their infallible commit records the memo result,
   pops/releases exactly that frame, and resumes the saved caller.
9. Completion of the synthetic root program enters `Completed` with its final
   typed value ready. `Accept` verifies the root type/context and clean stacks,
   consumes the last trace row, and enters `PendingPromotion`; it publishes no
   value yet. Its internal promotion commit publishes the computed value into
   the private expected-root relation and enters `Accepted`. It compares no
   producer value; analyzer/cache value comparison happens only in the later
   witness/output waves.

Frame serials are canonical ordinals: the root is zero and each actual
`Call` miss receives the next checked ordinal. A `MemoCall` creates no frame.
The verifier recomputes and compares every serial. The ordinal is bounded only
by actual trace rows and checked host/ID representability; it is not a 24-bit
field and has no policy maximum.

Every row first checks the current certificate owner, frame, program point,
and the coarse row family admitted at that point; it then executes the semantic
transition and finally compares the producer's labels. The `CallFamily`
exception is refined by the pending lookup protocol below before the row is
consumed. A missing row while nonterminal rejects. Any row after
`Accept` rejects. An `Accept` before synthetic-root completion rejects. Success leaves no
frame, continuation, pending expression, or unread row.

A terminating recursive call or billion-iteration loop has a finite valid
certificate, with one row per actual transition. A nonterminating execution
has no finite accepted certificate: every finite prefix ends in a nonterminal
state. There is no recursion-depth, loop-count, branchification, trace-length,
or wall-clock policy cap. Host representability and fallible storage remain
ordinary resource checks. User cancellation may stop verification externally,
but it never manufactures a value or valid artifact.

## Canonical completed-call memoization

Completed pure calls are memoized by exact content:

```text
MemoKey
  semantics version / function specialization
  canonical typed argument values by content

MemoEntry
  exact MemoKey / result: Value(VerifiedConstValueId) | Unit
```

The ordered fallible memo index stores only completed calls. An in-progress
recursive call cannot hit itself. At a call point, the verifier independently
derives whether the key is present. A hit requires one `MemoCall` row and
reuses the completed `Value | Unit` in one transition; a miss requires `Call` and
normal frame replay. The producer cannot choose between them. Equal hash or ID
is not content equality.

The aggregate-global memo starts empty exactly once and is not cleared between
canonical roots. A call point first accepts only the coarse `CallFamily` trace
tag and common frame/site/callee fields; it cannot yet require `Call` versus
`MemoCall`. It enters `PendingCallLookup` while retaining that borrowed row and
the completed staged actuals. The already-ready function signature derives one
logical `VerifiedConstantFunctionSpecializationKey` (template, generic environment,
input/return types). A read-only specialization-index lookup happens first. If
the key is absent, no completed memo entry can exist and the result is a miss.
If it is present, reusable traversal/mark scratch is grown by
resumable demand microsteps: each `prepare` inspects one current value node and
reserves at most the next traversal row, child cursor, or mark row; its commit
advances exactly that node/edge without allocation. A child-first comparison
with the persistent content index marks each argument `Existing(id) | Absent`.
Any `Absent` argument proves a miss because every completed memo key retains all
argument content; otherwise the canonical persistent ID sequence is looked up
in the memo index by the same one-node-at-a-time protocol. This read-only
canonicalization performs no promotion.

Only after lookup completes does the verifier require the retained row's exact
variant. A hit must be `MemoCall` and can execute after its common/label checks.
A miss must be `Call`. If its logical specialization was absent, the suspended
pending-call state pushes `FunctionSpecialization`, every required static-
binding root, and `Program` tasks onto the aggregate scheduler. Those tasks may
consume their own independent root certificates but never the retained caller
row; after they are `Ready`, their completed key must equal the originally
derived logical key. A following wave then reserves the fresh frame, original-
actual range, local cells, and continuation before the `Call` transition
consumes the retained row. An existing specialization skips those tasks.

The exact diagnostic order is coarse call-family/site checks, read-only
specialization/content/memo lookup and its scratch resources, exact
`Call | MemoCall` check, missing-specialization/static/program task semantics
and resources, frame resources, transition, then proposed labels. Thus a wrong
`MemoCall` for an absent specialization is rejected before attempting to build
that specialization. Probe mode
still stops before creating `PendingCallLookup`, because it has no certificate
row to classify.

On a miss the new frame retains an immutable `original_actual_refs` range,
separate from assignable port/local slots, with tagged ephemeral/persistent
references and verifier-owned use counts. It survives until return even if a
port is overwritten. `Return(Value | Unit)` first executes and validates its
trace row into a private `PendingPromotion` state without popping the frame.
`Accept` uses the same state for a proof result. Promotion traversal, mark, and
overlay-Patricia storage use the same resumable one-node/one-edge demand
microsteps as call lookup. No step first asks for storage proportional to an
untraversed graph. Each allocation-free commit walks child-first against both
the existing persistent index and the current batch overlay, eventually marking
each node `Existing(id)` or `New(predicted dense id)` and incrementally counting
only genuinely new value, limb, byte, array, rope, and index rows.

Only after that exact delta is known does a promotion-commit wave grow the
persistent arenas, memo key/entry/index arenas, and output references. Its
commit materializes the marked new nodes, inserts the completed memo entry,
pops/releases the frame, and resumes the caller without allocation. `Unit`
still promotes the original arguments needed by its memo key but has no result
value. Duplicate content in one batch or later calls maps to `Existing`; N
equal W-bit results therefore retain N memo rows plus one W-bit content object,
not N copies or an N-by-W worst-case reservation. A failure drops the private
pending state. The verifier never disables memoization, reruns a call, or
accepts a differently shaped trace.

## Failure contract

Typed-constant helpers and the VM return exactly one closed allocation-free
error. The joint source scheduler embeds it losslessly as described below:

```text
TypedConstantErrorV1
  rule: TypedConstantRuleIdV1
  owner: None |
         RawRef(TypedConstantRawTableKindV1, u64) |
         VerifiedRef(TypedConstantVerifiedRefKindV1, u64) |
         DependencyEdge(u64) |
         FunctionSpecialization(u64) |
         CertificateRoot(expected constant-root u64) |
         CertificateRow(raw invocation-certificate row u64) |
         TraceStep(trace u64, step u64) |
         ResourceDemand(TypedConstantResourceDemandIdV1) |
         ReservationSite(TypedConstantResourceSiteIdV1)
  context: None |
           ExpectedActual(u64, u64) |
           Pair(u64, u64) |
           Range(start u64, length u64, bound u64) |
           Widths(source u64, target u64) |
           Tag(u64) |
           Operation(ConstOperationV1,
                     ConstOperationOwnerRoleV1) |
           Capacity(elements u64, element_size u64)

TypedConstantPhaseV1 =
  Raw | Dependency | Typing | Coercion | Arithmetic |
  Function | Trace | Resource | Aggregate

TypedConstantVerifiedRefKindV1 =
  NameResolution | TypeUseInstance | NormalizedType |
  GenericEnvironment |
  ConstBinding | ConstExprProof | ConstantRoot |
  EnumVariantReplay | EnumFinalize |
  FunctionSignature | StaticBinding |
  Program | ProgramPoint | Coercion | ConstType | ConstValue |
  SourceGraphStaticValue | OutputWitness

ConstOperationOwnerRoleV1 =
  Whole | Operand(RawOperandRoleV1)

PrivateRawSyntaxTableKindV1 = the authoritative complete closed enum in
  source-semantic-inputs.md
PrivateRawSyntaxPoolKindV1 = the authoritative complete closed enum in
  source-semantic-inputs.md

TypedConstantRawTableKindV1 =
  SyntaxRow(PrivateRawSyntaxTableKindV1) |
  SyntaxPoolEntry(PrivateRawSyntaxPoolKindV1) |
  EnvironmentLineage |
  AnalyzerResolutionWitness | ResolvedTypeWitness |
  ExtentResolutionWitness | EnumResolutionWitness |
  EnumVariantValueWitness | GenericEnvironmentWitness |
  GenericBindingWitness | ConstantExprWitness |
  ProposedCoercion | ProposedDependency | ProposedValueNode |
  ProposedArrayExtent |
  ProposedStringByte | ProposedArrayInterval | ProposedStructMemberValue |
  InvocationCertificate | TraceStep |
  ArbitraryBits | ArbitraryByte | DecodedStringByte

ConstOperationV1 =
  RawDecode | ResolveName | BindActual | InferLocal | Literal |
  Coercion | Unary(RawUnaryOp) | Binary(RawBinaryOp) |
  Conditional | Concat | ArrayConstructor | StructConstructor | Select |
  Pattern | Decision | PureSystem(RawSystemFunction) | UserCall |
  Assignment(RawAssignmentOperator) | Loop(SourceForStepAssignmentOp) |
  Program | MemoLookup | Promotion | WitnessCompare

TypedConstantResourceWaveKindV1 =
  TaskDiscover | TaskFinalize | VmExecute |
  PromotionScratch | PromotionCommit | WitnessOutput

TypedConstantSemanticsV1 = CeloxSourceV0_20

TypedConstantResourceDemandIdV1
  semantics: TypedConstantSemanticsV1
  wave_serial: checked full-width u64
  wave_kind: TypedConstantResourceWaveKindV1
  resource_kind: TypedConstantResourceKindV1

TypedConstantResourceSiteIdV1
  demand: TypedConstantResourceDemandIdV1
  grow_ordinal_within_wave: checked u32

ProgramDescriptorKindV1 =
  Operand | Aggregate | Select | Pattern | Decision |
  Target | Call | Block | Loop | Cleanup

TypedConstantResourceKindV1 =
  DecodedStringValueBytes |
  ResolutionRows | ResolutionIndexNodes | DependencyEdgeRows |
  SchedulerTaskRows | SchedulerStackRows |
  ProofRows | ProofProvenanceRows | OperandRows |
  NormalizedTypeRows | ConstTypeRows | ExtentRows | ShapeSegmentRows |
  TypeMemberRows | EnumVariantRows | GenericEnvironmentRows |
  GenericBindingRows | CoercionRows |
  ConstructorIntervalRows | StructMemberValueRefs |
  StaticDependencyRows | SourceStaticEligibilityRows |
  SpecializationRows | StaticBindingRows | ProgramLocalRows |
  ProgramTempRows | ProgramPointRows | ProgramTypeRefs |
  ProgramDescriptorRows(ProgramDescriptorKindV1) | ProgramStateEffectRows |
  ProgramEdgeRows | ProgramInvokeSiteRows | LineageRows |
  PersistentValueRows | PersistentBitsRows | PersistentBitPlaneWords |
  PersistentStringRows | PersistentStringBytes |
  PersistentArrayNodes | PersistentRopeNodes | PersistentContentIndexNodes |
  MemoKeyRefs | MemoEntryRows | MemoIndexNodes |
  VmFrameRows | VmOriginalActualRefs | VmLocalRefs | VmTempRefs |
  VmValueStackRefs |
  VmEvalContinuationRows | VmDecisionContinuationRows |
  VmDecisionValueRefs | VmAggregateBuilderRows | VmAggregateValueRefs |
  VmTargetHandleRows | VmTargetSelectorRefs | VmStagedActualRows |
  VmLoopStateRows | VmBlockGenerationRows | VmCallLookupRows |
  EphemeralValueRows | EphemeralBitsRows | EphemeralBitPlaneWords |
  EphemeralStringRows | EphemeralStringBytes |
  EphemeralArrayNodes | EphemeralRopeNodes |
  ArithmeticScratchWords | TraversalScratchRows |
  PromotionMarkRows | PromotionOverlayNodes |
  WitnessMapRows | SourceProjectionRows | OutputRows

ProgramDescriptorKindV1::ALL =
  [Operand, Aggregate, Select, Pattern, Decision,
   Target, Call, Block, Loop, Cleanup]

TypedConstantResourceKindV1::ALL = the const-expanded flat array obtained by
  visiting `TypedConstantResourceKindV1` in written order and replacing
  `ProgramDescriptorRows(ProgramDescriptorKindV1)` at its position with one
  fully applied value for every `ProgramDescriptorKindV1::ALL` entry
TypedConstantResourceKindV1::COUNT =
  TypedConstantResourceKindV1::ALL.len()
Count = checked full-width u64 logical occupancy/delta, never a host `usize`
TypedConstantResourcePlanV1 =
  [Count; TypedConstantResourceKindV1::COUNT]
TypedConstantResourceLayoutV1 =
  the owned tuple whose ordinal-i field is the exclusive physical typed arena
  or reusable scratch buffer named by TypedConstantResourceKindV1::ALL[i]
```

The outer source preparation owns the one authoritative
`PrivateRawSemanticSyntaxV1`, including every private raw syntax row/pool,
magnitude row/byte arena, syntax-flattener traversal arena, raw-owner bitmap,
and raw-reference/order scratch arena. The typed verifier receives checked
borrowed views of those resources after their outer topology pass. It may use
`TypedConstantRawTableKindV1::SyntaxRow`, `SyntaxPoolEntry`, `ArbitraryBits`, and
`ArbitraryByte` to locate a diagnostic, but it cannot allocate, grow, clear,
retag, or include the corresponding physical arena in
`TypedConstantResourceKindV1::ALL`. Failure to construct or structurally verify
one of those outer resources occurs before the typed verifier and remains a
`SourceAggregateErrorV1::SourceLocal` resource/topology error.

The payload enums above are part of the closed typed-resource discriminant, not
open runtime labels. `TypedConstantResourceKindV1::ALL` is a stored/const-
generated array of fully applied physical resource values, not an iterator over
outer tags or an unexpanded payload variant. `COUNT` and every plan length are
derived from that exact array at compile time. Its ordinal order is outer
variant order and then written `ProgramDescriptorKindV1` order. Each expanded
value names exactly one typed-owned physical arena or reusable scratch buffer,
and every typed-owned physical arena has exactly one value. No outer-owned
resource and no two typed values may name the same allocation. Adding,
splitting, merging, or reordering an arena changes this layout version and its
const-exhaustive fixtures.

The easily confused mappings are normative:

| Logical storage | Exact resource kind |
| --- | --- |
| independently decoded string-value bytes before persistent promotion | `DecodedStringValueBytes` |
| verified normalized `usize` extents in `VerifiedExtentArena` | `ExtentRows`; the borrowed raw `RawExtentRow` remains outer-owned |
| source-static class/control cursor for one raw executable expression instance | `SourceStaticEligibilityRows` |
| persistent constant-VM `VerifiedBitsRow` and its payload/mask words | `PersistentBitsRows` / `PersistentBitPlaneWords` |
| ephemeral constant-VM `VerifiedBitsRow` and its payload/mask words | `EphemeralBitsRows` / `EphemeralBitPlaneWords` |
| independently derived constant-proof coercion rows | `CoercionRows` |
| program point input/output type-range entries | `ProgramTypeRefs` |
| one closed program payload descriptor family | `ProgramDescriptorRows(its ProgramDescriptorKindV1)` |
| frame-local/temp/target/call-buffer effect summaries attached to points | `ProgramStateEffectRows` |
| one derived program temporary definition/use row | `ProgramTempRows` |
| one live frame's checked temporary-value reference | `VmTempRefs`, whose row is exactly `VmTempRefRow` below |
| completed source-static root to exact outer source typed-value ID relation | `SourceProjectionRows` |

`DecodedStringValueBytes` is distinct from both outer-owned token/spelling and
raw-magnitude bytes and from persistent/ephemeral constant-value string bytes.
A decoded range cannot be charged to a borrowed magnitude arena or silently
allocated by the host `String` implementation.

`VerifiedTypedValueArena<SourcePhase>`, its source-phase bits/word arenas, and
`VerifiedPhaseCoercionArena<SourcePhase>` are owned and reserved only by the
outer source session. They are not aliases of `PersistentValueRows`,
`PersistentBitsRows`, or `CoercionRows`, which belong to the constant proof/VM.
`SourceProjectionRows` owns only the checked relation from a completed constant
root and its exact projected content to the outer session's canonical
`VerifiedSourceTypedValueId`; it owns no source-phase value, bit, word, or
coercion payload and cannot grow any source-phase arena.

`TypedConstantErrorV1::phase()` is derived from a total immutable
`RULE_META_V1[rule]`; phase is not independently stored or selectable. All
constructors are private rule-specific check-site constructors:

```text
RuleMetaV1
  phase: TypedConstantPhaseV1
  allowed owner variant bitset
  allowed context variant bitset
  precedence band: BorrowedRawSemantic | DemandSemantic | Representability |
                   Resource | Execute | ProducerCompare | FinalRelation

FailureCheckSiteV1
  stable check-site enum / public TypedConstantRuleIdV1 /
  fixed intra-band ordinal / owner extractor / context extractor
```

`TypedConstantRuleIdV1::ALL`, `RULE_META_V1`, and
`FailureCheckSiteV1::ALL` are const-exhaustive and tested bijectively: every
rule has exactly one metadata row and every rejecting branch names one check
site. A check site is internal program position, not a second public reason.
Unknown raw tags use `context: Tag(raw)` because they cannot be forged into a
closed `ConstOperationV1`. For a checked operation, `rule` remains the sole
failure reason and `Operation` only locates its whole/operand role.

The VM resource rows are not one open continuation union:

```text
VmEvalContinuationRow
  frame / invocation site / caller stack base / exact resume point /
  expected child proof and natural type
VmTempRefRow
  frame / derived temp ID / defining block generation / exact typed value ref
VmDecisionContinuationRow
  frame / decision ID / selector ref if case / current arm and deferred-merge cursor
VmAggregateBuilderRow
  frame / constructor ID / closed Array | Struct state /
  exact staged-value-ref range and count/member cursors
VmTargetHandleRow
  frame / assignment ID / base local / once-computed selector refs /
  selected access and block generation
VmStagedActualRow
  frame / call-buffer ID / call site / formal ordinal /
  ExplicitArgument(source ordinal) | DeclaredDefault(function-port ID) /
  exact materialized value ref
VmLoopStateRow
  frame / loop ID / range form / counter/bounds/step refs / one-shot state
VmBlockGenerationRow
  frame / block ID / generation / initialized-local and owned-temp ranges
VmCallLookupRow
  frame / trace row ordinal / call site / actual cursor / traversal cursor /
  Existing | Absent marks / unresolved | Hit(memo ID) | Miss
```

Every row belongs to exactly one live frame, except that the synthetic root
frame owns its rows until `Accept`. Each range is an exact disjoint range in the
corresponding reference arena; release order is target/aggregate/decision/eval
continuations, loop, block, locals, original actuals, then frame. The prepared
stack-effect/control table determines the maximum rows added by one microstep,
so no transition hides a `Vec`, map, or secondary allocator.

No field contains a `String`, `Vec`, source excerpt, arbitrary-width value, or
allocator message. Invalid raw numbers remain raw references and are never
forged into checked IDs. `Display` formatting and optional source-coordinate
lookup are lazy and outside the error value.

The enclosing source verifier uses the allocation-free sum variant
`SourceAggregateErrorV1::TypedConstant(TypedConstantErrorV1)`. It preserves every
field exactly; it does not format, stringify, remap, or collapse this error.

Stable rule IDs are reserved permanently:

| Category | Stable IDs |
| --- | --- |
| Raw | `CONST.RAW_TAG`, `CONST.RAW_REFERENCE`, `CONST.RAW_RANGE`, `CONST.RAW_PARTITION`, `CONST.RAW_OWNER`, `CONST.RAW_ORDER`, `CONST.RAW_ENCODING`, `CONST.RAW_TRACE_LAYOUT` |
| Dependency | `CONST.DEP_EDGE`, `CONST.DEP_COMPLETE`, `CONST.DEP_ROOT`, `CONST.DEP_ORDER`, `CONST.DEP_CYCLE`, `CONST.DEP_FORWARD_ENUM` |
| Typing | `CONST.TYPE_KIND`, `CONST.TYPE_ARITY`, `CONST.TYPE_OPERAND`, `CONST.TYPE_CONTEXT`, `CONST.TYPE_RESULT`, `CONST.TYPE_LITERAL`, `CONST.TYPE_SYSTEM_CALL`, `CONST.TYPE_ENUM_REPLAY` |
| Coercion | `CONST.COERCE_ROLE`, `CONST.COERCE_WIDTH`, `CONST.COERCE_SIGN`, `CONST.COERCE_DOMAIN`, `CONST.COERCE_XZ`, `CONST.COERCE_LOSSLESS`, `CONST.COERCE_POSITIVE`, `CONST.COERCE_RESULT` |
| Arithmetic | `CONST.ARITH_PRECONDITION`, `CONST.ARITH_SHIFT`, `CONST.ARITH_EXPONENT`, `CONST.ARITH_RESULT`, `CONST.ARITH_EXTENT` |
| Function | `CONST.FUNCTION_SPECIALIZATION`, `CONST.FUNCTION_SIGNATURE`, `CONST.FUNCTION_BINDING`, `CONST.FUNCTION_LOCAL`, `CONST.FUNCTION_CONTROL`, `CONST.FUNCTION_EFFECT`, `CONST.FUNCTION_RETURN`, `CONST.FUNCTION_CONST_ONLY` |
| Trace | `CONST.TRACE_OWNER`, `CONST.TRACE_ORDER`, `CONST.TRACE_ENTRY`, `CONST.TRACE_STATE`, `CONST.TRACE_TRANSITION`, `CONST.TRACE_INVOKE`, `CONST.TRACE_YIELD`, `CONST.TRACE_JUMP`, `CONST.TRACE_BRANCH`, `CONST.TRACE_CALL`, `CONST.TRACE_RETURN`, `CONST.TRACE_LOOP`, `CONST.TRACE_FRAME`, `CONST.TRACE_FINAL`, `CONST.TRACE_TRAILING` |
| Resource | `CONST.RESOURCE_COUNT_REPRESENTABLE`, `CONST.RESOURCE_PLAN_REPRESENTABLE`, `CONST.RESOURCE_ID_EXHAUSTED`, `CONST.RESOURCE_STORAGE_AVAILABLE` |
| Aggregate | `CONST.AGGREGATE_ORPHAN`, `CONST.AGGREGATE_OUTPUT`, `CONST.AGGREGATE_WITNESS` |

The V1 check-site registry fixes the otherwise easy-to-confuse mappings:

| Failure condition | Required public rule |
| --- | --- |
| unresolved/ambiguous/wrong-namespace/invisible/path-exhausted name or import cycle | `CONST.TYPE_CONTEXT` |
| raw call argument count/role malformed before resolution | `CONST.TYPE_ARITY` |
| mixed, missing, extra, duplicate, or unknown named actual after signature resolution | `CONST.FUNCTION_BINDING` |
| cast target name/kind mismatch | `CONST.TYPE_CONTEXT` |
| cast width nonintegral, X/Z, nonpositive, or ID/host-unrepresentable | `CONST.COERCE_WIDTH`, `CONST.COERCE_XZ`, `CONST.COERCE_POSITIVE`, or `CONST.RESOURCE_COUNT_REPRESENTABLE`, in that order |
| array count under/overfill, or struct member/default completeness | `CONST.ARITH_EXTENT`, `CONST.TYPE_ARITY`, or `CONST.TYPE_CONTEXT`, respectively |
| select kind/member mismatch; static bound/part geometry invalid | `CONST.TYPE_OPERAND`/`CONST.TYPE_CONTEXT`; then `CONST.ARITH_EXTENT` |
| local inference absent/noninferable/use-before/conflicting, or immutable assignment | `CONST.FUNCTION_LOCAL` |
| admitted-function capability/effect violation | `CONST.FUNCTION_EFFECT` |
| program point/edge/lowering mismatch or join stack/temp/local/target/continuation mismatch | `CONST.FUNCTION_CONTROL` |
| required certificate absent at the first executed user-call point | `CONST.TRACE_OWNER` with `CertificateRoot` |
| duplicate certificate for one root | `CONST.TRACE_ORDER` with the later `CertificateRow` |
| supplied certificate whose accepted path executes no user call | `CONST.TRACE_FINAL` with `CertificateRow` |
| memo hit/miss trace variant mismatch after lookup | `CONST.TRACE_CALL` with `TraceStep` |
| selected source-static root absent from the producer; extra, duplicate, or nonmaximal producer root; value/type/coercion/trace disagreement | `CONST.AGGREGATE_OUTPUT`, `CONST.AGGREGATE_ORPHAN`, then `CONST.AGGREGATE_WITNESS`, respectively |
| expected row missing/extra/duplicate/orphan; analyzer/cache field mismatch | `CONST.AGGREGATE_OUTPUT`, `CONST.AGGREGATE_ORPHAN`, then `CONST.AGGREGATE_WITNESS` |

Each table row expands to concrete `FailureCheckSiteV1` variants in the stated
left-to-right order. `CONST.ARITH_EXPONENT` is reserved but
`constructible_in_CeloxSourceV0_20 == false`: X/Z and negative exponent cases
have defined values above. X/Z arithmetic, divide/remainder by zero, X/Z or
oversized shift count, runtime invalid/X select, and zero/nonprogress/wrapping
loop steps likewise create values/transitions rather than errors. A future
semantics may activate a reserved rule only by changing the metadata version
and fixtures.

The category table also fixes metadata shapes: Raw rules use phase `Raw` and a
`RawRef` owner (global missing-table checks alone use `None`); Dependency uses
`DependencyEdge | VerifiedRef`; Typing/Coercion/Arithmetic use the innermost
`VerifiedRef` and `ExpectedActual | Widths | Operation | Range`; Function uses
`FunctionSpecialization | VerifiedRef` and `ExpectedActual | Operation`.
Trace-owner/order/final checks use `CertificateRoot | CertificateRow` until a
row is being consumed, after which transition/call/return/trailing checks use
`TraceStep`; their contexts are `ExpectedActual | Pair | Operation`. Resource
count, plan-representability, and ID-exhaustion checks use `ResourceDemand`,
while allocation failure alone uses `ReservationSite`; both use `Capacity`.
Those two owner variants losslessly retain the typed-specific IDs above:
`ResourceDemand` stores the exact semantics tag, checked full-width wave serial,
wave kind, and fully expanded typed resource kind; `ReservationSite` stores that
entire demand plus the checked grow ordinal within the wave. Neither is replaced
with an outer source-resource ID, a physical pointer, or a truncated generic
site number.
Aggregate uses the expected `VerifiedRef` or raw witness `RawRef` and
`ExpectedActual | Pair`. A rule-specific metadata row may narrow this set but
never widen it. Before a raw number becomes checked, its owner remains
`RawRef`; after conversion the same check site may not report it as raw. These
rules make `rule -> phase/allowed payload` a total function.

`TypedConstantRuleIdV1` is the sole failure-reason namespace; an error never
adds a second independently chosen “reason” classification. `Operation`
identifies only the closed operation and operand/owner role needed to locate
that rule's failed precondition. IDs are never renamed or reused after
publication.

## Deterministic error precedence

There is no whole-pipeline preallocation pass and no false D-before-E-before-F
ordering. The outer source session completes and owns raw framing/topology
before invoking this verifier. Typed verification has an allocation-free scan
of those borrowed checked views, borrowed-raw semantic checks, one joint
semantic scheduler, and final witness/output waves. The joint scheduler owns
one explicit stack of closed tasks:

```text
DemandTask =
  ResolveName | TypeUse | GenericEnvironment |
  ConstProofType | ConstProofValue | ConstBinding |
  EnumVariantReplay | EnumFinalize |
  FunctionSignature | FunctionSpecialization | StaticBinding | Program |
  VmProbe | VmCertified

DemandTaskKey =
  ResolveName(raw name occurrence, verified generic environment) |
  TypeUse(raw type use, verified generic environment) |
  GenericEnvironment(raw generic use, optional verified parent environment) |
  ConstProofType(ExpectedTypedConstantExprKey) |
  ConstProofValue(ExpectedTypedConstantExprKey) |
  ConstBinding(raw declaration, verified generic environment) |
  EnumVariantReplay(raw enum, verified environment, declaration ordinal) |
  EnumFinalize(raw enum, verified environment) |
  FunctionSignature(raw template, verified environment) |
  FunctionSpecialization(raw template, verified environment,
                         canonical input/return type key) |
  StaticBinding(raw function template, verified generic environment,
                raw declaration) |
  Program(Root(ExpectedTypedConstantExprKey) |
          Function(VerifiedConstantFunctionSpecializationId)) |
  VmProbe(ExpectedTypedConstantExprKey) |
  VmCertified(ExpectedTypedConstantExprKey, raw certificate occurrence)
```

The joint scheduler is owned by the source aggregate, but every task in this
closed `DemandTask` sum—including `ResolveName`, `TypeUse`, generic/enum tasks,
and constant/VM tasks—reports semantic failure through
`SourceAggregateErrorV1::TypedConstant(TypedConstantErrorV1)`. That nested error
is therefore the error of the joint typed-source/type-constant relation, not
only arithmetic execution. `SourceLocal` is used before the scheduler for
nonsemantic framing/source-proposal topology and after it for source
object/control/node/provenance relations. No scheduler microstep can choose
between two outer variants, so a type prerequisite reached from a constant
task has the same canonical error path as the same prerequisite reached from a
source object. The aggregate adds no competing check after a nested failure.

Tasks are created in canonical root order and, within a task, closed dependency
role then retained source ordinal. Each exact task identity has
`Unseen | Visiting | Ready` state. A type task may push a value task for an
extent; a value task may push a type task; enum finalization waits for its
variant tasks; and a concrete call miss creates the specialization/program task
for its independently derived argument environment. Program construction may
suspend on a type or static binding and resume without discarding work. Thus
type, value, specialization, program, and finite replay form one dependency
machine rather than pretending to be separable passes.

The key variants above are the only sharing/cycle identities. Function input
values belong only to `MemoKey`; none may form a program type. Equal signatures
share one program while argument differences replay or hit distinct memo
entries. Dense verified IDs are assigned only after the key's raw/environment
parts are checked and never occur in producer certificate keys.

Type-prerequisite and eager-value edges mark their target `Visiting` before it
is pushed. The first active backedge in this canonical traversal is the cycle
diagnostic. A guarded value task is not created until its verified control
state activates that exact edge; its expression is nevertheless fully type
checked. A dead short-circuit operand, conditional arm, repeat contribution
whose language rule is genuinely guarded, or dead call therefore reserves its
required proof/type rows but no value, VM, or promotion content. Function
invocation frames are execution state, not eager dependency edges, so finite
recursion is governed by its certificate rather than rejected as a static call
graph cycle.

Every typed task, VM, promotion, and witness/output action is a deterministic
demand wave:

1. `prepare` validates all preconditions observable from existing checked
   state and builds one
   `TypedConstantResourcePlanV1 =
   [Count; TypedConstantResourceKindV1::COUNT]` without allocation or mutation;
2. checked count/ID representability is tested, then required arenas grow in
   `TypedConstantResourceKindV1` ordinal order; and
3. `commit` performs the task microstep using only reserved storage. It may
   return a semantic error, but calls no allocator and publishes nothing.

The wave serial is the ordinal of the prepared semantic microstep, including
zero-growth waves, in the canonical traversal. It is independent of allocator
behavior. Outer framing/topology failure occurs before this verifier and is
ordered by the outer source contract, not by a typed-resource wave. Within this
verifier, a borrowed-raw semantic failure precedes the first scheduler root;
roots and their task stacks precede later roots; witness/output work is last.
Within a wave the order is current-state and semantic preconditions, count/ID
checks, resource kinds, allocation-free execution, then producer label
comparison. The first active dependency backedge is reported. This is the
complete diagnostic precedence; the verifier does not search for a preferable
later error or allocate an error collection.

For a trace microstep, owner/frame/program/coarse-row-family checks and the
required workspace plan precede execution. The instruction then executes
without an allocator call, after which its proposed lineage/successor labels
are compared. At a user-call site only, resumable `PendingCallLookup` waves sit
between the coarse `CallFamily` check and exact `Call | MemoCall` variant check;
the miss-frame reserve follows that exact check. Promotion is a separate pending
microstate and waves as specified above, so a Return/Accept transition itself
never hides persistent allocation. Final-state and trailing-row checks are last.

## Canonical reservation and transactional publication

`TypedConstantResourceLayoutV1` is indexed only by the fully expanded
`TypedConstantResourceKindV1::ALL`: every value names exactly one typed-owned
physical flat arena or reusable scratch buffer, and no other allocator-backed
collection exists on the typed proof path. Borrowed outer resources and outer
source-phase canonical arenas are deliberately absent. Free-list links and use
counts are fields of their ephemeral rows. Indexes are the named flat
Patricia/crit-bit arenas, not host hash maps. Adding, splitting, or reordering a
typed resource kind requires a semantics/layout version change.

For every prepared wave and expanded resource kind, the allocation-free pair
`(wave_serial, wave_kind, resource_kind)` defines one
`TypedConstantResourceDemandIdV1` before its count, ID, or capacity arithmetic
is attempted. It is a diagnostic value, not an arena row and requires no
reservation. A count overflow, plan overflow, or dense-ID exhaustion therefore
names that complete demand and creates no fictitious grow site. Only after the
required occupancy is representable and proves `required > capacity` is the
demand refined to one `TypedConstantResourceSiteIdV1`. Its
`grow_ordinal_within_wave` is the checked ordinal of the actual grow call among
all growing resource kinds in that wave, in expanded resource order.
`CONST.RESOURCE_STORAGE_AVAILABLE` alone names that site.

For one kind, let `required` be the checked retained/live occupancy after the
prepared commit, accounting for reusable free slots, and let `capacity` be its
logical capacity. No site exists when `required <= capacity`. Otherwise the one
site for that wave/kind grows to

```text
target = max(required, checked_double(capacity), 1)
checked_double(c) = 2*c when representable, otherwise required
```

The grow operation is fallible and exact with respect to logical capacity.
Allocator rounding may not be used by the verifier, alter the site stream, or
become addressable storage. Arenas never shrink during one verification.
Consequently each kind grows `O(log peak)` times and its logical capacity is
less than twice its nonzero high-water occupancy. Ephemeral value/node arenas
and arithmetic/traversal/promotion scratch reuse their high-water storage; the
persistent arenas grow only for independently demanded proof outputs and
content proved new by the promotion prepass. No all-trace worst-case
reservation, retry, smaller representation, memo disable, or fallback
execution is permitted.

For each compact successful resource-layout fixture, the baseline records its
canonical stream of `R` logical grow sites. Injected `fail-at-N` is run for
every `0 <= N < R` and must return
`CONST.RESOURCE_STORAGE_AVAILABLE` with that exact
`TypedConstantResourceSiteIdV1`;
`fail-at-R` is the successful control. Repeating the same input yields the same
site stream and error fields. Scale fixtures sample every resource kind without
multiplying a million-row run by `R`. Count/ID overflow creates no grow site for
that wave. An earlier semantic error retains precedence; an injected failure
precedes only execution of its own wave and all later work.

All typed scheduler, output-relation, mapping, value, program, and memo tables
remain private until the full aggregate and witness comparison succeeds. The
borrowed outer raw topology remains owned by the outer session throughout. A
later failure may leave mutated private VM/work state, which is simply dropped;
no rollback of unobservable private rows is required. Externally visible owners,
brands, lengths, mappings, and output arenas remain unchanged. Commit is an
infallible move of the prepared owner and its unbranded compact parts, after
which checked phase projections are exposed. Failure cannot expose a partial
artifact.

## Required adversarial and scale fixtures

Correctness fixtures cover every truth-table row and context boundary,
including:

- width-one logical-operand rejection, controlling-value short circuit,
  skipped errors/calls, procedural-if X-as-false, and conditional X merge;
- 1-, 64-, 65-, and million-bit literals/operations, signed/unsigned common
  coercion, Logic-to-Bit conversion, division by zero, X/Z, large/negative
  exponents, and shift counts wider than a host word;
- ordinary/wildcard equality with known mismatch plus X/Z, range
  endpoints, case/switch first-match versus expression merge, and
  inside/outside unknown propagation;
- `$bits`/`$size` across scalar, packed, unpacked, enum, struct, and union
  shapes, query values crossing the signed-32 boundary, unavailable size,
  `$clog2` X/zero/wide inputs, and `$onehot` mixed known/X/Z inputs;
- huge/zero repeats, exact fill, underfill, overfill, duplicate default,
  nested tail-shape repeat, and default with zero remaining elements;
- struct duplicate/unknown/missing members, member-context default typing,
  source order versus declaration order, rejected union construction, and
  union packed-bit reinterpretation;
- X/Z and known out-of-bounds selects, wrong-direction/range selects, and
  persistent-array lookup without expansion;
- distinct generic environments sharing a template, default actuals, exact
  content identity, aggregate actuals, dead guarded cycles, demanded cycles,
  and static type/eager cycles;
- one authoritative `RawFunctionPortRowV1.default` consumed through both the
  constant-VM projection and the
  [source-side runtime-function relation](./source-semantic-inputs.md#runtime-function-and-static-expression-fixtures),
  proving both map the same raw default occurrence while retaining distinct
  program/call IDs and completion evidence;
- two equal-type calls with unequal preceding-formal values and a missing input
  whose default reads that formal, proving the default is evaluated once per
  invocation, produces unequal staged values and memo keys, remains a
  `RuntimeFragment`, and contributes no invocation value to template,
  static-root, specialization, or program identity; self/later-formal default
  reads reject;
- executable-source literals and nested static operators producing one maximal
  `SourceGraphStaticValue`, plus `runtime + (static subtree)` producing exactly
  the subtree root and one expected source-graph `Constant` recipe;
- known short-circuit/conditional/decision controls suppressing a typed runtime
  port/read edge, versus X or runtime controls activating every semantically
  possible edge; suppressed descendants remain value-less runtime fragments;
- the same function template under distinct verified generic environments,
  proving static-root identity is template/environment based, while a
  port/local/`Let`/loop-dependent expression remains runtime-dependent even
  when every observed caller passes equal constants;
- an admitted static user call requiring its exact finite certificate, and a
  caller-specific constant-fold proposal rejected as a later-rewrite concern;
- missing/extra/swapped source-static frontier witnesses and a producer
  `Comptime`/SLT constant classification which disagrees with the independently
  derived frontier; and
- analyzer witnesses with swapped type, sign, mask, value, dependency,
  specialization, or trace data.

Trace fixtures include a million transitions, deep terminating recursion,
alternating call/return frames, memo hits/misses, first/middle/final malformed
rows, wrong branch/frame/callee, missing return, early/extra accept, and finite
prefixes of nontermination. A million-deep expression/constructor/select chain
proves that no host recursion remains. IDs crossing `2^24` prove that no packed
index truncates or aliases.

Every optional typed-owned pool and scratch kind participates in fail-at-N fixtures:
dependency/SCC worklists, contexts/coercions, value/mask limbs,
arithmetic scratch, persistent trees/ropes, specializations, programs, frames,
locals/VM temps, trace state, memo entries, and final output rows. The test
driver iterates the fully expanded `TypedConstantResourceKindV1::ALL`; this
prose list is illustrative and cannot omit a newly added typed-owned arena or
program-descriptor kind. Const-exhaustive resource fixtures additionally prove
that every expanded kind maps to one exclusive physical allocation, every
typed-owned allocation maps back to one kind, and no borrowed raw or outer
source-phase arena appears in `ALL`. A source-projection fixture grows only its
relation rows on the typed side; source value/bits/coercion growth is observed
only in the outer source resource stream. Certificate fixtures separately
require `CertificateRoot` for an absent
certificate, `CertificateRow` for duplicate/extra certificates, and
`TraceStep` only after a row exists. Resource fixtures require
`ResourceDemand` for count/plan/ID failures and `ReservationSite` only for an
injected typed grow failure; both retain the exact typed demand/site ID,
including wave serial and grow ordinal.

Complexity is asserted with deterministic operation counters and peak reserved
storage; wall time and RSS are supplemental only. Required bounds are:

```text
borrowed raw access/resolution:
                             O(referenced raw rows + resolved path components)
source-static frontier:     O(executable expression instances + operand edges +
                              activated guarded edges + static-control limb work)
demand scheduler:           O(demanded task nodes + derived edges + active stack)
expression DAG:             once per exact demanded proof key plus limb work
value content indexing:      O(key atoms + crit-bit branch depth) per promotion
array construction:         O(items + produced midpoint nodes), no extent expansion
concat construction:        O(parts), no repeat expansion
program lowering:            O(program points + program edges)
trace replay:               O(consumed transitions + executed operand/limb work)
ephemeral VM storage:       O(max over transitions of live frames + original actuals +
                              locals + stacks + ephemeral content + instruction/
                              canonicalization/promotion scratch)
persistent memo storage:    O(unique completed calls + unique retained argument/result content)
source-static class storage: O(executable expression instances)
typed published storage:    O(evaluated root content + selected source-static
                              relation rows + canonical index/tree/rope nodes)
```

Memo storage is `O(trace)` in the worst case; it is not hidden inside the live
frame bound. The same is true of distinct demanded proof outputs. Ephemeral
local versions are reclaimed and therefore do not grow with transition count
unless they are promoted into one of those explicitly persistent sets.
Logical reserved capacity for every `TypedConstantResourceKindV1` is less than
twice that kind's nonzero high-water term above; it is never a syntactic all-
branch or all-trace bound. Outer raw and source-phase arena capacities are
accounted only by the outer source resource contract.

A high-fanout DAG with exponentially many paths must still visit each exact
node/edge once. Source-static classification memoizes each exact
`(execution owner, raw occurrence, environment, role, context)` class/control
result;
it does not enumerate control paths or reevaluate one guarded edge for every
ancestor candidate. OneHot/Gray enum replay retains compact index/ordinal cursors.
Celox has never demonstrated these bounds on the required scale fixtures. They
are acceptance requirements for a capability not yet achieved, not a claim
that a formerly fast implementation merely regressed.

## Implementation and connection order

Implementation proceeds only through these complete boundaries:

1. in the outer source session, implement the exact patched parser adapter, its
   exhaustive iterative flattener, raw ownership/reference/order verification,
   and outer resource layout; then expose only checked borrowed raw views to the
   typed verifier and implement its separate
   `TypedConstantResourceLayoutV1`/demand-wave driver;
2. replace the private `BigUint`, nested-vector, string-error phase storage
   with the flat fallible bits/value/string/tree/rope arenas and artifact
   brands;
3. implement the canonical joint demand scheduler, independent resolution,
   environment/dependency tasks, contextual type/coercion tasks, and the
   executable-HIR source-static eligibility/frontier driver before any expected
   source-graph ID allocation;
4. implement and exhaustively test the pure literal/operator/system-function/
   aggregate evaluator and complete the value-sensitive guarded frontier;
5. implement the independently derived function program and streamed finite
   certificate VM, including memo and fail-at-N tests;
6. compare all analyzer/cache/frontier witnesses and finish the bidirectional
   aggregate relation; then
7. connect the already joint enum/type/constant relation and verified
   source-static projections to expected source-node production.

An earlier step may have private tests, but it is not a planner-ready artifact
and cannot be connected as a fallback. The existing private
`source_semantic.rs` checkpoint is migration input for steps 1--3, not an
implementation to polish around missing semantics.

## Primary semantic references

The closed V0_20 rules are based on the
[IEEE 1800-2023 SystemVerilog standard](https://standards.ieee.org/ieee/1800/7743/),
the V0_20 repository's pinned documentation submodule commit for
[numbers](https://github.com/veryl-lang/doc/blob/c3b01bb33092a81df0fa51dd2d49496faa163116/book/src/05_language_reference/02_lexical_structure/02_number.md),
[array literals](https://github.com/veryl-lang/doc/blob/c3b01bb33092a81df0fa51dd2d49496faa163116/book/src/05_language_reference/02_lexical_structure/03_array_literal.md),
[builtin types](https://github.com/veryl-lang/doc/blob/c3b01bb33092a81df0fa51dd2d49496faa163116/book/src/05_language_reference/03_data_type/01_builtin_type.md),
and [formal syntax](https://github.com/veryl-lang/doc/blob/c3b01bb33092a81df0fa51dd2d49496faa163116/book/src/07_appendix/01_formal_syntax.md),
plus the exact Veryl 0.20.1
[parser grammar](https://github.com/veryl-lang/veryl/blob/dfa101b1fd02484ec616f115366e86ee63c39c14/crates/parser/veryl.par),
[analyzer operator implementation](https://github.com/veryl-lang/veryl/blob/dfa101b1fd02484ec616f115366e86ee63c39c14/crates/analyzer/src/ir/op.rs),
and [emitter implementation](https://github.com/veryl-lang/veryl/blob/dfa101b1fd02484ec616f115366e86ee63c39c14/crates/emitter/src/emitter.rs).
Mutable `doc.veryl-lang.org` content is not allowed to change this semantics
tag.
The SystemVerilog committee's
[short-circuit clarification](https://www.accellera.org/images/eda/sv-bc/att-7432/997-draft4-v1.pdf),
[Logic-to-Bit conversion record](https://www.accellera.org/images/eda/sv-bc/1708.html),
and [wildcard-equality record](https://www.accellera.org/images/eda/sv-bc/10477.html)
are retained as primary rationale.

Relevant upstream corrections are the Veryl
[conditional width/context fix](https://github.com/veryl-lang/veryl/commit/e0506d390dbad0d5db62bc6196b64baf0ff3de03),
[mixed-signed wildcard fix](https://github.com/veryl-lang/veryl/commit/bf61865cf8941d51a24f380ab709a41ff36f389d),
[cast-context fix](https://github.com/veryl-lang/veryl/commit/2ca3a95833caa17614ac059bde2cf5e5f6d4c9af),
[negative-power fix](https://github.com/veryl-lang/veryl/commit/85a695179750),
and [dimension-order fix](https://github.com/veryl-lang/veryl/commit/605a1ab6344c0d710ec4c86d11ddd734f1b2a279),
plus the upstream
[direct-union-constructor issue](https://github.com/veryl-lang/veryl/issues/2128)
and [fix](https://github.com/veryl-lang/veryl/pull/2129).
They are evidence for the specified relation, not an instruction to trust any
particular analyzer cache.
