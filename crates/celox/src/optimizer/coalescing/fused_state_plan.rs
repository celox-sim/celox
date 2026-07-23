//! Analysis-only materialization model for fused StateSSA.
//!
//! Public backing stores always remain explicit.  Each FF range demand then
//! independently selects direct forwarding, pure rematerialization, or the
//! existing packed reload fallback.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::ir::cfg::SirCfg;
use crate::ir::{BlockId, RegionedAbsoluteAddr, RegisterId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct ProgramPoint {
    pub block: BlockId,
    pub instruction: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct StateRange {
    pub object: RegionedAbsoluteAddr,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DefinitionFact {
    pub point: ProgramPoint,
    pub source: RegisterId,
    pub stored_range: StateRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FragmentFact {
    pub range: StateRange,
    pub reaching_definitions: BTreeSet<ProgramPoint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DemandFact {
    pub load: ProgramPoint,
    pub range: StateRange,
    pub fragments: Vec<FragmentFact>,
    pub plan: DemandPlan,
    pub producer_block_distance: usize,
    pub loop_depth_delta: usize,
    pub rematerialization_cone_instructions: usize,
    pub producer_shared_uses: usize,
    pub keep_reason: Option<KeepReason>,
    pub failure_predicates: FailurePredicates,
    pub materialization_leaves: Vec<MaterializationLeaf>,
    pub direct_forward: Option<DirectForwardFact>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BoundaryRegisterClass {
    Gpr32,
    Gpr64,
    Gpr64Tuple(usize),
}

impl BoundaryRegisterClass {
    pub fn for_width(width: usize) -> Self {
        match width {
            0..=32 => Self::Gpr32,
            33..=64 => Self::Gpr64,
            _ => Self::Gpr64Tuple(width.div_ceil(64)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DirectForwardFact {
    pub producer: ProgramPoint,
    pub producer_value: RegisterId,
    pub use_site: ProgramPoint,
    pub register_class: BoundaryRegisterClass,
    pub allowed_placement_blocks: Vec<BlockId>,
    pub traversed_cfg_edges: Vec<(BlockId, BlockId)>,
    pub mandatory_live_in_blocks: BTreeSet<BlockId>,
    pub mandatory_live_out_blocks: BTreeSet<BlockId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum DemandPlan {
    DirectForward,
    Rematerialize,
    KeepPackedReload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum KeepReason {
    UnsupportedMemoryPhi,
    UnsupportedRecipe,
    ProducerNotPure,
    NoLegalPlacement,
    UnstableMemoryVersion,
    RematerializationMoreExpensive,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct FailurePredicates(u16);

impl FailurePredicates {
    pub const UNSUPPORTED_MEMORY_PHI: usize = 0;
    pub const UNSUPPORTED_RECIPE: usize = 1;
    pub const PRODUCER_NOT_PURE: usize = 2;
    pub const NO_LEGAL_PLACEMENT: usize = 3;
    pub const CONE_TOO_LARGE: usize = 4;
    pub const SHARED_PRODUCER: usize = 5;
    pub const DIRECT_LIVE_RANGE_LONG: usize = 6;
    pub const REMATERIALIZATION_MORE_EXPENSIVE: usize = 7;
    pub const UNSTABLE_MEMORY_VERSION: usize = 8;
    pub const COUNT: usize = 9;

    pub fn insert(&mut self, predicate: usize) {
        self.0 |= 1 << predicate;
    }

    fn contains(self, predicate: usize) -> bool {
        self.0 & (1 << predicate) != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PublicationFact {
    pub point: ProgramPoint,
    pub range: StateRange,
    pub staged_source: Option<RegisterId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct StateVersionId(usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct MaterializationSiteId(usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct UseClusterId(usize);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MaterializationLeaf {
    #[expect(dead_code, reason = "constants remain explicit in the initial cone")]
    Constant { register: RegisterId },
    DominatingSsa {
        register: RegisterId,
        definition_block: BlockId,
        insertion_point: ProgramPoint,
    },
    #[expect(
        dead_code,
        reason = "preserved-home frontier is added after initial analysis"
    )]
    ReloadPreservedHome {
        site: MaterializationSiteId,
        version: StateVersionId,
    },
    ReadPersistentState {
        register: RegisterId,
        original_load: ProgramPoint,
        insertion_point: ProgramPoint,
        range: StateRange,
        phase_version: usize,
    },
    #[expect(
        dead_code,
        reason = "control-pure regions are a later coverage extension"
    )]
    ControlMerge { inputs: Vec<StateVersionId> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DefiningRecipe {
    StoredValue {
        definition: ProgramPoint,
        register: RegisterId,
        stored_range: StateRange,
    },
    ControlMerge {
        inputs: Vec<StateVersionId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StateVersion {
    id: StateVersionId,
    range: StateRange,
    recipe: DefiningRecipe,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MaterializationSite {
    PublicBacking {
        id: MaterializationSiteId,
        range: StateRange,
        required_definitions: BTreeSet<ProgramPoint>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SourceAction {
    DirectForward,
    Rematerialize,
    KeepPackedReload {
        site: MaterializationSiteId,
        original_load: ProgramPoint,
        extract_range: StateRange,
    },
}

impl SourceAction {
    fn repair_rank(&self) -> u8 {
        match self {
            Self::DirectForward => 0,
            Self::Rematerialize => 1,
            Self::KeepPackedReload { .. } => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitAction {
    Dead,
    #[expect(
        dead_code,
        reason = "Milestone 1 defines the complete cluster contract"
    )]
    CarryToCluster(UseClusterId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UseClusterPlan {
    id: UseClusterId,
    load: ProgramPoint,
    versions: Vec<StateVersionId>,
    source: SourceAction,
    materialization_leaves: Vec<MaterializationLeaf>,
    exit: ExitAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectForwardContract {
    cluster: UseClusterId,
    producer: ProgramPoint,
    producer_value: RegisterId,
    use_site: ProgramPoint,
    register_class: BoundaryRegisterClass,
    allowed_placement_blocks: Vec<BlockId>,
    traversed_cfg_edges: Vec<(BlockId, BlockId)>,
    mandatory_live_in_blocks: BTreeSet<BlockId>,
    mandatory_live_out_blocks: BTreeSet<BlockId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FfPhaseEffect {
    StageNextFf {
        point: ProgramPoint,
        range: StateRange,
        source: RegisterId,
        earliest: ProgramPoint,
        latest_before: ProgramPoint,
    },
    CommitFfState {
        publications: Vec<(ProgramPoint, StateRange)>,
    },
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct BlockBoundaryContract {
    mandatory_live_ins: BTreeSet<StateVersionId>,
    mandatory_live_outs: BTreeSet<StateVersionId>,
    rematerializable_live_ins: BTreeSet<StateVersionId>,
    reload_at_entry: BTreeSet<MaterializationSiteId>,
    store_before_exit: BTreeSet<ProgramPoint>,
}

#[derive(Debug, Default)]
struct MaterializationModel {
    versions: Vec<StateVersion>,
    sites: Vec<MaterializationSite>,
    clusters: Vec<UseClusterPlan>,
    effects: Vec<FfPhaseEffect>,
    contracts: BTreeMap<BlockId, BlockBoundaryContract>,
    direct_contracts: Vec<DirectForwardContract>,
    store_dependents: BTreeMap<ProgramPoint, BTreeSet<StoreDependent>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum StoreDependent {
    Version(StateVersionId),
    Site(MaterializationSiteId),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct PlanSummary {
    pub versions: usize,
    pub sites: usize,
    pub clusters: usize,
    pub stage_next_ff_sites: usize,
    pub commit_ff_state_dependencies: usize,
    pub commit_ff_state_barriers: usize,
    pub block_contracts: usize,
    pub direct_boundary_contracts: usize,
    pub verified_uses: usize,
    pub invalidated_store_deletions: usize,
    pub direct_forward: usize,
    pub rematerialize: usize,
    pub keep_packed_reload: usize,
    pub same_block: usize,
    pub crosses_loop_depth: usize,
    pub maximum_block_distance: usize,
    pub maximum_rematerialization_cone: usize,
    pub maximum_producer_shared_uses: usize,
    pub block_distance_histogram: [usize; 7],
    pub cone_size_histogram: [usize; 7],
    pub version_demand_histogram: [usize; 6],
    pub keep_reason_histogram: [usize; 6],
    pub failure_predicate_histogram: [usize; FailurePredicates::COUNT],
    pub predicted_removed_loads: usize,
    pub predicted_removed_shifts: usize,
    pub predicted_removed_masks: usize,
    pub predicted_removed_merges: usize,
    pub predicted_added_rematerialization_instructions: usize,
    pub predicted_extended_cross_block_live_ranges: usize,
    pub predicted_relocated_closed_cones: usize,
    pub materialization_leaves: usize,
    pub maximum_materialization_leaves_per_cluster: usize,
    pub rss_after_model_build_kib: usize,
    pub rss_after_base_verification_kib: usize,
    pub rss_after_deletion_audit_kib: usize,
    pub rss_after_summary_kib: usize,
    pub verifier_passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PlanError {
    MissingDefinition(ProgramPoint),
    EmptyDemand(ProgramPoint),
    EmptyMerge(StateVersionId),
    ForwardVersionReference(StateVersionId),
    MissingMaterialization(MaterializationSiteId),
    RangeMismatch(ProgramPoint),
    PreservedStoreDeleted(ProgramPoint),
    ImplicitHome(UseClusterId),
    InvalidPhaseEffect(ProgramPoint),
    RepairCycle,
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for PlanError {}

pub(super) fn build_and_verify(
    cfg: &SirCfg,
    definitions: &BTreeMap<ProgramPoint, DefinitionFact>,
    demands: &[DemandFact],
    publications: &[PublicationFact],
) -> Result<PlanSummary, PlanError> {
    verify_repair_relation()?;
    let model = build_model(definitions, demands, publications)?;
    let rss_after_model_build_kib =
        super::fused_state_feasibility::resident_memory_kib().map_or(0, |value| value.0);
    let publication_set = collect_publications(&model);
    verify_model(cfg, &model, &publication_set)?;
    let rss_after_base_verification_kib =
        super::fused_state_feasibility::resident_memory_kib().map_or(0, |value| value.0);

    let named_stores = model
        .store_dependents
        .keys()
        .copied()
        .collect::<BTreeSet<_>>();
    for store in &named_stores {
        if !matches!(verify_store_deletion(&model, *store),
            Err(PlanError::PreservedStoreDeleted(point)) if point == *store)
        {
            return Err(PlanError::PreservedStoreDeleted(*store));
        }
    }
    let rss_after_deletion_audit_kib =
        super::fused_state_feasibility::resident_memory_kib().map_or(0, |value| value.0);

    let mut block_distance_histogram = [0; 7];
    let mut cone_size_histogram = [0; 7];
    let mut demands_per_version = BTreeMap::<Vec<(StateRange, Vec<ProgramPoint>)>, usize>::new();
    for demand in demands {
        block_distance_histogram[distance_bucket(demand.producer_block_distance)] += 1;
        cone_size_histogram[size_bucket(demand.rematerialization_cone_instructions)] += 1;
        let key = demand
            .fragments
            .iter()
            .map(|fragment| {
                (
                    fragment.range,
                    fragment.reaching_definitions.iter().copied().collect(),
                )
            })
            .collect();
        *demands_per_version.entry(key).or_default() += 1;
    }
    let mut version_demand_histogram = [0; 6];
    for uses in demands_per_version.into_values() {
        version_demand_histogram[fanout_bucket(uses)] += 1;
    }
    let mut keep_reason_histogram = [0; 6];
    for demand in demands
        .iter()
        .filter(|demand| demand.plan == DemandPlan::KeepPackedReload)
    {
        let reason = demand
            .keep_reason
            .ok_or(PlanError::ImplicitHome(UseClusterId(usize::MAX)))?;
        keep_reason_histogram[reason as usize] += 1;
    }
    let mut failure_predicate_histogram = [0; FailurePredicates::COUNT];
    for demand in demands {
        for (predicate, count) in failure_predicate_histogram.iter_mut().enumerate() {
            *count += usize::from(demand.failure_predicates.contains(predicate));
        }
    }

    let rss_after_summary_kib =
        super::fused_state_feasibility::resident_memory_kib().map_or(0, |value| value.0);
    Ok(PlanSummary {
        versions: model.versions.len(),
        sites: model.sites.len(),
        clusters: model.clusters.len(),
        stage_next_ff_sites: model
            .effects
            .iter()
            .filter(|effect| matches!(effect, FfPhaseEffect::StageNextFf { .. }))
            .count(),
        commit_ff_state_dependencies: model
            .effects
            .iter()
            .map(|effect| match effect {
                FfPhaseEffect::CommitFfState { publications } => publications.len(),
                FfPhaseEffect::StageNextFf { .. } => 0,
            })
            .sum(),
        commit_ff_state_barriers: model
            .effects
            .iter()
            .filter(|effect| matches!(effect, FfPhaseEffect::CommitFfState { .. }))
            .count(),
        block_contracts: model.contracts.len(),
        direct_boundary_contracts: model.direct_contracts.len(),
        verified_uses: model.clusters.len(),
        invalidated_store_deletions: named_stores.len(),
        direct_forward: demands
            .iter()
            .filter(|demand| demand.plan == DemandPlan::DirectForward)
            .count(),
        rematerialize: demands
            .iter()
            .filter(|demand| demand.plan == DemandPlan::Rematerialize)
            .count(),
        keep_packed_reload: demands
            .iter()
            .filter(|demand| demand.plan == DemandPlan::KeepPackedReload)
            .count(),
        same_block: demands
            .iter()
            .filter(|demand| demand.producer_block_distance == 0)
            .count(),
        crosses_loop_depth: demands
            .iter()
            .filter(|demand| demand.loop_depth_delta != 0)
            .count(),
        maximum_block_distance: demands
            .iter()
            .map(|demand| demand.producer_block_distance)
            .filter(|distance| *distance != usize::MAX)
            .max()
            .unwrap_or(0),
        maximum_rematerialization_cone: demands
            .iter()
            .map(|demand| demand.rematerialization_cone_instructions)
            .max()
            .unwrap_or(0),
        maximum_producer_shared_uses: demands
            .iter()
            .map(|demand| demand.producer_shared_uses)
            .max()
            .unwrap_or(0),
        block_distance_histogram,
        cone_size_histogram,
        version_demand_histogram,
        keep_reason_histogram,
        failure_predicate_histogram,
        predicted_removed_loads: demands
            .iter()
            .filter(|demand| demand.plan != DemandPlan::KeepPackedReload)
            .count(),
        // Exact post-ISel provenance is required before claiming these.
        predicted_removed_shifts: 0,
        predicted_removed_masks: 0,
        predicted_removed_merges: 0,
        predicted_added_rematerialization_instructions: demands
            .iter()
            .filter(|demand| demand.plan == DemandPlan::Rematerialize)
            .map(|demand| {
                demand.rematerialization_cone_instructions
                    + demand
                        .materialization_leaves
                        .iter()
                        .filter(|leaf| {
                            matches!(leaf, MaterializationLeaf::ReadPersistentState { .. })
                        })
                        .count()
            })
            .sum(),
        predicted_extended_cross_block_live_ranges: 0,
        predicted_relocated_closed_cones: demands
            .iter()
            .filter(|demand| demand.plan == DemandPlan::DirectForward)
            .count(),
        materialization_leaves: demands
            .iter()
            .map(|demand| demand.materialization_leaves.len())
            .sum(),
        maximum_materialization_leaves_per_cluster: demands
            .iter()
            .map(|demand| demand.materialization_leaves.len())
            .max()
            .unwrap_or(0),
        rss_after_model_build_kib,
        rss_after_base_verification_kib,
        rss_after_deletion_audit_kib,
        rss_after_summary_kib,
        verifier_passed: true,
    })
}

fn distance_bucket(distance: usize) -> usize {
    match distance {
        0 => 0,
        1 => 1,
        2..=3 => 2,
        4..=7 => 3,
        8..=15 => 4,
        16.. => {
            if distance == usize::MAX {
                6
            } else {
                5
            }
        }
    }
}

fn size_bucket(size: usize) -> usize {
    match size {
        0 => 0,
        1..=2 => 1,
        3..=4 => 2,
        5..=8 => 3,
        9..=16 => 4,
        17..=32 => 5,
        33.. => 6,
    }
}

fn fanout_bucket(uses: usize) -> usize {
    match uses {
        0 | 1 => 0,
        2 => 1,
        3..=4 => 2,
        5..=8 => 3,
        9..=16 => 4,
        17.. => 5,
    }
}

fn build_model(
    definitions: &BTreeMap<ProgramPoint, DefinitionFact>,
    demands: &[DemandFact],
    publications: &[PublicationFact],
) -> Result<MaterializationModel, PlanError> {
    let mut model = MaterializationModel::default();
    for definition in definitions.values() {
        let id = MaterializationSiteId(model.sites.len());
        model.sites.push(MaterializationSite::PublicBacking {
            id,
            range: definition.stored_range,
            required_definitions: BTreeSet::from([definition.point]),
        });
        model
            .contracts
            .entry(definition.point.block)
            .or_default()
            .store_before_exit
            .insert(definition.point);
    }
    for demand in demands {
        if demand.fragments.is_empty() {
            return Err(PlanError::EmptyDemand(demand.load));
        }
        let mut cluster_versions = Vec::with_capacity(demand.fragments.len());
        let mut required_definitions = BTreeSet::new();
        for fragment in &demand.fragments {
            let mut inputs = Vec::with_capacity(fragment.reaching_definitions.len());
            for point in &fragment.reaching_definitions {
                let definition = definitions
                    .get(point)
                    .ok_or(PlanError::MissingDefinition(*point))?;
                if definition.stored_range.object != fragment.range.object
                    || definition.stored_range.start > fragment.range.start
                    || definition.stored_range.end < fragment.range.end
                {
                    return Err(PlanError::RangeMismatch(*point));
                }
                let id = StateVersionId(model.versions.len());
                model.versions.push(StateVersion {
                    id,
                    range: fragment.range,
                    recipe: DefiningRecipe::StoredValue {
                        definition: *point,
                        register: definition.source,
                        stored_range: definition.stored_range,
                    },
                });
                inputs.push(id);
                required_definitions.insert(*point);
            }
            if inputs.is_empty() {
                return Err(PlanError::EmptyDemand(demand.load));
            }
            let version = if inputs.len() == 1 {
                inputs[0]
            } else {
                let id = StateVersionId(model.versions.len());
                model.versions.push(StateVersion {
                    id,
                    range: fragment.range,
                    recipe: DefiningRecipe::ControlMerge { inputs },
                });
                id
            };
            cluster_versions.push(version);
        }

        let source = match demand.plan {
            DemandPlan::DirectForward => SourceAction::DirectForward,
            DemandPlan::Rematerialize => SourceAction::Rematerialize,
            DemandPlan::KeepPackedReload => {
                let site_id = MaterializationSiteId(model.sites.len());
                model.sites.push(MaterializationSite::PublicBacking {
                    id: site_id,
                    range: demand.range,
                    required_definitions,
                });
                model
                    .contracts
                    .entry(demand.load.block)
                    .or_default()
                    .reload_at_entry
                    .insert(site_id);
                SourceAction::KeepPackedReload {
                    site: site_id,
                    original_load: demand.load,
                    extract_range: demand.range,
                }
            }
        };
        let cluster_id = UseClusterId(model.clusters.len());
        if demand.plan == DemandPlan::DirectForward {
            let fact = demand
                .direct_forward
                .clone()
                .ok_or(PlanError::ImplicitHome(cluster_id))?;
            model.direct_contracts.push(DirectForwardContract {
                cluster: cluster_id,
                producer: fact.producer,
                producer_value: fact.producer_value,
                use_site: fact.use_site,
                register_class: fact.register_class,
                allowed_placement_blocks: fact.allowed_placement_blocks,
                traversed_cfg_edges: fact.traversed_cfg_edges,
                mandatory_live_in_blocks: fact.mandatory_live_in_blocks,
                mandatory_live_out_blocks: fact.mandatory_live_out_blocks,
            });
        } else if demand.direct_forward.is_some() {
            return Err(PlanError::ImplicitHome(cluster_id));
        }
        model.clusters.push(UseClusterPlan {
            id: cluster_id,
            load: demand.load,
            versions: cluster_versions,
            source,
            materialization_leaves: demand.materialization_leaves.clone(),
            exit: ExitAction::Dead,
        });
    }

    for publication in publications {
        if let Some(source) = publication.staged_source {
            model.effects.push(FfPhaseEffect::StageNextFf {
                point: publication.point,
                range: publication.range,
                source,
                earliest: publication.point,
                latest_before: publication.point,
            });
        }
        model
            .contracts
            .entry(publication.point.block)
            .or_default()
            .store_before_exit
            .insert(publication.point);
    }
    if !publications.is_empty() {
        model.effects.push(FfPhaseEffect::CommitFfState {
            publications: publications
                .iter()
                .map(|publication| (publication.point, publication.range))
                .collect(),
        });
    }
    model.store_dependents = collect_store_dependents(&model);
    Ok(model)
}

fn verify_model(
    cfg: &SirCfg,
    model: &MaterializationModel,
    publications: &BTreeSet<(ProgramPoint, StateRange)>,
) -> Result<(), PlanError> {
    let commit_barriers = model
        .effects
        .iter()
        .filter(|effect| matches!(effect, FfPhaseEffect::CommitFfState { .. }))
        .count();
    if (!publications.is_empty() && commit_barriers != 1)
        || (publications.is_empty() && commit_barriers != 0)
    {
        return Err(PlanError::InvalidPhaseEffect(ProgramPoint {
            block: BlockId(0),
            instruction: 0,
        }));
    }
    for effect in &model.effects {
        if let FfPhaseEffect::StageNextFf {
            point,
            range,
            earliest,
            latest_before,
            ..
        } = effect
            && (earliest.block != point.block
                || latest_before.block != point.block
                || earliest.instruction > point.instruction
                || latest_before.instruction < point.instruction
                || !publications.contains(&(*point, *range)))
        {
            return Err(PlanError::InvalidPhaseEffect(*point));
        }
    }
    for (index, version) in model.versions.iter().enumerate() {
        if version.id != StateVersionId(index) {
            return Err(PlanError::ForwardVersionReference(version.id));
        }
        match &version.recipe {
            DefiningRecipe::StoredValue { .. } => {}
            DefiningRecipe::ControlMerge { inputs } => {
                if inputs.is_empty() {
                    return Err(PlanError::EmptyMerge(version.id));
                }
                if inputs.iter().any(|input| input.0 >= index) {
                    return Err(PlanError::ForwardVersionReference(version.id));
                }
            }
        }
    }
    if model.store_dependents != collect_store_dependents(model) {
        return Err(PlanError::PreservedStoreDeleted(ProgramPoint {
            block: BlockId(0),
            instruction: 0,
        }));
    }
    for cluster in &model.clusters {
        if cluster.versions.is_empty() {
            return Err(PlanError::ImplicitHome(cluster.id));
        }
        match &cluster.source {
            SourceAction::KeepPackedReload { site, .. } => {
                if site.0 >= model.sites.len() {
                    return Err(PlanError::MissingMaterialization(*site));
                }
            }
            SourceAction::DirectForward | SourceAction::Rematerialize => {}
        }
        for leaf in &cluster.materialization_leaves {
            match leaf {
                MaterializationLeaf::DominatingSsa {
                    definition_block,
                    insertion_point,
                    ..
                } if !cfg.dominates(*definition_block, insertion_point.block) => {
                    return Err(PlanError::ImplicitHome(cluster.id));
                }
                MaterializationLeaf::DominatingSsa {
                    insertion_point, ..
                }
                | MaterializationLeaf::ReadPersistentState {
                    insertion_point, ..
                } if insertion_point.block != cluster.load.block => {
                    return Err(PlanError::ImplicitHome(cluster.id));
                }
                MaterializationLeaf::ReloadPreservedHome { site, version } => {
                    if site.0 >= model.sites.len() || version.0 >= model.versions.len() {
                        return Err(PlanError::ImplicitHome(cluster.id));
                    }
                }
                MaterializationLeaf::ControlMerge { inputs } if inputs.is_empty() => {
                    return Err(PlanError::ImplicitHome(cluster.id));
                }
                MaterializationLeaf::Constant { .. }
                | MaterializationLeaf::DominatingSsa { .. }
                | MaterializationLeaf::ReadPersistentState { .. }
                | MaterializationLeaf::ControlMerge { .. } => {}
            }
        }
    }
    for contract in &model.direct_contracts {
        let cluster = model
            .clusters
            .get(contract.cluster.0)
            .ok_or(PlanError::ImplicitHome(contract.cluster))?;
        if !matches!(cluster.source, SourceAction::DirectForward)
            || cluster.load != contract.use_site
            || cluster.versions.iter().any(|version| {
                !matches!(
                    model.versions.get(version.0).map(|version| &version.recipe),
                    Some(DefiningRecipe::StoredValue { register, .. })
                        if *register == contract.producer_value
                )
            })
            || contract.allowed_placement_blocks.first().copied() != Some(contract.use_site.block)
            || contract.allowed_placement_blocks.last().copied() != Some(contract.use_site.block)
            || contract.allowed_placement_blocks.iter().any(|block| {
                !cfg.dominates(contract.producer.block, *block)
                    || !cfg.dominates(*block, contract.use_site.block)
            })
        {
            return Err(PlanError::ImplicitHome(contract.cluster));
        }
        match contract.register_class {
            BoundaryRegisterClass::Gpr32 | BoundaryRegisterClass::Gpr64 => {}
            BoundaryRegisterClass::Gpr64Tuple(chunks) if chunks >= 2 => {}
            BoundaryRegisterClass::Gpr64Tuple(_) => {
                return Err(PlanError::ImplicitHome(contract.cluster));
            }
        }
        if contract.traversed_cfg_edges.is_empty() {
            if !contract.mandatory_live_in_blocks.is_empty()
                || !contract.mandatory_live_out_blocks.is_empty()
            {
                return Err(PlanError::ImplicitHome(contract.cluster));
            }
        } else if contract.traversed_cfg_edges.iter().any(|(source, target)| {
            let Some(source) = cfg.block_index(*source) else {
                return true;
            };
            let Some(target) = cfg.block_index(*target) else {
                return true;
            };
            !cfg.successors[source].contains(&target)
        }) {
            return Err(PlanError::ImplicitHome(contract.cluster));
        }
    }
    Ok(())
}

fn collect_store_dependents(
    model: &MaterializationModel,
) -> BTreeMap<ProgramPoint, BTreeSet<StoreDependent>> {
    let mut dependents = BTreeMap::<ProgramPoint, BTreeSet<StoreDependent>>::new();
    for version in &model.versions {
        if let DefiningRecipe::StoredValue { definition, .. } = version.recipe {
            dependents
                .entry(definition)
                .or_default()
                .insert(StoreDependent::Version(version.id));
        }
    }
    for site in &model.sites {
        match site {
            MaterializationSite::PublicBacking {
                id,
                required_definitions,
                ..
            } => {
                for definition in required_definitions {
                    dependents
                        .entry(*definition)
                        .or_default()
                        .insert(StoreDependent::Site(*id));
                }
            }
        }
    }
    dependents
}

fn verify_store_deletion(
    model: &MaterializationModel,
    deleted_store: ProgramPoint,
) -> Result<(), PlanError> {
    if model.store_dependents.contains_key(&deleted_store) {
        Err(PlanError::PreservedStoreDeleted(deleted_store))
    } else {
        Ok(())
    }
}

fn collect_publications(model: &MaterializationModel) -> BTreeSet<(ProgramPoint, StateRange)> {
    model
        .effects
        .iter()
        .filter_map(|effect| match effect {
            FfPhaseEffect::CommitFfState { publications } => Some(publications),
            FfPhaseEffect::StageNextFf { .. } => None,
        })
        .flatten()
        .copied()
        .collect()
}

fn verify_repair(old: SourceAction, new: SourceAction) -> Result<(), PlanError> {
    if new.repair_rank() <= old.repair_rank() {
        return Err(PlanError::RepairCycle);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PlanRepairRank {
    unsplit_use_pairs: usize,
    direct_plans: usize,
    rematerialize_plans: usize,
}

fn plan_repair_rank(
    partition: &[BTreeSet<ProgramPoint>],
    actions: &[SourceAction],
) -> Option<PlanRepairRank> {
    if partition.len() != actions.len() || !is_partition(partition) {
        return None;
    }
    Some(PlanRepairRank {
        unsplit_use_pairs: partition
            .iter()
            .map(|cluster| {
                cluster
                    .len()
                    .saturating_mul(cluster.len().saturating_sub(1))
                    / 2
            })
            .sum(),
        direct_plans: actions
            .iter()
            .filter(|action| matches!(action, SourceAction::DirectForward))
            .count(),
        rematerialize_plans: actions
            .iter()
            .filter(|action| matches!(action, SourceAction::Rematerialize))
            .count(),
    })
}

fn verify_plan_repair(
    old_partition: &[BTreeSet<ProgramPoint>],
    old_actions: &[SourceAction],
    new_partition: &[BTreeSet<ProgramPoint>],
    new_actions: &[SourceAction],
) -> Result<(), PlanError> {
    let old_rank = plan_repair_rank(old_partition, old_actions).ok_or(PlanError::RepairCycle)?;
    let new_rank = plan_repair_rank(new_partition, new_actions).ok_or(PlanError::RepairCycle)?;
    let old_uses = old_partition
        .iter()
        .flatten()
        .copied()
        .collect::<BTreeSet<_>>();
    let new_uses = new_partition
        .iter()
        .flatten()
        .copied()
        .collect::<BTreeSet<_>>();
    let refines = new_partition.iter().all(|new_cluster| {
        old_partition
            .iter()
            .filter(|old_cluster| new_cluster.is_subset(old_cluster))
            .count()
            == 1
    });
    if old_uses != new_uses || !refines || new_rank >= old_rank {
        return Err(PlanError::RepairCycle);
    }
    Ok(())
}

fn is_partition(partition: &[BTreeSet<ProgramPoint>]) -> bool {
    let mut uses = BTreeSet::new();
    partition
        .iter()
        .all(|cluster| !cluster.is_empty() && cluster.iter().all(|use_site| uses.insert(*use_site)))
}

fn verify_repair_relation() -> Result<(), PlanError> {
    let placeholder = MaterializationSiteId(usize::MAX);
    let point = ProgramPoint {
        block: BlockId(0),
        instruction: 0,
    };
    let range = StateRange {
        object: RegionedAbsoluteAddr {
            region: 0,
            instance_id: crate::ir::InstanceId(0),
            var_id: veryl_analyzer::ir::VarId::default(),
        },
        start: 0,
        end: 1,
    };
    verify_repair(SourceAction::DirectForward, SourceAction::Rematerialize)?;
    verify_repair(
        SourceAction::DirectForward,
        SourceAction::KeepPackedReload {
            site: placeholder,
            original_load: point,
            extract_range: range,
        },
    )?;
    verify_repair(
        SourceAction::Rematerialize,
        SourceAction::KeepPackedReload {
            site: placeholder,
            original_load: point,
            extract_range: range,
        },
    )?;
    verify_plan_repair(
        &[BTreeSet::from([
            point,
            ProgramPoint {
                block: BlockId(0),
                instruction: 1,
            },
        ])],
        &[SourceAction::DirectForward],
        &[
            BTreeSet::from([point]),
            BTreeSet::from([ProgramPoint {
                block: BlockId(0),
                instruction: 1,
            }]),
        ],
        &[SourceAction::DirectForward, SourceAction::DirectForward],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{BasicBlock, ExecutionUnit, InstanceId, RegionedAbsoluteAddr, SIRTerminator};
    use veryl_analyzer::ir::VarId;

    fn point(instruction: usize) -> ProgramPoint {
        ProgramPoint {
            block: BlockId(0),
            instruction,
        }
    }

    fn range() -> StateRange {
        StateRange {
            object: RegionedAbsoluteAddr {
                region: 0,
                instance_id: InstanceId(0),
                var_id: VarId::default(),
            },
            start: 0,
            end: 8,
        }
    }

    fn one_block_cfg() -> SirCfg {
        let block: BasicBlock<RegionedAbsoluteAddr> = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Return,
        };
        let eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(BlockId(0), block)].into_iter().collect(),
            register_map: crate::HashMap::default(),
        };
        SirCfg::analyze(&eu).unwrap()
    }

    #[test]
    fn preserved_home_is_concrete_and_invalidated_by_store_deletion() {
        let definition = DefinitionFact {
            point: point(0),
            source: RegisterId(0),
            stored_range: range(),
        };
        let definitions = BTreeMap::from([(definition.point, definition)]);
        let demands = [DemandFact {
            load: point(1),
            range: range(),
            fragments: vec![FragmentFact {
                range: range(),
                reaching_definitions: BTreeSet::from([point(0)]),
            }],
            plan: DemandPlan::KeepPackedReload,
            producer_block_distance: 0,
            loop_depth_delta: 0,
            rematerialization_cone_instructions: 1,
            producer_shared_uses: 1,
            keep_reason: Some(KeepReason::RematerializationMoreExpensive),
            failure_predicates: FailurePredicates::default(),
            materialization_leaves: Vec::new(),
            direct_forward: None,
        }];
        let summary = build_and_verify(&one_block_cfg(), &definitions, &demands, &[]).unwrap();
        assert_eq!(summary.versions, 1);
        assert_eq!(summary.sites, 2);
        assert_eq!(summary.verified_uses, 1);
        assert_eq!(summary.invalidated_store_deletions, 1);
        assert!(summary.verifier_passed);
    }

    #[test]
    fn repair_relation_is_strictly_monotonic() {
        assert!(verify_repair(SourceAction::DirectForward, SourceAction::Rematerialize).is_ok());
        assert!(
            verify_repair(
                SourceAction::Rematerialize,
                SourceAction::KeepPackedReload {
                    site: MaterializationSiteId(0),
                    original_load: point(1),
                    extract_range: range(),
                }
            )
            .is_ok()
        );
        assert_eq!(
            verify_repair(
                SourceAction::KeepPackedReload {
                    site: MaterializationSiteId(0),
                    original_load: point(1),
                    extract_range: range(),
                },
                SourceAction::DirectForward
            ),
            Err(PlanError::RepairCycle)
        );

        let first = point(1);
        let second = point(2);
        let joined = [BTreeSet::from([first, second])];
        let split = [BTreeSet::from([first]), BTreeSet::from([second])];
        assert!(
            verify_plan_repair(
                &joined,
                &[SourceAction::DirectForward],
                &split,
                &[SourceAction::DirectForward, SourceAction::DirectForward],
            )
            .is_ok()
        );
        assert_eq!(
            verify_plan_repair(
                &split,
                &[SourceAction::DirectForward, SourceAction::DirectForward],
                &joined,
                &[SourceAction::DirectForward],
            ),
            Err(PlanError::RepairCycle)
        );
    }

    #[test]
    fn stage_and_commit_are_distinct_effects() {
        let publication = PublicationFact {
            point: point(2),
            range: range(),
            staged_source: Some(RegisterId(0)),
        };
        let summary =
            build_and_verify(&one_block_cfg(), &BTreeMap::new(), &[], &[publication]).unwrap();
        assert_eq!(summary.stage_next_ff_sites, 1);
        assert_eq!(summary.commit_ff_state_dependencies, 1);
        assert_eq!(summary.commit_ff_state_barriers, 1);
        assert_eq!(summary.block_contracts, 1);
    }
}
