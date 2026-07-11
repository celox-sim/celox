# Decision-region architecture

This document freezes the replacement control, scheduling, and allocation
architecture. The currently implemented SSA allocator and first binary mux
lowering are migration inputs, not assumed-correct or performance-qualified
foundations. This is a phase design, not a list of independent peepholes.
Implementations must preserve this order and must not recover from a failed
proof with a legacy path, retry loop, or size/count cutoff.

## Why the binary transform is not enough

Celox has never completed the pinned Heliodor Linux-boot gate at a competitive
speed. Historical status-zero records were compile-only, and the timings below
are explicitly partial-run diagnostics rather than a fast end-to-end baseline.

On the pinned Heliodor Linux-boot input, source-DAG lowering observes 22,344
muxes. It converts 2,579 to binary CFG, correctly keeps 16,509 whose local
expected cost is unfavorable, and rejects 3,227 profitable candidates only
because a nontrivial expression is shared below the two arm roots. Only one
shared node is currently hoisted. The resulting `eval_comb` still contains a
5,545-instruction straight region, has measured pressure 2,229 before and 2,024
after scheduling, and allocates a 79,216-byte spill frame.

The Veryl AOT comparison evaluates all 31 combinational chunks every tick, so
event-driven execution is not the explanation. The measured GCC output
contains 159 indirect jump-table dispatches, balanced searches, and
branch/cmov hybrids; this is consistent with multiway recovery from the
generated equality chains, though the compiler's internal recognition trace
was not captured. Its 31 functions
also confine each stack frame to at most about 1,464 bytes. Celox therefore
needs explicit control regions, correct placement of shared data nodes, and
verified pressure boundaries. Changing a per-mux threshold cannot provide any
of those properties.

## Fixed phase order

```text
patched pinned parser/tokens, syntax-lineage analyzer witnesses, and the
  untrusted module symbolic-evaluation proposal with
  SourceControlProvenance/SourceRootId
  -> verifier-owned complete private raw HIR
  -> joint verified typed HIR, type/extent/enum relation, and constant proofs
  -> derived TriIntent, ExpectedSourceValueGraph, and InputSemanticFacts
  -> proposal comparison and complete PreparedPhaseSLTNodeFactsV1<SourcePhase>
  -> source-root/action/observer/provenance aggregate verification
  -> simultaneous commit of FrozenSourceArtifact, FrozenSLTNodeArena<SourcePhase>,
     and branded PhaseSLTNodeFactsV1<SourcePhase> (caches dropped)
  -> deterministic whole-unit hierarchy mapping into a temporary draft
  -> atomization, then artifact-global ExternalRootId assignment
  -> constant-rewrite verification
  -> observer-occurrence materialization
  -> checked PhaseSLTNodeFactsV1<OccurrencePhase>, frozen arena, and frozen root/action
     identity/ownership registries as FrozenOccurrenceArtifact
  -> occurrence-valued GlobalActionOrderSkeleton verification
  -> artifact-global control CFG and SSA memory/environment/effect token verification
  -> occurrence-distinct VersionedValueCandidate construction
  -> OccurrenceExecutionSafety verification
  -> topological canonical InstValue resolution and ControlResolutionOverlay verification
  -> GlobalScheduledActionGraph data/token-edge verification
  -> FrozenControlValueArtifact with safety proofs retained and construction caches dropped
  -> ControlEligibilityPlan verification
  -> maximal ControlSkeleton legality verification
  -> ScheduleEarly / ScheduleLate legality envelopes and state-specific use maps
  -> bottom-up gate/decision selection
  -> rejected-region contraction
  -> one final DAG placement
  -> GateFormationPlan and DecisionFormationPlan input verification
  -> SIR construction, runtime-function/invoke formation-output relation,
     gate/decision formation-output relation, and DecisionRegion verification
  -> target-independent SIR optimization and DecisionRegion re-verification
  -> target RuntimeCallLoweringPlanV1 verification
  -> target DecisionLoweringPlan verification
  -> instruction selection with explicit native MInvoke and MDecision
  -> RuntimeCallLoweringOutputRelationV1 verification against the emitted backend artifact
  -> target decision legalization and LoweredDecisionWitness output verification
  -> predicate-aware SLP/store combining
  -> semantic pressure-frontier block splitting and CFG verification
  -> PreScheduleCFGNormalization and machine-constraint markers
  -> per-function pre-schedule dependence graph and virtual-liveness verification
  -> one pressure-aware MIR SchedulePlan per function and output-permutation verification
  -> MIRMemoryTokenAnalysis, CSSA, next-use, and loop analysis
  -> PressureRegion cut selection
  -> cut materialization and verified RegionalAllocationInput/RegionalNextUse
  -> one cut-constrained regional Braun--Hack SpillPlacementPlan
  -> SSA reconstruction
  -> PostMaterializationCFGNormalization and Perm materialization
  -> pressure/home/Perm verification
  -> one FrameLayoutPlan from final home and phi/Perm types
  -> CutResult verification
  -> final phi-congruence classification and affinity coloring
  -> verified ParallelCopyPlan, SSA destruction, and final assignment relation
  -> copy/probability-aware block layout verification
  -> emission
```

The techniques solve different problems:

| Phase | Responsibility | It must not do |
| --- | --- | --- |
| Control provenance | retain source `if`, `case`, and ternary predicates before symbolic merging loses them | decide target profitability |
| DAG placement | place each pure `InstValue` once at the latest legal site inside its execution-safety domain | invent or remove control |
| Gate selection | choose eager dataflow versus control using expected target work | clone a global cache per arm |
| Decision lowering | choose value table, jump table, search tree, ordered chain, or branchless tail | repair invalid or overlapping semantics |
| Runtime-function formation | compile each verified type/environment program once and retain exact invoke/writeback boundaries | recursively inline the call DAG or merge calls by current values |
| MIR scheduling | choose one verified topological instruction permutation that does not worsen the pressure objective | infer or omit dependencies inside the heuristic |
| PressureRegion planning | choose verified full-register cuts, then constrain the one ordinary spill-placement plan | split into functions or retry allocation |
| Component affinity | reduce copies after feasibility is proved | force an unavailable common color |
| Parallel copy | implement every final phi/Perm edge with simultaneous semantics | invent an untracked scratch register or alter edge meaning |
| Block layout | choose fallthroughs after copies are known | change CFG edges or SSA semantics |

These are not alternative allocators or interchangeable optimizations. Their
dependency is:

```text
source meaning
  -> root/action/value-occurrence identity
  -> value-unresolved global control/order skeleton
  -> SSA state/effect tokens
  -> versioned values, resolved action uses, and legal placement envelopes
  -> gate/decision profitability and final placement
  -> canonical SIR control plus shared runtime functions and invokes
  -> verified target call/decision lowering and canonical MIR control
  -> verified pre-schedule dependencies and one pressure-aware permutation per function
  -> rebuilt MIR state/CSSA/next-use analyses
  -> pressure-region cuts, RegionalNextUse, and one regional spill-placement plan
  -> reconstruction/Perm, frame layout, coloring, verified parallel copies, layout
```

Provenance answers *which occurrence executes*. The value-unresolved action
skeleton answers *which operations and control paths occur* without pretending
that their state-dependent values are already known. Token SSA then answers
*which state version each occurrence observes*, after which occurrence uses
can be resolved to structurally versioned `InstValue`s. ScheduleEarly/
ScheduleLate answers *where a pure value may execute once*; Decision formation/
lowering chooses control shape only after those answers are fixed. MIR
scheduling then reorders only within the verified dependency/effect graph to
shorten live ranges and avoid spills. It is the first spill-avoidance
mechanism, not a semantic rewrite: it cannot cross a token, predicate,
fixed-register, or bundle boundary. Pressure cuts and the single constrained
spill-placement plan handle the residual pressure; frame layout, coloring,
and edge copies come only
after feasibility is proved. No later phase repairs an earlier missing proof.

## 1. Source control provenance

Recovering predicates from an arbitrary hash-consed mux DAG is not the primary
algorithm. A shared node may be reached under many syntactic contexts, and
expanding those contexts or repeatedly recomputing their LCA can be nonlinear.
Symbolic evaluation must instead retain the source control tree when it creates
the muxes.

The source and flattened forms are deliberately different artifacts. Symbolic
evaluation cannot name token-dependent `InstValue`s which do not exist yet. It
therefore emits module-local `SourceControlProvenance` in terms of source value
occurrences. Flattening maps that to an occurrence-valued
`ControlOccurrencePlan`; token SSA later resolves every occurrence use to a
final `InstValue` and produces `ControlResolutionOverlay`. A verifier checks each
relation rather than letting one phase mutate the meaning of an ID in place.
An arena may hold several combinational declarations and, after flattening,
several module instances, so each form is a forest of isolated control units
rather than one arena-wide tree:

```text
SourceArtifactId, SourceInstanceId, SourceSemanticObjectId, SourceInputId,
DraftOccurrenceSemanticObjectId, DraftOccurrenceInputId,
OccurrenceSemanticObjectId, OccurrenceInputId,
SourceRootId, SourceControlUnitId,
SourcePredicateRegionId,
SourceControlPointId, SourceControlEdgeId, SourceGateId, SourceDecisionId,
SourceGateResultMergeId, SourceGatedMuxId, SourceDecisionResultMergeId,
SourceValueOccurrenceId, SourceObserverId, SourceObserverOccurrenceId,
SourceControlActionId, SourceDynamicAddressPlanId, SourceForFoldTemplateId,
SourceRuntimeEventSiteId, SourceSyntheticOriginId,
ExpectedSourceTargetHandleId,
SourceWriteDomainId, SourceBindingId, SourceEffectStreamId,
SourceCanonicalProducerId,
ExpectedSourceRuntimeCallInstanceId, RuntimeSourceExecutionLineageId,
VerifiedRuntimeFunctionSpecializationId,
ValueOccurrenceId, RootExpansionId, ActionExpansionId,
ControlUnitId, ExternalRootId, ObserverId,
ObserverOccurrenceId, ControlActionId, GateId, DecisionId, GatedMuxId,
GateResultMergeId, DecisionResultMergeId, PredicateRegionId, ControlPointId,
ControlEdgeId,
GlobalControlPointId, GlobalControlEdgeId, InstValueId, DynamicAddressPlanId,
RuntimeEventSiteId, MemoryTokenId, EnvironmentTokenId, EffectTokenId,
ForFoldTemplateId, WriteDomainId, BindingId, EffectStreamId,
OccurrencePreparedTargetHandleId,
OccurrenceRuntimeCallInstanceId, OccurrenceRuntimeExecutionLineageId,
CanonicalProducerId, SLTMemoryDependencyId, SLTEnvDependencyId:
checked u32 IDs

AtomizationOriginId, GlueOriginId, SyntheticOriginId,
ExecutionSafetyProofId: checked u32 proof IDs
DecisionCaseId, DecisionEdgeId: checked u32 canonical SIR decision IDs
ExpectedSourceUseId, ExpectedSourceResultId: checked IDs in the independently
derived expected source value graph
ExpectedOccurrenceNodeId, ExpectedOccurrenceUseId,
ExpectedOccurrenceResultId, OccurrenceRewriteId: checked proof-graph IDs
VersionedValueCandidateId: checked construction-only u32 ID

PhaseNodeId<SourcePhase>, PhaseNodeId<DraftOccurrencePhase>,
PhaseNodeId<OccurrencePhase>:
checked `usize` indices into their owning phase arena

SourceArmOrdinal: checked u32 ordinal within one source decision
DecisionArmOrdinal: checked u32 ordinal within one occurrence decision

SourceFoldPointId, SourceFoldEdgeId, SourceFoldActionId,
SourceFoldValueOccurrenceId, SourceFoldDynamicAddressPlanId,
SourceFoldPreparedTargetHandleId,
SourceFoldPredicateRegionId, SourceFoldCanonicalProducerId,
ExpectedSourceFoldUseId, ExpectedSourceFoldResultId:
checked u32 IDs scoped by SourceForFoldTemplateId

FoldPointId, FoldEdgeId, FoldActionId, FoldValueOccurrenceId,
FoldDynamicAddressPlanId, FoldPreparedTargetHandleId,
FoldValueCandidateId, FoldInstValueId,
FoldMemoryTokenId, FoldEnvironmentTokenId, FoldEffectTokenId:
checked u32 IDs scoped by ForFoldTemplateId

FoldPredicateRegionId, FoldCanonicalProducerId,
FoldMemoryDependencyId, FoldEnvDependencyId,
ExpectedFoldUseId, ExpectedFoldResultId:
checked u32 IDs scoped by ForFoldTemplateId

VerifiedSourceSemanticContext
  canonical module identity and typed declaration/input rows
  canonical typed executable HIR
  independently derived expected root/action/control-result specifications
  if/ternary/case constructs and exact pattern semantics
  observer definitions and runtime-event sites
  shared type-specialized runtime-function programs, call instances, and
    Root-or-RuntimeCall execution lineages
  ForFold semantic specifications
  explicitly pinned synthetic/ordinary-mux origins

ExpectedSourceValueGraph
  canonical producer rows derived from VerifiedTypedSourceHIR
  canonical use rows: HIR uses plus derived root/action/gate/decision/
    projection/fixed-value uses
  canonical result rows: HIR results plus derived gated/decision-step/
    pinned results
  exact owner/role/site/type/node recipe and ordered producer edges per row
  expected source action/observer/runtime-call/dynamic/ForFold records and
    access summaries

ExpectedSourceControlGraph
  canonical units/regions/points/edges, entry/exit/parent/owner kinds, ordered
    action slots, root membership, and gate/decision topology derived from HIR
    plus the same closed normalization rules
  exact bijection to every SourceControlProvenance unit/region/point/edge row;
    no producer-added empty unit, point, edge subdivision, or unreachable
    structurally valid control row is permitted

`ExpectedSourceValueGraph` has one normative producer independent of symbolic
evaluation. Before assigning any expected ID, the joint typed/constant verifier
computes the controller-first activated-edge relation and maximal
`SourceGraphStaticValue` frontier from raw executable HIR. Starting from that
completed relation and `VerifiedTypedSourceHIR`, an iterative worklist walks
declarations by canonical module/source coordinate, statements in language
order, activated expression operands by operator-defined ordinal, and derived
control/action/result slots by fixed rule ordinal. Every demanded runtime
semantic evaluation position emits an `EvaluateHere` use; one maximal
projectable static frontier root emits one Constant definition and no child
value occurrences; a statically suppressed but well-typed operand emits none.
Unknown/runtime control retains every semantically possible arm. Every other
language/action/control result emits a `Definition`; only a closed language rule
that reuses an already evaluated condition, action result, merge step, loop
binding, or pinned value emits `FixedValue`. Pure CSE and raw-node equality
never select `FixedValue`.
Expected IDs are dense in that traversal order. The builder then derives the
canonical producer relation and dependency DAG from these rows. Producer wire
tables are at most compatibility caches: current decode ignores them, rebuilds
the relation, and compares/discards an old cache before freeze.

SourceControlProvenance
  source inputs / units / regions / points / edges / roots / actions
  gates / gate result merges / decisions
  source_value_occurrences / source_gated_muxes /
  source_decision_result_merges / source canonical producer relation
  source observers / source observer occurrences / runtime sites
  runtime-function programs / call instances / execution lineages
  dynamic address plans / ForFold templates / pinned synthetic origins

SourceValueOccurrence
  semantic_node: PhaseNodeId<SourcePhase>
  site: SourceOccurrenceSite
  ordered_operands: [SourceOccurrenceUse]

SourceOccurrenceSite
  Use { site: SourceControlUseSite,
        semantic_use: ExpectedSourceUseId,
        owner: SourceUseOwner,
        role: SourceOccurrenceUseRoleV1,
        value_source: EvaluateHere |
                      FixedValue(SourceCanonicalProducerId, ValueFlowReason) }
  Definition { site: SourceControlSite,
               semantic_result: ExpectedSourceResultId,
               owner: SourceDefinitionOwner }

SourceUseOwner
  ValueOperand(owner occurrence, operand ordinal) |
  ActionOperand(action, operand ordinal) |
  GateCondition(gate) | GateResultOperand(result merge, condition/then/else) |
  DecisionSelector(decision) |
  DecisionPatternOperand(decision, arm, pattern, operand) |
  DecisionPatternPredicate(decision, arm, pattern) |
  DecisionArmPredicate(decision, arm) |
  DecisionResultOperand(result merge, arm,
                        SourceDecisionResultOperandRoleV1)

SourceOccurrenceUseRoleV1 =
  Value | Address | Predicate | Argument | PreviousValue |
  MergeOperand | Recurrence
SourceDecisionResultOperandRoleV1 = SelectedArm | Default

SourceDefinitionOwner
  ActionResult(action, result ordinal) |
  GatedMuxResult(source gated mux) |
  PinnedSyntheticResult(SourceSyntheticOriginId)

SourceOccurrenceUse / SourceOccurrenceDef
  checked newtype views that require respectively a Use or Definition row

SourceCanonicalProducerRelation
  producer_of: one SourceCanonicalProducerId per source occurrence
  producer_occurrence: one EvaluateHere/Definition occurrence per producer ID
  FixedValue rows name that producer ID directly, never another fixed use
  canonical inverse occurrence lists cover every occurrence exactly once

This relation and its dependency DAG are derived by the aggregate verifier
from the expected rows plus verified flow/operand records. They are not trusted
wire inputs; an old-wire copy is compared as a derived cache and discarded.

ValueFlowReason = DataSource | AddressSource | PreviousValue |
                  ObserverTrigger | MergeArm | LoopCarried

SourceControlSite = (SourceControlPointId, slot: usize)
SourceControlUseSite = Slot(SourceControlSite) | Edge(SourceControlEdgeId)

SourceControlUnit
  root_region: SourcePredicateRegionId
  entry / exit: SourceControlPointId
  roots: [SourceRootId]

SourceControlPoint
  unit: SourceControlUnitId
  region: SourcePredicateRegionId
  kind: SourceControlPointKindV1
  ordered_actions: [SourceControlActionId]
  predecessor / successor edges: [SourceControlEdgeId]

SourceControlPointKindV1 =
  UnitEntry | ArmEntry | RegionExit | GateHeader |
  DecisionHeader | Join | Continuation | Effect

SourceControlEdge
  unit: SourceControlUnitId
  predecessor / successor: SourceControlPointId
  kind: SourceControlEdgeKindV1

SourceControlEdgeKindV1 =
  Ordinary | GateArm | DecisionArm | DecisionDefault | UnitBoundary

SourceRoot
  unit: SourceControlUnitId
  source-order identity
  semantic_specification: SourceRootSemanticSpecificationV1
  ordered_operands: [SourceOccurrenceUse]
  disposition: Scheduled(SourceControlActionId) | MetadataOnly

SourceRootSemanticSpecificationV1 =
  ExecutableUnit(Module | Interface) |
  ProtoDeclaration | ObjectDeclaration | StaticBinding |
  TypeDeclaration | FunctionTemplate | Process | ContinuousAssignment |
  InstanceConnection { declared_port_ordinal: u32 } |
  BindConnection | ModportExposure | RuntimeEvent | RetainedForFold

SourceControlAction
  unit: SourceControlUnitId
  owner: (SourceControlPointId, action_index: usize)
  ordered_operands: [SourceOccurrenceUse]
  results: [SourceOccurrenceDef]
  semantic_accesses: SourceSemanticAccessSummary
  kind: source-valued action kind using only Source-prefixed IDs

SourceControlActionKind =
  ActionSemanticKind<SourceRootId, SourceInputId, SourceBindingId,
                     ExpectedSourceTargetHandleId,
                     SourceDynamicAddressPlanId, SourceObserverId,
                     SourceRuntimeEventSiteId,
                     ExpectedSourceRuntimeCallInstanceId,
                     SourceForFoldTemplateId,
                     SourceControlSite, SourceInputResolution>

ActionSemanticKind<Root, Input, Binding, Target, DynamicPlan, Observer, RuntimeSite,
                   RuntimeCall, ForFoldTemplate, Site, Resolution>
  ReadInput { result_slot, input: Input, resolution: Resolution }
  CaptureValue { result_slot, source_operand }
  BindEnvironment { result_slot, source_operand, binding: Binding }
  EvaluatePinned { result_slot, ordered_operand_slots }
  StoreRoot { root: Root,
              target: StaticTarget(Target) |
                      DynamicTarget(Target, DynamicPlan),
              value_operand, observed_old_operand: optional,
              capture_enable_sites: [Site], triggers: [Root] }
  RuntimeEvent { root: Root, observer: Observer, site: RuntimeSite,
                 predicate_operand, argument_operands,
                 enabled_value_operand: optional,
                 consume-enabled / termination }
  InvokeRuntimeFunction { optional root: Root, instance: RuntimeCall,
                          operand_role_range, result_role_range }
  ForFold { optional root: Root, result_slot, template: ForFoldTemplate }

Every operand/result field above is a checked ordinal into the owning action's
`ordered_operands`/`results` array. The kind contains no second value use or
definition. `SourceControlActionKind`, occurrence `ControlActionKind`, and the
resolved action view instantiate the same variant/slot schema for every
*primary* expansion; mapping may apply only the declared Whole/BitRange slice
and typed-ID substitutions, and resolution changes only array values. A
rootless helper is intentionally a different action and is governed by the
closed helper rule below, not this primary-shape equality.

NestedActionSemanticKind<Input, Binding, Target, DynamicPlan, EffectStream,
                         RuntimeSite, RuntimeCall, Site, Resolution>
  ReadInput { result_slot, input: Input, resolution: Resolution }
  CaptureValue { result_slot, source_operand }
  BindEnvironment { result_slot, source_operand, binding: Binding }
  EvaluatePinned { result_slot, ordered_operand_slots }
  StoreState { target: StaticTarget(Target) |
                       DynamicTarget(Target, DynamicPlan),
               value_operand, observed_old_operand: optional,
               capture_enable_sites: [Site] }
  PublishRuntimeEvent { stream: EffectStream, site: RuntimeSite,
                        predicate_operand, argument_operands, termination }
  InvokeRuntimeFunction { instance: RuntimeCall,
                          operand_role_range, result_role_range }

Nested variants also use only checked operand/result ordinals. The expected
runtime-function or ForFold graph fixes the exact variant, slots, and semantic
access summary; a nested action cannot acquire an outer root identity.
`CaptureValue` purpose and the proof which requires an `EvaluatePinned` action
are already fixed by the exact expected result/action owner and its inverse.
Neither action stores a second producer-selected purpose/reason scalar which
could disagree with that owner.

`InvokeRuntimeFunction` is a verified nested-program boundary, not an ordinary
value node and not a return-only shorthand. The action joins two distinct
authoritative objects defined by
[Source semantic objects and input accesses](./source-semantic-inputs.md):

- `SourceRuntimeFunctionInvokeOperandRoleV1` and
  `SourceRuntimeFunctionInvokeResultRoleV1` are the sole role enums;
- `VerifiedRuntimeFunctionProgramV1` and
  `ExpectedSourceRuntimeCallInstanceV1` own respectively the shared program and
  one call instance; and
- `SourceRuntimeFunctionInvokeOwnerTargetV1` and
  `SourceRuntimeFunctionInvokeV1` own the action-specific role pools, nested
  mapping, writebacks, component summaries, and combined summary.

This document does not redeclare shortened aliases for those V1 types. The
outer/fold action kind's `RuntimeCall` field names only the expected occurrence
instance. Its exact `SourceRuntimeFunctionInvokeOwnerTargetV1` inverse selects
`Outer`, `RuntimeProgram`, or `ForFold`; those variants use respectively an
outer action plus occurrence instance, a specialization-scoped program action
plus `RuntimeFunctionProgramNestedCallInstanceIdV1`, or a template-scoped fold
action plus occurrence instance.
Each invoke action owns exactly one `SourceRuntimeFunctionInvokeV1`, every
invoke row maps back to that action and slot ranges, and non-invoke actions own
none. No owner namespace may substitute the call-instance row or another
equal-numbered action namespace for this action-owned aggregate.

The operand-role range is a bijection to the owning action's existing operand
ordinals: explicit arguments are evaluated in source order, then missing input
defaults in formal order. An output/inout actual prepares its one target handle
once; inout reads that same handle, and copyout reuses it. The result-role range
is optional return first and every flattened output/inout result in declaration
order. Copyout is also in that declaration order. The roles contain ordinals,
not a duplicate value-ID list. Missing, extra, reordered, cross-instance, or
return-only rows reject even when types or current bits happen to match.

Program sharing is by the complete type specialization above and never by
argument value. The complete instantiated identity key is the pair
`RuntimeSourceExecutionLineageRowV1 = Root | RuntimeCall(parent, call,
specialization)` and `RuntimeSourceLocalScopeV1 = Body |
ForFold(SourceForFoldTemplateId)`. `ExpectedSourceRuntimeCallInstanceV1`
records its caller scope explicitly. Call instances and instantiated slots
remain distinct through that pair. A retained ForFold changes only local scope;
it creates neither a lineage row nor one row per iteration. The
verifier rejects the first canonical self-edge or nontrivial SCC
in the runtime function-template call graph before specialization. It uses a
worklist and imposes no call-depth, body-size, loop-trip, or compilation-time
cap. Constant-function evaluation remains the separate finite certificate VM
and cannot prove or replace this runtime relation.

The invoke action is scheduled and atomized only as `Whole`. Its semantic access
summary is independently recomputed from setup, the shared program, and
copyout, and its result publication plus all memory/environment/effect token
updates is atomic at the outer boundary. Nested program actions remain visible
to the dedicated boundary verifier; neither source construction nor a later
planner may inline an unverified body, clone the call per result slice, omit a
discarded expression result's effects, or keep a return while dropping a
writeback.

InputResolutionKind<DynamicPlan> = Memory | Environment | StaticComposite |
                                   DynamicOverlay(DynamicPlan)
SourceInputResolution = InputResolutionKind<SourceDynamicAddressPlanId>
SourceFoldInputResolution =
  InputResolutionKind<SourceFoldDynamicAddressPlanId>

SourceSemanticObject
  object: SourceSemanticObjectId
  exact declaration/binding identity
  independently derived full width / signedness / positive-type class /
    Bit-or-Logic domain
  Ordinary | TriIntent(VerifiedSourceTriIntentId)
  exact default-clock/reset role, if any
  canonical aggregate dimensions and full flattened stride vector

SourceInput
  input: SourceInputId
  object: SourceSemanticObjectId
  exact HIR read role and static access
  exact selected dimensions / part-select kind / index count and stride prefix
  independently derived result width / result signedness /
    result positive-type class / result domain
  expected semantic input row from VerifiedSourceSemanticContext

`SourceSemanticObjectId` and `SourceInputId` are deliberately different
namespaces. One object can have several valid input rows: for example
`mem[i]`, `mem[i][j]`, and a dynamic part-select have different exact stride
prefixes. Conversely, ForFold state overlap, write-domain membership, and
object bounds are checked by `(SourceSemanticObjectId, bit range)`, never by
input-row identity. Canonical source-input rows are derived in expected-HIR
traversal order and are not copied from an `SLTNode::Input` payload. A phase
input node can name only that row plus its ordered index children; its child
count must equal the row and it has no duplicate object/access/stride fields to
override. The expected value graph separately matches each index child to the
exact HIR operand occurrence.

Object dimensions are derived with checked arithmetic as unpacked extents,
then packed-width extents, then the normalized intrinsic struct/union/enum
width, including one. Every extent is resolved and nonzero;
suffix products define strides and their checked product must equal the
independently derived object width. The verifier does not use Veryl's
unchecked `Shape::total`, `Type::total_width`, struct-width addition, or
Celox's legacy `resolve_total_width` as an oracle. `Bit`, `Logic`, clock/reset,
and recursively normalized enum/struct/union data domains form the closed
accepted set; unknown, SystemVerilog, string, floating, and non-data kinds are
rejected until a distinct executable value-domain rule exists. A producer
cannot make a wrong `[2, 3]` shape self-consistent by supplying width six and
strides `[2, 1]` because none of those summaries is proof-bearing input.
The complete normative normalization, identity, mapping, and producer-
connection contract is specified in
[Source semantic objects and input accesses](./source-semantic-inputs.md).

SourceWriteDomain
  checked module-local semantic state identity and normalized type/access
  exact owner in ExpectedSourceValueGraph

SourceBindingRoleV1 =
  LexicalEnvironment | RetainedForCounter | RetainedForState

SourceBinding
  checked module-local semantic binding identity and normalized type
  role: SourceBindingRoleV1
  exact owner in ExpectedSourceValueGraph

SourceEffectStreamKindV1 = RuntimeOutput | RuntimeAssertion | Termination
SourceEffectPublicationRuleV1 = OnActionExecution | OnPredicateFalse

SourceEffectStream
  checked module-local effect identity
  kind: SourceEffectStreamKindV1
  publication_rule: SourceEffectPublicationRuleV1
  exact owner in ExpectedSourceValueGraph

SourceSemanticAccessSummary
  canonical sorted exact read/write SourceWriteDomainIds
  canonical sorted exact read/write SourceBindingIds
  canonical sorted exact SourceEffectStreamIds and
    SourceEffectPublicationKindV1 values

SourceEffectPublicationKindV1 = Definite | Conditional

SourceObserver
  observer: SourceObserverId
  kind: SourceObserverKindV1
  metadata: SourceObserverMetadataV1
  exact sensitivity/capture/event semantic specification

SourceObserverKindV1 = CombinationalProcess | RetainedForFold
SourceObserverMetadataV1
  activation_group_count: nonzero u32
  capture_policy: SnapshotAtActivation
  event_order: SourceOrder

SourceObserverOccurrence
  observer / occurrence IDs
  exact Primary or Trigger owner/group/ordinal semantics

SourceRuntimeEventSite
  site: SourceRuntimeEventSiteId
  emit_rule: SourceRuntimeEventEmitRuleV1
  exact predicate/argument/termination semantics

SourceRuntimeEventEmitRuleV1 =
  Display | Write | AssertContinue | AssertFatal | Finish

SourcePinnedSyntheticMuxOrigin
  proof: SourceSyntheticOriginId
  semantic HIR owner / reason: SourcePinnedSyntheticReasonV1 /
    SourceValueOccurrenceId
  exact pinned dataflow-select semantics

SourcePinnedSyntheticReasonV1 = RequiredDataflowSelect

SourceDynamicAddressPlan
  plan: SourceDynamicAddressPlanId
  owner action / once-prepared ExpectedSourceTargetHandleId /
    semantic input / object type and width
  ordered typed index uses: [SourceOccurrenceUse]
  dimensions / exact part-select geometry / selected width
  offset / address_known / bounds_when_known / access_guard:
    SourceOccurrenceUse
  access semantics: CheckedRead | CheckedOverlayWrite
```

### Shared Veryl-0.20 `for` transition relation

Every Veryl V0_20 source `for` identifier is emitted as `i32`: signed,
two-state, and exactly 32 bits. The verifier derives that type from the retained
source loop declaration and requires the analyzer/type witness to agree; a
producer-selected counter width or signedness is not evidence. Define `C(x)` as
ordinary assignment coercion of the typed value `x` to this exact `i32` type:
source signedness chooses widening, narrowing keeps the low 32 bits, and the
final Logic-to-Bit conversion maps X/Z to zero. A singleton has only
`A = C(value)`; a bounded range has `A = C(start)` and `B = C(end)`. Empty tests
and header comparisons are signed 32-bit comparisons after those coercions.
Each consumer evaluates/acquires exactly the operands present in that closed sum
once at loop entry; a backedge reuses the verified counter values. It never
duplicates a singleton value into a synthetic end occurrence.

```text
SourceForCounterRuleV1 = CeloxSourceV0_20SignedI32
SourceForCounterRule = generated source-facing alias of
  SourceForCounterRuleV1
SourceForRangeForm = SingleForward | SingleReverse |
                     ForwardExclusive | ForwardInclusive |
                     ReverseExclusive | ReverseInclusive
```

The six range forms are closed:

| Source form | Empty before initialization when | Initial counter | Header predicate |
| --- | --- | --- | --- |
| singleton `a` | never | `A` | one-shot `first` state |
| singleton `rev a` | never | `A` | one-shot `first` state |
| forward exclusive `a..b` | `A >= B` | `A` | `i < B` |
| forward inclusive `a..=b` | `A > B` | `A` | `i <= B` |
| reverse exclusive `rev a..b` | `B <= A` | `B - 1` | `i >= A` |
| reverse inclusive `rev a..=b` | `B < A` | `B` | `i >= A` |

For reverse exclusive, `B - 1` is formed only after the empty predicate is
known false. Reverse inclusive starts at `B`; it never subtracts one merely
because the range is reversed. Initialization is assignment-coerced to the
same `i32` counter. After a singleton body the one-shot state becomes complete
without applying a counter update. The default update is `i += 1` for either
nonsingleton forward form and `i -= 1` for either nonsingleton reverse form.

An explicit step replaces only that default update. It is still type/value
verified and acquired once for a singleton form, but the singleton completes
without executing it. The one shared closed tag set is:

```text
SourceForStepAssignmentOpV1 =
  AddAssign("+=") | SubAssign("-=") | MulAssign("*=") |
  DivAssign("/=") | RemAssign("%=") |
  BitAndAssign("&=") | BitOrAssign("|=") | BitXorAssign("^=") |
  LogicShiftLeftAssign("<<=") | LogicShiftRightAssign(">>=") |
  ArithShiftLeftAssign("<<<=") | ArithShiftRightAssign(">>>=")

SourceForStepAssignmentOp = generated source-facing alias of
  SourceForStepAssignmentOpV1
```

The spelling, tag, and operator rule are one-to-one; in particular logical and
arithmetic shifts cannot share a producer tag. A step evaluates the counter
LHS once, applies the corresponding closed typed integral operator to that
signed 32-bit LHS and the typed step operand, and assignment-coerces the result
back to `i32`. Arithmetic and bitwise results, including the default `+ 1` and
`- 1`, wrap modulo `2^32`; shifts use the exact arbitrary-width count rule
before any host conversion. Division/remainder by zero and X/Z follow the
shared four-state operator rule and final two-state counter conversion. There
is no host integer arithmetic in this relation.

Zero steps, `next == current`, movement in either direction, and signed wrap
are ordinary results of the update relation. None is a verifier correctness
error, overflow exit, progress assertion, iteration limit, or license for a
fallback. After a true continue, the transition is `Advance` exactly when the
updated counter satisfies the same header predicate; otherwise it is
`NormalRangeExit`. Therefore a nonprogressing in-range update takes the
backedge and may run forever.

This transition relation has three distinct consumers:

- a source loop which Veryl has already expanded at compile time has no
  `PhaseForFold`/`SourceForFoldTemplate`; the source aggregate checks the finite
  expanded HIR and its exact source lineage rather than inventing a VM trace;
- a non-expanded source loop becomes the pinned `PhaseForFold`/
`SourceForFoldTemplate` runtime relation below. Its singleton-or-bounded/step operands
  are acquired exactly where that source relation specifies, and compilation
  makes no claim that the runtime loop terminates; and
- a `RawConstStatement::For` reached while evaluating a constant function uses
  the independently derived constant-function small-step VM. Its concrete
  recursion/loop termination is established only by a finite accepted trace,
  with one row per actual transition and no depth, iteration, wall-clock, or
  branchification cap. The VM never creates or consumes a `PhaseForFold` row.

These consumers share only the closed range/counter/operator semantics. Their
IDs, ownership rows, execution timing, and termination evidence never alias.
In V0_20 an explicit step is a completed typed constant proof: both a
`SourceForFoldTemplate` and a constant-function VM evaluate/acquire it once on
loop entry and reuse the value at each update. Both then call the same
`SourceForStepAssignmentOp` value rule.

The pinned analyzer currently lowers an operator-less range to the empty
exclusive pair `A..A`, while the pinned emitter lowers it to an inclusive
counter loop which can execute more than once after a wrapping or nonprogressing
update. Both disagree with the source singleton relation and must be fixed.
Neither producer result is a verifier oracle.

Required fixtures cover both one-element singleton forms, plus empty and
singleton instances of all four bounded range forms;
`i32::MIN`/`i32::MAX` boundaries; every explicit step tag; division/remainder
by zero; X/Z step operands; zero, nonprogressing, direction-changing, and
wrapping updates; a finite trace which terminates after wrap; and finite
prefixes of nonterminating executions, each of which remains nonterminal.
Cross-consumer fixtures prove that an expanded source loop, a retained
`PhaseForFold`, and a constant-function VM trace cannot reuse one another's IDs
or completion evidence.

```text
SourceForFoldTemplate
  template: SourceForFoldTemplateId
  unit: SourceControlUnitId / owner_action: SourceControlActionId
  counter SourceBindingId / exact signed 32-bit Bit type
  range_inputs: Single { value_use, SingleForward | SingleReverse } |
                Between { start_use, end_use,
                          ForwardExclusive | ForwardInclusive |
                          ReverseExclusive | ReverseInclusive }
  counter update: DefaultByRangeDirection |
                  Explicit(outer step use, SourceForStepAssignmentOp)
  transition_semantics: SourceForFoldTransitionSemantics
  canonical parallel state outer initial uses and result state
  body: SourceFoldGraph
  expected: ExpectedSourceForFoldGraph
  exact read/write/environment/effect summaries

SourceForFoldTransitionSemantics
  language_semantics_version
  counter_rule: SourceForCounterRuleV1::CeloxSourceV0_20SignedI32
  exact singleton-or-start/end and optional-step typed source facts with
    derived assignment/operator coercions
  counter_initial_and_bound:
    SingleForward(A, never empty, initial A, one-shot first) |
    SingleReverse(A, never empty, initial A, one-shot first) |
    ForwardExclusive(A, B, empty A>=B, initial A, signed <) |
    ForwardInclusive(A, B, empty A>B, initial A, signed <=) |
    ReverseExclusive(A, B, empty B<=A,
                     otherwise initial sub32(B, 1), signed >=) |
    ReverseInclusive(A, B, empty B<A, initial B, signed >=)
  counter_step_rule: SingleCompleteWithoutUpdate |
    SourceForStepAssignmentOp with exact operand/result width, signedness,
    domain conversion, and modulo-2^32 assignment
  post_update_range_rule: Advance | NormalRangeExit
  continue_condition: SourceForContinueRuleV1 over the typed continue use

SourceForContinueRuleV1 = TrueContinuesFalseOrXzExits

This row is independently derived from the typed HIR ForFold operator and
language version. It fixes the expected HeaderCondition, Counter recurrence
update, ContinueLatch predicate, transition outcome, and normal exit edges.
Step zero, `next == current`, direction changes, wrap, every closed step
operator, and X/Z conversion are handled only by these rules; a wire-supplied
formula, progress bit, overflow bit, or two-state tag is not proof. Header and
continue truth reduction use the same closed `IfReduction` semantics as source
control.

SourceFoldGraph
  topology: SourceFoldControlTopology
  private SourceFoldActionIds
  SourceFoldValueOccurrences / canonical producer relation / producer DAG
  SourceFoldDynamicAddressPlans / SourceFoldRecurrenceRelation
  parallel state updates / header condition / continue use /
    transition outcome use in SourceFoldUseSite namespace
  exact nested semantic access/effect rows

SourceFoldControlTopology =
  FoldControlTopology<SourceFoldPredicateRegionId, SourceFoldPointId,
                      SourceFoldEdgeId, SourceFoldOccurrenceUse,
                      SourceFoldActionId>

FoldControlTopology<Region, Point, Edge, Use, Action>
  root_region / entry / header / body_entry / continue_latch /
    transition_dispatch / normal_exit
  region rows: ID / optional parent / entry / normal_exit /
    owner: LoopRoot | Body
  point rows: ID / region / kind: Entry | Header | BodyEntry | Body |
    ContinueLatch | TransitionDispatch | NormalExit /
    ordered Action slots /
    exact predecessor-successor Edge IDs
  edge rows: ID / predecessor / successor /
    kind: EntryHeader | HeaderBody | HeaderExit | BodyFlow |
          ContinueExit | ContinueDispatch | TransitionAdvance |
          TransitionRangeExit /
    optional predicate: Use with exact polarity/outcome pattern
  exactly Entry->Header, Header->{BodyEntry,NormalExit}, body flow to
    ContinueLatch, ContinueLatch->{NormalExit,TransitionDispatch}, and
    TransitionDispatch->{Header,NormalExit};
    TransitionAdvance is the sole backedge/cycle
  removing TransitionAdvance yields one reachable DAG covering every row;
    NormalExit post-dominates every finite exiting path

FoldEdgePredicateV1 = True | False | Advance | NormalRangeExit

HeaderBody/HeaderExit project the same primary `HeaderCondition` use with
true/false polarity; it compares the direction-specific current counter/bound
from transition semantics. ContinueDispatch/ContinueExit project one primary
`ContinueCondition` use with true/false polarity. TransitionAdvance and
TransitionRangeExit project one primary `TransitionOutcome` use with the
exhaustive mutually exclusive Advance/NormalRangeExit outcome patterns; it is
evaluated only on the continue-true path. Edge rows cannot supply independent
predicate occurrences. A nonprogressing or wrapped next counter which still
satisfies the header predicate is `Advance`, not a third outcome.

ExpectedSourceForFoldGraph
  independently derived from the typed HIR ForFold semantic specification
  complete ExpectedSourceFoldUseId/ExpectedSourceFoldResultId rows
  expected private region/point/edge/action/value/dynamic/recurrence rows

SourceFoldUseSite = Slot(SourceFoldPointId, slot) | Edge(SourceFoldEdgeId)

SourceFoldAction
  owner point/slot, ordered SourceFoldOccurrenceUse operands and
    SourceFoldOccurrenceDef results
  exact SourceSemanticAccessSummary
  kind: NestedActionSemanticKind<SourceInputId, SourceBindingId,
          SourceFoldPreparedTargetHandleId,
          SourceFoldDynamicAddressPlanId, SourceEffectStreamId,
          SourceRuntimeEventSiteId, ExpectedSourceRuntimeCallInstanceId,
          SourceFoldUseSite, SourceFoldInputResolution>

SourceFoldValueOccurrence
  semantic source-phase node or fixed private runtime leaf:
    SourceFoldRuntimeLeafKindV1
  flow: SourceFoldValueFlow
  ordered_operands: [SourceFoldOccurrenceUse]

SourceFoldRuntimeLeafKindV1 = OuterEntry | HeaderParameter | ExitParameter

SourceFoldValueFlow
  Use { semantic_use: ExpectedSourceFoldUseId,
        site / owner: SourceFoldUseOwner /
        role: SourceFoldOccurrenceUseRoleV1,
        value_source: EvaluateHere |
          FixedValue(SourceFoldCanonicalProducerId, ValueFlowReason) }
  Definition { semantic_result: ExpectedSourceFoldResultId,
               site,
               owner: OuterEntry(outer SourceOccurrenceUse) |
                      HeaderParam(Counter | State(state ordinal)) |
                      ExitParam(State(state ordinal)) |
                      ActionResult(SourceFoldActionId, result ordinal) }

SourceFoldOccurrenceUseRoleV1 =
  Value | Address | Predicate | Argument | PreviousValue |
  Recurrence | Transition

SourceFoldUseOwner
  ValueOperand(SourceFoldValueOccurrenceId, operand ordinal) |
  ActionOperand(SourceFoldActionId, operand ordinal) |
  HeaderCondition | ContinueCondition | TransitionOutcome |
  RecurrenceUpdate(Counter | State(state ordinal))

SourceFoldOccurrenceUse / SourceFoldOccurrenceDef
  checked template-scoped views requiring respectively Use/Definition

SourceFoldCanonicalProducerRelation / SourceFoldProducerDependencyDAG
  same derived total producer/inverse and per-iteration DAG contracts as the
  outer source graph, keyed by SourceFoldCanonicalProducerId
  HeaderParam and OuterEntry definitions are operand-free leaves

SourceFoldRecurrenceRelation
  one row for counter and every state header parameter
  exact outer entry producer, header definition, parallel update producer,
    unique TransitionAdvance backedge slot; each state also names one ExitParam
    whose HeaderExit operand is current HeaderParam and whose ContinueExit and
    TransitionRangeExit operands are the parallel update producer
  all three edge uses are retained even when the two update values are equal;
    ExitParam predecessor coverage is exact and ordered by SourceFoldEdgeId
  result-state projection names only that state ExitParam; counter has no exit
    parameter unless a future typed result explicitly exposes it
  recurrence edges are not SourceFoldProducerDependencyDAG operand edges

SourceFoldPreparedTargetHandle
  id: SourceFoldPreparedTargetHandleId scoped by SourceForFoldTemplateId
  exact assignable fold-body target syntax / object / normalized access / type
  ordered selector SourceFoldOccurrenceUses
  prepared exactly once; old-value reads and writebacks reuse this ID

SourceFoldDynamicAddressPlan
  plan / owner SourceFoldActionId / once-prepared
    SourceFoldPreparedTargetHandleId / expected HIR dynamic-select row
  ordered typed SourceFoldOccurrenceUse indices, object/type/width,
    dimensions, part-select geometry, offset, address-known,
    bounds-when-known, access guard, access semantics, and result projection

Every source outer/fold/program `StaticTarget` names its namespace's prepared
handle directly. Every `DynamicTarget(handle, plan)` has one exact inverse plan
row naming the same handle; a plan or handle cannot be shared across those
nominal namespaces. Selector evaluation constructs the handle once before an
old-value read and copyout reuses it.

SourcePredicateRegion
  unit: SourceControlUnitId
  parent: optional SourcePredicateRegionId
  entry / exit: SourceControlPointId
  owner: Root | GateTrue(SourceGateId) | GateFalse(SourceGateId) |
         DecisionArm(SourceDecisionId, SourceArmOrdinal) |
         DecisionDefault(SourceDecisionId)

SourceGate
  unit: SourceControlUnitId
  parent_region: SourcePredicateRegionId
  condition: SourceOccurrenceUse
  header / join / continuation: SourceControlPointId
  true_region / false_region: SourcePredicateRegionId
  result_merges: [SourceGateResultMergeId]
  origin: If | Ternary
  condition_semantics: SourceConditionSemantics

SourceConditionSemantics
  IfReduction { language_semantics_version, source domain: Bit | Logic,
                width, rule: SourceIfReductionRuleV1 }
  TernaryBitMerge { language_semantics_version,
                    condition domain/width,
                    then/else/result coercions,
                    rule: SourceTernaryBitMergeRuleV1 }

SourceIfReductionRuleV1 = OneIsTrueZeroXzFalse
SourceTernaryBitMergeRuleV1 =
  KnownSelectOtherwiseEqualBitsElseCanonicalX

For `CeloxSourceV0_20`, both variants require `width == 1`; the rule ID
distinguishes known-zero, known-one, and X/Z handling, not an arbitrary-width
truth reduction. A future profile which admits wide conditions requires a new
semantics tag and rule.

The aggregate verifier derives this variant and every coercion/rule ID from
the typed HIR, then evaluates the closed language
formula for known, X, and Z bits. A wire-supplied truth table is never evidence.
Source-phase synthetic control gates/decisions are not part of the initial
schema; source synthetic origins may pin dataflow values but cannot invent
control topology. Any future source synthetic control requires a new finite
rule enum and independently derived expected-control rows before admission.

SourceGateResultMerge
  unit: SourceControlUnitId
  gate: SourceGateId
  merge_site: SourceControlSite
  condition / then_value / else_value: SourceOccurrenceUse
  result: SourceOccurrenceDef
  mux: SourceGatedMuxId

SourceDecisionArm
  ordinal: SourceArmOrdinal
  ordered_patterns: [SourcePatternOccurrence]
  predicate: SourceOccurrenceUse
  region: SourcePredicateRegionId

SourcePatternOccurrence
  EqWildcard { pattern: SourceTypedPatternOperand,
               coercion: SourceCoercion,
               predicate: SourceOccurrenceUse }
  Range { lower / upper: SourceTypedPatternOperand,
          lower_comparison / upper_comparison: SourceComparison,
          upper_inclusive,
          predicate: SourceOccurrenceUse }

SourceTypedPatternOperand
  value: SourceOccurrenceUse
  source domain: Bit | Logic
  width / signedness / optional exact constant: VerifiedSourceTypedValueId

SourceDecision
  unit: SourceControlUnitId
  parent_region: SourcePredicateRegionId
  selector: SourceOccurrenceUse
  dispatch_header / join / continuation: SourceControlPointId
  ordered_arms: nonempty [SourceDecisionArm]
  default_region: SourcePredicateRegionId
  source_semantics: SourceCaseSemantics

SourceCaseSemantics
  language_semantics_version / case operator kind
  first_match_rule: SourceCaseFirstMatchRuleV1
  default_rule: SourceCaseDefaultRuleV1
  xz_rule: SourceCaseXzRuleV1
  selector source domain / width / signedness
  per-pattern derived SourceCoercion / SourceComparison rules

SourceCaseFirstMatchRuleV1 =
  ProceduralFirstKnownTrue | ExpressionOrderedTruth3Merge
SourceCaseDefaultRuleV1 =
  RequiredExpressionDefault | ProceduralDefaultOrFallthrough
SourceCaseXzRuleV1 =
  ExactFourState | PatternXzWildcard | SwitchKnownOne

The verifier derives case/casez/range/default behavior from the HIR operator
and language version, not from an arbitrary semantic table. A default-only
source case is canonically emitted as its linear default body and produces no
`SourceDecision`, arm, decision merge, or gated step; both expected-graph and
provenance builders apply that same closed normalization.

SourceGatedOwner = GateResult(SourceGateResultMergeId) |
                   DecisionStep(SourceDecisionResultMergeId,
                                source_arm: SourceArmOrdinal)

SourceGatedKey
  unit: SourceControlUnitId
  owner: SourceGatedOwner
  condition / then_value / else_value: SourceOccurrenceUse
  merge_site: SourceControlSite

SourceGatedMux
  key: SourceGatedKey
  semantic_node: PhaseNodeId<SourcePhase>
  result: SourceOccurrenceDef

SourceDecisionResultMerge
  unit: SourceControlUnitId
  decision: SourceDecisionId
  merge_site: SourceControlSite
  result: SourceOccurrenceDef
  default_value: SourceOccurrenceUse
  selected_arm_values: [SourceOccurrenceUse]
  ordered_steps: [SourceDecisionMergeStep]

SourceDecisionMergeStep
  source_arm: SourceArmOrdinal
  predicate / selected_value / incoming_value: SourceOccurrenceUse
  result: SourceOccurrenceDef
  mux: SourceGatedMuxId

SourceValueOccurrenceRef
  instance: SourceInstanceId
  occurrence: SourceValueOccurrenceId

SourceObserverRef = SourceRef<SourceObserverId>
SourceObserverOccurrenceRef =
  (SourceInstanceId, SourceObserverId, SourceObserverOccurrenceId)

ControlOccurrencePlan
  source instance table referencing the owning source catalog
  mapped-source and explicit synthetic-origin relations
  inputs / write domains / bindings / effect streams
  observers / runtime sites / runtime-function call instances and execution
    lineages / dynamic plans / ForFold templates
  units / regions / points / edges / roots / occurrence_actions
  gates / gate result merges / decisions / gated_muxes /
  decision_result_merges
  value_occurrences / canonical producer relation /
  producer dependency DAG / ordinary rewrite relations / root-order barriers

OccurrenceInput
  object: OccurrenceSemanticObjectId
  semantic/type/access/index-geometry row
  origin: MappedSource(SourceRef<SourceInputId>) |
          PortGlue(GlueOriginId) | Synthetic(SyntheticOriginId)

OccurrenceSemanticObject
  exact flattened variable/binding/storage identity and independently derived
  width / signedness / Bit-or-Logic domain / aggregate dimensions
  origin: MappedSource(SourceRef<SourceSemanticObjectId>) |
          PortGlue(GlueOriginId) | Synthetic(SyntheticOriginId)

WriteDomain
  exact state partition / capture / observer / global-unknown semantics
  origin: MappedSource(SourceRef<SourceWriteDomainId>) |
          Synthetic(SyntheticOriginId)

Binding
  exact type/lifetime/environment semantics
  origin: MappedSource(SourceRef<SourceBindingId>) |
          Synthetic(SyntheticOriginId)

EffectStream
  exact ordered publication/termination stream semantics
  origin: MappedSource(SourceRef<SourceEffectStreamId>) |
          Synthetic(SyntheticOriginId)

SemanticAccessSummary
  canonical sorted exact read/write WriteDomainIds
  canonical sorted exact read/write BindingIds
  canonical sorted exact EffectStreamIds and publication kinds

ValueOccurrence
  unit: ControlUnitId
  flow: OccurrenceValueFlow
  origin: MappedSource { semantic_node, source: SourceValueOccurrenceRef } |
          Atomized { semantic_node, proof: AtomizationOriginId } |
          PortGlue { semantic_node, proof: GlueOriginId } |
          ObserverSynthetic { semantic_node,
                              source: SourceObserverOccurrenceRef } |
          PinnedSynthetic { semantic_node, proof: SyntheticOriginId } |
          RuntimeState { semantic_node, proof: SyntheticOriginId }
  ordered_operands: [OccurrenceUse]

OccurrenceValueFlow
  Use { site: ControlUseSite,
        semantic_use: ExpectedOccurrenceUseId,
        owner: OccurrenceUseOwner,
        role,
        value_source: EvaluateHere |
                      FixedValue(CanonicalProducerId, ValueFlowReason) }
  Definition { site: ControlSite,
               semantic_result: ExpectedOccurrenceResultId,
               owner: OccurrenceDefinitionOwner }

OccurrenceUseOwner
  ValueOperand(owner occurrence, operand ordinal) |
  ActionOperand(action, operand ordinal) |
  GateCondition(gate) | GateResultOperand(result merge, condition/then/else) |
  DecisionSelector(decision) |
  DecisionPatternOperand(decision, arm, pattern, operand) |
  DecisionPatternPredicate(decision, arm, pattern) |
  DecisionArmPredicate(decision, arm) |
  DecisionResultOperand(result merge, arm/role)

OccurrenceDefinitionOwner
  ActionResult(action, result ordinal) | GatedMuxResult(gated mux) |
  PinnedSyntheticResult(proof)

AtomizationOrigin
  source: SourceValueOccurrenceRef
  unit / source atom ordinal / exact source and result bit ranges
  verified source-to-occurrence node rewrite

GlueOrigin
  unit / canonical instance-port connection row
  exact source and destination semantic/type/access relation

SyntheticOrigin
  unit / kind / expected synthetic object ID
  exact operands, site, and result relation matched to the independently
    derived expected row; it is a plan witness, never its own specification

VerifiedFlattenedSemanticContext
  FrozenSourceCatalog plus canonical elaborated instance/port/type/alias rows
  independently derived expected unit/input/domain/binding/effect tables
  complete semantic inputs for mapped source, atomization, glue, observer, and
    required synthetic derivation before ordinary rewrites
  closed OccurrenceDerivationRuleVersion implemented by the aggregate verifier;
    no producer-supplied synthetic specification extends these rules

ExpectedOccurrenceGraph
  canonical expected control/value/action/root/proof rows derived only from
    the frozen source catalog, verified elaboration/glue/observer inputs, and
    the closed versioned derivation rules
  canonical ExpectedOccurrenceUseId/ExpectedOccurrenceResultId rows with exact
    owner/role/site/type/node recipe and ordered producer edges
  expected semantic node recipes and complete reachability roots
  no rows copied from ControlOccurrencePlan or its SyntheticOrigin table

OccurrenceRewriteRelation
  rewrite: OccurrenceRewriteId
  input ExpectedOccurrenceNodeIds / output occurrence-phase nodes
  exact permitted ordinary rule and total old-producer-to-new relation
  complete occurrence/root/action projection and unchanged gated identities

OccurrenceUse
  checked newtype view requiring a Use row

OccurrenceDef
  checked newtype view requiring a Definition row

CanonicalProducerRelation
  producer_of: one CanonicalProducerId per occurrence
  producer_occurrence: one EvaluateHere/Definition occurrence per producer ID
  FixedValue rows name that producer ID directly, never another fixed use
  canonical inverse occurrence lists cover every occurrence exactly once

This relation and `OccurrenceProducerDependencyDAG` are likewise rebuilt from
`ExpectedOccurrenceGraph` and verified flow rows. `ControlOccurrencePlan`
owns the verified result, but a raw plan cannot establish it by assertion.

OccurrenceProducerDependencyDAG
  one node per canonical producer
  ordered edges to canonical producers of EvaluateHere/definition operands
  fixed-action-definition ownership and propagated ValueFlowReason facts

OccurrenceAction
  id: ControlActionId
  unit / owner
  origin: MappedSource(ActionExpansionId,
                       Primary(primary ordinal) | Helper(helper ordinal)) |
          Glue(GlueOriginId, action ordinal) |
          Observer(ObserverId, ObserverOccurrenceId, action ordinal) |
          Synthetic(SyntheticOriginId, action ordinal)
  ordered_operands: [OccurrenceUse]
  results: [OccurrenceDef]
  semantic_accesses: SemanticAccessSummary
  kind: occurrence-valued ControlActionKind

Runtime-function occurrence identity uses the exact semantic-owned V1 records
`OccurrenceRuntimeRootOwnerV1`,
`OccurrenceRuntimeExecutionLineageRowV1`,
`OccurrenceRuntimeLocalScopeV1`,
`SourceToOccurrenceRuntimeLocalScopeRowV1`,
`OccurrenceRuntimeProgramActionCoordinateV1`,
`OccurrenceRuntimeFunctionInvokeOwnerTargetV1`,
`OccurrenceRuntimeFunctionInvokeV1`, and
`OccurrenceRuntimeFunctionCallInstanceV1`. This document defines no shortened
occurrence enum or call-instance alias.

`OccurrenceRuntimeExecutionLineageRowV1::Root` retains the exact
`RuntimeSourceRootRoleV1` and matching `OccurrenceRuntimeRootOwnerV1` variant:
source-instance-qualified catalog owner, Whole `ControlRootRef`, scheduled
`ObserverOccurrenceId`, or source-instance-qualified initializer owner.
`RuntimeCall` retains the exact parent and `OccurrenceRuntimeCallInstanceId`.
ForFold never creates a lineage row.

The complete semantic-owned `SourceToOccurrenceRuntimeFunctionRelationV1`
maps execution lineages/root owners, local scopes, program slots/actions,
formal/local objects, inputs/results, targets/plans/coercions/events/folds,
invoke owner variants, operand/result roles, and formal-order writebacks. Every
map has an exact inverse and boundary token/value equality. In particular,
`Body` maps to `Body`, while `ForFold(SourceForFoldTemplateId)` maps to
`ForFold(the exact mapped ForFoldTemplateId)`; only the tag is unchanged, never
the checked template-ID namespace. No occurrence row is created for an
iteration or equal current bits.
For a shared nested program call, the relation is a total bijection from
`(RuntimeFunctionProgramNestedCallInstanceIdV1,
OccurrenceRuntimeExecutionLineageId, OccurrenceRuntimeLocalScopeV1)` to one
`OccurrenceRuntimeCallInstanceId`; the immutable program action retains only
its structural site ID.

ControlRootRef
  unit: ControlUnitId
  root: ExternalRootId

ControlValueDraft
  verified FrozenOccurrenceArtifact
  dense occurrence-to-InstValue relation
  action-indexed token-flow overlay / inst values / token tables
  construction-only InstValue/dependency/resolved-gated maps

ControlResolutionOverlay
  exactly one InstValueId per ValueOccurrenceId
  exactly one token-flow row per ControlActionId
  resolved dynamic-address views plus same-ID ForFoldTokenOverlay and
  ForFoldValueResolutionOverlay tables
  inst_values / memory, environment, and effect token tables

ControlUnit
  root_region: PredicateRegionId
  entry / exit: ControlPointId
  roots: [ControlRootRef]
  origin: MappedSource(instance: SourceInstanceId,
                       source_unit: SourceControlUnitId) |
          PortGlue(GlueOriginId) | ObserverSynthetic(SyntheticOriginId)

ControlRootIdentity
  reference: ControlRootRef
  origin: SourceExpansion(expansion: RootExpansionId, atom_ordinal: usize)
        | ObserverMetadataOrigin(observer: ObserverId)
        | ObserverOccurrenceOrigin(observer: ObserverId,
                                   occurrence: ObserverOccurrenceId)
        | GlueOrigin(proof: GlueOriginId, root_ordinal: usize)
        | SyntheticOrigin(proof: SyntheticOriginId, root_ordinal: usize)
  kind: LogicPath(expansion: RootExpansionId, atom_ordinal: usize)
      | ObserverMetadata(observer: ObserverId)
      | RuntimeEventOccurrence(observer: ObserverId,
                               occurrence: ObserverOccurrenceId)
      | PortGlue(proof: GlueOriginId, root_ordinal: usize)
      | Synthetic(proof: SyntheticOriginId, root_ordinal: usize)
  disposition: Scheduled(action: ControlActionId) | MetadataOnly

OccurrenceRoot
  identity: ControlRootIdentity
  ordered_operands: [OccurrenceUse]

ResolvedControlRoot
  identity: same ControlRootIdentity
  ordered_operands: [InstUse]

ControlSite = (ControlPointId, slot: usize)
ControlUseSite = Slot(ControlSite) | Edge(ControlEdgeId)

InstUse
  role: InstUseRole
  value: InstValueId
  site: ControlUseSite

InstDef
  value: InstValueId
  site: ControlSite

PredicateRegion
  unit: ControlUnitId
  parent: optional PredicateRegionId
  entry / exit: ControlPointId
  owner: Root | GateTrue(GateId) | GateFalse(GateId)
       | DecisionArm(DecisionId, arm) | DecisionDefault(DecisionId)

ControlPoint
  unit: ControlUnitId
  region: PredicateRegionId
  kind: UnitEntry | ArmEntry | RegionExit | GateHeader
      | DecisionHeader | Join | Continuation | Effect
  ordered actions
  predecessor / successor ControlPointIds

ControlEdge
  unit: ControlUnitId
  predecessor / successor: ControlPointId
  kind: Ordinary | GateArm | DecisionArm | DecisionDefault | UnitBoundary

ResolvedControlAction
  id: ControlActionId
  unit: ControlUnitId
  owner: (ControlPointId, action_index: usize)
  ordered_operands: [InstUse]
  results: [InstDef]
  memory_flow: [MemoryTokenFlow]
  environment_flow: [EnvironmentTokenFlow]
  effect_flow: [EffectTokenFlow]
  kind: ControlActionKind

MemoryTokenFlow
  domain: WriteDomainId
  incoming: MemoryTokenId
  outgoing: optional MemoryTokenId

EnvironmentTokenFlow
  binding: BindingId
  incoming: EnvironmentTokenId
  outgoing: optional EnvironmentTokenId

EffectTokenFlow
  stream: EffectStreamId
  incoming: EffectTokenId
  outgoing: optional EffectTokenId

ControlActionKind =
  ActionSemanticKind<ControlRootRef, OccurrenceInputId, BindingId,
                     OccurrencePreparedTargetHandleId,
                     DynamicAddressPlanId, ObserverId, RuntimeEventSiteId,
                     OccurrenceRuntimeCallInstanceId,
                     ForFoldTemplateId, ControlSite, InputResolution>

InputResolution = InputResolutionKind<DynamicAddressPlanId>
FoldInputResolution = InputResolutionKind<FoldDynamicAddressPlanId>

OccurrenceGate
  unit: ControlUnitId
  source: SourceRef<SourceGateId>
  parent_region
  condition: OccurrenceUse
  header / true_region / false_region / join / continuation
  result_merges: [GateResultMergeId]
  origin: If | Ternary
  condition_semantics

ResolvedGate
  verified view of the same row with condition: InstUse

OccurrenceGateResultMerge
  unit / gate / merge_site
  source: MappedSource(SourceRef<SourceGateResultMergeId>, exact atom range)
  condition / then_value / else_value: OccurrenceUse
  result: OccurrenceDef
  mux: GatedMuxId

ResolvedGateResultMerge
  verified view of the same row with InstUse / InstDef values

OccurrenceDecision
  unit: ControlUnitId
  source: SourceRef<SourceDecisionId>
  parent_region
  selector: OccurrenceUse
  dispatch_header / join / continuation: ControlPointId
  ordered arms: nonempty [OccurrenceDecisionArm]
  default_region
  source_semantics: SourceCaseSemantics

OccurrenceDecisionArm
  ordinal: DecisionArmOrdinal
  source_arm: SourceArmOrdinal
  ordered patterns: [OccurrencePattern]
  predicate: OccurrenceUse
  region: PredicateRegionId

OccurrencePattern
  EqWildcard(pattern: OccurrenceTypedPatternOperand,
             coercion: SourceCoercion,
             predicate: OccurrenceUse)
  Range(lower / upper: OccurrenceTypedPatternOperand,
        lower_comparison: SourceComparison,
        upper_comparison: SourceComparison,
        upper_inclusive,
        predicate: OccurrenceUse)

OccurrenceTypedPatternOperand
  value: OccurrenceUse
  source domain: Bit | Logic
  width / signedness
  optional exact constant: VerifiedOccurrenceTypedValueId

ResolvedDecision / ResolvedDecisionArm / ResolvedPattern
  verified views of the same topology and decision semantics with every
  OccurrenceUse replaced by its reconstructed InstUse

Initial occurrence-phase Gate/Decision/gated-merge rows must map a verified
source row; glue, observer, and other synthetic units may own actions and roots
but do not invent this control topology. A future synthetic control rule needs
new expected-row and mapping variants rather than reusing `SyntheticOriginId`.

SourceCoercion
  source width / source signedness
  target width / target signedness
  context: SelfDetermined | AssignmentValue | ExplicitCast |
           CommonExpressionOperand |
           ForFoldCounterOperand(SourceForCounterRule)
  width action: Identity | Truncate | ZeroExtend | SignExtend

`SourceCoercion` is a derived row, not a producer-selected extension tag.  The
context fixes the extension basis before the width action is checked:

- `SelfDetermined` requires identical source/target types and `Identity`;
- `AssignmentValue` and `ExplicitCast` use source signedness when widening,
  while the typed destination/cast independently fixes target signedness;
- `CommonExpressionOperand` uses the operator-derived common result
  signedness when widening, so one unsigned ternary/binary arm forces
  zero-extension of every arm into the common type; and
- `ForFoldCounterOperand` uses the closed language-version rule named by the
  independently derived transition-semantics row. For Veryl V0_20 its target
  is always signed two-state width 32. Start/end/initial and final step-result
  conversion use ordinary assignment rules, including source-signed widening,
  low-bit truncation, Logic-to-Bit conversion, and modulo-`2^32` wrap. Bound
  comparison is then signed width 32. The step RHS retains the operand context
  required by the exact shared `SourceForStepAssignmentOp`; shift counts remain
  self-determined and are never truncated to a host word.

Truncation is independent of the extension basis.  The verifier derives the
exact target width from the typed HIR context; a wire may not choose a wider
type merely because its extension kind is internally consistent.

SourceComparisonOperatorV1 =
  LessThan | LessThanOrEqual | GreaterThan | GreaterThanOrEqual

SourceComparison
  operator: SourceComparisonOperatorV1 / signedness / SourceCoercion

OccurrenceGatedOwner = GateResult(GateResultMergeId) |
                       DecisionStep(DecisionResultMergeId,
                                    arm: DecisionArmOrdinal)

OccurrenceGatedKey
  unit: ControlUnitId
  owner: OccurrenceGatedOwner
  condition / then_value / else_value: OccurrenceUse
  merge_site: ControlSite

DraftOccurrenceGatedKey
  same relation using private checked unit-local occurrence, owner, and site IDs

OccurrenceGatedMux
  key: OccurrenceGatedKey
  source: MappedSource(SourceRef<SourceGatedMuxId>, exact atom range)
  semantic_node: PhaseNodeId<OccurrencePhase>
  result: OccurrenceDef

ResolvedGatedMux
  unit: ControlUnitId
  semantic_node: PhaseNodeId<OccurrencePhase>
  result: InstDef
  condition / then_value / else_value: InstUse
  merge_site: ControlSite
  owner: GateResult(GateResultMergeId) |
         DecisionStep(DecisionResultMergeId, arm: DecisionArmOrdinal)

OccurrenceDecisionResultMerge
  unit / decision / merge_site
  source: MappedSource(SourceRef<SourceDecisionResultMergeId>, exact atom range)
  result: OccurrenceDef
  default_value: OccurrenceUse
  selected_arm_values: [OccurrenceUse]
  ordered steps: [(arm: DecisionArmOrdinal, arm_value: OccurrenceUse,
                   mux: GatedMuxId)]

ResolvedDecisionResultMerge
  unit / decision / merge_site
  result: InstDef
  default_value: InstUse
  selected_arm_values: [InstUse]  // one edge-specific use per occurrence arm
  ordered steps: [(arm: DecisionArmOrdinal, arm_value: InstUse,
                   mux: GatedMuxId)]
```

Before token SSA, every field shown above as `InstUse`/`InstDef` has the same
shape but contains `OccurrenceUse`/an occurrence definition. No source or
flattening producer may allocate an `InstValueId`. Resolution substitutes the
exact reaching tokens and emits a total occurrence-to-instance relation; the
final verifier reconstructs that substitution from the token analysis.
`OccurrenceAction` carries its exact semantic read/write/bind/effect summary
but no token-flow fields. `ResolvedControlAction` preserves the same checked
ID, owner, kind, operand/result arity, and semantic relation while replacing
occurrences with instances and adding the flows produced by
`SLTStateTokenAnalysis`; the final verifier checks both sides bidirectionally.
`ResolvedControlRoot` similarly resolves only its ordered operand occurrences.
These `Resolved*` records are verified views composed from the immutable
occurrence row and its same-ID overlay; the final artifact does not store a
second copy of topology, owner, identity, or action kind.

The source gated registry is not inferred from equal raw mux nodes. For every
`SourceGatedMux`, its `result` occurrence must be at `key.merge_site`, name
`semantic_node`, and list exactly `condition`, `then_value`, and `else_value`
in that order. The three operand occurrences must name the three raw mux child
nodes and their recorded use sites must be legal for the owner. `key.unit`,
the owner, all occurrences, and the merge site must belong to one source
control unit. A gate-result-owned key must match that result record's exact
gate condition, arm values, result slot, and merge site;
a decision-step key must match the corresponding merge step. Conversely every
source-arena `Gated(SourceGatedKey)` construction identity has exactly one
`SourceGatedMuxId`, and every source gated record names exactly that node and
key. Every source decision merge step has one gated mux, including constant
conditions and equal raw arm nodes. Before token SSA, raw node equality cannot
prove value equality because the occurrences may see different reaching
memory or environment states. A later rewrite may remove such a merge only
from complete versioned values and with its own input/output relation; no
producer-supplied elision flag is accepted as proof.

Every source use/definition is matched bidirectionally to one
`ExpectedSourceUseId`/`ExpectedSourceResultId` from the independently derived
expected value graph and to its primary owner/ordinal. `EvaluateHere` and
definition rows list the exact semantic-node operands in order. A `FixedValue`
use lists no reevaluated operands and names a preceding canonical producer ID
that is either `EvaluateHere` or a definition (never another `FixedValue`),
with the same semantic value/type/unit. It is legal at its recorded slot/edge;
its `ValueFlowReason` is derived from the owner role. Every definition is
referenced by its declared owner exactly once, while any number of checked
fixed uses may reach a producer. This relation, not raw node
equality, is the source-level def-use graph.

For each source action, the expected graph fixes the action kind, operand-role
order, result arity/order, and semantic access summary. Operand rows have
`ActionOperand(action, ordinal)` at slot `action_index`; result rows have
`ActionResult(action, result_ordinal)` at slot `action_index + 1`. The verifier
derives the summary from the typed action semantics and rejects missing or
conservative-looking extra domains/bindings/effects. A scheduled root's
operand list is an exact declared projection of its one root-bearing action;
metadata roots have no action. The occurrence verifier repeats the same
contract after source expansion/atomization and checks every mapped action
against its source expansion row.

For each `SourceDecisionResultMerge`, selected values cover the decision's arm
ordinals exactly once and `ordered_steps` lists those ordinals in reverse
priority. The first step's `incoming_value` is exactly `default_value`; every
later incoming use has
`FixedValue(canonical_producer_of[previous_step.result], MergeArm)`. At
each step, the referenced gated record derives exactly
`SourceGatedKey { unit, owner: DecisionStep(this merge, source_arm),
condition: predicate, then_value: selected_value, else_value: incoming_value,
merge_site }`, and its result definition equals the step result. The final
step result equals the merge result. Each decision-owned
source gated mux occurs in exactly one step.
Each step predicate is `FixedValue` of the canonical producer of that source
arm's dispatch predicate;
selected/default values are the exact edge-specific fixed values. No predicate
or arm read is reissued at the merge after arm effects.

Each `SourceGateResultMerge` is listed exactly once by its gate and derives
`SourceGatedKey { unit, owner: GateResult(this result), condition, then_value,
else_value, merge_site }`; its mux row has the same result. Gate-result and
decision-step memberships are disjoint and together cover the source gated
table. The result-merge ID makes two output slots distinct even when all raw
operands and sites happen to match.
The result record's condition use is at the merge/dataflow site and must be
`FixedValue(canonical_producer_of[gate.condition], MergeArm)`. It is a distinct
occurrence/site but
resolves to the exact header candidate/token version rather than reissuing a
condition read after arm effects.

`VerifiedSourceSemanticContext` is built by an independent iterative traversal
of the typed pre-symbolic HIR, not copied from provenance rows. It enumerates
the exact expected control constructs, roots, scheduled actions, result slots,
observers, runtime sites, patterns, and loop specifications. The source
aggregate verifier compares those specifications bidirectionally with
`SourceControlProvenance`; a producer cannot omit the same gate, root, action,
or observer from two self-declared tables and pass. `SourceControlPoint`'s slot
count must be `ordered_actions.len() + 1` and must equal the narrower source
occurrence-boundary row. Source wire decoding retains the canonical typed
semantic snapshot and derives the specifications again before checking
provenance.

Construction identity is not serialized, so source freeze also classifies
every mux node from independent semantics as exactly
`Gated(SourceGatedMuxId)` or `Ordinary`; the sets are disjoint and cover all
mux nodes. A gated node has exactly one gated record. Each expected
ordinary/pinned synthetic mux origin maps to exactly one ordinary node, but
several semantic origins may correctly share that canonical node. Every
ordinary mux node has one or more such origins, so an omitted gated record
cannot turn into origin-free ordinary dataflow. Occurrence freeze applies the
analogous node classification and many-origins-to-one-ordinary-node relation
from the complete `ExpectedOccurrenceGraph`, verified mapped source use/result
rows, atomization, glue, observer, pinned/runtime synthetic origins, and every
verified `OccurrenceRewriteRelation` output. Each ordinary mux origin maps to
one node and every ordinary mux node has a nonempty inverse origin list before
ordinary canonicality is checked. All non-mux nodes must be
`OrdinarySemantic`. A future control-owned
node kind requires a new explicit identity and registry rather than reuse of
`Gated`.

Classification alone is not liveness coverage. Source freeze marks every node
backward-reachable from all expected use/result producers, roots, actions,
control semantics, observers, dynamic plans, and nested templates; every arena
node must be marked. Occurrence freeze repeats this from
`ExpectedOccurrenceGraph`, verified rewrite outputs, mapped/synthetic control
rows, roots/actions, and nested templates. An unreferenced constant/add/mux or
other padding node is rejected even when structurally unique and ordinary.

The occurrence registry repeats the same bijection with
`OccurrenceGatedKey`; here each key contains the complete `OccurrenceUse`, not
just a value number, so edge-specific arm uses and their roles/sites cannot
alias. Token resolution preserves `GatedMuxId` and substitutes each exact
occurrence use/definition with its reconstructed `InstUse`/`InstDef`, producing
`ResolvedGatedMux`. Its separate resolved-gated key is
`(unit, owner, condition, then_value, else_value, merge_site)` with the full
versioned uses. That key belongs only to `ControlValueDraft` interning and
never allocates or reopens an SLT node.

A root has no single execution use. Dynamic read-modify-write has distinct
address, old-value, and RHS occurrences; an event has separate predicate and
ordered argument occurrences; a loop has outer bounds/initials and nested body
occurrences. Every executable operand therefore carries its own
`ControlUseSite` in its `ValueOccurrence.flow`. Every `OccurrenceUse` must name
a `Use` row and every `OccurrenceDef` a `Definition` row; the owner tables cover
a primary semantic owner exactly once. The ownership/projection rule is fixed:

| Value field | Primary use owner | Required projections of the same row |
| --- | --- | --- |
| action operand, including root RHS/old/address, runtime predicate/argument, observer trigger, and outer ForFold bound/initial | `ActionOperand(action, ordinal)` | action kind slot; scheduled root, dynamic plan, observer/runtime-site, or ForFold outer field as applicable |
| pure value operand | `ValueOperand(value, ordinal)` | phase node operand only |
| gate condition | `GateCondition(gate)` | gate row and control edge predicate |
| gate result condition/arms | `GateResultOperand(merge, role)` | gated key and mux operand slot |
| decision selector/pattern/arm/result-step field | its exact `Decision*` owner variant | decision semantics, edge predicate, merge step, or gated key named by that variant |

Source rows obey the identical table with source-prefixed IDs. Projection
tables contain checked references to the primary use; they never allocate a
second use or change its role/site. Every action-kind/dynamic/observer/ForFold/
root field is covered bidirectionally by exactly the table row applicable to
its variant. Reusing the same semantic node at another site creates
a distinct occurrence, so the dense occurrence-to-instance overlay cannot
collapse reads that see different tokens. Slot uses occur immediately before
their owning action only for `ActionOperand`; other slot/edge legality is
derived from its control owner.
Gamma, phi, gated-mux arm, and decision-arm inputs use the explicit incoming
`ControlEdgeId`, because no point slot can identify which predecessor supplied
them. Action and gated-merge results are fixed definitions at their recorded
slots; ordinary pure-value definitions are chosen later by the verified
placement plan and are not forged as occurrence definitions. Kept/contracted
state-specific use maps retain these same `InstUse` records rather than moving
a root-wide site.

The occurrence verifier applies the same closed def-use rules as the source
verifier. `EvaluateHere` rows mirror the phase node's ordered operands;
`FixedValue` rows have no reevaluated operands, match their preceding
canonical producer's semantic value/type/unit, and satisfy slot/edge
reachability. Every
definition owner and every primary use owner/ordinal points back to that exact
row. `OccurrenceDataSource`, `AddressSource`, and `PreviousValue` edges in the
action-order skeleton are derived by a memoized reverse walk of
`OccurrenceProducerDependencyDAG`. For each consumer action operand it
collects every upstream canonical producer owned by an action and propagates
the exact role/`ValueFlowReason` through arbitrary pure `EvaluateHere` chains.
Thus an `ActionResult -> pure expression -> action operand` dependency cannot
vanish because the fixed value was not adjacent; these are not self-declared
replacement edges. Final resolution requires a fixed use and its canonical producer to name the
same `InstValueId`, while an `EvaluateHere` row is instantiated from its exact
site and reaching tokens.

Actions are the only owners of scheduled-root execution and token flow; pure
`InstValue` evaluation is owned separately by the verified placement plan.
Version advance and publication are never standalone actions that can be
separated from the operation they describe: a `StoreRoot`, `BindEnvironment`,
`RuntimeEvent`, `InvokeRuntimeFunction`, or `ForFold` publishes its results and
output tokens atomically.
`ReadInput` records whether a load came from memory, a loop/function
environment, a statically assembled value, or a dynamic overlay; later passes
do not infer that distinction from a raw `SLTNode::Input`. `EvaluatePinned` is
reserved for proved non-movable computations, not a generic escape hatch for
a failed placement proof.

For a scheduled root, the root-facing projection of its action operands must
equal `ResolvedControlRoot.ordered_operands` role-for-role, value-for-value, and
site-for-site. Its action kind must carry that same root reference. Conversely,
each root-bearing `StoreRoot`, `RuntimeEvent`, `InvokeRuntimeFunction`, or outer
`ForFold` action is
named by exactly one scheduled root; helper read/capture/bind/pinned actions
are rootless. Definition/publication sites come only from the action result
records; there is no root-wide use or publication site.

Each action has at most one token flow for a given domain, binding, or effect
stream. An absent `outgoing` is a use only. A present `outgoing` must be the
unique `MayDef`, `Bind`, or `Action` token definition owned by that same action
and must name `incoming` as its predecessor. The action-kind verifier derives
the exact read/write/effect set from the semantic operation and rejects both a
missing flow and a conservative-looking extra flow; alias uncertainty is
represented by the verified global domain, not by inventing unrelated token
edges.

`ExternalRootId` is dense and append-only over the complete flattened artifact,
not module-local. `ControlActionId` and all three token-ID namespaces are
likewise artifact-global because global token and ordering edges name them
directly. The `unit` in every `ControlRootRef` and action must agree with its
record. Unit-local control/value IDs may not cross units; cross-unit execution
order is represented only by the global action graph defined in section 2.

Each control unit belongs to one expanded combinational execution instance;
flattening appends a unit with checked ID remapping rather than merging its
root into another unit. Cross-unit region, point, value, gate, decision, or mux
references are invalid. Every emitted `LogicPath`, observer root, and runtime
event root carries its `ControlUnitId` and `ExternalRootId`; membership is never
reconstructed later from a shared arena or artificial ordering between
declarations. Roles distinguish result, RHS, old value, dynamic address,
local/pre-lower input, guard, ordered argument, loop runner, position input,
and effect enable/action operands.

### Root identity and flattening lifecycle

Source modules and flattened artifacts use different namespaces:

```text
FrozenSourceCatalog
  canonical [FrozenSourceArtifact] indexed by SourceArtifactId

SourceInstance
  path: CanonicalInstancePath
  source_artifact: SourceArtifactId

SourceRef<LocalId> = (SourceInstanceId, LocalId)
SourceRootRef = SourceRef<SourceRootId>
SourceValueOccurrenceRef = SourceRef<SourceValueOccurrenceId>

MappedSourceRelations
  unit / input / value / action / gate / decision expansions
  write-domain / binding / effect-stream expansions
  gate-result / decision-result / gated-mux expansions with exact bit ranges
  root / observer / runtime-site / runtime-call-instance / execution-lineage /
    dynamic-plan / ForFold-template expansions
  complete SourceToOccurrenceRuntimeFunctionRelationV1, including four-role
    runtime root owners, local-scope rows, program slots/actions/formals/locals/
    inputs/results/targets/plans/coercions/events/fold rows, invoke owner
    variants/roles/writebacks, inverse origins, and boundary equality
  explicit synthetic-origin rows for every non-source object

SourceUseExpansion
  source instance / SourceValueOccurrenceId / ExpectedSourceUseId
  nonempty canonical emitted rows:
    (ValueOccurrenceId, ExpectedOccurrenceUseId, Whole | exact BitRange,
     mapped owner/role/site and action operand/result context)

SourceResultExpansion
  source instance / SourceValueOccurrenceId / ExpectedSourceResultId
  nonempty canonical emitted rows:
    (ValueOccurrenceId, ExpectedOccurrenceResultId, Whole | exact BitRange,
     mapped definition owner/site/action result ordinal/gated-result owner)

SourceProducerExpansion
  SourceRef<SourceCanonicalProducerId> -> nonempty canonical emitted
    CanonicalProducerIds with Whole/exact BitRange

SourceControlProvenance
  module-local source roots in source order

ControlOccurrencePlan / ControlResolutionOverlay
  flattened artifact-global ControlRootRefs and ControlActionIds

RootExpansion
  source: SourceRootRef
  unit: ControlUnitId
  emitted: nonempty [RootExpansionEntry]

RootExpansionEntry
  root: ControlRootRef
  slice: Whole | BitRange(lsb, msb, source root/result ordinal)
  atom_ordinal / inverse ControlRootIdentity

ActionExpansion
  source: SourceRef<SourceControlActionId>
  unit / optional RootExpansionId
  primary emitted: nonempty [ActionExpansionEntry]
  rootless helpers: canonical [HelperExpansionEntry]

ActionExpansionEntry
  action: ControlActionId
  slice: Whole | BitRange(lsb, msb, source result ordinal)
  optional exact RootExpansionEntry for root-bearing actions
  primary ordinal / inverse OccurrenceAction origin

  InvokeRuntimeFunction requires the sole `Whole` entry and an exact
    `OccurrenceRuntimeFunctionCallInstanceV1` plus
    `OccurrenceRuntimeFunctionInvokeV1` outer-owner inverse; its individual
    return/output/inout result projections may atomize only after the invoke
    boundary completes

HelperExpansionEntry
  action: ControlActionId / typed purpose / helper ordinal
  rule: HelperDerivationRule
  scope: SharedWhole | Primary(primary ordinal)
  slice: Whole | exact BitRange matching that scope's source/result ordinal
  exact source expected-use/result projections, derived action kind/slots,
    result type, and SemanticAccessSummary
  inverse OccurrenceAction origin

HelperDerivationRule
  finite versioned enum for required read-input, dynamic-address,
    old-value capture, environment-bind, and pinned-evaluation helpers
  each variant deterministically derives the helper action kind, operand/result
    slots, types, owner site, and access summary from the source action plus
    verified flattened semantics

ControlActionCoordinate
  Mapped(instance path, source control-unit/action ordinals,
         Whole or atom lsb, primary/helper ordinal) |
  Glue(connection ordinal, action kind/ordinal) |
  Observer(observer/occurrence ordinals, action kind/ordinal) |
  Synthetic(synthetic kind/ID, action ordinal)

ControlRootCoordinate
  Mapped(instance path, source root ordinal, atom lsb) |
  Glue(connection/root ordinal) | Observer(observer/occurrence ordinal) |
  Synthetic(synthetic kind/ID, root ordinal)

RootOrderBarrier
  before: RootExpansionId or singleton derived root
  after: RootExpansionId or singleton derived root
```

Catalog module keys are unique and strictly canonical; source instances are
unique by canonical path, strictly ordered by that path, and name an existing
catalog entry. Resolving a source root/occurrence/observer reference first
checks the instance row and then checks the module-local ID in that exact
source artifact. A bare module-local ID is never accepted at an occurrence
boundary.

`MappedSourceRelations` is verified by source-object kind, not as an untyped
bag. Each instantiated source unit, input, gate, decision, observer, runtime
site, runtime call instance, execution lineage, dynamic plan, and loop template
has exactly one mapped owner row.
Roots, values, gate/decision result merges, and gated muxes may
atomize only into a nonempty canonical sequence of disjoint bit ranges whose
ordered union is the complete source result range. Every mapped row points
back to exactly one source ref and range; every non-mapped row has one explicit
synthetic/glue/observer origin. Swapping equal-shaped source objects, dropping
one and inventing another, or losing a bit range therefore fails the
bidirectional relation.

Use and result mapping is never inferred from the generic value range alone.
`SourceUseExpansion` and `SourceResultExpansion` cover every instantiated
expected source use/result exactly once; each output expected occurrence row
has one inverse. Atomized rows are strictly ordered, disjoint, nonempty bit
ranges whose union is the complete typed source result, while a `Whole` row is
the sole row for that source slot. Owner kind, role, control site, operand or
result ordinal, and gate/decision merge owner are mapped field-by-field.
`SourceProducerExpansion` is derived from those verified flow rows, not supplied
as an independent equivalence claim, and proves that no fixed use can switch
to another equal-shaped producer during flattening.

Action cardinality is kind-specific. A `StoreRoot` expansion is zipped with
its `RootExpansion` target slices even though the action has no result row; a
value-producing action slices an explicit result ordinal; `RuntimeEvent`,
binding/effect publication, and unsliced ForFold actions use `Whole` and have
exactly one primary expansion. Each emitted scheduled root names exactly one
primary action in the corresponding expansion. Read/capture/address helpers
are rootless but belong to that expansion with a typed purpose/ordinal. A
`SharedWhole` helper is emitted once and may project to every primary; a
`Primary(i)` helper has exactly that primary's slice and inverse. Global
action IDs are assigned by `ControlActionCoordinate`, which totally orders
mapped, rootless, glue, observer, and synthetic actions without inventing a
source-root ordinal for objects that have none.

The lifecycle is fixed:

1. After logic-path extraction, assign module-local `SourceRootId`s in source
   order. Assign observer-definition IDs in their separate table and verify the
   complete source artifact; observer definitions are not source roots. Insert
   each immutable source artifact once into `FrozenSourceCatalog` in canonical
   source-module order. Bare module-local IDs never cross that catalog
   boundary.
2. Traverse hierarchy in canonical `InstancePath` order and first commit one
   `InstanceRegistryTxn` per instance. It allocates/deduplicates that
   instance's mapped input, write-domain, binding, effect-stream, observer
   definition, and runtime-site rows exactly once. Multiple source control
   units in the instance use those existing IDs; observer roots/actions are
   still materialized only in step 8. A failed instance transaction commits
   none of these registries.
3. Map one entire source control unit
   into a temporary occurrence-valued draft; ordinary semantic-node and
   `DraftOccurrenceGatedKey` caches are distinct.
4. Atomize the draft before assigning final roots. One source `LogicPath` may
   expand to several final paths, recorded by `RootExpansion`.
5. Atomically commit the unit's nodes, value occurrences, control objects,
   roots, and actions. Allocate fresh artifact-global `ExternalRootId`s by
   `ControlRootCoordinate` and `ControlActionId`s by
   `ControlActionCoordinate`. A failed draft commits none of
   these registries. Every mapped source occurrence uses
   `SourceValueOccurrenceRef`; its `SourceInstanceId` must equal the one in the
   owning `ControlUnit.origin`, whose catalog row supplies the source artifact
   and canonical instance path.
6. Represent a durable source ordering edge `A -> B` by one checked
   `RootOrderBarrier`. In the global graph it becomes a virtual barrier node
   with every action in `expansion(A)` entering it and every action in
   `expansion(B)` leaving it. This proves the Cartesian ordering relation in
   `O(|A| + |B|)` edges rather than materializing `O(|A| * |B|)` pairs.
7. Constant inlining/rewrite preserves every `ControlRootRef` and every gated
   mux before token SSA; it may rewrite only ordinary nodes. A pass that
   changes node/root cardinality emits `OccurrenceRewriteRelation` against the
   independent `ExpectedOccurrenceGraph`; that witness is retained in
   `FrozenOccurrenceArtifact`/`OccurrenceWire` and verified again on decode.
   It never renumbers an existing registry in place. Gated value
   equivalence is considered only after versioned values exist.
8. Materialize observer metadata and occurrences as derived roots with
   `ObserverMetadataOrigin`/`ObserverOccurrenceOrigin`, append their actions,
   and verify every observer relation. They are not entries in
   `RootExpansion`, which is reserved for source roots.
9. Verify the final mapped SLT arena after every node-producing mapper,
   rewrite, glue unit, and observer action has completed. Then freeze the arena
   and the root/action identity, ownership, and occurrence registries together.
   Later token resolution may only attach a verified resolved record to the
   same action ID; no later pass allocates a node, reuses an action identity,
   or renumbers an external ID.

Parent/child port glue is a synthetic checked control unit per concrete port
connection, not an unowned path. `LogicPathId` and vector position are
temporary construction coordinates and never durable identity or serialized
ordering. Hash-map iteration is not an ordering source anywhere in this
lifecycle. Dense root/action ID order is a reproducibility property, not
semantic execution order; only control edges and `RootOrderBarrier`s carry
ordering semantics.

The predicate regions are SESE ownership regions; scheduling and dominance use
the explicit control-point CFG and its dominator tree. `GateHeader`, `Join`,
and `Continuation` are distinct even when a first lowering could place them at
the same machine offset. This represents sequential gates in one parent region
without pretending that every action in the parent executes at one point.
The verifier also builds post-dominance. A region's entry dominates all of its
points, its exit post-dominates all of them, every edge entering the region from
outside targets its entry, and every edge leaving it originates at its exit.
For a gate it checks the complete shape: the header has exactly the true- and
false-arm entry successors, each arm exit reaches the declared join, and the
join reaches the declared continuation with no bypass edge. Decision arms and
default obey the analogous recorded `dispatch_header -> arm/default -> join ->
continuation` shape. These are the SESE facts on which arm
exclusivity and laminar contraction rely; reachability or entry dominance alone
is not accepted.

There are three semantic gated-key namespaces: source, occurrence, and
resolved-value. Their physical construction indices are distinct: one source
builder index; one `DraftOccurrenceGatedKey` index per unit draft; a translated
`OccurrenceGatedKey` index in the transaction overlay/global occurrence
builder; and a resolved-gated index in `ControlValueDraft` over versioned
`(condition, then_value, else_value, owner, merge_site)` uses. The source,
draft, and global occurrence indices allocate SLT mux nodes and are discarded
with their builders/freezes. The resolved index allocates no SLT node and is
dropped at final freeze. None
silently reuses an identical raw mux owned by an unrelated gate or two
same-shaped muxes whose reads/bindings resolve to different tokens. Ordinary
pure SLT nodes retain semantic structural interning. All IDs use checked allocation;
exhaustion is a structured producer error.

Control/table ID fields are private `u32`s. Builders use
`u32::try_from(length)` and return `IdExhausted { kind, attempted_length }`;
phase-node IDs, widths, and counts remain checked `usize` and acquire no
artificial 32-bit limit. Forward references use
fallible `reserve`/`define` slots, and `finish` rejects every undefined or
doubly defined slot before exposing the artifact.

The symbolic evaluator passes an explicit source region/site through statement
and expression evaluation. It does not keep an implicit mutable "current
gate" stack in the arena. Flattening maps each verified source gate/decision
identity to a fresh `GateId`/`DecisionId` for one module-expanded or unrolled
execution instance. Output muxes from one symbolic merge have distinct
`SourceGatedMuxId`s but share their exact source gate/decision owner; mapping
gives each one a distinct `GatedMuxId` under the corresponding fresh owner.
Another module instance or unrolled execution receives different final IDs. A
source `case` creates one source decision, not a binary
chain that later has to guess selector identity. One source arm retains its ordered
nonempty list of equality/wildcard or half-open/inclusive range patterns; the
arm predicate is their disjunction, and source arms retain first-match order.
Provenance keeps the language's source matching semantics and the exact
predicate instance for each pattern even when it is four-state and ineligible
for software control. Only a later formation proof may produce a canonical
`TwoStateDisjoint` or `TwoStatePriority` decision. Ternaries and source `if`
statements create `Gate` entries with the language's exact condition semantics.
Compiler-synthesized muxes not owned by source control require an explicit
`PinnedSyntheticMuxOrigin` demanded by the independent source semantic context
and remain pinned to dataflow-select semantics in this schema. Legacy muxes remain in
`LegacyStructuralArena` and cannot enter this planner; there is no second
correctness fallback inside the new pipeline.

Observer definition and scheduled capture occurrence are different objects.
`CombObserver` becomes one `ObserverMetadata` root with `MetadataOnly`
disposition. Each generated `LogicPathTarget::CombCaptureEvent` is an
independent scheduled `RuntimeEventOccurrence` root:

```text
ObserverMetadata
  observer: ObserverId
  source: MappedSource(SourceObserverRef) | Synthetic(SyntheticOriginId)

ObserverOccurrence
  observer: ObserverId
  source: MappedSource(SourceObserverOccurrenceRef) |
          Synthetic(SyntheticOriginId)
  kind: Primary |
        Trigger { triggering_root: ControlRootRef,
                  activation_group, occurrence_ordinal }

RuntimeEventSite
  site: RuntimeEventSiteId
  source: MappedSource(SourceRef<SourceRuntimeEventSiteId>) |
          Synthetic(SyntheticOriginId)
  exact predicate/argument/termination semantics
```

Every observer has exactly one primary occurrence. For every distinct
`(activation_group, triggering root)` and every member of that group there is
exactly one trigger occurrence in canonical ordinal order. Guard, arguments,
loop runner, consume-enabled behavior, fatal behavior, and site ID must agree
with the observer and `RuntimeEventSite` definition. A `RuntimeEventSite` is a
definition-table row, not a root. An `SLTForEffect` is an action inside the
owning `ForFold` template, not a top-level root.

`SourceObserverId`, `SourceObserverOccurrenceId`,
`SourceRuntimeEventSiteId`, `ExpectedSourceRuntimeCallInstanceId`,
`RuntimeSourceExecutionLineageId`, `SourceDynamicAddressPlanId`, and
`SourceForFoldTemplateId` are module-local.
For each `SourceInstanceId`, mapping allocates fresh artifact-global
`ObserverId`, `ObserverOccurrenceId`, runtime-site, runtime-call-instance,
execution-lineage,
`DynamicAddressPlanId`, and `ForFoldTemplateId` rows and records a total
source-reference relation. The occurrence verifier
checks those relations bidirectionally; a source observer, runtime site,
runtime call, lineage, dynamic-address plan, or loop template cannot disappear,
and no
artifact-global row can be invented without an explicit verified synthetic
origin.

The three verifiers are written before their corresponding producers. The
source verifier checks only source IDs, semantic nodes, source sites, and
source structure. The occurrence-plan verifier checks flattened IDs, actions,
roots, sites, and semantic read/write sets without reading token or
`InstValue` tables. The final verifier additionally checks token flows and the
occurrence-to-instance resolution. Raw schemas and verified schemas are
different Rust types. Aggregate verifiers
consume raw rows and return private verified tables; a successful
`verify(&raw) -> ()` that leaves the caller holding forgeable raw records is
not a phase boundary. Derived plans borrow branded handles from their owner,
and cross-artifact handle composition returns a structured brand error.

Between them the verifiers check:

- every control unit has one rooted region tree and every non-root region has
  one owner, with no cross-unit references;
- control points form the recorded CFG, action order is total within each
  point, every unit is a finite single-entry/single-exit DAG, and computed
  dominance/post-dominance agrees with every SESE region;
- every scheduled root names exactly one action in the same unit, every
  metadata root names none, every action owner/slot is exact, and every root
  and action operand has its own valid role-specific use site; each
  root-bearing action has exactly one matching root, the root operand
  projection is exact, and helper actions are rootless;
- every runtime-function invoke has exactly one call instance and parent-first
  execution lineage; its role ranges, setup order, shared-program boundary,
  nested calls/folds, prepared targets, result order, writebacks, and complete
  semantic summary agree bidirectionally with the expected source/occurrence
  call graph, and no ForFold creates an execution-lineage row;
- in the final artifact, each action's semantic read/write/effect set agrees
  exactly with its per-domain token flows, every outgoing action token is
  defined by that action, and no `(action, domain/binding/stream)` flow is
  duplicated;
- gate/decision children name that owner and have the declared parent;
- each gate/decision header, join, and continuation belongs directly to its
  parent region, while every arm/default entry and exit belongs directly to
  the corresponding child region;
- every gated mux's semantic node exists, its result and ordered operand
  instances reconstruct that exact mux at the recorded merge site, and its
  owner is exact;
- every source pattern's recorded predicate exactly implements its wildcard or
  range rule, an arm predicate is their nonempty disjunction, and every
  decision result merge preserves source priority/default order;
- selector, patterns, and conditions have nonzero and compatible widths;
- condition semantics agree with the source construct and any claimed
  `KnownTwoState` fact;
- all control entries are reachable from their unit entry and all instantiated
  values/actions are backward-reachable from that unit's scheduled roots or
  recorded control semantics;
- every value occurrence has exactly one origin variant; mapped/atomized rows
  resolve through the owning `SourceInstance`, glue rows match the immutable
  connection table, observer rows match one source observer occurrence, and
  synthetic rows are demanded by the independent semantic context, with no
  optional or default source relation;
- every source root has one nonempty verified `RootExpansion`; the final root
  registry is dense, deterministic, and in exact one-to-one correspondence
  with atomized logic paths, observer metadata records, and scheduled observer
  occurrences by external ID, kind, disposition, role-tagged operand order,
  action, and unit, with no missing, duplicate, or extra root;
- source-expanded roots and observer-derived roots have disjoint, exact origins;
  roots and actions commit/freeze together, and every root-order barrier names
  complete existing expansion/singleton sets without a Cartesian edge table;
- each observer/site/group has exactly the primary and trigger occurrences
  specified above, and no metadata-only observer enters the scheduled graph;
- every occurrence use resolves to the exact structurally versioned value for
  its semantic node, ordered operands, use site, and reaching tokens, with no
  unresolved, multiply resolved, or extra instance; and
- serialization/deserialization validates canonical caches without changing
  IDs or retaining a cache in a frozen arena.

Unit-CFG acyclicity is checked with a worklist/topological count, without a
depth or iteration cap. A runtime invoke remains one outer DAG action with a
separately verified nested program. `OccurrenceRuntimeFunctionCallInstanceV1` maps every
program-relative value, action, target handle, nested call/fold, exit value, and
effect token both ways through the complete
`SourceToOccurrenceRuntimeFunctionRelationV1`; the Root-or-RuntimeCall lineage
is remapped, `Body` remains `Body`, and a source ForFold template ID is remapped
to its exact occurrence `ForFoldTemplateId`. The outer
token transfer is the exact nested entry-to-exit summary, not a producer-supplied
may-effect set. Nested program construction is completed in reverse topological
runtime-call-graph order; equal type specializations share immutable program
rows, while equal argument values never merge instances. The resolved artifact
retains this boundary and mapping rather than materializing an unchecked
recursive inline tree.

A source loop already expanded by the Veryl emitter has
disappeared into finite ordinary source HIR and owns no template. A retained
runtime `ForFold` remains one pinned outer action/value and never introduces a
backedge into the outer control-point graph. Its runtime loop is nevertheless
explicit in a separately verified nested template; this is not the
constant-function certificate VM:

```text
OccurrenceForFoldTemplate
  template: ForFoldTemplateId
  unit: ControlUnitId / owner_action: ControlActionId
  origin: MappedSource { source: SourceRef<SourceForFoldTemplateId>,
                         body_mapping: SourceToOccurrenceForFoldRelation }
  counter: { binding, exact signed 32-bit Bit facts,
             range_inputs:
               Single { value_use: OccurrenceUse,
                        SingleForward | SingleReverse } |
               Between { start_use: OccurrenceUse, end_use: OccurrenceUse,
                         ForwardExclusive | ForwardInclusive |
                         ReverseExclusive | ReverseInclusive },
             update: DefaultByRangeDirection |
                     Explicit(step OccurrenceUse,
                              SourceForStepAssignmentOp) }
  states: [OccurrenceForFoldState]
  body: OccurrenceForFoldBody
  expected: ExpectedOccurrenceForFoldGraph
  result_state: state_index
  exact_read_domains / exact_write_domains
  exact_environment_reads / exact_environment_writes
  exact_effect_streams

OccurrenceForFoldState
  target
  initial_use: OccurrenceUse
  update_use: FoldOccurrenceUse

OccurrenceForFoldBody
  loop_binding / state_bindings
  topology: OccurrenceFoldControlTopology
  FoldActionId actions/effects and FoldValueOccurrences
  FoldCanonicalProducerRelation / FoldProducerDependencyDAG
  FoldDynamicAddressPlans / FoldRecurrenceRelation
  parallel_updates
  header_condition: FoldOccurrenceUse
  continue_use: FoldOccurrenceUse
  transition_outcome: FoldOccurrenceUse
  exit / backedge

OccurrenceFoldControlTopology =
  FoldControlTopology<FoldPredicateRegionId, FoldPointId, FoldEdgeId,
                      FoldOccurrenceUse, FoldActionId>

ExpectedOccurrenceForFoldGraph
  derived only from the mapped ExpectedSourceForFoldGraph plus verified type/
    atomization rules
  complete ExpectedFoldUseId/ExpectedFoldResultId and private control/action/
    value/dynamic/recurrence rows

SourceToOccurrenceForFoldRelation
  total mappings for source-fold regions, points, edges, actions, values,
    expected uses/results, canonical producers, dynamic plans, state/update/
    header/continue/transition-outcome/result slots and exact outcome patterns
  Whole/exact atom ranges and inverse coverage for every mapped nested row

FoldControlUseSite = Slot(FoldPointId, slot) | Edge(FoldEdgeId)

FoldValueOccurrence
  semantic occurrence-phase node or verified fixed private runtime leaf
  flow: FoldValueFlow
  ordered_operands: [FoldOccurrenceUse]

FoldValueFlow
  Use { semantic_use: ExpectedFoldUseId,
        site / owner: FoldUseOwner / role,
        value_source: EvaluateHere |
          FixedValue(FoldCanonicalProducerId, ValueFlowReason) }
  Definition { semantic_result: ExpectedFoldResultId,
               site,
               owner: OuterEntry(outer OccurrenceUse) |
                      HeaderParam(Counter | State(state ordinal)) |
                      ExitParam(State(state ordinal)) |
                      ActionResult(FoldActionId, result ordinal) }

FoldUseOwner
  ValueOperand(FoldValueOccurrenceId, operand ordinal) |
  ActionOperand(FoldActionId, operand ordinal) |
  HeaderCondition | ContinueCondition | TransitionOutcome |
  RecurrenceUpdate(Counter | State(state ordinal))

FoldOccurrenceUse / FoldOccurrenceDef
  checked template-scoped newtype views

FoldAction
  private owner point/slot, ordered FoldOccurrenceUse operands and
  FoldOccurrenceDef results
  exact SemanticAccessSummary
  kind: NestedActionSemanticKind<OccurrenceInputId, BindingId,
          FoldPreparedTargetHandleId, FoldDynamicAddressPlanId,
          EffectStreamId, RuntimeEventSiteId,
          OccurrenceRuntimeCallInstanceId,
          FoldControlUseSite, FoldInputResolution>

FoldCanonicalProducerRelation / FoldProducerDependencyDAG
  one FoldCanonicalProducerId per EvaluateHere/Definition producer and one
  producer per FoldValueOccurrence; FixedValue rows name it directly
  ordered per-iteration operand edges only; OuterEntry and HeaderParam are
  operand-free leaves, so the producer graph is a DAG

FoldRecurrenceRelation
  one row for counter and every state HeaderParam
  exact OuterEntry definition, header leaf, parallel update producer, unique
    TransitionAdvance backedge edge/slot; each state names one ExitParam with
    edge-specific HeaderExit=current-header and ContinueExit/
    TransitionRangeExit=parallel-update operands
  all three edge uses are retained and ordered by FoldEdgeId even when two
    values are equal
  outer-result projection names only the selected state ExitParam; counter has
    no exit parameter
  the update-to-header recurrence is verified here and is never an operand
    edge in FoldProducerDependencyDAG or FoldInstValueKey

FoldDynamicAddressPlan
  plan: FoldDynamicAddressPlanId / owner FoldActionId
  source: SourceFoldDynamicAddressPlanId
  object/type/width, ordered typed FoldOccurrenceUse indices, dimensions,
    exact part-select geometry, offset, address-known, bounds, access guard,
    access semantics, and result/action projection

FoldMemoryTokenDefKey = Entry(WriteDomainId) |
  ActionDef(WriteDomainId, FoldActionId) |
  ControlPhi(WriteDomainId, FoldPointId) |
  HeaderPhi(WriteDomainId, FoldPointId) |
  ExitPhi(WriteDomainId, FoldPointId)
FoldEnvironmentTokenDefKey = Entry(BindingId) |
  ActionDef(BindingId, FoldActionId) |
  ControlPhi(BindingId, FoldPointId) |
  HeaderPhi(BindingId, FoldPointId) |
  ExitPhi(BindingId, FoldPointId)
FoldEffectTokenDefKey = Entry(EffectStreamId) |
  ActionDef(EffectStreamId, FoldActionId) |
  ControlPhi(EffectStreamId, FoldPointId) |
  HeaderPhi(EffectStreamId, FoldPointId) |
  ExitPhi(EffectStreamId, FoldPointId)

FoldMemoryTokenDef / FoldEnvironmentTokenDef / FoldEffectTokenDef
  typed Entry, ActionDef, and Phi rows in their separate namespaces
  every Phi names its exact FoldPointId and one incoming token per predecessor
    FoldEdgeId in stable order
  ControlPhi is permitted only at a non-header/non-normal-exit body join;
    HeaderPhi and ExitPhi are the specialized recurrence/boundary rows

ForFoldTokenOverlay
  same template ID
  outer incoming action tokens -> private entry FoldTokenIds
  header phis for every loop-carried memory domain, environment binding, and
    effect stream / backedge tokens / exit FoldTokenIds
  private exit tokens -> outer outgoing action tokens
  exact nested action flows and summary equality proof

  each namespace's keys are allocated densely in lexicographic order into its
    separate FoldMemory/FoldEnvironment/FoldEffect token table before
    definitions are filled; each HeaderPhi checked slot is defined exactly
    once with entry and unique backedge operands in FoldEdgeId order
  every outgoing state namespace has one exit merge: `Common(token)` when the
    HeaderExit/ContinueExit/TransitionRangeExit inputs are identical, otherwise
    an ExitPhi with all three edge operands in FoldEdgeId order (equal edge
    values are not omitted); the outer outgoing action token names
    only this verified merge

FoldResolvedOperand = Outer(InstValueId) | Local(FoldInstValueId)

FoldPlacementClass = Ordinary(FoldCanonicalProducerId) |
                     HeaderParam(Counter | State(state ordinal)) |
                     ExitParam(State(state ordinal)) |
                     ActionResult(FoldActionId, result ordinal)

FoldValueCandidate
  transient FoldValueCandidateId allocated in deterministic
    FoldCanonicalProducerId topological order with producer-ID tie break
  producer / semantic origin / FoldPlacementClass
  ordered producer operands / exact private token reads /
    required FoldPredicateRegionId

FoldExecutionSafety
  producer: non-OuterEntry FoldCanonicalProducerId
  classification: Total | DomainRestricted(FoldPredicateRegionId)
  witness: ExecutionSafetyProofId owned by this template/producer

FoldInstValueKey
  template / semantic origin / FoldPlacementClass
  ordered_operands: [FoldResolvedOperand]
  canonical strictly ordered direct memory/environment FoldToken pairs
  execution_domain: Total | DomainRestricted(FoldPredicateRegionId)

FoldInstValueFacts
  memory_dependencies: FoldMemoryDependencyId
  environment_dependencies: FoldEnvDependencyId

FoldProducerResolution
  producer: FoldCanonicalProducerId / result: FoldResolvedOperand

FoldOccurrenceValueResolution
  occurrence: FoldValueOccurrenceId
  producer: FoldCanonicalProducerId
  result: FoldResolvedOperand

FoldPlacementPlan
  one row for every Local(FoldInstValueId): value / exact FoldControlSite
  Ordinary values use template-local ScheduleEarly/ScheduleLate bounds;
    HeaderParam, ExitParam, and ActionResult use their unique fixed sites
  exact operand/token-def dominance, execution-region, and per-iteration
    def-before-use relation over OccurrenceFoldControlTopology

ForFoldValueResolutionOverlay
  same template ID
  non-Outer-producer-indexed normalized safety/witness and total
    FoldProducerResolution
  FoldInstValueKey/Facts tables and total FoldOccurrenceValueResolution
  resolved nested action/dynamic-plan uses
  body exit result -> outer ForFold action result relation

ForFoldOutcomeBoundary
  NormalExit { selected state ExitParam -> outer action result,
               normal token merges -> outer outgoing flows }

ResolvedForFoldTemplate
  verified view of OccurrenceForFoldTemplate plus token/value/placement overlays
```

All raw `Fold*Id` tables are physically nested inside their owning template on
the wire and in memory; no bare fold-local ID appears in an artifact-global
table. `OuterEntry` may reference only an outer operand of the owning ForFold
action, which dominates the template, never that action's result. Header
counter/state values are opaque fixed leaves. The sole semantic cycle is the
checked `FoldRecurrenceRelation`; candidate/value interning remains
topological. The outer result is related only by the exit-result boundary and
does not become an operand back into the template.

An `OuterEntry` canonical producer has no `FoldValueCandidate` or
`FoldInstValueId`: its producer-resolution row is exactly
`Outer(outer_occurrence_resolution[owning_action_operand])`. Every other
producer has exactly one candidate and resolves to `Local(FoldInstValueId)`.
The occurrence relation composes this partition with
`FoldCanonicalProducerRelation`; an outer/local tag mismatch, a bridge value,
or a local value shared between templates is invalid.

The template-local placement verifier computes dominators on the private CFG
including its one backedge and checks each `FoldInstValue` independently of the
outer acyclic placement. A local operand definition and every referenced
memory/environment token definition must dominate the selected site and all
uses; restricted values remain inside their exact FoldPredicateRegion. Header
params are defined at Header, action results immediately after their action,
and ExitParams only at NormalExit. Ordinary evaluation is placed once per
iteration at the latest legal site. No placement row may hoist a local value to
the outer unit or move a tokenized read across a loop action/phi.

The aggregate verifier inverts every outer `ActionSemanticKind::ForFold` row:
each source and occurrence template has exactly one same-unit owner action,
that action names exactly that template/result slot, mapped owners agree with
`ActionExpansion`, and every OuterEntry references one of that action's exact
operand projections. A template cannot be shared by two actions or cross a
control unit. Normal token and result boundaries name only this owner.

Within a fold, action operands are primary `ActionOperand` rows and nested
dynamic/action-kind fields project those same rows exactly as in the outer
ownership table. Only a pure value operand, header/continue condition,
transition outcome, or counter/state recurrence-update use has its
corresponding non-action primary
owner. The expected
fold graph fixes every owner/site/ordinal and the source-to-occurrence body
relation maps them field-by-field.

Start, end, and initials execute once at outer-action operand sites. At each
iteration entry the template binds counter and state values; effects execute
in source order; every update and the continue condition reads the same
iteration-entry bindings; updates are parallel; a false continue exits with
the already computed next states. A true continue evaluates the closed
`TransitionOutcome`: only `Advance` updates the counter and takes the backedge,
while `NormalRangeExit` takes the normal exit with next states. Zero,
nonprogress, direction change, and wrap cannot select an error path because no
such path exists; an updated in-range counter, including an unchanged one, is
`Advance`. The occurrence template owns a private
site/control namespace and is verified, including exact access/effect
summaries, before `FrozenOccurrenceArtifact`; the outer action skeleton
consumes those summaries. Step 3 then builds its private token overlay. Every
loop-carried binding/domain/effect stream has exactly one header phi with the outer
entry token and the unique backedge token as its two inputs; normal exit tokens
merge all three normal predecessor edges exactly. Only its outer operands, exact may-read/may-write and
environment summaries, summarized effect flow, and result may cross into the
enclosing control unit. The resolution verifier matches all nested read,
write, binding, and effect summaries bidirectionally to the frozen outer
`ForFold` action and occurrence template, including incoming tokens for reads.
It does not build a new semantic summary after token SSA. A body read can
therefore never be moved across
an outer write merely because the body itself does not write that domain.
All `Fold*Id`s are interpreted only with their owning `ForFoldTemplateId`; no
private point/edge/action/occurrence/value/token ID may enter an outer table.
The outer unit CFG remains acyclic because the sole backedge exists only in
the nested graph. The final verifier proves the two boundary relations: global
incoming/outgoing action tokens equal the nested entry/exit token overlay, and
the nested exit state selected by `result_state` exactly defines the outer
ForFold action result.

`PhaseSLTNodeFactsV1<P>` is the prerequisite and retained fact artifact for each
new phase; it is not a recursive `get_width` and cannot be paired with another
phase's arena. Existing lowering temporarily keeps an explicitly legacy fact
adapter until the step-4 switch. Every phase arena is a canonical append-only
DAG: each child ID is strictly smaller than its owner ID. Producers allocate
completed operands before their users; forward
references, self references, and cycles are noncanonical IR and fail
`GRAPH.CHILD_PRECEDES_OWNER`. This is a representation invariant, not a graph
size cutoff.

Verification first scans all edges without dereferencing an unchecked ID, then
computes the complete node facts and lowerability in arena order with checked arithmetic. It
needs no reverse-edge CSR, Kahn queue, recursion, or `Option<usize>` table, so
its persistent facts are one fixed `PhaseSLTNodeFactV1<P>` row per node:

```text
PhaseSLTNodeFactV1<P>
  width: nonzero usize / signed / positive_type
  static_domain: Bit | Logic
  value_class: Evaluation | MaterializedStorage
  mask_class: AlwaysZero | MayCarryXZ
  lowerable: bool
```

There is no separate type lookup which can disagree with this row. The exact
closed node sum, coercion ownership, per-variant width/signed/positive/domain/
value/mask transfer table, and `PhasePrimitiveV1` lowerability inventory are
normatively defined in
[Source semantic objects and input accesses](./source-semantic-inputs.md).
Replay implements that table with an exhaustive match; this document does not
define a shorter competing rule. In particular, `Coerce` is a first-class node,
every unary/binary/mux operand is a `PhaseValueUseV1` with an independently
verified coercion, contextual mux width may exceed the maximum natural arm
width, and a materializing Bit coercion is the only ordinary coercion allowed
to clear X/Z. Equality, relational, logical, wildcard equality, and wildcard
inequality produce width one; concat uses checked nonzero addition; selector
and condition uses require the exact width/coercion stated by the expected
graph. This same fact table becomes the sole width API so verifier and lowering
cannot disagree or panic on malformed IDs, underflow, overflow, or a deep
graph.

Signedness is likewise the one normative transfer relation: arithmetic and
bitwise binary results use the raw-natural signedness rule while extension uses
the common verified coercion target; shifts inherit the raw left signedness;
comparison/logical/reduction/concat/slice results are unsigned. A whole-object
input read and a read selected only through unpacked dimensions preserve the
declared result signedness; any packed bit/part select is unsigned even when its
range covers the complete packed width. This derives from access provenance,
not a flat range comparison. Dynamic unpacked-array extraction is one exact
semantic input row, not a signed whole-object input plus a generic `Slice`.
ForFold state initial/update coercions use assignment materialization.
The raw source proposal retains the original arbitrary-width bound and step
references without allowing them to choose the counter type. During semantic
relation step 2, the joint typed verifier derives their exact values and the
signed-32 bound coercions, comparison, and complete
`+= -= *= /= %= &= |= ^= <<= >>= <<<= >>>=` step-operator relation in the
independently verified `SourceForFoldTransitionSemantics` row.
Static-domain, value-class, and mask facts for input leaves come from an explicit verified
`InputSemanticFacts<P>` context built from the declaration/flattened semantic
object table plus the independently derived exact HIR input rows;
`SLTNode::Input` alone does not encode object identity, exact access geometry,
or `Bit` versus `Logic`. `PhaseSemanticObjectId<P>` identifies the declaration,
binding, or flattened object, while `PhaseInputId<P>` identifies one exact read
geometry. Input rows carry the object ID, normalized static base/part rule,
  exact ordered index roles, extents/strides, selected width, and derived result
  signedness/positive-type/static-domain/value-class/mask-class. `PhaseSLTNodeV1::Input` carries only the input ID and ordered
index child IDs; it cannot repeat or override access, stride, width,
signedness, or domain. ForFold state rows and result identity use
`PhaseSemanticObjectId<P>` plus a bit range, so two different read geometries
cannot hide overlapping state on the same object.

```text
InputSemanticFacts<P>
  objects: [SemanticObjectFact<P>]
  inputs: [InputAccessFact<P>]

SemanticObjectFact<P>
  object: PhaseSemanticObjectId<P>
  object_width / declared_signed / declared_positive_type / domain
  exact PhaseObjectResolution<P> / default role
  canonical [(extent, stride)]

InputAccessFact<P>
  input: PhaseInputId<P>
  object: PhaseSemanticObjectId<P>
  static base / normalized part-select rule / selected width
  ordered index roles with extent, stride, evaluation-mask use, and
    PackedEvaluationX | UnpackedStorageDefault invalid behavior
  result_signed / result_positive_type / result_static_domain
  result_value_class / result_mask_class
```

The object table is dense in canonical typed-declaration/binding traversal
order. The input table is dense in expected-HIR traversal order, and only an
identical complete input key may reuse an earlier row. Neither analyzer
`HashMap` iteration order nor producer node/cache order allocates either ID.

Derived mask facts are recomputed over the checked node DAG. A materialized
`Bit` object read is `AlwaysZero` only when every possible invalid selector is
unpacked and therefore returns a Bit storage default. Any dynamic/unknown
packed selector yields `MayCarryXZ` and can produce X evaluation content even
with static Bit result domain. A Logic object read is likewise not known
two-state merely because its index is. Index children participate in
lowerability, mask class, and the separately verified address-known/bounds/
partial-lane plan. A serialized boolean or producer-supplied tag is never proof.

The new pipeline uses phase-typed `PhaseNodeId<SourcePhase>`,
`PhaseNodeId<DraftOccurrencePhase>`, and
`PhaseNodeId<OccurrencePhase>` with `PhaseSLTNodeV1<P>` payloads whose child and
input/object fields carry that same phase. `PhaseInputId<P>` and
`PhaseSemanticObjectId<P>` are distinct checked namespaces even when both are
backed by dense `u32` tables.
The public legacy `NodeId` is accepted only by `LegacyStructuralArena`; it
cannot be passed to a new frozen artifact. Raw wire integers are decoded first
to untrusted raw rows and become phase-typed IDs only after aggregate
verification.

Source construction is owned by `SourceArtifactBuilder`; one mapped unit is
owned by a separate `UnitOccurrenceDraftBuilder`. Each wraps a private
`MutableSLTNodeArena<Phase>` whose input IDs are source-, draft-, or
occurrence-phase typed. Ordinary allocation is
`try_intern_ordinary(completed_node)`. Gated allocation exists only on the
owning aggregate builder as
`try_intern_gated_mux(complete_phase_key, completed_mux)`; the low-level arena
cannot append a gated identity without the matching provenance registry. The
source method stages/reserves the node, result occurrence, gated registry row,
and coordinated `key -> { node, mux_id, result }` cache before one semantic
commit. A cache hit returns that whole canonical handle and creates no second
result or registry row. The unit-draft builder obeys the same contract with
`DraftOccurrenceGatedKey`. Internal arena preparation reports
`InternOutcome::{Existing, Inserted}`, but no producer API exposes a bare
gated phase-node ID result.

This ordering is acyclic: reserve the source gate/decision-merge owner and
merge site; complete condition/arm occurrences; derive a key containing no
result, node, or `SourceGatedMuxId`; perform the coordinated intern; then define
the reserved owner/merge row with the returned result and mux handle.
`CheckedSlots::finish` rejects any owner left undefined or defined twice.

The phase key is complete and fixed-size: source or unit-local operand
occurrences, owner, unit, and merge site. A boolean "do not intern" tag is not
a key. The artifact-global `OccurrenceGatedKey` is constructed only after
every local control/value/site ID has a prospective final mapping. Token
resolution does not reopen either SLT arena; its resolved-gated map belongs to
`ControlValueDraft` and allocates no SLT node. Ordinary and gated requests use
disjoint identity maps, so equal raw muxes cannot collapse owners or reuse an
ordinary mux. A repeated complete key must return the same coordinated handle
or fail on different semantics.

Interning does not clone an `SLTNode` into a map key and does not rely on a
hash-collision complexity assumption. `PhaseSLTNodeV1<P>` has one exhaustive
canonical total order over variant tag and direct payload. The arena stores an
index-only AVL tree whose links/heights are parallel construction vectors;
comparisons read the one owned node payload in the arena. Complete fixed-size
gated keys and the coordinated provenance handles use the same pre-reserved
index-tree scheme. Rotations mutate only construction indices after all
semantic rows/capacity have been staged, and lookup/insertion takes worst-case
`O(log nodes)` structural comparisons. The indices are dropped at freeze.
Large concat/ForFold/input-index payloads therefore have one owned
construction copy in flat phase-local pools and are named by checked ranges.
Constants name a `VerifiedTypedValueId<P>` in the same prepared aggregate; its
payload and X/Z mask name canonical ranges in one flat, fallibly reserved limb
arena. A fixed node descriptor never owns a `BigUint`, `Vec`, string, or
single-element wrapper around one. Context-aware canonical comparison reads
the referenced value/range contents rather than comparing numeric IDs. On the
supported 64-bit host the construction descriptor and one AVL link are each
bounded to 32 bytes by layout tests. AVL absence uses `usize::MAX` only as a
private construction sentinel: prospective insertion rejects that index
before mutation, and the sentinel is neither a serialized ID nor an
input-dependent node cap. Replay rebuilds the same canonical index transiently
to reject noncanonical serialized ordinary duplicates.

Every allocation validates that children already exist, computes width from
the checked prefix with the same shared rule used by replay, and reserves all
affected storage before changing a semantic length or mapping. The new
ForFold state rows are producer-canonical by checked input ID and bit range;
the verifier checks adjacent rows for strict order/disjointness in one pass,
instead of allocating per-target vectors and sorting. Initial/update pairs are
parallel state bindings and carry explicit result-state identity, so this
storage canonicalization does not reorder effects; effect rows remain in
source order. Direct lowerability is
also one shared helper used by construction and replay. Semantic, ID, and
capacity failures are allocation-free structured errors and leave prior
IDs/facts unchanged. Error rows contain a closed rule/owner/context tuple and
no `String`, formatting result, or allocator message. There is no third-party
scalar construction on the proof path, public node vector, infallible `alloc`,
recursive `get_width`, or hidden retry. Structural replay
scans edges and recomputes facts against the same width allocation, then builds
only packed derived bitsets.

Hierarchy mapping never calls per-node interning directly on an artifact-global arena
while a unit can still fail. It builds and atomizes a `UnitOccurrenceDraft`
with local checked IDs and `DraftOccurrenceGatedKey`, then verifies its arena
and complete local control relation. `OccurrenceArtifactTxn` computes checked
prospective final ranges from the unchanged global lengths and translates all
local input, control, value, site, root/action, dynamic-plan, and
ForFold-template IDs. It receives the already verified
instance registry; observer definitions/runtime sites are not reallocated by
the unit transaction. Source-to-draft input mapping is
verified against the source instance/type rows before node construction; the
transaction maps every `DraftOccurrenceInputId` to the existing final
`OccurrenceInputId`. This input relation is a total many-local-to-one-final
function, not a fresh bijection. In append order it remaps child
`PhaseNodeId<DraftOccurrencePhase>` values to occurrence-phase IDs and
only then derives each final `OccurrenceGatedKey`. Its overlay interning maps
query the immutable global prefix plus staged entries: ordinary hits may name
an existing global node, new nodes receive prospective append IDs, and a gated
key collision with different semantics fails before commit.

The transaction records and verifies `DraftToFinalRelation`. For nodes this is
a total local-to-global function and append-order child homomorphism, not a
bijection: ordinary interning may map several local nodes to one existing
global node. Every new global node has exactly one staged representative and
every existing-node hit proves equal remapped semantics, including remapped
input IDs. Fresh unit-local control/occurrence/owner/site/root/action/
plan/template and gated-key namespaces are bijective (or have the explicit
atomization expansion cardinality above).
It then verifies the composite view consisting of the already committed,
structurally checked prefix and the isolated staged unit. All ID exhaustion,
collision, payload materialization, and destination `Vec`/map `try_reserve`
operations happen before any
length or mapping changes. After successful reservation the private commit
path only moves staged owned records into reserved storage; it performs no
allocation or fallible semantic work. A failed draft, overlay, proof, or
reservation therefore changes no global length or map. Glue and observer units
use the same transaction. There is no public per-node global allocation API.

`FrozenSLTNodeArena<P>` has no public standalone constructor or `Deserialize`
implementation. There are two separate owning SLT freezes and one final
control/value freeze:

```text
FrozenSourceArtifact
  private nonserialized ArtifactBrand
  CanonicalSourceModuleKey
  VerifiedSourceSemanticContext (canonical typed semantic HIR snapshot)
  VerifiedTypedValueArena<SourcePhase> and flat node-payload pools
  ExpectedSourceControlGraph derived from that exact snapshot
  ExpectedSourceValueGraph derived from that exact snapshot
  FrozenSLTNodeArena<SourcePhase>
  VerifiedSourceControlProvenance

FrozenOccurrenceArtifact
  private nonserialized ArtifactBrand
  FrozenSourceCatalog and dense SourceInstance table
  VerifiedFlattenedSemanticContext (instance/type/glue/alias rows)
  VerifiedTypedValueArena<OccurrencePhase> and flat node-payload pools
  ExpectedOccurrenceGraph derived from that exact context
  FrozenSLTNodeArena<OccurrencePhase>
  VerifiedControlOccurrencePlan
  VerifiedOccurrenceRewriteRelation retained as part of that plan

FrozenControlValueArtifact
  private nonserialized ArtifactBrand
  owned FrozenOccurrenceArtifact
  producer-indexed safety proofs and ProducerValueResolution
  VerifiedControlResolutionOverlay / token SSA / action graph records
```

After source symbolic evaluation, `FrozenSourceArtifact` consumes the source
builder, its original typed semantic HIR snapshot, and complete
`SourceControlProvenance`; hierarchy mapping receives only entries in the
immutable catalog. After flattening/atomization/observer materialization,
`FrozenOccurrenceArtifact` consumes the occurrence builder together with the
source catalog, flattened semantic rows, complete `ControlOccurrencePlan`,
root/action registries, and every source-to-occurrence relation. The final
builder consumes that exact occurrence artifact and the final frozen artifact
nests it; an arena or plan from another artifact cannot be substituted. Final
storage contains only same-ID resolution overlays and new value/token/graph
tables. Resolved roots, actions, gates, decisions, and muxes are composed
views, so occurrence topology and identity cannot diverge from a duplicated
final copy.
Private verified views carry the owner's in-memory brand and reject a mixed
brand. Brands are recreated after verification and are never serialized or
used instead of checking a wire relation.

Each aggregate freeze first performs structural replay, then proves that every
node has exactly one `OrdinarySemantic` or `Gated(complete key)` construction
identity. It checks the ordinary and phase-appropriate gated AVL indices
bidirectionally, replays canonical structural ordering/equality, and rejects
any stored noncanonical ordinary duplicate,
matches every gated identity bidirectionally to the verified
`GatedMux`/owner/merge-site registry, and proves that every remaining node is
ordinary. Only then does it seal allocation and take/drop the construction-
identity vector, ordinary/gated index trees, and coordinated
provenance cache (not `clear`, which retains bucket capacity). These structures
retain construction-only capacity and key state, but never a deep copy of an
ordinary node payload. The frozen arena retains nodes and compact facts;
durable gated identity remains in the verified provenance registry, not in a
duplicate construction cache.

Live-builder freeze and wire decode have different untrusted inputs. A live
builder has construction identities, so aggregate verification derives the
classification independently from semantic/provenance rows and compares the
two complete tables before dropping construction state. A current wire carries
no construction identity; decode derives a temporary gated/ordinary
classification solely from the complete expected occurrence graph, the
verified mapped/atomized/glue/observer/pinned-runtime/rewrite origin relations,
and gated registries, then excludes gated nodes while rebuilding the
ordinary canonical index. Neither path uses "not mentioned as gated" as proof
of ordinary identity.

A pre-freeze semantic rewrite builds a new mutable arena and a verified old-to-
new relation covering the node, construction identity, and corresponding gated
provenance/key together. A frozen semantic rewrite similarly creates an entire
new aggregate artifact plus relation; it cannot mutate a frozen arena or
silently rebuild a cache during lowering.

There are likewise three aggregate boundaries, never an arena-only
planner-ready wire. The source boundary is in-memory first; the names below do
not assign a persistent byte schema:

```text
SourceRawAggregate { pinned parsed syntax/tokens, analyzer witnesses,
                     nodes, source provenance }
OccurrenceRawAggregate { [FrozenSourceArtifact], canonical elaborated
                 instance/type/glue/alias/observer semantic-input rows,
                 nodes, occurrence plan }
ControlValueRawAggregate { FrozenOccurrenceArtifact,
                   dense occurrence-to-InstValue IDs,
                   InstValue rows with normalized safety classifications,
                   producer-indexed safety-witness and value-resolution rows,
                   action-indexed token-flow overlays and token definitions,
                   nested ForFold token/producer-safety/value overlays,
                   raw action-order/scheduled-edge rows }
```

The byte-level envelope, section/pool canonicality, allocation plan, and the
private unclassified source-node stage are specified separately in
[Private source-wire framing and staging](./source-wire-format.md). That
document is not a `SourceWireV1` declaration: no source schema version or
frozen artifact exists until the complete typed-HIR, provenance, expected-graph,
and ordinary/gated classification relation below can be verified as one
aggregate.

Only aggregate `try_prepare_source_aggregate` entry points are public. A byte
`decode_and_prepare` adapter, when one exists, privately constructs that same raw
aggregate and cannot publish it. New persistent wires are version-tagged and
reject unknown fields; only an explicit old-version adapter may discard
obsolete derived fields. Source verification first flattens the pinned parsed
syntax/tokens to its private raw semantic tables and converts that view to
`VerifiedTypedSourceHIR`, then independently traverses
that verified semantic snapshot and derives expected source
specifications and `InputSemanticFacts<SourcePhase>`, structurally
replays nodes, verifies source provenance, construction classification, and
all-node expected-graph reachability, and
freezes. Occurrence decoding first verifies the nested source catalog, then
the elaborated instance/type/glue/alias/observer semantic inputs, derives
`ExpectedOccurrenceGraph` solely from those verified inputs plus the decoder's
closed `OccurrenceDerivationRuleVersion`, never from the raw occurrence plan or
its synthetic-origin rows, and derives
`InputSemanticFacts<OccurrencePhase>`, replays nodes, checks every source
instance/reference, rewrite, occurrence, and all-node reachability relation,
and freezes. Final decoding consumes that exact verified occurrence value. It
derives exact logical token definitions/flows; reconstructs producer-indexed
outer and template candidates from occurrence semantics and reaching tokens;
recomputes normalized safety and witnesses; structurally interns
`InstValue`/`FoldInstValue` from exact keys; recomputes dependency facts;
compares producer-resolution and dense occurrence overlays; resolves dynamic,
gated, action, and ForFold views; then derives and compares all scheduled
edges. Raw value rows or producer-supplied safety classifications are never
inputs to those expected results. It freezes only after scope, recurrence,
outer-result boundary, and resolved-gated relations all pass. Widths, derived
fact bitsets, brands, construction identities, and caches are not proof-bearing
wire fields. An explicit compatibility adapter discards any old copies before
verification; the current decoder never silently accepts them. No internal
verified record implements standalone `Deserialize`.

An optional raw aggregate byte decoder is nonrecursive and allocation-fallible
before any semantic verifier runs. Every encoded aggregate starts with an exact `schema_version`
and checked flat-table lengths; recursive HIR/control shapes are encoded as
flat rows with checked IDs. Before `try_reserve`, the decoder proves with
checked multiplication/addition that each length and payload fits the remaining
encoded bytes and that the sum of requested owned payload cannot exceed what
the byte stream can describe. Strings, limbs, operand arrays, and nested
template tables obey the same rule. Invalid lengths and capacity failure return
stable structured decode errors without partially exposing an artifact. This
is a structural bound derived from the actual input size, not an
input-dependent semantic node/CFG cutoff, and explicit worklists prevent host
stack overflow on deeply nested valid input.

A legacy arena-only wire may deserialize only to an explicitly
`LegacyStructuralArena`; it cannot be passed to the new planner or be called
frozen. Among ordinary nodes, the lowest verified arena node ID is the
construction cache hit;
a serialized additional raw duplicate record is noncanonical and rejected.
Every `GatedMux.semantic_node` is excluded from the ordinary cache even when its
raw `SLTNode` equals an ordinary or differently owned mux. Source-gated,
occurrence-gated, and final versioned caches are checked at their three separate
boundaries with their respective complete keys. Serialized IDs and unit
isolation remain unchanged; lowering never recreates a persistent cache.

During migration only version-tagged `LegacySourceArenaWireV0` may contain an
optional legacy provenance field. It always decodes to
`LegacyStructuralArena`, regardless of presence or emptiness, and no value of
that type can enter the new planner or be upgraded by relabeling. Migration to
the new pipeline requires a verified canonical typed executable HIR or
re-parses and type-checks the original source, then reruns the complete source
producer and aggregate verifier. A legacy artifact without either input
remains legacy and is rejected by the new planner. No new mutable or frozen artifact has optional
provenance, and there is no verify-or-ignore API. The old single-root
`map_addr` remains a legacy-only operation; the whole-unit draft mapper is the
only new provenance-aware mapping boundary. Provenance can neither disappear
nor alias between flattened instances.

Metadata failure is a producer error. It never causes an unverified attempt to
reconstruct a decision from the mux chain.

Decision-result verification first checks one selected value for every source
arm (including the unchanged/default value when that arm does not assign the
result). It then starts at the recorded default and folds source arms in reverse
priority order. The first incoming use is the default and every later one is a
verified fixed use of the previous mux definition. Exactly one recorded gated
mux per arm has that arm's predicate/selected value as condition/true input and
the incoming use as false input. This remains required for
equal raw arms and constant source predicates. The final instance must equal
the recorded result. Every decision-owned mux occurs in exactly one result
merge. This proves both the per-arm edge values and the multi-output
priority/default chains before versioned optimization or profitability decides
whether the decision remains dataflow or becomes control.

## 2. Maximal control skeleton and DAG placement

Before building the maximal skeleton, `ControlEligibilityPlan` classifies every
source gate/decision with an independently checked proof or a semantic
`PinnedDataflow` result. A branch is legal only when its source condition
semantics are branch-equivalent and the condition has a `KnownTwoState` proof
(a `MaterializedStorage` Bit value, an `AlwaysZero` fact, or an independently
recomputed actual zero mask). Static Bit domain alone is insufficient. A Logic condition is
not made branchy merely by attaching a two-state tag. Four-state ternaries whose
X/Z behavior cannot be represented by branch control and nodes containing
`ForFold`, runtime-function invokes, runtime events, stores, or other effects
are pinned to their existing semantics.

For a source decision, eligibility replays each recorded typed wildcard/range
predicate and coercion. It requires a two-state selector, constant normalized
patterns, and proves an exact mapping to canonical masked or signed/unsigned
range patterns. Wildcard X/Z bits may become zero care bits only when that is
the source operator's verified meaning; range bounds must have zero X/Z mask.
The lower-bound and upper-bound comparisons each retain and replay their own
width/sign coercion; they may form one canonical range only when both prove the
same normalized selector domain and interval. Signed extension and truncation
are preserved explicitly, and all ranges in one canonical decision must prove
one common comparison order.
This proof exists before placement or profitability. A source case that cannot
be canonicalized stays as its already verified ordered predicate/mux dataflow;
formation is never allowed to discover semantic ineligibility after the DP.

The first skeleton contains every source gate/decision proved eligible by that
artifact. Eligibility is separate from profitability.

Construction here is deliberately staged so token-dependent values are not
used to build the graph that defines their tokens. First, the frozen
occurrence plan produces a value-unresolved order skeleton:

```text
GlobalActionOrderSkeleton
  actions: [ControlActionId]
  scheduled_roots: [ControlRootRef]
  barriers: [RootOrderBarrier]
  occurrence_edges: [OccurrenceActionEdge]
  semantic_accesses: ControlActionId -> exact read/write/bind/effect summary

OccurrenceActionEdge
  before / after: ActionOrBarrier
  reason: LocalControl | OccurrenceDataSource | AddressSource |
          PreviousValue | ExplicitRootOrder | ObserverTrigger

ArtifactControlGraph
  entry / exit: GlobalControlPointId
  points / edges: GlobalControlPointId / GlobalControlEdgeId
  local_point_embedding: (ControlUnitId, ControlPointId) -> GlobalControlPointId
  local_edge_embedding: (ControlUnitId, ControlEdgeId) -> GlobalControlEdgeId
  action_owner: ControlActionId -> (GlobalControlPointId, slot)
```

The skeleton is built from occurrence IDs and action semantic summaries only.
It contains every action exactly once, no metadata-only root, and every virtual
root-order barrier exactly once. Its edge union is acyclic. The
`ArtifactControlGraph` composes complete verified unit CFG fragments, glue
units, root barriers, and observer-trigger paths into one finite
single-entry/single-exit DAG. It preserves each local path and edge identity;
synthetic cross-unit edges connect complete unit exits/entries in the
canonical root schedule rather than sharing unit-local points. Every local
action/site has exactly one global embedding. This super-CFG, not a guessed
linear action order or a cross-unit `InstValue`, is the domain on which state
SSA is constructed.

Token identity is SSA identity, never an integer counter:

```text
MemoryTokenDefKey = Entry(WriteDomainId) |
                    Action(WriteDomainId, ControlActionId) |
                    Phi(WriteDomainId, GlobalControlPointId)
EnvironmentTokenDefKey = Entry(BindingId) |
                         Action(BindingId, ControlActionId) |
                         Phi(BindingId, GlobalControlPointId)
EffectTokenDefKey = Entry(EffectStreamId) |
                    Action(EffectStreamId, ControlActionId) |
                    Phi(EffectStreamId, GlobalControlPointId)

MemoryTokenDef
  Entry { domain }
  MayDef { domain, action, incoming }
  Phi { domain, point: GlobalControlPointId,
        incoming: [(GlobalControlEdgeId, token)] }

EnvironmentTokenDef
  Entry { binding }
  Bind { binding, action, incoming }
  Phi { binding, point: GlobalControlPointId,
        incoming: [(GlobalControlEdgeId, token)] }

EffectTokenDef
  Entry { stream }
  Action { stream, action, incoming }
  Phi { stream, point: GlobalControlPointId,
        incoming: [(GlobalControlEdgeId, token)] }
```

Each domain, binding, or effect stream has exactly one `Entry`. Every token has
one definition. Action definitions agree bidirectionally with the semantic
summary and owning action's token flow, and every phi has exactly one incoming
token per global CFG predecessor edge. A trivial phi whose incoming tokens are
all identical is not created; all uses name the common token. A merge of
distinct tokens creates a fresh phi token: a read of an incoming token cannot
be reissued after the merge. The token verifier independently replays the
global CFG in deterministic order and rejects an omitted/extra def, phi input,
or action flow.

The expected key set is derived before token IDs exist. For each verified
domain/binding/stream independently, a sparse def/use block list and pruned
iterated-dominance-frontier worklist derives exactly the required action defs
and nontrivial phi keys. Each of the three key sets is sorted separately by its
closed variant/semantic-ID/owner order and receives dense IDs in its matching
`MemoryTokenId`, `EnvironmentTokenId`, or `EffectTokenId` table; IDs never
index a tagged union table. Phi inputs are then filled exactly once in
`GlobalControlEdgeId` order using
checked slots. Dominance renaming maintains only stacks and sparse facts for
the state key currently processed. It never clones a complete state map at
each control point. The verifier independently rederives the sparse def/use/
phi key set and transfer equations from action summaries and the CFG before
comparing any supplied token row. Work and storage are proportional to
explicit token defs, uses, phi operands, and the documented dominance-frontier
work, not `control_points * all_state_keys`.

Only after that proof does the final builder create a transient
canonical-producer-indexed candidate relation. An `EvaluateHere` or definition
row creates one
topologically ordered candidate; a `FixedValue` occurrence is bound to
its `CanonicalProducerRelation` producer's existing candidate and cannot
acquire new reaching tokens:

```text
VersionedValueCandidate
  transient ID allocated by deterministic Kahn order over
    OccurrenceProducerDependencyDAG, minimum CanonicalProducerId first
  producer: CanonicalProducerId
  source occurrences: exact nonempty canonical inverse list from
    CanonicalProducerRelation
  unit / semantic node or runtime origin
  placement_class: OrdinaryCandidate(primary occurrence) |
                   ActionResult(ControlActionId, result ordinal) |
                   GatedResult(GatedMuxId) |
                   PinnedSyntheticResult(SyntheticOriginId)
  ordered_operands: [VersionedValueCandidateId]
  exact direct memory/environment reads at the occurrence site
  exact reaching tokens and required predicate region

OccurrenceExecutionSafety
  producer: CanonicalProducerId
  classification: Total | DomainRestricted(PredicateRegionId)
  witness: ExecutionSafetyProofId

ExecutionSafetyWitness
  owner: Outer(CanonicalProducerId) |
         Fold(ForFoldTemplateId, FoldCanonicalProducerId)
  closed operation-proof rule ID
  exact verified node/type/token/knownness/domain fact references
  derived preconditions and normalized classification

ProducerValueResolution
  producer: CanonicalProducerId
  value: InstValueId

OccurrenceValueResolution
  occurrence: ValueOccurrenceId
  producer: CanonicalProducerId
  value: InstValueId

ValueKey = InstValueId

InstValueKey
  unit: ControlUnitId
  origin: Slt(PhaseNodeId<OccurrencePhase>) |
          RuntimeState(SyntheticOriginId)
  placement_class: Ordinary |
                   ActionResult(ControlActionId, result ordinal) |
                   GatedResult(GatedMuxId) |
                   PinnedSyntheticResult(SyntheticOriginId)
  ordered_operands: [InstValueId]
  direct_memory_reads: canonical strictly ordered unique
    [(WriteDomainId, MemoryTokenId)]
  direct_environment_reads: canonical strictly ordered unique
    [(BindingId, EnvironmentTokenId)]
  execution_domain: Total | DomainRestricted(PredicateRegionId)

InstValueFacts
  memory_dependencies: SLTMemoryDependencyId
  environment_dependencies: SLTEnvDependencyId

InstValue
  key: InstValueKey
  facts: InstValueFacts
```

`ExecutionFactTable` is derived, never decoded as proof. In deterministic
candidate topological order it stores, per outer or fold producer:

```text
ExecutionFacts
  type/domain/width and exact predicate region
  KnownBits { known_zero, known_one, known_xz } as disjoint packed bitsets
  unsigned_interval / signed_interval: finite closed interval or Unknown
  purity: Pure | ReadsTokenizedState | PublishesEffect
  arithmetic_preconditions: nonzero / signed-min-minus-one / shift facts
  address_preconditions: address-known / bounds / pointer-valid /
    nonfaulting / nonvolatile / non-MMIO / exact reaching-token facts
```

The lattice has fixed variants and finite-height components: bit knowledge only
grows from unknown to known, an interval only narrows from `Unknown` to one
derived closed interval, and boolean preconditions only move from unproved to
proved. Because the producer graph is a DAG, the implementation evaluates each
closed transfer once rather than iterating to a cap. `OperationFactRule` and
`OperationSafetyRule` are exhaustive versioned enums over every
`PhaseSLTNodeV1<P>`, runtime/header origin, typed coercion, and dynamic-access kind.
They derive facts from operand facts, verified types, token rows, and address
plans; an unknown rule/version is a decode error, not `Total`.

Safety normalization first rejects effect publication/speculation, then applies
the one rule for the operation. Division/remainder require proved nonzero and
signed-overflow exclusions; pointer/memory rules require every recorded address
precondition; four-state rules require the exact knownness needed to preserve
X/Z semantics. If every required predicate is proved, classification is
`Total`; otherwise it is exactly `DomainRestricted` to the producer's verified
execution region. `ExecutionSafetyWitness` is a replay trace naming the closed
rule and fact rows, and decode independently recomputes both facts and the
classification before comparing it. No wire flag, interval, known-bit mask, or
witness premise is accepted as an input fact.

`OccurrenceExecutionSafety` is computed topologically over transient candidates
and exact token/type/operation facts before final value interning, then retained
under the stable canonical producer ID. Candidate rows are in total bijection
with canonical producers, their occurrence lists equal the producer relation's
inverse lists, and their operand producers equal the independently derived
producer DAG. Topological interning
then substitutes final operand IDs. A `Total` ordinary candidate may share
within one unit; a `DomainRestricted` key includes its exact predicate region.
Fixed placement classes include their complete owner/result ordinal and never
collapse with each other or with ordinary values. The verifier retains the
safety classification used in each final identity and keeps its witness in a
separate verified proof table. Every producer has exactly one
`ExecutionSafetyWitness` and every witness one owner; decode rederives its
closed proof rule from verified facts before comparison. A supplied `Total`
flag or witness is never used to construct the expected classification. The
verifier then builds the total producer-to-value relation
and the dense occurrence-to-`InstValueId` overlay by composing it with
`CanonicalProducerRelation`; only afterward does it derive the scheduled
graph. Equivalent witnesses for the same normalized classification cannot
split value identity. Thus safety neither mutates a frozen identity nor
depends on one.

`InstValueId` is a checked ID into a canonical AVL-interned, structurally
versioned value table ordered only by `InstValueKey`. The owning unit and
ordered operand instance IDs are
part of identity,
so values are never shared across independently scheduled control units and
noncommutative `old_x - current_x` and `current_x - old_x` cannot collide even
though their occurrence node ID and transitive token sets are equal. A leaf input
records, at its ordered read action/site, the reaching token of every write
domain that may alias it. Loop/function-local bindings similarly record their
exact iteration/environment token. Thus the same semantic occurrence node is
instantiated as different values across a relevant store or environment change
without invalidating it for unrelated changes.

`SLTMemoryDependencyId` is a root ID in a canonical persistent Patricia trie
of exact `(WriteDomainId, MemoryTokenId)` keys; the set may contain two tokens
of one write domain. `SLTEnvDependencyId` is the analogous binding/token trie.
All outer and nested dependency sets instantiate this same schema:

```text
DependencyKey = (semantic_id as big-endian u32) || (token_id as big-endian u32)
bit positions are compared from 63 down to 0

CanonicalDependencyNode
  Empty                         // exactly arena row/root 0
  Leaf { key: DependencyKey }
  Branch { common_prefix: u64, prefix_len: 0..63,
           zero: child, one: child }

CanonicalDependencyArena<SemanticId, TokenId, RootId>
  row 0 is Empty and no other Empty exists
  every nonempty child precedes its parent
  a Branch's children are nonempty and distinct; every key below both children
    has exactly common_prefix[0..prefix_len], the next bit is respectively
    zero/one, and prefix_len is the longest common prefix of subtree min/max
  Leaf keys are unique; child order and prefix masking are canonical
```

`SLTMemoryDependencyId`/`SLTEnvDependencyId` index the two outer arenas.
Each `ForFoldTemplateId` physically owns separate
`FoldMemoryDependencyId`/`FoldEnvDependencyId` arenas with the identical
invariants and its scoped token IDs. Decode recomputes subtree min/max and every
branch prefix iteratively, so equal extensional sets have one possible root and
a malformed alternative shape is rejected.

Dense dependency-node IDs are deterministic construction results: row 0 is
Empty; while visiting outer/fold canonical producers in their fixed order,
direct keys are processed in key order and transient union worklists process
zero before one, assigning a row at its first structural creation. Decode
replays that exact construction with a transient AVL index and compares every
row/root ID bidirectionally. It then marks backward reachability from every
`InstValueFacts`/`FoldInstValueFacts` root; every non-Empty arena row must be
marked, and Empty must be the unique row 0. Multiple value facts may reference
one canonical root, but a structurally duplicate node row, alternative dense
ordering, or orphan trie padding row is rejected even if all referenced sets
are extensionally equal.

Durable dependency arenas contain only canonical path-compressed Patricia
leaf/branch nodes, interned by canonical structural AVL keys,
so a prefix chain does not materialize a fresh sorted vector at every value.
Set union is a transient memoized operation keyed by the ordered pair of
canonical roots and returns the unique extensional Patricia root; union
operation nodes are neither serialized nor valid dependency roots. The memo is
dropped at freeze. The verifier replays each unique durable trie node once and
recomputes every root from direct facts plus operand unions. These exact sets
support alias, placement, and move-legality
proofs, but are not value identity because they deliberately discard ordered
operand association. They live in `InstValueFacts`; a key hit must reproduce
identical facts or verification fails. `InstValueKey` structural interning
includes the exact ordered operand association and token IDs. Its construction index is an
arena-owned AVL tree over the complete canonical row order; it neither depends
on randomized hashes nor retains cloned rows in a side map.

Resolution then produces the final scheduling artifact:

```text
GlobalScheduledActionGraph
  control_graph: ArtifactControlGraph
  actions / roots / barriers copied from the verified skeleton
  edges: [ScheduledActionEdge]
  memory_tokens / environment_tokens / effect_tokens

ScheduledActionEdge
  before / after: ControlActionId
  reason: LocalControl | DataSource | AddressSource | PreviousValue |
          ObserverTrigger | MemoryToken | EnvironmentToken | EffectToken
```

The final verifier recomputes every occurrence-to-`InstValue` substitution,
then derives the exact data and token edges and compares their complete set to
the graph. Unit-local regions, points, gates, decisions, and values remain
isolated; cross-unit dataflow is a store/read action plus global token edge,
never a shared `InstValueId`. Source, address, and old-value occurrences remain
separate uses and edges even when they name the same state atom. This prevents
a read-modify-write address at one token and an old-value read at another token
from collapsing into contradictory set-based edges. The union of control,
barrier, data, and token dependencies must be acyclic, and canonical order is
derived from lifecycle coordinates rather than hash iteration.
After this verification, the final control/value builder is consumed and all
source-occurrence, `InstValue`, dependency-set, and gated-instance interning
maps are taken and dropped. The frozen artifact retains only dense records,
compact facts, and verified relations required by later passes. Frozen
deserialization validates canonicality with transient maps and drops them just
as the frozen SLT arena does.

`WriteDomain` distinguishes at least state partitions, capture-enable state,
observer-trigger state, event streams, and a global unknown domain.

Purity does not imply speculatability. `OccurrenceExecutionSafety` classifies a
versioned candidate as `Total` only with an operation-specific, independently recomputed
proof that eager execution cannot trap, fault, publish an effect, or change X/Z
semantics. Division/remainder additionally require divisor-nonzero and signed
overflow safety (or a proved total lowering); dynamic memory reads require
address/fault and reaching-token proofs. Otherwise the value is
`DomainRestricted` to the exact predicate region in which the occurrence
executes. That classification is consumed into `InstValue` identity. Two identical
non-speculatable expressions originating in disjoint arms therefore remain two
instances; total values canonicalize to the unit root and may share. Work is
proportional to candidate edges plus packed fact payload words and does not
create combinations of path contexts.

Dynamic access is represented once and shared by combinational reads,
read-modify-write, module glue, and FF/testbench lowering:

```text
OccurrenceDynamicAddressPlan
  owner_action: ControlActionId
  source: MappedSource(SourceRef<SourceDynamicAddressPlanId>) |
          Synthetic(SyntheticOriginId)
  object / semantic variable type / object_width
  ordered_indices: [OccurrenceDynamicIndexUse]
  dimensions: [(extent, stride)]
  aggregate_dimension_count
  part: None | Colon { lsb, elements, stride } |
        PlusColon { anchor_index, elements, stride } |
        MinusColon { anchor_index, elements, stride } |
        Step { anchor_index, elements, stride }
  offset: OccurrenceUse
  selected_width
  address_known: OccurrenceUse
  bounds_when_known: OccurrenceUse
  access_guard: OccurrenceUse
  access_semantics: CheckedRead | CheckedOverlayWrite

OccurrenceDynamicIndexUse
  operand: OccurrenceUse
  source_domain: Bit | Logic
  source_width / signedness / exact normalization coercion
  extent / stride

ResolvedDynamicAddressPlan
  verified view of the same geometry/owner with every occurrence use replaced
  by its final InstUse; no duplicate geometry row is stored
```

The occurrence-valued geometry, owner, roles, and exact semantic access
summary are verified before `FrozenOccurrenceArtifact`. Token SSA consumes
that summary; final resolution only substitutes the already verified uses.
It never constructs or changes an address plan after token analysis.

The verifier derives each typed index use from the source variable/select and
requires its action operand role/site to match. For regular aggregate indices,
`aggregate = sum(normalize(index_i) * stride_i)`. A static `:` contributes
`lsb * stride`; `+:` contributes `anchor * stride`; `-:` contributes
`(anchor - (elements - 1)) * stride`; and `step` contributes
`(anchor * elements) * stride`. `selected_width = elements * stride` for a
part select and is the remaining aggregate stride otherwise. Every addition,
subtraction, and multiplication in this normalization is checked.

`address_known` is a `KnownTwoState` one-bit value which is true exactly when
every normalized index/anchor evaluation has an empty X/Z mask. Static Bit
domain does not contribute true by itself because a Bit-typed intermediate may
carry transient X before materialization. `bounds_when_known` is a
`KnownTwoState` one-bit value equal to the conjunction of
`0 <= index_i < extent_i` for every aggregate index,
`0 <= anchor && anchor + elements <= extent` for `+:`,
`anchor < extent && anchor + 1 >= elements` for `-:`,
`0 <= anchor && anchor * elements + elements <= extent` for `step`, and
`offset + selected_width <= object_width`; static `:` bounds are verified when
the plan is created. `access_guard` is the exact two-state conjunction
`address_known && bounds_when_known`. Comparisons occur in the original normalized arbitrary-
width domain before conversion to `usize` or a machine pointer, so wrapping a
large runtime index cannot pass the guard. A `Logic` comparison containing X/Z
is never used directly as control. The backend output verifier proves that
pointer conversion and every direct machine memory access are dominated by
`access_guard == true`; the false path implements the exact default/partial-
lane/no-op semantics without forming an out-of-object pointer. The verifier
also proves a
bidirectional owner relation: a plan belongs to exactly one `ReadInput` with
`DynamicOverlay(plan)` or one dynamic `StoreRoot` target, and every such action
has exactly one plan.

Every dynamic access remains a checked `ControlAction`, including a statically
proved in-bounds one; the proof merely permits its backend implementation to
use an ordinary direct load/store. A non-static `CheckedRead` is
non-speculatable, and `CheckedOverlayWrite` is one atomic old-value/address/RHS
action. A backend may lower the checked action to a branch, mask, or
indivisible checked-load bundle, but may not omit the source action. An eager
mux containing an unchecked load does not count as a guard.

Runtime semantics retain each selector's packed/unpacked dimension tag and are
applied left-to-right. With a known address, in-range lanes read/write the
object; an out-of-range unpacked read substitutes the recursive materialized
element default, while an out-of-range packed lane produces X evaluation
content. Known indexed part-selects retain in-range lanes and use X for only
their out-of-range packed lanes. If an address component has X/Z, an unpacked
read uses its element default, a packed read is all-X regardless of static
domain, and a write is a no-op. A later Bit materialization may convert that X
to zero. Backends may use masked or branched implementations, but the output
verifier checks this exact relation and proves that no machine memory access
occurs outside the object.

Alias analysis declares a finite set of write domains and the sparse
`may_write_domain(read_class)` relation. A static nonaliasing store advances
only its exact domain; every overlapping read class names that domain. A
dynamic, containing, or pointer write advances the conservative domain chosen
for it, and every read it may affect names that domain. The global domain is
therefore present in every read signature and is advanced by a completely
unknown write. The verifier checks that the relation conservatively covers
every may-alias pair; uncertainty maps to global rather than omitting a fact.

Stores, releases, runtime events, captures, bindings, and folds are ordered
actions connected by the SSA tokens above. `SLTStateTokenAnalysis`
independently walks `ArtifactControlGraph`, reconstructs every
Entry/MayDef/Phi relation, ordered occurrence-to-instance resolution,
dependency summary, and `ValueKey`. It never recomputes a numeric version
counter. A placement planning unit is a verified segment of
`GlobalScheduledActionGraph` whose global entry/exit token interface is
explicit; no value moves across an unrepresented effect or cross-unit boundary.

Every direct memory/environment read also receives a path-sensitive
token-validity set. `SLTStateTokenAnalysis` computes reaching tokens at every
`ControlUseSite`: a slot uses the tokens reaching that exact action boundary,
while an edge use sees the token carried by that one global predecessor edge.
A site is valid for the read only when the exact recorded token reaches it for
every may-write domain/binding. At a merge, incoming `{v0, v1}` is represented
by fresh `vphi` and is not proof of `v0`. Equivalently, each path's first token
kill forms a frontier, but there is no assumed single linear "next action".
ScheduleLate selects only from valid sites on the earliest-to-latest dominance
path, and the verifier independently recomputes that membership. This prevents
moving `read x@v0` after a write that creates `v1` while still allowing it in a
non-writing arm where `v0` reaches. A transitive consumer may legally execute
later only through the already materialized `InstValueId` operand; it may not
silently reissue the old load.

Placement is expressed at action boundaries, not merely by predicate region:

```text
ControlSite = (ControlPointId, slot: usize)
ControlUseSite = Slot(ControlSite) | Edge(ControlEdgeId)
```

For a point with `N` ordered actions, slots `0..=N` are the positions before,
between, and after them. An action at index `i` executes between slots `i` and
`i + 1`; a CFG edge leaves the predecessor's final slot and enters successor
slot zero. Point dominance plus slot order defines slot-site dominance. An
edge use maps to the predecessor's final slot for availability/LCA purposes
but retains its edge identity for token and gamma semantics. This
distinguishes a gate header, join, and continuation within one parent region
and prevents a value from moving across an effect merely because both actions
have the same region owner.

The placement algorithm follows the ScheduleEarly/ScheduleLate structure used
for sea-of-nodes global code motion:

1. Build direct def-use and user lists once from every resolved `InstUse` in
   roots, actions, gates, decisions, patterns, muxes, and result merges in one
   planning unit. Root/action fixed uses are ordinary LCA inputs rather than a
   special post-placement check.
2. In topological order, compute each `InstValue`'s earliest legal `ControlSite` from
   its operands, token facts, pinned memory/effect constraints, and required
   execution domain.
3. In reverse topological order, compute its latest `ControlSite` as the LCA of
   every fixed root/action/gate/decision `InstUse` site plus the selected sites
   of all already placed ordinary users in the expanded site-dominance tree.
   Gamma/merge operands use the final site of their actual arm predecessor as
   the use site. A gate-result-owned mux contributes fixed operand-use sites: its
   condition at the gate header immediately before dispatch, and each arm only
   at its corresponding exit edge. A kept decision uses its selector/patterns
   at the recorded dispatch header and each `selected_arm_value` only on that
   source arm's exit edge. A contracted decision instead uses the recorded arm
   predicates/values in its ordered mux steps at the merge site. These two
   state-specific use maps are explicit inputs to the DP and final placement;
   a decision-owned mux is never treated as an unnamed binary gate.
4. Choose the latest legal site on the earliest-to-latest dominance path that
   minimizes the pressure/loop-frequency cost. Each instance is emitted once;
   values sharing a site are emitted in verified def-before-use topological
   order.

Control-site dominance uses iterative Euler numbering and an RMQ/binary-
lifting LCA index. The verifier recomputes action slots and requires each
earliest/latest/selected triple to lie on one dominance path. A value may cross
arms only through an explicit gamma/merge result; a raw cross-arm use is a
producer error. A `DomainRestricted` assignment must be contained by its
region entry/exit; it cannot be hoisted to the LCA of disjoint arms. The planner
does not manufacture path-context combinations: distinct restricted instances
already correspond to distinct recorded source occurrences. Each value and use
edge is processed a constant number of times, for
`O((values + uses) log control_sites)` time and linear storage.

This placement gives the desired shared-expression rule directly. A value used
by both arms is placed at their common parent. A value shared only inside one
arm stays in that arm. A value shared by several outputs governed by one source
gate is computed once for that gate, rather than rediscovered independently by
each output mux.

### Profitability without a placement fixed point

Gate selection and placement affect one another, but they do not form a retry
loop:

1. Compute legality envelopes once on the maximal eligible skeleton.
2. Give every `ValueKey` exactly one cost owner: the lowest gate/decision whose
   keep/contract state can change that value's execution predicate. Values
   wholly owned by a child occur only in the child summary; values shared by
   children occur once in their parent summary.
3. For both states of each laminar region, build a `RegionStateSummary`. Its
   interface contains owned-value execution predicates, fixed state-specific
   use sites, incoming/outgoing value chunks, intrinsic/control/copy work, and
   reach weights. The pressure term is the additive chunk count on the minimal
   dominator subtree connecting those fixed uses, accumulated with Euler
   interval differences. It is a site-independent lower-bound proxy, not the
   pressure of a provisional or final placement. Child summaries expose only
   boundary values and cost-per-invocation, so composing a parent neither
   recounts child values nor expands descendant contexts.
4. Run one bottom-up dynamic program. For every gate/decision compare `kept`
   against `contracted`; a kept state pays control, expected misprediction,
   merge/copy, and live-frontier costs, while a contracted state pays eager arm
   work and selects. Child summaries are stored as cost-per-invocation; the
   parent applies the reach weight for its own kept/contracted state. Runtime
   cost is primary; equal runtime chooses smaller code, an additive frontier-
   chunk pressure proxy, then fewer regions.
5. Contract every rejected region and run ScheduleEarly/ScheduleLate exactly
   once on the final tree.

All comparisons are strict expected-cost comparisons. There is no arm count,
node count, region count, CFG size, or iteration budget. Dynamic profile data
may replace static weights later without changing legality or termination.
Contraction maps a subtree only into its existing parent and may not move a
value across an undecided ancestor arm. This laminar invariant makes the child
tables composable.

A contracted state is absent, rather than merely expensive, if it would execute
any `DomainRestricted` arm instance outside its required domain. The same rule
applies to decision contraction and reverse if-conversion. Only a newly verified
`Total` proof may make that eager state legal; allocation or performance failure
never changes the classification.

The selected DP emits a `CostWitness` listing each selected
`RegionStateSummary` and each owned value/control/copy/frontier term exactly
once. An independent verifier reconstructs the selected state-specific use map
and summaries from provenance and the contracted control tree, without reading
the final placement choices. A mismatch is an analysis error; it does not
trigger another selection pass. Final placement has its own verifier and is
allowed to beat or exceed the declared lower-bound pressure proxy.
Profitability remains a target heuristic, but ownership and arithmetic are
fully checked.

Selection also emits a `GateFormationPlan` with one entry for every gate-result-owned
`GatedMux`. For a kept gate, verification relates the original condition and
true/false operands one-to-one to the corresponding branch edges and join
arguments, including complete `InstValueId`s and widths; swapping arms requires an
explicit, independently proved condition inversion. For a contracted gate it
proves that the original dataflow `Mux(condition, then, else)` remains and that
no residual arm branch claims to implement it. It also proves that every
original gated result is represented exactly once and that no new result is
introduced. This semantic relation is checked against the emitted SIR; a
placement that merely satisfies dominance and types is insufficient.

`DecisionFormationPlan` covers every decision-owned result merge. In a kept
decision, each canonical pattern entry must map back to its source arm, every
source arm/default edge must pass that merge's recorded
`selected_arm_value`/`default_value`, and the join result must replace the
recorded final result exactly once. In a contracted decision, no canonical
terminator may claim it; the verified ordered predicate/mux DAG and its final
result remain. Thus both profitability states have an explicit condition/value
use map and an arm-by-arm semantic output proof.
The two input verifiers jointly prove that their owner sets are disjoint and
their union contains every `GatedMux` exactly once.

The independent `PlacementPlan` verifier recomputes def-use edges and proves
that every pure `InstValue` is assigned exactly once, operands dominate their users,
each assignment site lies between its earliest and latest legal sites, the
within-site order is def-before-use, gated arm nodes cannot execute on another
arm, every restricted value stays in its required execution domain, pinned
effects did not move, and every root receives a dominating value of the
declared width and memory/environment tokens.

### Verified runtime-function formation

Runtime functions remain shared functions downstream. Recursively inlining a
call DAG would duplicate a callee for every path, enlarge the native function
presented to scheduling and allocation, and make code size proportional to an
expanded call tree rather than the verified input. V1 therefore has one
target-independent SIR function per complete occurrence program environment and
one opaque invoke at each verified call boundary:

```text
SIRRuntimeFunctionId, SIRRuntimeInvokeId, OccurrenceRuntimeProgramEnvironmentId:
  checked dense u32 IDs

OccurrenceRuntimeProgramEnvironment
  source-program specialization
  complete flattened mapping of captured objects/bindings/effect streams
  no actual value, call-instance ID, profile count, or analyzer cache identity

SIRRuntimeFunction
  id / OccurrenceRuntimeProgramEnvironmentId
  hidden SimulationState capability
  flattened Input/Inout value parameters in formal/member order
  verified program CFG, locals, nested invoke sites, and retained ForFold regions
  result tuple: optional Return, then Output/Inout values in formal/member order
  exact read/write/binding/effect summary and every normal/termination exit

SIRRuntimeInvoke
  id / exact OccurrenceRuntimeCallInstanceId / callee SIRRuntimeFunctionId
  existing ordered operand registers plus operand-role bijection
  result registers plus result-role bijection
  once-prepared caller target handles and formal-order writebacks
  exact semantic and token boundary

RuntimeFunctionFormationPlan
  consumes the complete verified SourceToOccurrenceRuntimeFunctionRelationV1
  total program-row-to-SIR-function and call-instance-to-invoke mappings
  total nested control/value/action/formal/local/object/input/result/target/
    dynamic-plan/coercion/event/fold, OccurrenceRuntimeFunctionInvokeV1 owner,
    and entry/exit mappings
  unchanged invoke roles/writebacks and boundary token/value equality
  exact inverse coverage and semantic output relation
```

The environment key permits structurally identical flattened instances to
share generated code only when every captured address/binding/effect mapping is
identical; input bits never enter it. Programs and calls are formed in reverse
topological runtime-call-graph order with explicit worklists. Every verified
program has exactly one SIR function in its occurrence environment and every
call instance has exactly one invoke. There is no inline-vs-call heuristic,
body-size threshold, retry, or legacy fallback in V1. A future inliner must
provide a separate total `RuntimeFunctionInlinePlan` and semantic output
relation before it may replace an invoke.

`SIRRuntimeInvoke` is an effectful multi-definition operation. Ordinary SIR
optimizers treat the invoke, its termination status, and its writebacks as one
opaque ordered bundle; they may project a completed value result but cannot
move a load/store/effect or another writeback into the bundle. The standalone
SIR verifier replays the callee summary, checks all operand/result roles and
types, proves the exact caller target handle is reused, and checks declaration-
order copyout. Dropping an unused return never drops the invoke. SIR function
return edges carry the complete result tuple; early source returns first join
through verified program exit parameters.

Each backend consumes a verified closed `RuntimeCallLoweringPlanV1`. Its logical ABI has
the simulation-state capability, typed value parameters, typed result tuple,
and explicit termination status. The target plan selects concrete register,
stack, or caller-frame locations; proves nonoverlap, alignment, total width,
all argument/result moves, target-handle writebacks, and status propagation;
and relates the emitted call/output bundle back to the one SIR invoke. Arbitrary
width or multi-result values may use a caller-owned frame, but its layout is a
checked plan derived from the complete logical signature rather than a fixed
field-count encoding.

```text
RuntimeCallLoweringPlanV1
  exact target/ABI rule version and closed physical resource inventory
  one selected function ABI row per SIRRuntimeFunctionId
  one selected call bundle row per SIRRuntimeInvokeId
  ordered argument/result/status locations, moves, clobbers, successors, and
    writeback locations derived from the complete logical signature

RuntimeCallLoweringOutputRelationV1
  total bijection from every SIRRuntimeFunctionId in the artifact to exactly one
    backend function body and verified concrete ABI signature
  total bijection from every SIRRuntimeInvokeId to exactly one MInvoke/call
    bundle in the mapped caller function
  total ordered maps for every logical argument/result/status location, move,
    normal/termination edge, prepared target, and formal-order writeback
  exact inverse coverage: no backend function, invoke, ABI move, result,
    writeback, call edge, or call-clobber record without one mapped SIR owner
```

The output verifier derives this relation from the completed SIR and emitted
backend artifact; it does not accept producer-supplied coverage counts. It
checks the function-call graph, callee identity, signature, roles, complete
control/effect summary, and every forward/inverse row before either caller or
callee can enter scheduling or allocation.

Native MIR carries an explicit `MInvoke` bundle with fixed ABI uses/defs,
caller-clobber masks, state-base preservation, memory/effect ordering, and
normal/termination successors. Cranelift and Wasm carry the same logical
function/invoke relation through their own typed call primitives. Each shared
MIR function is scheduled, analyzed, and register-allocated independently;
values live across `MInvoke` exclude every declared caller-clobbered register,
and the allocator/output verifier checks that constraint. A call-lowering
failure is a structured backend error, never permission to inline, truncate,
or retry another allocator. Code and verifier storage are linear in shared
program rows plus call sites, result roles, and ABI moves, not in a recursively
expanded call tree.

## 3. Canonical DecisionRegion

A selected multiway source decision is retained in canonical SIR instead of
immediately becoming a binary diamond chain. Its conceptual terminator is:

```text
Decision(selector,
         ordered [(case_id, pattern, target, edge_args, probability)],
         default_target + default_edge_args,
         semantics = TwoStateDisjoint | TwoStatePriority,
         range_order = Unsigned | Signed)

DecisionPattern = Masked(value, care_mask) |
                  Range(lower, upper, upper_inclusive)

DecisionCaseOrigin =
  SourceArm(SourceRef<SourceDecisionId>, SourceArmOrdinal, pattern ordinal) |
  GateChain(ordered GateIds, chain case ordinal)
```

A masked pattern matches when
`(selector & care_mask) == (value & care_mask)`; `care_mask` is all ones for an
exact equality. A range matches `lower <= selector < upper`, or `<= upper` when
inclusive, in the decision's one declared signed or unsigned order. All
range patterns in one decision use that same order; eligibility pins a source
case with mixed range-comparison orders to dataflow. All operands are
normalized selector-width bit vectors using the eligibility witness's exact
source coercion.

`DecisionFormationPlan` has two producer forms. A source `case` supplies its
selector and ordered source arms directly; an arm with several patterns is
expanded to ordered canonical entries retaining one `source_arm` identity and
one target/argument tuple through `DecisionCaseOrigin`. A maximal nested
`if`/Gate chain may be combined only
after the formation verifier proves that every comparison uses
the same selector (modulo width-preserving identity/casts), that no effect lies
between tests, and that default/priority order is unchanged. This is the point
where separate source gates become one decision; target lowering never guesses
the relationship from machine blocks.

Accumulator-guarded priority chains have an additional canonical proof. The
accepted form is `acc = mux(guard_i && acc == default, value_i, acc)` with one
initial default, no intervening definition, and no observation of an
intermediate accumulator. The verifier checks the complete def-use chain and
proves by induction that it is an ordered first-match decision. It must also
prove at least one of: every selected `value_i` differs from the sentinel,
the guards are mutually exclusive, or a separate monotone matched bit (rather
than the result value) prevents later updates. Otherwise an earlier arm that
returns the sentinel can be overwritten and the rewrite is illegal. A partial
match, extra accumulator use, or different sentinel remains ordinary gates.

The SIR verifier checks the ordinary CFG/SSA edge contract plus:

- selector type and width, normalized key/mask widths, and zero bits outside
  the selector width, plus normalized and nonempty range bounds;
- exact-key uniqueness by sorting for `TwoStateDisjoint`, in
  `O(cases log cases)`; duplicate exact keys remain legal, ordered, and
  generally redundant in `TwoStatePriority`;
- every `TwoStateDisjoint` pattern pair is nonoverlapping: mask/mask uses
  `((value_i ^ value_j) & care_i & care_j) != 0`, range/range uses interval
  intersection, and mask/range uses the exact bit-DP described below;
- recorded terminator order and first-match semantics for
  `TwoStatePriority` arms (overlap is legal); source-order correspondence is
  separately owned by `DecisionFormationPlan`;
- one default edge, total edge argument arity/types, and target existence;
- an explicitly two-state selector value, until four-state X/Z branch behavior
  is specified. A `MaterializedStorage` Bit value or independently recomputed
  `AlwaysZero` definition qualifies; static Bit domain alone does not. Otherwise the
  producer must insert a typed `KnownTwoState` conversion whose zero-mask fact
  is recomputed by the standalone SIR verifier from the value's definitions;
  a semantics tag or unverified annotation alone never qualifies.

Before SIR construction, a separate `DecisionFormationPlan` verifier relates
every arm back to the same provenance selector and proves the source pattern,
priority, and default mapping. The standalone SIR verifier does not trust or
need access to the discarded SLT arena; it rechecks the complete canonical
CFG, type, key, mask, and edge contract.

The baseline disjoint verifier is deliberately `O(cases^2 * selector_chunks)`;
it is finite, exact, and is not a case-count cap. Range/range overlap is an
ordinary interval-intersection check after every selector/pattern in a signed
decision is order-normalized by flipping the sign bit. Mask/range overlap uses
a linear bit-DP in that same common normalized domain
that proves whether any selector in the interval satisfies the fixed mask; it
does not enumerate selector values. Overlapping wildcard/range
decisions remain ordered priority decisions and need no disjointness proof. A
target forms the precedence graph containing `i -> j` for every originally
ordered pair `i < j` whose patterns overlap. Its reordered cases must be a
topological ordering of that graph. Therefore disjoint cases may cross, but
the relative order of every overlapping pair is preserved; proving only that
the moved subset is internally disjoint is insufficient.

Every logical case/default edge has a distinct `DecisionEdgeId`. Before phi
construction it is materialized as a case-specific trampoline, even when two
cases name the same target, because their edge arguments may differ.
Every case also retains an immutable `DecisionCaseId`, its exact
`DecisionCaseOrigin`, and dense `PriorityRank` established by the formation
output relation. IDs are dense in canonical formation order and do not require
a fictitious source arm for a gate chain. The standalone
verifier requires every overlapping pair to appear in increasing rank order.

Every backend must understand the canonical terminator. The initial semantic
implementation may legalize it to an ordered branch tree, but this is a normal
target lowering selected before code generation, not an error fallback. Native
code keeps exact/disjoint decisions long enough to select a table. Cranelift
and Wasm use those trampolines/dispatch blocks because their table primitives
do not directly model arbitrary per-case SIR block arguments or structured
labels.

### Target strategy selection

`DecisionLoweringPlan` is an explicit, verified artifact. Candidate strategies
are:

- constant-result value lookup table for an exact-key disjoint decision whose
  cases and default have no effects and pass only one constant result to one
  merge;
- bounds check plus dense jump table for exact, disjoint keys;
- bit tests for small result/target sets;
- probability-weighted balanced comparison tree for sparse exact keys and
  proved nonoverlapping ranges;
- ordered early-exit chain for priority/wildcard/range arms; and
- a hybrid whose cost-selected tail is reverse-if-converted to `cmov`/select
  only when every tail arm is pure/eager-safe and first-match order is retained.

The target enumerates applicable strategies and minimizes:

```text
expected dynamic comparisons, branches, misses, loads, and edge copies
then code bytes + read-only table bytes
then pressure/live-frontier cost
```

Planning consumes the canonical decision only after all target-independent SIR
optimization has completed and the decision has been re-verified. Native ISel
then preserves it as `MDecision`; strategy legalization consumes the plan and
immediately checks the MIR/table output relation. No unrelated optimizer runs
between a witness and that output and no later pass may rewrite the decision
without producing a new verified plan.

Applicability is proved from key range, target support, and semantics. Density
or case count may appear in the cost equation, but never as a correctness or
termination cutoff. Sparse-tree splitting follows probability-weighted
near-optimal search-tree construction; jump-table and bit-test clusters follow
the same separation used by LLVM/GCC switch lowering.

A value lookup table covers the inclusive `[minimum_key, maximum_key]` span.
The generated code performs a full-selector-width unsigned bounds check before
any subtraction, truncation, or scaled indexing; out-of-range values select the
default. Every hole in the span is initialized to the default result. The plan
verifier checks arithmetic overflow, entry count, every key-to-index mapping,
all hole/default entries, result width, and that the eventual index conversion
cannot discard selector bits before the bounds proof.

The lowering plan contains a concrete `LoweredDecisionWitness`, not just a
strategy name and cost. Its comparison/range/bit-test/table nodes refer to the
original full-width selector, and every leaf names the original
`DecisionEdgeId` and exact edge arguments. Strategy-specific verification
accepts only a typed `DecisionTestGraph`: pattern-test/compare nodes, verified
bit-set or table dispatch nodes, reverse-fold select-tail nodes, and original
edge/default leaves. It does not attempt equivalence on an arbitrary CFG.

For a disjoint strategy, interval/key/bit/table construction rules prove every
pattern's entire match set reaches the same leaf and the complement reaches
default. For priority, the verifier checks an exact permutation of canonical
patterns plus the overlap precedence graph, then structurally replays an
ordered `Test(pattern, match_leaf, next)` chain or a reverse select fold with an
explicit monotone matched bit. Induction on that representation proves arm
`i` receives exactly `Match_i \ union(Match_j, j < i)` (possibly empty for a
shadowed duplicate) and default receives the complement; no exponential set
representation is built. An output
verifier then matches that witness arm-by-arm to the actual lowered SIR/MIR
CFG, trampolines, and table artifact. Ordinary CFG validity cannot substitute
for this cross-phase semantic proof; a locally valid graph with two case edges
swapped must fail.

Native ISel exposes a multi-successor `MDecision` terminator to CFG, liveness,
regalloc, and layout verification. Strategy legalization expands trees/chains
before allocation; a jump-table `MDecision` remains explicit through layout so
all targets stay visible. Jump tables use 64-bit absolute target entries in a
read-only data artifact plus relocation records patched after executable memory
is allocated. This avoids an optimization-dependent relative-range failure.
The baseline code obtains the separately mapped table base with a 64-bit
absolute `movabs` relocation patched after both mappings exist, so it has no
implicit +/-2 GiB code-to-data assumption. A later RIP-relative form is legal
only when `DataLayoutPlan` and the post-allocation emitter verifier prove the
signed displacement range. The emitter verifies entry count, every trampoline
label, table-base and entry relocation site/width, and default behavior before
encoding.

Before adding the SIR terminator, successor enumeration, edge identities and
arguments, register uses, renumbering, dominance traversal, serialization, and
display are centralized in common `SIRTerminator` APIs. Optimizer and backend
passes consume those APIs; adding a case edge cannot be silently omitted by an
old exhaustive match.

Pattern/order/edge-identity fields are private and immutable through ordinary
SIR optimization; case/default targets and the edge-argument list, order,
roles, and types are likewise opaque. Register/block renumbering may update
only typed references through the common API. The function retains the
formation-verified decision-origin table, and every post-pass verification
compares selector identity, case/source-arm IDs, patterns, priority ranks,
targets, edge identities, and edge-argument correspondence against it. An
optimizer that genuinely rewrites a decision must
instead emit a `DecisionRewritePlan` relating the old and new test graph and
pass its semantic output verifier before the origin table is replaced. Swapping
overlapping priority arms and swapping their ranks together is therefore not a
way to evade verification.

## 4. Predicate-aware vectorization

SLP/store combining runs after the final control tree exists. A pack may contain
only operations with the same execution predicate and compatible memory
dependence. It may combine adjacent scalar loads/stores or wide copies with
XMM/target vector operations, but it cannot pull an unselected arm back into
eager execution. Compare+branch, fixed-register sequences, release/event
publication, `MemCopy`, and an established SLP pack are indivisible bundles for
later pressure cuts.

The initial native pack is deliberately a `VectorMemPack`: an indivisible
memory-transfer bundle whose XMM scratch registers are fixed internal
temporaries with explicit uses/clobbers. No vector virtual value, phi, bundle
input, or bundle result may be live outside it; externally visible values
remain memory or ordinary GPR values. Its verifier checks same predicate,
alias/order, alignment/width legality, complete byte coverage, fixed-scratch
availability, and zero XMM live-in/live-out. The pressure model charges the
bundle's class-specific scratch occupancy, while the existing `K = 14`
coloring contract remains specifically the GPR class. Arithmetic SLP with live
vector values is not enabled until class-specific liveness, homes, Perm
matching, pressure cuts, and coloring have their own verifier contract; it is
not smuggled through the GPR allocator.

For the initial implementation, "same execution predicate" means the scalar
operations are ordinary unpredicated instructions in the same verified MIR
basic block after decision legalization, with no branch/effect boundary inside
their dependence window. `MIRBlockId` is the predicate witness and the output
verifier rechecks membership/order. Cross-block packs are inapplicable until a
persistent SIR-to-MIR predicate identity and equivalence verifier are designed;
the provenance predicate is not guessed from layout.

`SLPPlan` is a semantic witness. For each pack it records every replaced scalar
operation, its execution predicate and order, source/destination byte range,
the exact scalar-bit to vector-lane/byte mapping, target endianness, alignment
fact, vector width, and fixed scratch/clobber set. Verification proves the pack
touches exactly the union of the scalar bytes with no gap or extra access,
preserves load/store order for every may-alias pair, has identical fault/
addressability bounds, and reconstructs each scalar result or store byte.
An output verifier matches the witness to the emitted vector bundle and proves
all replaced scalar operations disappeared exactly once. A same-predicate or
same-width heuristic without this lane/byte proof is not an accepted pack.

### Verified MIR scheduling

Scheduling has an explicit verified input, not a collection of dependencies
rediscovered inside the heuristic:

```text
MIRScheduleInput
  normalized CFG and instruction/bundle identities
  SSA def-use and block live-in/live-out facts
  RAW / WAR / WAW edges from verified may-alias classes and original order
  effect/publication, trap/fault, predicate/control, terminator edges
  fixed-register/clobber and machine-constraint edges
  indivisible bundle membership/order

MIRSchedulePlan
  exact per-block permutation of instruction or bundle identities
  original and selected GPR/class pressure peaks and weighted live integrals
```

The plan producer is one deterministic forward list scheduler over each
verified bundle DAG. It first computes successor critical-path length in reverse
topological order, initial remaining-use counts, and class-specific live chunk
counts. Its indexed ready heap uses this complete priority tuple (lower first
unless stated otherwise):

```text
(prospective class peak vector in canonical target-class order,
 live-chunk vector after the bundle,
 newly live result chunks,
 negative last-use chunks,
 negative critical-path length,
 original bundle ordinal)
```

Bundle internal fixed scratch/clobber occupancy is included in prospective
peaks. Scheduling a use decrements its remaining count; only the transition to
one remaining use changes another ready bundle's last-use score, found through
the intrusive unscheduled-use list. Newly ready bundles and that final user are
updated in the indexed heap, so a large fanout is not rescanned after every
use. With `N` bundles, `M` dependence edges, and `U` operand uses, producer work
is `O(N + M + (N + U) log N)` and storage is `O(N + M + U)` per function.

The producer computes the exact identity and list-candidate metrics once. A
class peak counts simultaneously live chunks plus bundle scratch at every
boundary; its integral is the sum of live chunks times the block's exact
`ReachWeight`. Products/sums use a typed `FallibleNaturalCostArena` over the
same pre-reserved flat-word substrate as verified constants, but in a distinct
non-interchangeable ID namespace. It never constructs `BigUint`/`BigInt`, so
arithmetic neither wraps nor imposes a semantic function-size limit. The
candidate is selected only if peak and integral are componentwise
no greater than identity for every target class; otherwise the prescribed plan
is identity. This comparison is part of the one construction rule, before the
output verifier, not a retry after verification failure. The verifier derives
both metric vectors independently.

The input verifier derives SSA edges and every may-alias/effect/trap order from
unscheduled MIR, lowering-origin links, conservative target alias rows, and
the original instruction order. This pre-schedule dependence graph is distinct
from post-schedule `MIRMemoryTokenAnalysis`; a later token analysis cannot be
used retroactively as proof that an earlier reordering was legal.

The output verifier requires an exact permutation, keeps terminators and fixed
positions legal, preserves bundle contiguity/internal order, and proves the
selected order is topological for every input edge. It recomputes virtual
liveness/pressure from the selected order. For every target register class in
the target's fixed canonical class order, both peak occupancy and weighted
live integral must be componentwise no greater than the original; an identity
permutation is always legal, while a pressure-reducing reorder is the first
spill-avoidance mechanism. There is one plan, no retry or instruction
count cutoff. All instruction-indexed facts are invalidated afterward;
`MIRMemoryTokenAnalysis`, CSSA, next-use, loop/SCC facts, and every allocator
input are rebuilt from the scheduled MIR.

At the step-4 production switch, legacy `BranchifyMux` is removed from the new
pipeline's pass registry/API and is not invoked before or after placement or
SIR optimization. It exists only inside the explicitly legacy pipeline before
that switch. Any future select-to-control transform is a new
`DecisionRewritePlan` with semantic input/output relation and full SIR/MIR
revalidation; generic CFG/SSA verification alone cannot prove mux equivalence.
An untracked second control selector cannot create new diamonds.

After semantic-frontier splitting, `PreScheduleCFGNormalization` materializes branch
edge blocks and explicit machine-constraint markers before any scheduler or
allocator analysis. `MIREdgeLineage` retains each original ordinary edge or
`DecisionEdgeId` through decision legalization, native `MDecision`, normalized
MIR edges, later cut/split edges, phi/Perm rows, parallel-copy fragments, and
the final `EmissionFragment`. A lineage row maps one logical edge to a nonempty
ordered descendant path, not one arbitrary descendant. Every synthetic segment
has a typed owner (`DecisionTrampoline`, `FrontierSplit`, `CutMaterialization`,
`Reload`, or `CopyStub`) and exactly one predecessor/successor position on that
path. The row designates exactly one edge-exclusive `copy_segment`; it must
execute iff that logical edge executes and cannot be shared with another arm.
Two case edges with different arguments never merge merely because they share
a target. Every CFG-changing phase emits and verifies the next total lineage
relation, segment coverage/order, and copy ownership, then rebuilds CFG,
dominance, and liveness facts; no stale pre-transform fact is reused.

Normalization also derives a `ScratchEligibleEdge` superset using only the
pre-schedule CFG and machine constraints: every incoming edge of an existing
multi-predecessor block (the only places where ordinary/pruned-IDF
reconstruction phis can arise), every edge with phi/decision arguments, every
constraint-marker predecessor, and every semantic cut-candidate edge. It emits
a verified `EdgeCopyScratchReservation` fixed-register bundle on each such
lineage edge. Later split/cut/reload/copy segments inherit the reservation;
topology verifiers prove that post-materialization phi/Perm/cut copies occur
only on a descendant of this exact superset. A newly introduced copy on any
other lineage is invalid rather than grounds for late scratch allocation.

The target row declares the physical register, register class, width, implicit
flag effects, and clobber semantics. At the copy segment, that register is
removed from every live-through source and destination allowed-color mask;
the constraint/Perm matching verifier checks the exclusion and rejects a
required-color conflict. The marker is visible to scheduling, liveness,
pressure, MIN, and coloring. Consequently stack/home copies and cycle breaks
cannot alias a logical source/destination or borrow a register that allocation
did not reserve.

The initial target's spill, reload, state-reload, cut-store, rematerialization,
and copy primitives form a closed `PostMaterializationPrimitiveInventory`.
They may use only RSP/R15, immediates, their symbolic frame home, and the
inherited edge scratch; they introduce no new allocatable fixed operand,
early-clobber, or clobber marker. A future primitive with another constraint
must add a `PotentialInsertionConstraint` to the pre-schedule lineage and
`ScratchEligibleEdge` derivation before it can be selected. Post-materialization
normalization verifies this inventory, so a late Perm/copy edge cannot arise
from an undeclared constraint.

## 5. PressureRegion planning

A `PressureRegion` is a full register cut inside one native function. It is not
a separate function and does not impose a call ABI. The function keeps one
prologue/epilogue; regions connect with direct jumps/fallthrough. R15 (the
simulation-state base) and RSP are the only permitted implicit machine state
across a full cut. A versioned `TargetResourceInventory` enumerates every
allocatable class, FLAGS/condition state, fixed vector/scratch resource, and
other implicit target state. The full-cut interface requires zero live-through
value in every allocatable class, dead FLAGS, zero vector/bundle live-out, and
no undeclared implicit resource; a future register class is rejected until it
adds a materialization/interface rule.

Every cross-region logical value has exactly one complete recipe:

```text
VerifiedMIRConstantValueId = checked dense ID in the function-local verified
constant table; its row has exact lineage to a verified occurrence constant or
a closed MIR-derived constant rule

CrossValueMaterialization =
    ConstantRemat(VerifiedMIRConstantValueId)
  | StateReload(StateReloadRecipe, lazy reconstruction sites)
  | BoundaryHome(home,
                 edge-sensitive source stores,
                 lazy reload/reconstruction sites)
```

The referenced constant row carries exact canonical value/mask limb ranges,
width, signedness, and Bit/Logic domain and is content-compared by the backend
verifier. The initial rematerialization form is only that exact constant; an
expression rematerializer requires a future independently checked semantic
recipe. `BoundaryHome` is one paired materialization kind, not separate
"store" and "reload" choices. Its stores cover every incoming cut edge with
that edge's logical source, while reloads remain lazy near successor uses.

Reloading every value at region entry is forbidden because it recreates the
pressure. Existing pruned-IDF reconstruction places fresh representatives and
merge phis. For a phi crossing a cut, every predecessor stores its
corresponding logical source to the same home; the all-path home verifier proves
the successor reload.

Candidates come only from semantic frontiers: decision entry/merge edges,
completed top-level store roots, memory-effect component boundaries, and
verified loop/SCC entry or exit edges. A legal instruction frontier is first
split into real MIR blocks, before CFG normalization, CSSA, next-use, loop, or
spill analysis. `PressureRegion` therefore partitions whole blocks and cuts
only real edges; it never invalidates an analysis by splitting a block after
that analysis. Compare/branch, machine-constraint, release/event, `MemCopy`,
and SLP bundles cannot be split. Fixed instruction counts and maximum region
counts are not candidates or guards.

`PressureRegionTree` is independently reproducible. The verifier first
condenses SCCs, numbering each component by its least stable block ID; every
loop/irreducible SCC is one rigid atom. It computes dominators and
post-dominators on the condensation DAG. For each semantic frontier it derives
the complete boundary-edge set and the unique maximal block set whose entry
dominates every member, whose exit post-dominates every member, and whose only
external edges are that boundary. Rows failing this SESE equation are not
candidates. Equal block/boundary sets merge their sorted frontier origins.
Candidates are sorted by `(block_count, entry_id, exit_id, first_origin_id)`;
two overlapping sets with neither containing the other and every candidate
inside their overlap component become one typed `CrossingRigidGroup` and are
ineligible for internal cuts. For each remaining laminar set, the parent is the
least strict superset (ties by the same tuple); the whole function is the root,
and uncovered atoms become residual leaves in stable ID order. The tree
verifier rederives all sets, duplicate merges, crossing groups, parents, and
child order without consulting the proposed tree.

`MIRMemoryTokenAnalysis` is an explicit post-scheduling artifact, separate
from `SLTStateTokenAnalysis`. It recomputes reaching tokens from actual MIR
memory/effect order using the same `WriteDomainId` and conservative
read-class-to-domain relation. If lowering must refine domains, it records an
explicit conservative mapping back to every SLT domain; equality of unrelated
numeric IDs is never assumed. Lowering records checked memory-origin links
where they exist, and a cross-artifact verifier checks the same state object,
read class, and complete mapped write-domain set. Absence of such a link only
disables state reload for that value.

An unchanged-token fact alone is not a reload recipe. Every proposed state
reload carries a `StateReloadRecipe` identifying the state object/address,
value and mask lanes where applicable, byte/bit slice, load widths, endianness,
concatenation order, and zero/sign/no-extension operations needed to reproduce
the exact logical value. Its verifier symbolically checks the recipe against
the value's defining origin and proves that every referenced byte has the same
reaching MIR token at the reload. Only that semantic equality proof permits
the token-equivalent state option. It also proves from target memory-object
metadata that the read is nonvolatile, non-MMIO, non-atomic/non-observable,
properly aligned, address-valid and nonfaulting at every proposed insertion
site, and has no publication, trap, or ordering effect absent from the original
value. The insertion site must dominate its reconstructed uses without crossing
a conflicting effect/trap barrier. Same bytes/token alone are insufficient; if
any property is unproved, `StateReload` is not a legal candidate and the value
uses constant rematerialization or `BoundaryHome` instead.

Before cut selection, `CandidateCrossValuePlan` is built for *every* eligible
tree boundary. Edge-sensitive liveness derives its exact cross-value/source
set. For each value it lists every legal `ConstantRemat`, verified
`StateReload`, and typed `BoundaryHome` recipe and computes exact target work
from the closed primitive inventory, rounded load/store width/alignment, and
the boundary edge's `ReachWeight`. It selects the least work; ties use
Constant, then State, then BoundaryHome, then stable value/home ID. The plan
and cost are independently verified before the selector runs. `CutPlan` is an
exact projection of these already selected rows for detached boundaries and
cannot change a recipe, source, or cost after profitability used it.

`ReachWeightArtifact` covers every block/edge exactly once. A verified complete
same-build profile uses nonnegative counters divided by their global nonzero
GCD; without one, the initial static rule assigns weight one to every
executable block/edge. All weights and target primitive/spill costs are
arbitrary-precision unsigned integers. A missing/partial/stale profile is not
mixed with static weights.

PressureRegion selection precedes, but does not iterate with, the one
Braun--Hack `SpillPlacementPlan`. Here `MIN` is Braun--Hack's resident-set
operation: at each program point it keeps pinned/current operands and evicts
the unpinned logical value with farthest verified next use until the register
set meets both its class capacity and the exact constraint-matching contract
below. Global next-use feeds the pre-cut cost model; actual regional MIN
uses only the verified cut-projected `RegionalNextUse` defined below.

```text
PressureEvent
  stable event / deepest tree owner / program-point Euler interval
  register class / signed live-chunk delta / ReachWeight

PressureCostSummary
  canonical persistent range-delta root over owned PressureEventIds
  mandatory transfer/copy primitive rows already present in MIR
  selected CandidateCrossValuePlan rows for a detached boundary
  finalized per-class excess integral and arbitrary-precision total cost
```

The persistent range tree has one leaf per canonical program point and stores
class live chunks, weighted `max(0, live_chunks - class_capacity)`, lazy deltas,
and subtree min/max; range-add descends whenever a delta crosses the capacity.
`fuse(a,b)` applies the disjoint event roots and concatenates already verified
mandatory primitive rows in stable ID order. `finalize` reads the root's exact
weighted excess and adds only (a) target spill-unit cost, (b) the preverified
candidate materialization rows, and (c) existing mandatory decision/edge
transfer primitives. It never predicts post-color affinity copies. Persistent
merge/range visits are memoized by canonical root pair; their distinct count is
reported and included in the complexity contract rather than called linear.
The resulting exact Celox target proxy is:

```text
cost = sum(reach_weight * excess_live_chunk_integral * spill_unit_cost)
     + sum(cut_edge_weight * exact_boundary_store_reload_work)
     + transfer/copy work
```

Every live-range contribution is owned by its deepest laminar region and
referenced once by the persistent Euler structure, so child summaries compose
without running MIN for each alternative. This is a checked Celox profitability
heuristic, not a claim that the proxy equals the eventual optimal spill count
or a theorem from Braun--Hack. Laminar decision regions use one deterministic
bottom-up `fuse` versus `full cut` selector. General CFG is first SCC
condensed; irreducible and reducible loop SCCs are atomic in the initial design,
so there is no undefined internal cut-set. On the resulting acyclic graph,
dominance/post-dominance constructs a canonical laminar
`PressureRegionTree` of verified SESE regions. A semantic frontier is a legal
cut candidate only when it is the complete incoming and outgoing edge boundary
of one tree node; crossing candidates and rigid residual subgraphs remain
atomic. Every
block belongs to exactly one leaf/residual owner, so deepest ownership and
Euler aggregation are defined.

`CutSelectionWitness` records the exact postorder recurrence. A row starts with
the node's own open `PressureCostSummary` and zero closed cost, then folds
children in canonical tree order. For each child it constructs exactly two
candidates: `attach` fuses the child's selected open summary into the current
open summary and adds its already closed cost; `detach` keeps the current open
summary and adds `finalize(child.open) + child.closed +
boundary_materialization(child)`. It compares
`finalize(candidate.open) + candidate.closed` using fallible arbitrary-
precision integers, selects the smaller, and selects `attach` on equality;
remaining structural ties use the lexicographic boundary-edge list. The row
stores both summaries/costs and the choice, and the verifier recomputes them.
`boundary_materialization(child)` is exactly the already verified
`CandidateCrossValuePlan` projection for that boundary, never a post-selection
estimate.
The root is finalized once. This recurrence is deliberately a reproducible
linear tree heuristic, not a claim of globally optimal CFG partitioning.
`detach` cuts both sides of the child's complete boundary and charges
materialization in both directions; the selector is never applied directly to
a general DAG. After selection,
remove the union of selected boundary edges and compute the maximal weakly
connected components of the remaining CFG. Those components, not a tree node
with a hole, are the final pressure-region partition; parent prelude, child,
and parent continuation may therefore be three regions. The verifier proves
every original edge is either internal to exactly one component or is an
explicit full-cut edge, and recomputes all cross-value recipes on that final
partition. The acyclic graph uses finite
integer `ReachWeight`s from profile counts or normalized static edge weights;
an opaque residual/loop's internal work is identical in both boundary
alternatives.

After selection, `W_exit = W_entry = empty` is the planner's resident-register
boundary condition, not a claim that pre-materialization MIR liveness is
already empty. Actual zero liveness for the complete
`TargetResourceInventory` is established only by `CutResult`.
Each exact constant/state/boundary-home recipe is selected first in
`CandidateCrossValuePlan` and projected unchanged into `CutPlan` for chosen
boundaries; Braun--Hack may neither add nor replace coupling on the cut edge.

Before MIN, a verifier constructs one `RegionalAllocationInput` per maximal
component:

```text
RegionalAllocationInput
  region / owned blocks and internal edges
  one synthetic entry node/edge per incoming cut edge, plus analysis root
  exact original target / MIREdgeLineage / edge-specific phi operand
  edge-indexed incoming recipe definitions and lazy availability
  outgoing BoundaryHome CutStore uses and constant/state-remat terminations
  RegionalNextUse
  region-local CSSA classes and home namespace
  RegionalValueLineage slice

RegionalNextUse
  independently rebuilt from the materialized regional CFG/instructions
  incoming synthetic recipe definitions as fresh starts
  BoundaryHome segments terminated by fixed outgoing CutStore uses
  constant/state-remat source segments terminated at the cut and successor
    segments restarted by their fresh rematerialization/reload definitions
  internal loop/backedge next-use preserved
```

The analysis root connects to distinct per-incoming-edge synthetic nodes and
never merges their meanings. The regional CFG verifier proves a one-to-one
relation to original cut edges, targets, lineage, recipes, and edge-specific
phi operands. Each node seeds only the availability proved for its predecessor.
Only `BoundaryHome` emits a fixed outgoing `CutStore` use; constant/state
rematerialization ends the source segment and starts a fresh successor value.
Pre-cut global next-use is used only by cut-selection cost and is never reused
as allocator input. `RegionalNextUse` is recomputed over the materialized
regional instruction graph as a reverse multi-source shortest-path problem on
the finite sparse product graph `(live logical value, program point)`. Each
real use is a zero-distance source; checked nonnegative instruction/edge/
loop-exit weights and stable use IDs define one total lexicographic distance.
Deterministic Dijkstra relaxation produces the least fact, and an unreachable
pair is `NoNextUse`. A separate verifier checks every local-use and successor
Bellman equation. Thus loop SCCs and backedges need neither a convergence guess
nor an iteration cap: with `F` materialized sparse facts and `T` product
transitions, construction is `O((F + T) log F)` time and `O(F + T)` storage.
Internal loop/SCC backedges remain inside the atomic region and participate in
that same finite graph. Region-local home identity is
`(PressureRegionId, CSSACongruenceClass)` and is disjoint from boundary-home
identity, which allows the regional spill arena to be reused safely.

The sparse product domain itself is independently total: edge-sensitive
liveness plus local/synthetic definitions and uses derives exactly one row for
every `(LogicalValue, BeforeUses|AfterUsesBeforeDefs|block entry|block exit)`
at which the value is live or locally referenced, and no other row. Every
product transition has both endpoint rows; `NoNextUse` is an explicit fact,
not an omitted row. The verifier compares this domain and inverse incidence
before Bellman equations, so deleting an inconvenient value/point row cannot
make a smaller self-consistent analysis pass.

The regional Braun--Hack state is proof-bearing:

```text
RegionalSpillState
  W_entry / W_exit: block -> canonical resident LogicalValue set
  S_entry / S_exit: block -> canonical resident-with-valid-home set
  before_uses / after_uses_before_defs states for every instruction
  edge coupling rows and deferred-backedge rows
```

`S` is always a subset of `W`. For `v in S`, the same typed symbolic home is
valid on every region-root-to-point path; a nonresident live value which can be
reloaded has the corresponding all-path `HomeValid` fact. A freshly defined
value enters `W` but not `S`; a reload from a proved home enters both; exact
rematerialization enters `W` and its separate rematerializable class. Eviction
removes a value from both sets. It emits a store exactly when the value has a
future use, is neither rematerializable nor already in `S`, and therefore needs
its first valid home on that path. The all-path verifier rejects a reload not
dominated on every path by that store/home or a boundary recipe.

Blocks are processed in deterministic regional reverse postorder. For an
ordinary block, `must = intersection(pred.W_exit)` and
`may = union(pred.W_exit)` over already processed predecessors. `W_entry`
starts with `must`, applies the same stable farthest eviction until the entry
constraint is matchable, then admits values from `may - must` by increasing
`RegionalNextUse` and stable logical ID while the entry constraint remains
matchable. At a loop/SCC header, unknown backedges are not consulted: candidates
are exactly live-in/phi logical values with a verified use in that loop region,
ordered the same way; live-through-but-unused values are excluded. This is the
fixed Celox loop-header adaptation and makes their reload occur on the incoming
edge rather than inside/back around the loop. Once chosen, a header entry state
never changes.

For the known predecessors of `B`, first set
`S_entry[B] = (union(pred.S_exit)) intersect W_entry[B]`. Coupling an edge
`P -> B` then emits:

```text
ReloadOrRemat = W_entry[B] - W_exit[P]
Spill = (S_entry[B] - S_exit[P]) intersect W_exit[P]
```

and the verifier proves that the post-coupling edge state is exactly
`W_entry[B], S_entry[B]`. A loop backedge whose predecessor is not yet
processed records a checked deferred row and applies these same equations once
that predecessor is complete; it cannot revise the header state. Cut edges are
owned exclusively by `CutPlan` and never receive this coupling.

For each instruction `I`, let `R = uses(I) - W`. The plan first inserts the
proved reload/rematerialization for every `R`, adds those uses to `W` (and to
`S` only for a valid-home reload), and runs `limit` at `BeforeUses` while all
uses are pinned. After the uses execute, it removes every dying use with
`NoNextUse`, then runs a second `limit` at `AfterUsesBeforeDefs`, measuring
next use from `I.next` and reserving the exact result/early-clobber constraint.
Finally it adds definitions to `W` and removes them from `S`. `limit` evicts the
unpinned value with greatest `RegionalNextUse`; equal distances evict the
greatest stable logical ID. It repeats only until the phase's exact matching is
feasible. Thus a dying operand color can be reused by a normal result, while a
result cannot be treated as available before its definition.

`SpillPlacementPlan` records every state and operation above. Its independent
verifier rederives block initialization, both per-instruction transitions,
deferred coupling, `S subset W`, all-path home validity, and the rule that a
logical SSA value is stored at most once on any root-to-point path (mutually
exclusive edge stores are permitted). Missing/extra stores, reloads, states,
or coupling rows fail even if final pressure happens to be small.

MIN runs once inside each final region with those fixed inputs. A logical value
that crosses a cut may still have a source- or successor-side regional segment
spilled normally inside that region; only independent materialization or
coupling *on the cut edge* is forbidden. Regional results and cut recipes are
combined into one symbolic `SpillPlacementPlan`, followed by one global
pruned-IDF SSA reconstruction. The plan contains complete store/reload/
rematerialization sites, typed symbolic home identities, size/alignment
requirements, and regional-arena ownership, but no concrete frame offsets or
copy-temporary requirement. It is materialized once; coloring failure never
requests another cut.

After reconstruction, `PostMaterializationCFGNormalization`, final phi/Perm
materialization, and their pressure/home verifiers have succeeded, one
`FrameLayoutPlan` assigns concrete offsets. The frame contains a boundary-home
area, one reusable regional spill arena, and an explicit parallel-copy
temporary area.
Boundary homes initially receive unique identities with size/alignment
requirements; there is no unproved memory-live-range slot coloring. Full cuts
prove that region-local homes do not survive into another region. The
`FrameLayoutPlan` allocates one arena sized and aligned to the maximum regional
requirement rather than the sum of regional frames, plus the maximum
copy-temporary size/alignment conservatively derived from the now-final typed
phi/Perm rows and the target primitive inventory before coloring. For each row
the target enumerates every legal register/home source-destination combination
and its rounded temporary size/alignment; layout takes the maximum, not the raw
semantic bit width. One slot is shared because copy segments execute
sequentially and the resolver/verifier permits at most one active saved cycle
component at a time, restoring it before starting the next. The later
`ParallelCopyPlan` may use less but
cannot request a larger slot, so frame layout depends on neither colors nor
copy resolution and there is no phase cycle.

Input `CutPlan` verification proves region partitioning, legal edges, exact
edge-sensitive planned cross-value sets, one valid materialization kind per
value, MIR memory-token/reload-recipe facts, and boundary-home identity plus
size/alignment. It does not assign final offsets or claim that
pre-materialization liveness is already empty. The `SpillPlacementPlan`
verifier proves every symbolic ordinary home, store/reload/rematerialization
site, type, regional ownership, and size/alignment requirement before MIR is
changed. The later `FrameLayoutPlan` verifier proves concrete boundary and
regional-home offsets, arena maximum reuse, frame nonoverlap/alignment/bounds,
copy-temporary capacity, and total coverage of those symbolic homes. A separate
post-reconstruction `CutResult` verifier proves all-path stores, the complete
zero/allowed target-resource interface across each full cut, reload dominance,
phi meaning, and the
existing pressure/home/Perm contracts against that final layout.

`PostMaterializationCFGNormalization` is a separate output relation after SSA
reconstruction. It normalizes only newly inserted spill/reload/cut blocks,
rebuilds dominance/liveness, materializes final Perm rows, and extends
`MIREdgeLineage`; it never reuses or reruns the pre-schedule normalization
analysis.

### Constraint feasibility and constructive coloring

Before home formation, `PreSpillCSSANormalization` constructs Method-I CSSA.
For each existing `d = phi(s_0, ..., s_n)`, it inserts a fresh edge-local
`s'_i = s_i`, a fresh phi result `d' = phi(s'_0, ..., s'_n)`, and an entry copy
`d = d'`; the already normalized edge blocks make each source copy execute on
exactly one edge. A separate edge-sensitive liveness verifier rebuilds phi
congruence classes and proves that no two members of a class interfere. The
class owns one typed symbolic home identity. This paragraph is normative and
does not import correctness from the current interim implementation described
in [Native register allocation](./native-register-allocation.md).

`RegionalValueLineage` is a total relation from each original CSSA logical
value/class to its source-region segment, every edge-specific synthetic entry
definition, successor-region segment, boundary recipe/home where applicable,
reconstruction definition/phi, and final use. Every regional or reconstructed
value has one inverse row. A synthetic entry or reconstruction phi may combine
only versions of the same original class and same regional/boundary home; it
cannot merge equal-typed values from different homes. `RegionalAllocationInput`,
`SpillPlacementPlan`, reconstruction, and `CutResult` each verify and extend
this same-ID lineage rather than recreating equivalence from value shape.

Machine feasibility is not reduced to the scalar condition `|W| <= K`.
The target supplies, at every instruction/bundle/edge-scratch marker, the
allocatable colors for each register class, exact required operand/result
colors, clobbered colors, tied operands, early-clobber results, and fixed
internal occupancy as one verified `TargetConstraintPoint`:

```text
ConstraintEntity
  stable operand/result/live-through chunk identity and register class
  present_at: BeforeUses and/or AfterUsesBeforeDefs
  allowed/required color mask

ConstraintEquality
  target-required tied operand/result entities which must have one color

ConstraintInterference
  distinct equality-quotient entities simultaneously present in a phase

TargetConstraintPoint
  BeforeUses: ordinary uses + live-through + early-clobber results
  AfterUsesBeforeDefs: live-through + all results
  clobbers / fixed scratch and implicit occupancy per phase
```

Normal dying uses are absent from the second phase, so an untied result may
reuse their color. An early-clobber result is present in the first phase and
interferes with every untied use required by the target. A tied operand/result
forms one equality-quotient entity across both phases; allowed masks are
intersected and contradictory required colors are invalid. Live-through
entities retain one color across phases and exclude clobbered/fixed scratch
colors. The verifier derives phase presence and all equality/interference rows
from target instruction semantics; a producer cannot mark two simultaneous
entities noninterfering.

Regional MIN pins current operands and any value required by the instruction.
For each of the two phases it constructs the bipartite graph from equality-
quotient entities to allowed colors and requires an injective matching for
every `ConstraintInterference` clique, with cross-phase equality/live-through
colors fixed. It processes quotient entities by stable class/chunk/value ID;
the canonical augmenting-path matcher visits physical colors in target order
and predecessor entities in that same stable order. This defines one
byte-reproducible matching. MIN evicts the farthest-next-use unpinned row until
both linked phase graphs have a matching, not merely until cardinality is at
most `K`. If the still-pinned graph has no
matching, instruction selection or the machine-constraint producer is invalid;
the allocator cannot retry, silently spill an operand required at that point,
or borrow an unrecorded register.

After spill reconstruction, every constraint marker receives one full-live
`PermConstraintPlan` row containing exactly the values register-live at the
boundary, fresh result identities, the complete two-phase equality/
interference/allowed-mask relation, and the canonical linked matchings.
Dominated uses are renamed to the
fresh results. The verifier independently rebuilds boundary liveness and
allowed masks, checks one-to-one row coverage, the matching, required colors,
clobbers, tied operands, and renaming dominance, and proves that the Perm
disconnects the interference components on its two sides. A memory-resident
value has no row; its later reload/rematerialization is already a fresh
definition. This is why each full-live Perm has at most the proved class
capacity after spilling.

`FinalColoringPlan` then colors each strict-SSA component once. It seeds each
Perm-entry component with that verified matching and scans the dominance-based
perfect-elimination order for chordal SSA interference, choosing only a color
outside the exact currently-live forbidden set and the row's required/
clobbered mask. Phi congruence and two-address relations are weighted
preferences only. The coloring verifier independently rebuilds edge-sensitive
liveness and the perfect-elimination relation, covers every allocatable VReg
exactly once, and checks every simultaneous interference, fixed color,
clobber, tied operand, and Perm seed. Because pressure/matching feasibility is
proved before this scan, coloring failure is a producer bug and never requests
more spilling or a different cut.

## 6. Final phi-congruence affinity

Method-I CSSA before home formation remains unchanged. After reconstruction
and Perm materialization, the final phi graph is rebuilt with a DSU and its
components are classified by an independent interference check. A component
proved conventional may use component-wide preferences. A non-conventional
component is still valid MIR, but receives only direct-pair and ordinary
weighted affinities. It is not silently treated as CSSA and is not a reason to
renormalize after Perm.

Components are not contracted to one mandatory color. Coloring uses a
component-wide soft preference:

- one consistent required color, or a nonempty intersection of future allowed
  masks, may seed the preferred color;
- conflicting required colors or an empty allowed-mask intersection produce
  no class seed (never an error by themselves); members then use only feasible
  local/weighted affinities;
- the first feasible member may establish a preference for later siblings;
- two-address and weighted phi affinities contribute preferences;
- required colors, forbidden/clobbered colors, the current active set, and Perm
  perfect matching always take precedence; and
- if the common color is unavailable, that member is colored locally and the
  residual move remains for verified SSA destruction.

This propagates color intent between sibling decision arms and toward future
RCX/RAX/RDX constraints without making coloring failure trigger spilling. The
classification verifier reuses the streaming edge-sensitive liveness analysis
and accounts for `O(instructions + uses + phi rows + sparse live facts)` work;
DSU/selection then costs
`O((vregs + phi rows) alpha(vregs) + K * vregs)` with `K = 14`.

## 7. Copy-aware block layout

Before layout, `ParallelCopyPlan` is derived from the final normalized
phi/Perm rows, colors, homes, and `MIREdgeLineage`. It covers every phi/Perm
destination on every edge exactly once and introduces no other logical copy.
Its verifier interprets each edge copy as simultaneous assignment, checks
source values before any overwrite, and replays acyclic moves and cycles to the
declared final assignment. A cycle temporary is an explicitly sized/aligned
frame-owned copy slot in `FrameLayoutPlan`. Stack/home-to-stack/home transfers and
cycle save/restore use only the lineage edge's pre-allocation
`EdgeCopyScratchReservation`; there is no hidden allocatable GPR. The verifier
checks the target copy primitive's widths, register clobber, frame accesses,
and scratch bundle coverage.

The plan covers an identity assignment as `ElidedIdentity`: it consumes one
logical phi/Perm row but emits zero instructions. Every nonidentity planned
move appears exactly once on its designated copy segment. The SSA-destruction
output verifier proves every phi/Perm row disappears exactly once and that no
allocatable virtual operand remains in `EmissionFragment`; encoded physical
register/home operands must equal the assignment side table and satisfy
required/forbidden colors, clobbers, and frame bounds. This is the final
allocation relation; a feasible side-table color that was not used by the
encoded output still fails, and successful coloring alone is not completion.

Block order then becomes a separate post-coloring artifact. SSA destruction first
materializes a typed `EmissionFragment` graph containing real blocks and
case/edge-copy code fragments plus referenced read-only table fragments; copy
stubs are no longer hidden local labels for layout purposes. Code and read-only
data receive separate mappings. Trace formation orders only executable
fragments and uses edge probability and actual copy work so the likely edge,
merge, and copy fragment can fall through together. It does not change CFG
semantics. `BlockLayoutPlan` verification proves that every executable fragment
occurs exactly once, entry is first, every fallthrough targets the next
executable CFG successor, branch targets are unchanged, copy code remains on
its selected edge, and no executable edge reaches a data fragment.
`DataLayoutPlan` separately proves every referenced table occurs once with
valid alignment/relocations; all code and table labels must resolve.

## 8. Termination and complexity contract

All trees and graphs use explicit worklists; deeply nested source control and
phi webs must not recurse on the host stack.

Constant-function loop replay is linear in the finite certificate's consumed
small-step rows plus executed typed-value/limb work. A trace exhausted in a
nonterminal loop state rejects; an accepted trace has no unread row. There is
no recursion/iteration/trace-length policy cap and no zero-step, nonprogress,
or wrap shortcut. In contrast, verification of a retained runtime
`PhaseForFold` is proportional to its finite template rows and does not execute
or claim termination of the runtime backedge.

- source-static frontier classification: `O(expression instances + typed
  operand edges + activated guard edges + static-control limb work)` time and
  linear class/control-cursor storage. Every raw expression/context instance is
  classified once; suppressed edges are typed but not value-walked, while an
  unknown/runtime controller contributes each semantically possible edge once.
  Persistent packed projections are content-interned, so equal completed roots
  share bits/value content while retaining distinct proof-origin rows; storage
  is proportional to unique projection content plus selected frontier roots,
  never paths or call-input combinations;
- canonical SLT structural verification: `O(nodes + child edges + payload
  words)` time, where payload words include constant limbs, concat/index/state/
  effect rows, one width per node
  plus packed fact bitsets, and no reverse-edge graph proportional to child
  edges after the structural scan;
- clone-free construction/deserialization canonicalization: worst-case
  `O(payload words * log nodes)` canonical comparisons and `O(nodes)` AVL
  index storage, dropped at freeze;
- source/root/provenance/control verification: linear in source/final roots,
  emitted atom expansions, explicit ordering edges, control points, gates,
  decision arms/patterns, and merge steps, plus documented dominator work;
- global action/token verification: linear in actions, explicit action edges,
  root-order-barrier endpoints, token definitions, phi operands, and sparse
  alias-domain relations; no atom-expansion Cartesian product or expanded
  transitive-set copy is
  stored;
- value resolution: let `V` be outer/nested candidate plus final-value rows,
  `P` the total operand/direct-read payload words in all value keys, `T` the
  durable Patricia nodes, `Q` the distinct transient union-root-pair visits,
  and `E` explicit token def/use/phi edges. Worst-case construction is
  `O(P log V + T log T + Q log Q + E)` plus documented CFG/dominator work,
  and storage is `O(P + V + T + Q + E)`. `Q` is reported independently and
  may be quadratic in `T`; it is not hidden behind a linear claim. Candidate/
  AVL indices and union-pair memo entries are dropped at freeze, while only
  canonical value rows, explicit relations, and durable Patricia nodes remain;
  no candidate or value row is cloned into a side-map key;
- def-use plus placement: `O((values + uses) log control_sites)`;
- gate selection: linear in its region tree; pressure-region selection is
  linear in tree rows plus the explicitly reported distinct persistent
  range-tree merge/range visits used by `PressureCostSummary`;
- exact-key clustering: `O(cases log cases)`; disjoint pattern verification:
  `O(cases^2 * selector_chunks)`;
- decision-test witness replay: linear in lowering graph nodes plus the same
  pairwise overlap relation;
- CFG/SCC/SESE analysis: `O(blocks + edges)` (or documented near-linear
  dominator cost);
- SLT and MIR token analyses: proportional to their memory actions, alias
  edges, dependency sets, and sparse reaching-token facts;
- pressure summaries: linear in owned live-range events plus the region tree;
- cut materialization: proportional to actual cross-region values;
- final congruence classification: linear in MIR/phi edges plus sparse liveness,
  followed by inverse-Ackermann DSU; and
- coloring/layout/emission: linear in MIR, CFG edges, and emitted table/copy
  entries for fixed `K`.

Every producer, decoder, transformer, and verifier phase is an atomic
`Result<OwnedOutputArtifact, PhaseErrorV1>`. The error schema is a lossless
closed sum:

```text
PhaseErrorV1 =
  Source(SourceAggregateErrorV1) |
  Brand(ArtifactBrandErrorV1) |
  Local(PhaseLocalErrorV1)

PhaseResourceWaveV1 =
  InputScan | ExpectedDerive | ProposalCompare | PlanConstruct |
  PlanVerify | OutputMaterialize | OutputVerify | Commit

OccurrenceResourceKindV1 =
  SemanticObjectRows | InputRows |
  TypedValueRows | BitsRows | BitPlaneWords | PhaseCoercionRows |
  NodeRows | NodeInputIndexEntries | NodeOperandEntries |
  NodeForStateEntries | NodeForEffectEntries |
  NodeForEffectArgumentEntries | NodeConcatPartEntries | NodeFactRows |
  UnitRows | RegionRows | PointRows | EdgeRows | RootRows | ActionRows |
  ActionOperandEntries | ActionResultEntries |
  ValueOccurrenceRows | ValueOperandEntries |
  GateRows | GateResultMergeRows | DecisionRows | DecisionArmEntries |
  DecisionPatternEntries | DecisionResultMergeRows | DecisionStepEntries |
  GatedMuxRows | ObserverRows | ObserverOccurrenceRows | RuntimeEventRows |
  RuntimeExecutionLineageRows | RuntimeLocalScopeRows |
  RuntimeCallInstanceRows | RuntimeProgramMappingRows |
  RuntimeInvokeRoleEntries | RuntimeWritebackRows |
  DynamicAddressRows | DynamicAddressIndexEntries |
  ForFoldTemplateRows | FoldRegionRows | FoldPointRows | FoldEdgeRows |
  FoldActionRows | FoldActionOperandEntries | FoldActionResultEntries |
  FoldValueRows | FoldValueOperandEntries | FoldDynamicAddressRows |
  FoldStateRows | FoldRecurrenceRows | FoldEffectRows |
  TriIntentRows | TriNetRows | TriDriverRows | TriReadRows |
  ExpectedUseRows | ExpectedResultRows | ExpectedControlRows |
  SourceMappingRows | RewriteRows | CanonicalProducerRows |
  DependencyRows | ReachabilityRows | OwnerBits |
  ConstructionIndexNodes | TraversalScratchRows | TransactionRows

ControlValueResourceKindV1 =
  ActionOrderRows | GlobalPointRows | GlobalEdgeRows |
  MemoryTokenRows | EnvironmentTokenRows | EffectTokenRows |
  TokenFlowRows | TokenPhiOperandEntries |
  CandidateRows | CandidateOperandEntries | SafetyRows |
  InstValueRows | InstValueOperandEntries | FoldInstValueRows |
  PersistentTrieNodes | UnionPairMemoRows | ResolutionRows |
  DynamicResolutionRows | ForFoldTokenOverlayRows |
  ForFoldValueOverlayRows | ScheduledEdgeRows |
  OwnerBits | ConstructionIndexNodes | TraversalScratchRows

DecisionResourceKindV1 =
  EligibilityRows | SkeletonRegionRows | SkeletonEdgeRows |
  EarlySiteRows | LateSiteRows | UseMapRows | RegionSummaryRows |
  CostWitnessRows | SelectionRows | ContractionRows | PlacementRows |
  GateFormationRows | DecisionFormationRows | DecisionPatternRows |
  DecisionCaseRows | LoweringPlanRows | LoweringWitnessRows |
  VectorPackRows | OwnerBits | ConstructionIndexNodes | TraversalScratchRows

SirResourceKindV1 =
  FunctionRows | FunctionParameterEntries | BlockRows | BlockParameterEntries |
  InstructionRows | OperandEntries | DefinitionRows | UseRows |
  TerminatorEdgeRows | DecisionRows | DecisionCaseRows |
  RuntimeFunctionRows | RuntimeInvokeRows | RuntimeAbiLogicalRows |
  OriginRows | RewriteRows | OwnerBits | TraversalScratchRows

MirResourceKindV1 =
  FunctionRows | BlockRows | InstructionRows | OperandEntries |
  PhiRows | PhiOperandEntries | ConstraintRows | BundleRows |
  DecisionRows | DecisionTargetEntries | InvokeRows | InvokeClobberEntries |
  EdgeLineageRows | DependencyRows | MemoryTokenRows |
  OwnerBits | TraversalScratchRows

AllocationResourceKindV1 =
  CfgBlockRows | CfgEdgeRows | DominatorRows | LoopRows |
  ScheduleInputRows | SchedulePlanRows | LivenessRows | LiveFactEntries |
  CssaRows | NextUseRows | NextUseEntryRows | PressureEventRows |
  PressureSummaryRows | PressureRegionRows | CutRows |
  RegionalInputRows | RegionalNextUseRows | SpillPlanRows |
  SpillPlacementRows | ReconstructionRows | PermRows |
  HomeRows | FrameLayoutRows | CongruenceRows | AffinityRows |
  ColorRows | AssignmentRows | ParallelCopyRows | CopyMoveRows |
  OwnerBits | WorklistRows | SparseFactRows | ConstructionIndexNodes

LayoutResourceKindV1 =
  EmissionFragmentRows | BlockOrderRows | FallthroughRows |
  CopyFragmentRows | DataFragmentRows | RelocationPlanRows |
  OwnerBits | TraversalScratchRows

EmissionResourceKindV1 =
  CodeBytes | DataBytes | LabelRows | RelocationRows |
  StackMapRows | UnwindRows | VerificationRows

PhaseResourceKindV1 =
  Occurrence(OccurrenceResourceKindV1) |
  ControlValue(ControlValueResourceKindV1) |
  Decision(DecisionResourceKindV1) | Sir(SirResourceKindV1) |
  Mir(MirResourceKindV1) | Allocation(AllocationResourceKindV1) |
  Layout(LayoutResourceKindV1) | Emission(EmissionResourceKindV1)

PhaseResourceDemandIdV1
  semantics: exact pipeline/rule version / wave_serial: u64 /
  wave: PhaseResourceWaveV1 / resource: PhaseResourceKindV1

PhaseResourceSiteIdV1
  demand: PhaseResourceDemandIdV1 / grow_ordinal_within_wave: u32

PhaseLocalErrorV1
  rule: PhaseLocalRuleIdV1
  owner: None | Node(u64) | Unit(u64) | Region(u64) | Point(u64) |
         Edge(u64, u64) | Block(u64) | Instruction(u64, u64) |
         Value(u64) | Use(u64, u64) | Token(u64) | Constraint(u64) |
         ResourceDemand(PhaseResourceDemandIdV1) |
         ReservationSite(PhaseResourceSiteIdV1)
  context: None | ExpectedActual(u64, u64) | Pair(u64, u64) |
           Range(u64, u64, u64) | Widths(u64, u64) |
           RegisterMask(u64) | Capacity(u64, u64)

PhaseLocalRuleIdV1 =
  Occurrence(OccurrenceRuleV1) |
  ControlValue(ControlValueRuleV1) |
  Decision(DecisionRuleV1) |
  Scheduling(SchedulingRuleV1) |
  Sir(SirRuleV1) | Mir(MirRuleV1) |
  Allocation(AllocationRuleV1) |
  Layout(LayoutRuleV1) | Emission(EmissionRuleV1) |
  Resource(PhaseResourceRuleV1)

OccurrenceRuleV1 =
  SourceMapping | Instance | InterfaceGlue | Alias | Observer | TriNet |
  RuntimeFunctionMapping | ExpectedGraph | NodeReplay | Reachability |
  Classification
ControlValueRuleV1 =
  TokenDefinition | TokenFlow | Safety | Candidate | Resolution |
  Dependency | RuntimeFunctionBoundary | ForFoldBoundary | ExpectedBijection
DecisionRuleV1 =
  Skeleton | Placement | Region | Gate | Pattern | Profitability |
  Vectorization | Witness
SchedulingRuleV1 =
  Dependency | AliasOrder | MemoryOrder | Permutation | ResourceOrder |
  PressureOrder | Witness
SirRuleV1 =
  DenseId | Entry | EdgeEndpoint | PredecessorSuccessor | Terminator |
  Definition | Use | Dominance | Phi | Opcode | Type | Width | MemoryAccess |
  RuntimeFunction | RuntimeInvoke | RuntimeCallAbi | RuntimeCallOutput
MirRuleV1 =
  DenseId | Entry | EdgeEndpoint | PredecessorSuccessor | Terminator |
  Definition | Use | Dominance | Phi | Opcode | Type | Width | MemoryAccess |
  FixedUse | FixedDef | Clobber | EarlyClobber |
  RuntimeInvoke | RuntimeCallAbi | RuntimeCallOutput
AllocationRuleV1 =
  Cfg | Cssa | Liveness | NextUse | Pressure | SpillPlan |
  Reconstruction | ColorFeasibility | Coloring | Assignment |
  EdgeLocation | ParallelCopy | SsaDestruction
LayoutRuleV1 = BlockOrder | Fallthrough | EdgeCopyPlacement | Reachability
EmissionRuleV1 = Constraint | Location | StackFrame | Encoding | Relocation
PhaseResourceRuleV1 =
  CountRepresentable | PlanRepresentable | IdExhausted | StorageAvailable
```

`PhaseResourceKindV1::ALL` expands every outer/subkind pair in written order;
`COUNT` is that checked compile-time length and every demand plan is exactly
`[CheckedCount; COUNT]`. Each expanded value names one and only one physical
growable arena and every phase-local arena, including construction scratch,
has one value. Adding/splitting storage changes the rule version and the
const-exhaustive resource-to-arena fixture; no numeric, `Other`, or generic
allocator tag exists. The source-local and typed-constant verifiers instantiate
the same demand/site shape with their own closed wave/resource enums, so all
three retain `wave_serial` and grow ordinal losslessly.

`CountRepresentable`, `PlanRepresentable`, and `IdExhausted` permit only
`ResourceDemand`; they identify failure before a grow is attempted.
`StorageAvailable` permits only `ReservationSite`, which identifies the one
actual `try_reserve_exact` call. Both use `Capacity`; neither may substitute a
row owner or erase the demand ID. `RULE_META_V1` and the check-site table test
these owner/context restrictions exhaustively.

The nested variant is the only lift path. A typed-constant failure is always
`PhaseErrorV1::Source(SourceAggregateErrorV1::TypedConstant(error))`; there is
no direct/flattened typed-constant variant. `From<SourceAggregateErrorV1>` and
`From<ArtifactBrandErrorV1>` are allocation-free moves preserving every field.
Brand comparison precedes local-ID lookup. The variant of
`PhaseLocalRuleIdV1` derives the phase; it is not a separately writable field.
Each phase has an exhaustive immutable rule-metadata/check-site table fixing
allowed owner/context variants and local check ordinal, with one malformed
fixture per check site. Adding a rule or enum variant requires updating that
table and fails the const exhaustiveness test otherwise.

A phase reserves/stages all output and validates
its complete input/output relation before publishing the owned artifact; error
return leaves the input and destination unchanged. Valid or invalid external
input, ID exhaustion, capacity failure, malformed CFG, infeasible machine
constraint, and verifier disagreement are ordinary structured errors. No
production path uses `panic!`, `assert!`, `unwrap`, or `expect` for them, and no
error selects a legacy allocator, partial artifact, retry, or correctness
fallback.

There is no iterative branchification, allocation retry, packed 24-bit ID,
input-dependent traversal limit, CFG cap, or legacy correctness fallback.

## 9. Verifier-first implementation sequence

1. Complete only the representation-verifier substrate: checked raw versus
   phase-branded IDs and ranges, fallible count/reservation arithmetic,
   transactional flat arenas, artifact-brand checks, closed rule/error
   registries, and the private cache-free frozen shell. Its tests construct
   malformed rows directly and must cover every rejecting check before any
   semantic producer is changed. This step does **not** construct
   `InputSemanticFacts`, derive typed constants, replay a source node, or call a
   mutable arena verified/frozen. There is no gated arena API yet because gated
   allocation must be coordinated with provenance. Measure the substrate's
   RSS/time on synthetic 100k and 1M flat-row inputs, but do not treat those
   measurements as an end-to-end performance result.
2. Close and implement the semantic relation in its dependency order. First
   flatten the patched pinned parser input into the complete private raw HIR and
   map every producer witness from its public syntax-lineage key. Next run the
   joint typed-HIR/type/constant verifier and publish only verified types,
   values, coercions, environments, and function programs. From that artifact,
   independently derive Tri intent, the complete expected source value graph,
   and `InputSemanticFacts`; compare the untrusted source proposal only after
   those expected rows exist. Only then compute the closed per-variant
   `PhaseSLTNodeFactsV1<P>` table in canonical append order, replay source
   structure, and verify roots/actions/observers, control provenance, phase
   keys, and coordinated gated registries as one aggregate relation. All raw
   source/occurrence schemas, aggregate adapters, and input/output relations
   must already be closed, with adversarial fixtures, before either producer is
   changed. Then make symbolic evaluation emit module-local
   roots/actions/observers and complete `SourceControlProvenance` through
   `SourceArtifactBuilder`; consume the successful relation into
   `FrozenSourceArtifact` and drop construction state. Only then implement the deterministic
   whole-unit draft mapper over that immutable catalog, staged
   `OccurrenceArtifactTxn`, atomization-to-`RootExpansion`, atomic global
   root/action assignment, compressed root-order barriers, ordinary-node-only
   constant rewrite relation, occurrence-valued dynamic-address/ForFold
   templates with exact semantic summaries, and observer/glue occurrence
   materialization.
   Recompute final facts, verify occurrence construction identities against
   `ControlOccurrencePlan`, and consume catalog, semantic context, arena, and
   all registries into `FrozenOccurrenceArtifact`. Verify current and legacy
   adapters at their exact aggregate boundaries; never expose an arena-only
   planner-ready wire or vector-position root identity.
3. Build and verify the occurrence-valued `GlobalActionOrderSkeleton`, compose
   and verify `ArtifactControlGraph`, construct SSA memory/environment/effect
   tokens plus exact boundary overlays for already verified runtime-function
   programs/call instances and nested ForFold templates. Construct outer,
   runtime-program-, and template-scoped versioned candidates, verify
   their normalized execution-safety classifications, and only then intern
   final `InstValue`/`FoldInstValue` tables. Build the dense occurrence value
   overlays and resolved dynamic-address/ForFold views after those IDs exist,
   then resolve all per-operand slot/edge uses. Derive and verify
   the final `GlobalScheduledActionGraph` and freeze the
   same-ID resolution overlay while nesting the exact occurrence artifact,
   after dropping construction interning maps; do not duplicate occurrence
   topology in final storage. In diagnostic mode, build and verify
   `ControlEligibilityPlan`, the maximal
   `ControlSkeleton`, state-specific use maps and legality envelopes,
   `RegionStateSummary`, the one bottom-up DP and `CostWitness`, contraction,
   the one final `PlacementPlan`, and
   `GateFormationPlan`/`DecisionFormationPlan`; report the 3,227 currently
   rejected cases. This step does not switch lowering because canonical
   Decision SIR is not available yet.
4. Centralize SIR instruction/terminator def/use/effect/edge/renumber APIs. Add
   verified shared `SIRRuntimeFunction`/`SIRRuntimeInvoke` formation, logical
   call signatures, total program/call output relations, and malformed-input
   tests; then add verified target call-frame/ABI plans and explicit `MInvoke`
   constraints to every backend and to scheduling/allocation. Instruction
   selection consumes `RuntimeCallLoweringPlanV1`, then the completed backend
   artifact must immediately pass `RuntimeCallLoweringOutputRelationV1` before
   decision legalization, scheduling, liveness, or allocation. No runtime call
   is recursively inlined or silently expanded. Next add canonical `Decision`
   SIR plus malformed-input verifier tests and teach all backends the semantics
   through explicit trampolines/legal lowering before native jump-table
   optimization. Re-run the complete step-3 pipeline, runtime-call and
   gate/decision formation output relations, optimizer origin checks, and
   backend semantic tests; only then make it the sole source-DAG lowering path.
5. Add explicit multi-successor native `MDecision` verification and verify
   `DecisionLoweringPlan` plus its `LoweredDecisionWitness` output relation,
   starting with
   sparse balanced trees and dense jump tables; accept each with semantic and
   same-build runtime tests.
6. Add same-block `VectorMemPack` through verified `SLPPlan`. Its output then
   flows through newly rebuilt frontier splitting, scheduling, liveness, and
   every later MIR analysis; no pre-SLP fact is reused.
7. Add verified semantic-frontier block splitting,
   CFG/decision-edge normalization, machine-constraint markers, verified
   `MIRScheduleInput` and one `MIRSchedulePlan`; rebuild every analysis, then
   add `MIRMemoryTokenAnalysis`, `StateReloadRecipe`, input `CutPlan`, verified
   `RegionalAllocationInput`/`RegionalNextUse`, symbolic `SpillPlacementPlan`,
   post-reconstruction `FrameLayoutPlan`, and output `CutResult`. Select
   PressureRegions first, fix planner
   empty-register cut interfaces, then run the single cut-constrained
   Braun--Hack plan inside the final regions.
8. After reconstruction, add final phi-congruence classification and
   component-wide soft affinity only for components proved conventional. Then
   add the exact `ParallelCopyPlan`, final assignment/SSA-destruction relation,
   typed code/data fragments, and copy/probability-aware
   `BlockLayoutPlan`/`DataLayoutPlan`, only after their input and
   output-relation verifiers exist.

Each step lands as a valid phase boundary. Existing binary lowering remains the
current implementation until step 4 replaces it after the complete verified
pipeline is available; it is never selected because a new plan failed
verification. The final acceptance gate requires both runners to report a
same-input full Heliodor `status=pass`, Celox to report
`compile_only=false`, and Celox wall time to be no greater than the
corresponding `veryl-cc` wall time. Compile-only status, projected time, IR
size, and a partial timing window are never accepted as performance results.
The required reproducible gate interface is:

```sh
HELIODOR_REF=7ad830fc0f8506c934b61a853ce2eadfa5926b82 \
HELIODOR_TESTS=test_soc_linux_boot \
HELIODOR_RUNNERS="veryl-cc celox" \
HELIODOR_TIMEOUT_SEC=300 \
CELOX_OPT_LEVEL=O2 \
scripts/run-heliodor-bench.sh gate
```

`gate` is distinct from the diagnostic `run` command. It installs and selects
the pinned Veryl version in benchmark-owned storage, rejects a dirty or
non-matching Celox/Heliodor checkout, builds Celox with `--locked` in a fresh
invocation-owned target directory, and forces full execution for both runners
in separate detached Heliodor worktrees. It records exactly the paired rows
produced by that invocation, validates their semantic pass markers (including
Celox's single exact native/O2/`four_state=false`/`compile_only=false` config
record and the pinned Veryl one-passed/zero-failed completion record), checks
source and executable immutability, uses monotonic process time, and exits
nonzero unless the Celox time is no greater than the Veryl time. Its
external-process-free contract fixtures are
`scripts/tests/run-heliodor-bench-gate.sh`. This makes the acceptance gate
executable, but Celox has not yet returned a competitive full Linux-boot pass
from it; merely appending diagnostic result rows still is not acceptance.

### Research boundary

The architecture composes published algorithms but is not, as a whole, a
published allocator copied verbatim:

- Method-I CSSA, Braun--Hack `W`/`S` spilling and MIN, pruned-IDF SSA
  reconstruction, dominance-derived chordal SSA coloring, and correctness-
  first out-of-SSA translation are established algorithms used as the
  constructive allocation core;
- applying that core independently to verified synthetic regional entries,
  replacing scalar `K` tests with two-phase matching-aware multi-class MIN,
  integrating early-clobber/tied constraints, full-live Perm seeds, and
  pre-reserved edge scratch are Celox/target adaptations whose guarantees do
  not follow directly from the papers and whose input/output relations Celox
  must prove;
- semantic-frontier candidates, the SESE/laminar `PressureRegionTree`, the
  additive cost proxy, and `CutSelectionWitness` recurrence are Celox-specific
  heuristics and make no optimality claim; and
- source/occurrence aggregate verification, edge lineage, state-reload recipes,
  full-cut result proofs, scratch reservations, and the executable macro gate
  are Celox-specific correctness and acceptance contracts.

Published results justify the inner algorithms only under their stated
preconditions. They do not prove Celox's region selection, mapping, machine
constraints, or emitted code; the verifiers above provide those missing
relations.

## References and implementation comparisons

- Cliff Click, [*Global Code Motion / Global Value
  Numbering*](https://doi.org/10.1145/223428.207154), PLDI 1995: the
  ScheduleEarly/ScheduleLate placement model.
- Jens Knoop, Oliver Rüthing, and Bernhard Steffen, [*Lazy Code
  Motion*](https://doi.org/10.1145/143103.143136), PLDI 1992: safe/economical
  placement without unnecessary register pressure.
- Matthias Braun, Sebastian Buchwald, Sebastian Hack, Roland Leißa, Christoph
  Mallon, and Andreas Zwinkau, [*Simple and Efficient Construction of Static
  Single Assignment Form*](https://c9x.me/compile/bib/braun13cc.pdf), CC 2013:
  sealed SSA construction and trivial-phi elimination for environment/state
  tokens.
- LLVM, [*MemorySSA*](https://llvm.org/docs/MemorySSA.html): the concrete
  `liveOnEntry`/MemoryDef/MemoryUse/MemoryPhi model against which Celox's
  Entry/MayDef/Phi memory-token design is compared. Celox retains verified
  partitions and effect streams rather than copying LLVM's one-memory-domain
  precision choice.
- Sebastian Hack, Daniel Grund, and Gerhard Goos, [*Register Allocation for
  Programs in SSA-Form*](https://doi.org/10.1007/11688839_20), CC 2006, and
  Sebastian Hack and Gerhard Goos, [*Optimal Register Allocation for SSA-form
  Programs in Polynomial Time*](https://doi.org/10.1016/j.ipl.2006.01.008),
  IPL 2006: chordal SSA interference, and the separation of spilling,
  coalescing, and coloring used by the allocator architecture.
- Matthias Braun and Sebastian Hack, [*Register Spilling and Live-Range
  Splitting for SSA-Form
  Programs*](https://doi.org/10.1007/978-3-642-00722-4_13), CC 2009: the
  `W`/`S` dataflow, global-next-use `MIN`, edge coupling, and SSA reconstruction
  which Celox runs once inside the selected full-cut regions.
- Benoit Boissinot, Alain Darte, Fabrice Rastello, Benoît Dupont de Dinechin,
  and Christophe Guillon, [*Revisiting Out-of-SSA Translation for Correctness,
  Code Quality, and Efficiency*](https://doi.org/10.1109/CGO.2009.19), CGO
  2009: correctness-first CSSA/out-of-SSA interference and coalescing.
- [LLVM `SwitchLoweringUtils`](https://www.llvm.org/doxygen/SwitchLoweringUtils_8h_source.html):
  jump-table, bit-test, and probability-weighted search-tree clustering.
- [GCC tree switch conversion](https://gnu.googlesource.com/gcc/+/refs/heads/master/gcc/tree-switch-conversion.h):
  simple-case, jump-table, and bit-test clusters.

These references supply algorithms and comparisons, not Celox's correctness
contract. The contracts above are enforced by Celox verifiers before a phase's
artifact is consumed.
