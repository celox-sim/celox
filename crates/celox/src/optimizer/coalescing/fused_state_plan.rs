//! Analysis-only materialization model for fused StateSSA.
//!
//! Public backing stores always remain explicit.  Each FF range demand then
//! independently selects direct forwarding, pure rematerialization, or the
//! existing packed reload fallback.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum DemandPlan {
    DirectForward,
    Rematerialize,
    KeepPackedReload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum KeepReason {
    MultipleReachingDefinitions,
    UnsupportedRecipe,
    ProducerNotPure,
    NoLegalPlacement,
    RematerializationMoreExpensive,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct FailurePredicates(u16);

impl FailurePredicates {
    pub const MULTIPLE_REACHING_DEFINITIONS: usize = 0;
    pub const UNSUPPORTED_RECIPE: usize = 1;
    pub const PRODUCER_NOT_PURE: usize = 2;
    pub const NO_LEGAL_PLACEMENT: usize = 3;
    pub const CONE_TOO_LARGE: usize = 4;
    pub const SHARED_PRODUCER: usize = 5;
    pub const DIRECT_LIVE_RANGE_LONG: usize = 6;
    pub const REMATERIALIZATION_MORE_EXPENSIVE: usize = 7;
    pub const COUNT: usize = 8;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SourceAction {
    DirectForward,
    Rematerialize,
    KeepPackedReload(MaterializationSiteId),
}

impl SourceAction {
    fn repair_rank(self) -> u8 {
        match self {
            Self::DirectForward => 0,
            Self::Rematerialize => 1,
            Self::KeepPackedReload(_) => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitAction {
    Dead,
    #[expect(dead_code, reason = "Milestone 1 defines the complete cluster contract")]
    CarryToCluster(UseClusterId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UseClusterPlan {
    id: UseClusterId,
    load: ProgramPoint,
    versions: Vec<StateVersionId>,
    source: SourceAction,
    exit: ExitAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FfPhaseEffect {
    StageNextFf {
        point: ProgramPoint,
        range: StateRange,
        source: RegisterId,
    },
    CommitFfState {
        point: ProgramPoint,
        range: StateRange,
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
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct PlanSummary {
    pub versions: usize,
    pub sites: usize,
    pub clusters: usize,
    pub stage_next_ff_sites: usize,
    pub commit_ff_state_dependencies: usize,
    pub block_contracts: usize,
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
    pub keep_reason_histogram: [usize; 5],
    pub failure_predicate_histogram: [usize; FailurePredicates::COUNT],
    pub predicted_removed_loads: usize,
    pub predicted_removed_shifts: usize,
    pub predicted_removed_masks: usize,
    pub predicted_removed_merges: usize,
    pub predicted_added_rematerialization_instructions: usize,
    pub predicted_extended_cross_block_live_ranges: usize,
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
    RepairCycle,
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for PlanError {}

pub(super) fn build_and_verify(
    definitions: &BTreeMap<ProgramPoint, DefinitionFact>,
    demands: &[DemandFact],
    publications: &[PublicationFact],
) -> Result<PlanSummary, PlanError> {
    verify_repair_relation()?;
    let model = build_model(definitions, demands, publications)?;
    verify_model(&model, &BTreeSet::new())?;

    let named_stores = model
        .sites
        .iter()
        .flat_map(|site| match site {
            MaterializationSite::PublicBacking {
                required_definitions,
                ..
            } => required_definitions.iter().copied().collect::<Vec<_>>(),
        })
        .collect::<BTreeSet<_>>();
    for store in &named_stores {
        let deleted = BTreeSet::from([*store]);
        if !matches!(
            verify_model(&model, &deleted),
            Err(PlanError::PreservedStoreDeleted(point)) if point == *store
        ) {
            return Err(PlanError::PreservedStoreDeleted(*store));
        }
    }

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
    let mut keep_reason_histogram = [0; 5];
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
            .filter(|effect| matches!(effect, FfPhaseEffect::CommitFfState { .. }))
            .count(),
        block_contracts: model.contracts.len(),
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
            .map(|demand| demand.rematerialization_cone_instructions)
            .sum(),
        predicted_extended_cross_block_live_ranges: demands
            .iter()
            .filter(|demand| {
                demand.plan == DemandPlan::DirectForward
                    && demand.producer_block_distance != 0
                    && demand.producer_block_distance != usize::MAX
            })
            .count(),
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
                SourceAction::KeepPackedReload(site_id)
            }
        };
        let cluster_id = UseClusterId(model.clusters.len());
        model.clusters.push(UseClusterPlan {
            id: cluster_id,
            load: demand.load,
            versions: cluster_versions,
            source,
            exit: ExitAction::Dead,
        });
    }

    for publication in publications {
        if let Some(source) = publication.staged_source {
            model.effects.push(FfPhaseEffect::StageNextFf {
                point: publication.point,
                range: publication.range,
                source,
            });
        }
        model.effects.push(FfPhaseEffect::CommitFfState {
            point: publication.point,
            range: publication.range,
        });
        model
            .contracts
            .entry(publication.point.block)
            .or_default()
            .store_before_exit
            .insert(publication.point);
    }
    Ok(model)
}

fn verify_model(
    model: &MaterializationModel,
    deleted_stores: &BTreeSet<ProgramPoint>,
) -> Result<(), PlanError> {
    for (index, version) in model.versions.iter().enumerate() {
        if version.id != StateVersionId(index) {
            return Err(PlanError::ForwardVersionReference(version.id));
        }
        match &version.recipe {
            DefiningRecipe::StoredValue { definition, .. } => {
                if deleted_stores.contains(definition) {
                    return Err(PlanError::PreservedStoreDeleted(*definition));
                }
            }
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
    for site in &model.sites {
        match site {
            MaterializationSite::PublicBacking {
                required_definitions,
                ..
            } => {
                if let Some(point) = required_definitions
                    .iter()
                    .find(|point| deleted_stores.contains(point))
                {
                    return Err(PlanError::PreservedStoreDeleted(*point));
                }
            }
        }
    }
    for cluster in &model.clusters {
        if cluster.versions.is_empty() {
            return Err(PlanError::ImplicitHome(cluster.id));
        }
        match cluster.source {
            SourceAction::KeepPackedReload(site) => {
                if site.0 >= model.sites.len() {
                    return Err(PlanError::MissingMaterialization(site));
                }
            }
            SourceAction::DirectForward | SourceAction::Rematerialize => {}
        }
    }
    Ok(())
}

fn verify_repair(old: SourceAction, new: SourceAction) -> Result<(), PlanError> {
    if new.repair_rank() <= old.repair_rank() {
        return Err(PlanError::RepairCycle);
    }
    Ok(())
}

fn verify_repair_relation() -> Result<(), PlanError> {
    let placeholder = MaterializationSiteId(usize::MAX);
    verify_repair(
        SourceAction::DirectForward,
        SourceAction::Rematerialize,
    )?;
    verify_repair(
        SourceAction::DirectForward,
        SourceAction::KeepPackedReload(placeholder),
    )?;
    verify_repair(
        SourceAction::Rematerialize,
        SourceAction::KeepPackedReload(placeholder),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{InstanceId, RegionedAbsoluteAddr};
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
        }];
        let summary = build_and_verify(&definitions, &demands, &[]).unwrap();
        assert_eq!(summary.versions, 1);
        assert_eq!(summary.sites, 2);
        assert_eq!(summary.verified_uses, 1);
        assert_eq!(summary.invalidated_store_deletions, 1);
        assert!(summary.verifier_passed);
    }

    #[test]
    fn repair_relation_is_strictly_monotonic() {
        assert!(verify_repair(
            SourceAction::DirectForward,
            SourceAction::Rematerialize
        )
        .is_ok());
        assert!(verify_repair(
            SourceAction::Rematerialize,
            SourceAction::KeepPackedReload(MaterializationSiteId(0))
        )
        .is_ok());
        assert_eq!(
            verify_repair(
                SourceAction::KeepPackedReload(MaterializationSiteId(0)),
                SourceAction::DirectForward
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
        let summary = build_and_verify(&BTreeMap::new(), &[], &[publication]).unwrap();
        assert_eq!(summary.stage_next_ff_sites, 1);
        assert_eq!(summary.commit_ff_state_dependencies, 1);
        assert_eq!(summary.block_contracts, 1);
    }
}
