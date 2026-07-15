//! Independent all-path proof for spill-home stores and reloads.
//!
//! This verifier consumes a [`SpillPlan`] without trusting its `S` states.  It
//! constructs sparse Boolean SSA for only the homes queried by a
//! non-rematerialized reload or `SpillPhi`. Store sites are `true`
//! definitions, function entry is `false`, and iterated-dominance-frontier
//! meets are AND nodes. This proves the same all-path property without a dense
//! block-by-home matrix. Stores and reloads at one point are parallel plan
//! operations; materialization emits all stores before any reload, and this
//! verifier uses the same ordering.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt;

use crate::backend::native::mir::{BlockId, MFunction, SpillKind, VReg};

use super::cfg::NormalizedCfg;
use super::spill_plan::{LogicalValue, PlannedOp, ProgramPoint, SpillHome, SpillPlan};

const RELOAD_RULE: &str = "HOME.RELOAD_ALL_PATH_STORE";
const SPILL_PHI_RULE: &str = "HOME.SPILL_PHI_ALL_PATH_STORE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HomeLocation {
    Point(ProgramPoint),
    Edge {
        predecessor: BlockId,
        successor: BlockId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HomeVerifyError {
    pub rule: &'static str,
    pub location: Option<HomeLocation>,
    pub value: Option<LogicalValue>,
    pub home: Option<SpillHome>,
    pub message: String,
}

impl fmt::Display for HomeVerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.rule)?;
        if let Some(location) = self.location {
            write!(f, " at {location:?}")?;
        }
        if let Some(value) = self.value {
            write!(f, ": logical={value:?}")?;
        }
        if let Some(home) = self.home {
            write!(f, " home={home:?}")?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for HomeVerifyError {}

/// Prove that every non-rematerialized reload has a same-home store on every
/// path reaching it.
///
/// `SpillPhi` is materialized as predecessor-edge stores rather than a store at
/// its nominal point.  Those implicit stores are modeled exactly as
/// reconstruction models them: a source already in `S_exit` reuses its home;
/// otherwise that incoming edge creates the home.
pub(super) fn verify(
    func: &MFunction,
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
) -> Result<(), HomeVerifyError> {
    verify_with_work(func, cfg, plan).map(|_| ())
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct HomeVerifyWork {
    homes: usize,
    dominator_blocks: usize,
    frontier_members_visited: usize,
    phi_nodes: usize,
    phi_inputs: usize,
    fact_updates: usize,
    query_checks: usize,
    edge_query_checks: usize,
}

fn verify_with_work(
    func: &MFunction,
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
) -> Result<HomeVerifyWork, HomeVerifyError> {
    if func.blocks.is_empty() {
        return Ok(HomeVerifyWork::default());
    }
    if cfg.predecessors.len() != func.blocks.len()
        || cfg.successors.len() != func.blocks.len()
        || plan.s_exit.len() != func.blocks.len()
    {
        return Err(structural_error(
            "HOME.MODEL_SHAPE",
            "CFG/SpillPlan block counts do not match the function",
        ));
    }

    let mut point_ops = vec![BTreeMap::<usize, Vec<PlannedOp>>::new(); func.blocks.len()];
    let mut universe = BTreeSet::<SpillHome>::new();
    let mut spilled_phis = Vec::<SpilledPhi>::new();
    let mut seen_spilled_phis = BTreeSet::<LogicalValue>::new();

    for &(point, operation) in &plan.point_ops {
        let Some(&block) = cfg.block_index.get(&point.block) else {
            return Err(operation_error(
                "HOME.POINT_BLOCK",
                Some(HomeLocation::Point(point)),
                operation,
                "point operation names a block outside the normalized CFG",
            ));
        };
        if point.instruction >= func.blocks[block].insts.len() {
            return Err(operation_error(
                "HOME.POINT_INSTRUCTION",
                Some(HomeLocation::Point(point)),
                operation,
                "point operation is not before an existing instruction",
            ));
        }
        verify_operation_home(func, plan, operation, HomeLocation::Point(point))?;
        universe.insert(operation_home(operation));
        if let PlannedOp::SpillPhi { value, home } = operation {
            if !seen_spilled_phis.insert(value) {
                return Err(operation_error(
                    "HOME.SPILL_PHI_UNIQUE",
                    Some(HomeLocation::Point(point)),
                    operation,
                    "logical value has more than one SpillPhi operation",
                ));
            }
            let Some(phi) = func.blocks[block]
                .phis
                .iter()
                .find(|phi| phi.dst.0 == value.0)
            else {
                return Err(operation_error(
                    "HOME.SPILL_PHI_SITE",
                    Some(HomeLocation::Point(point)),
                    operation,
                    "SpillPhi does not name a phi destination in its block",
                ));
            };
            if point.instruction != 0 {
                return Err(operation_error(
                    "HOME.SPILL_PHI_SITE",
                    Some(HomeLocation::Point(point)),
                    operation,
                    "SpillPhi must be at block entry",
                ));
            }
            spilled_phis.push(SpilledPhi {
                block,
                point,
                value,
                home,
                sources: phi.sources.clone(),
            });
        } else {
            point_ops[block]
                .entry(point.instruction)
                .or_default()
                .push(operation);
        }
    }

    let mut edge_ops = HashMap::<(usize, usize), Vec<PlannedOp>>::new();
    for (&(predecessor, successor), operations) in &plan.edge_ops {
        let Some(successors) = cfg.successors.get(predecessor) else {
            return Err(structural_error(
                "HOME.EDGE",
                "edge operation predecessor is outside the normalized CFG",
            ));
        };
        if !successors.contains(&successor) {
            return Err(structural_error(
                "HOME.EDGE",
                "edge operation does not name a normalized CFG edge",
            ));
        }
        let location = HomeLocation::Edge {
            predecessor: func.blocks[predecessor].id,
            successor: func.blocks[successor].id,
        };
        for &operation in operations {
            if matches!(operation, PlannedOp::SpillPhi { .. }) {
                return Err(operation_error(
                    "HOME.EDGE_SPILL_PHI",
                    Some(location),
                    operation,
                    "SpillPhi is a block-entry operation, not an edge operation",
                ));
            }
            verify_operation_home(func, plan, operation, location)?;
            universe.insert(operation_home(operation));
        }
        edge_ops.insert((predecessor, successor), operations.clone());
    }
    let rematerializable_homes = universe
        .iter()
        .copied()
        .filter(|home| is_rematerializable_home(func, plan, *home))
        .collect::<BTreeSet<_>>();
    let required_homes = point_ops
        .iter()
        .flat_map(|points| points.values().flatten())
        .chain(edge_ops.values().flatten())
        .filter_map(|operation| match operation {
            PlannedOp::Reload { value, home } if !is_rematerialized_logical(func, *value) => {
                Some(*home)
            }
            _ => None,
        })
        .chain(
            spilled_phis
                .iter()
                .filter(|phi| !is_rematerialized_logical(func, phi.value))
                .map(|phi| phi.home),
        )
        .collect::<BTreeSet<_>>();

    // Reconstruction turns a spilled phi into one store per incoming edge
    // unless the source's S_exit says that the shared congruence home is
    // already valid.  Keep these stores edge-specific: a store on one arm may
    // not establish a home on another arm.
    let mut implicit_edge_stores = HashMap::<(usize, usize), BTreeSet<SpillHome>>::new();
    for spilled_phi in &spilled_phis {
        for &(predecessor_id, source) in &spilled_phi.sources {
            let Some(&predecessor) = cfg.block_index.get(&predecessor_id) else {
                return Err(HomeVerifyError {
                    rule: "HOME.SPILL_PHI_EDGE",
                    location: Some(HomeLocation::Point(spilled_phi.point)),
                    value: Some(spilled_phi.value),
                    home: Some(spilled_phi.home),
                    message: format!("phi source predecessor {predecessor_id} is not in the CFG"),
                });
            };
            if !cfg.successors[predecessor].contains(&spilled_phi.block) {
                return Err(HomeVerifyError {
                    rule: "HOME.SPILL_PHI_EDGE",
                    location: Some(HomeLocation::Point(spilled_phi.point)),
                    value: Some(spilled_phi.value),
                    home: Some(spilled_phi.home),
                    message: format!("{predecessor_id} is not a predecessor of the phi block"),
                });
            }
            let source_logical = LogicalValue(source.0);
            if source.0 >= func.vregs.count() || plan.homes.of_vreg(source) != spilled_phi.home {
                return Err(HomeVerifyError {
                    rule: "HOME.SPILL_PHI_CLASS",
                    location: Some(HomeLocation::Edge {
                        predecessor: predecessor_id,
                        successor: func.blocks[spilled_phi.block].id,
                    }),
                    value: Some(source_logical),
                    home: Some(spilled_phi.home),
                    message: "phi source and destination do not share the planned home".into(),
                });
            }
            if !plan.s_exit[predecessor].contains(&source_logical)
                && !rematerializable_homes.contains(&spilled_phi.home)
            {
                implicit_edge_stores
                    .entry((predecessor, spilled_phi.block))
                    .or_default()
                    .insert(spilled_phi.home);
            }
        }
    }

    let mut definition_blocks = HashMap::<SpillHome, BTreeSet<usize>>::new();
    for (block, operations_by_point) in point_ops.iter().enumerate() {
        for operations in operations_by_point.values() {
            let mut stores = BTreeSet::new();
            collect_actual_stores(operations, &rematerializable_homes, &mut stores);
            for home in stores
                .into_iter()
                .filter(|home| required_homes.contains(home))
            {
                definition_blocks.entry(home).or_default().insert(block);
            }
        }
    }
    let mut edge_stores = implicit_edge_stores.clone();
    for (predecessor, successors) in cfg.successors.iter().enumerate() {
        for &successor in successors {
            if let Some(operations) = edge_ops.get(&(predecessor, successor)) {
                let stores = edge_stores.entry((predecessor, successor)).or_default();
                collect_actual_stores(operations, &rematerializable_homes, stores);
            }
        }
    }
    edge_stores.retain(|_, stores| {
        stores.retain(|home| required_homes.contains(home));
        !stores.is_empty()
    });

    // An edge definition can be represented without expanding the CFG.  If
    // the predecessor has one successor, the store is a block-exit
    // definition.  Otherwise normalization guarantees a dedicated
    // one-predecessor successor, where it is a block-entry definition.
    let mut entry_stores = vec![BTreeSet::<SpillHome>::new(); func.blocks.len()];
    let mut exit_stores = vec![BTreeSet::<SpillHome>::new(); func.blocks.len()];
    for (&(predecessor, successor), stores) in &edge_stores {
        let definition_block = if cfg.successors[predecessor].len() == 1 {
            exit_stores[predecessor].extend(stores.iter().copied());
            predecessor
        } else if cfg.predecessors[successor].as_slice() == [predecessor] {
            entry_stores[successor].extend(stores.iter().copied());
            successor
        } else {
            return Err(structural_error(
                "HOME.EDGE_NOT_ISOLATED",
                "edge store is neither on a single-successor predecessor nor a dedicated edge block",
            ));
        };
        for &home in stores {
            definition_blocks
                .entry(home)
                .or_default()
                .insert(definition_block);
        }
    }

    let mut work = HomeVerifyWork {
        homes: required_homes.len(),
        ..HomeVerifyWork::default()
    };
    let (mut phis, phis_by_block) =
        place_sparse_phis(cfg, &required_homes, &definition_blocks, &mut work);
    let pending = rename_and_collect_queries(
        func,
        cfg,
        &point_ops,
        &edge_ops,
        &edge_stores,
        &entry_stores,
        &exit_stores,
        &rematerializable_homes,
        &required_homes,
        &spilled_phis,
        &phis_by_block,
        &mut phis,
        &mut work,
    )?;
    verify_sparse_queries(cfg, &phis, pending)?;

    Ok(work)
}

#[derive(Debug)]
struct SpilledPhi {
    block: usize,
    point: ProgramPoint,
    value: LogicalValue,
    home: SpillHome,
    sources: Vec<(BlockId, VReg)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SparseDefinition {
    False,
    True,
    Phi(usize),
}

#[derive(Debug)]
struct SparsePhi {
    block: usize,
    home: SpillHome,
    inputs: Vec<SparseDefinition>,
}

#[derive(Debug)]
struct PendingQuery {
    rule: &'static str,
    location: HomeLocation,
    value: LogicalValue,
    home: SpillHome,
    definition: SparseDefinition,
    message: &'static str,
}

fn place_sparse_phis(
    cfg: &NormalizedCfg,
    homes: &BTreeSet<SpillHome>,
    definition_blocks: &HashMap<SpillHome, BTreeSet<usize>>,
    work: &mut HomeVerifyWork,
) -> (Vec<SparsePhi>, Vec<Vec<(SpillHome, usize)>>) {
    let mut phis = Vec::<SparsePhi>::new();
    let mut phis_by_block = vec![Vec::<(SpillHome, usize)>::new(); cfg.predecessors.len()];
    for &home in homes {
        // The virtual false definition at function entry participates in SSA
        // construction just like an ordinary definition. In canonical CFGs
        // its frontier is empty, but including it keeps loop-entry behavior
        // explicit.
        let mut definitions = definition_blocks.get(&home).cloned().unwrap_or_default();
        definitions.insert(0);
        let mut queue = definitions.iter().copied().collect::<VecDeque<_>>();
        let mut placed = BTreeSet::<usize>::new();
        while let Some(definition) = queue.pop_front() {
            for &frontier in &cfg.dominance_frontier[definition] {
                work.frontier_members_visited += 1;
                // Function entry is a hard false boundary even if malformed
                // control flow contains a backedge to it. Incoming CFG edges
                // cannot override the invocation path's uninitialized state.
                if frontier == 0 {
                    continue;
                }
                if !placed.insert(frontier) {
                    continue;
                }
                let id = phis.len();
                phis.push(SparsePhi {
                    block: frontier,
                    home,
                    inputs: Vec::with_capacity(cfg.predecessors[frontier].len()),
                });
                phis_by_block[frontier].push((home, id));
                work.phi_nodes += 1;
                if definitions.insert(frontier) {
                    queue.push_back(frontier);
                }
            }
        }
    }
    (phis, phis_by_block)
}

#[allow(clippy::too_many_arguments)]
fn rename_and_collect_queries(
    func: &MFunction,
    cfg: &NormalizedCfg,
    point_ops: &[BTreeMap<usize, Vec<PlannedOp>>],
    edge_ops: &HashMap<(usize, usize), Vec<PlannedOp>>,
    edge_stores: &HashMap<(usize, usize), BTreeSet<SpillHome>>,
    entry_stores: &[BTreeSet<SpillHome>],
    exit_stores: &[BTreeSet<SpillHome>],
    rematerializable_homes: &BTreeSet<SpillHome>,
    required_homes: &BTreeSet<SpillHome>,
    spilled_phis: &[SpilledPhi],
    phis_by_block: &[Vec<(SpillHome, usize)>],
    phis: &mut [SparsePhi],
    work: &mut HomeVerifyWork,
) -> Result<Vec<PendingQuery>, HomeVerifyError> {
    let mut children = vec![Vec::<usize>::new(); func.blocks.len()];
    for block in 1..func.blocks.len() {
        let Some(parent) = cfg.idom[block] else {
            return Err(structural_error(
                "HOME.DOMINATOR_TREE",
                "non-entry block has no immediate dominator",
            ));
        };
        children[parent].push(block);
    }
    let mut spilled_by_block = vec![Vec::<usize>::new(); func.blocks.len()];
    for (index, spilled_phi) in spilled_phis.iter().enumerate() {
        spilled_by_block[spilled_phi.block].push(index);
    }

    enum Action {
        Enter(usize),
        Exit(Vec<(SpillHome, Option<SparseDefinition>)>),
    }

    let mut current = HashMap::<SpillHome, SparseDefinition>::new();
    let mut pending = Vec::<PendingQuery>::new();
    let mut actions = vec![Action::Enter(0)];
    while let Some(action) = actions.pop() {
        let block = match action {
            Action::Exit(changes) => {
                for (home, previous) in changes.into_iter().rev() {
                    if let Some(previous) = previous {
                        current.insert(home, previous);
                    } else {
                        current.remove(&home);
                    }
                }
                continue;
            }
            Action::Enter(block) => block,
        };
        work.dominator_blocks += 1;
        let mut changes = Vec::<(SpillHome, Option<SparseDefinition>)>::new();

        for &(home, phi) in &phis_by_block[block] {
            set_current(
                &mut current,
                &mut changes,
                home,
                SparseDefinition::Phi(phi),
                work,
            );
        }
        for &home in &entry_stores[block] {
            set_current(
                &mut current,
                &mut changes,
                home,
                SparseDefinition::True,
                work,
            );
        }

        // SpillPhi is a block-entry query. It observes incoming edge stores,
        // but not ordinary point stores before instruction zero.
        for &spilled_index in &spilled_by_block[block] {
            let spilled = &spilled_phis[spilled_index];
            if !is_rematerialized_logical(func, spilled.value) {
                pending.push(PendingQuery {
                    rule: SPILL_PHI_RULE,
                    location: HomeLocation::Point(spilled.point),
                    value: spilled.value,
                    home: spilled.home,
                    definition: current_definition(&current, spilled.home),
                    message: "spilled phi home is not stored on every incoming path",
                });
                work.query_checks += 1;
            }
        }

        for (&instruction, operations) in &point_ops[block] {
            apply_stores(
                operations,
                rematerializable_homes,
                required_homes,
                &mut current,
                &mut changes,
                work,
            );
            collect_reload_queries(
                func,
                operations,
                &current,
                HomeLocation::Point(ProgramPoint {
                    block: func.blocks[block].id,
                    instruction,
                    side: super::spill_plan::PointSide::Before,
                }),
                &mut pending,
                work,
                false,
                None,
            );
        }

        for &home in &exit_stores[block] {
            set_current(
                &mut current,
                &mut changes,
                home,
                SparseDefinition::True,
                work,
            );
        }

        // Capture edge reloads and sparse-phi operands from the same block-exit
        // state. Edge stores are parallel operations and therefore precede
        // both kinds of uses.
        for &successor in &cfg.successors[block] {
            let stores = edge_stores.get(&(block, successor));
            if let Some(operations) = edge_ops.get(&(block, successor)) {
                collect_reload_queries(
                    func,
                    operations,
                    &current,
                    HomeLocation::Edge {
                        predecessor: func.blocks[block].id,
                        successor: func.blocks[successor].id,
                    },
                    &mut pending,
                    work,
                    true,
                    stores,
                );
            }
            for &(home, phi) in &phis_by_block[successor] {
                let definition = if stores.is_some_and(|stores| stores.contains(&home)) {
                    SparseDefinition::True
                } else {
                    current_definition(&current, home)
                };
                phis[phi].inputs.push(definition);
                work.phi_inputs += 1;
            }
        }

        actions.push(Action::Exit(changes));
        for &child in children[block].iter().rev() {
            actions.push(Action::Enter(child));
        }
    }

    Ok(pending)
}

fn set_current(
    current: &mut HashMap<SpillHome, SparseDefinition>,
    changes: &mut Vec<(SpillHome, Option<SparseDefinition>)>,
    home: SpillHome,
    definition: SparseDefinition,
    work: &mut HomeVerifyWork,
) {
    let previous = current.get(&home).copied();
    if previous == Some(definition) || (previous.is_none() && definition == SparseDefinition::False)
    {
        return;
    }
    changes.push((home, previous));
    current.insert(home, definition);
    work.fact_updates += 1;
}

fn current_definition(
    current: &HashMap<SpillHome, SparseDefinition>,
    home: SpillHome,
) -> SparseDefinition {
    current
        .get(&home)
        .copied()
        .unwrap_or(SparseDefinition::False)
}

fn apply_stores(
    operations: &[PlannedOp],
    rematerializable_homes: &BTreeSet<SpillHome>,
    required_homes: &BTreeSet<SpillHome>,
    current: &mut HashMap<SpillHome, SparseDefinition>,
    changes: &mut Vec<(SpillHome, Option<SparseDefinition>)>,
    work: &mut HomeVerifyWork,
) {
    for &operation in operations {
        if let PlannedOp::Spill { home, .. } = operation
            && !rematerializable_homes.contains(&home)
            && required_homes.contains(&home)
        {
            set_current(current, changes, home, SparseDefinition::True, work);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_reload_queries(
    func: &MFunction,
    operations: &[PlannedOp],
    current: &HashMap<SpillHome, SparseDefinition>,
    location: HomeLocation,
    pending: &mut Vec<PendingQuery>,
    work: &mut HomeVerifyWork,
    edge: bool,
    edge_stores: Option<&BTreeSet<SpillHome>>,
) {
    for &operation in operations {
        let PlannedOp::Reload { value, home } = operation else {
            continue;
        };
        if is_rematerialized_logical(func, value) {
            continue;
        }
        let definition = if edge_stores.is_some_and(|stores| stores.contains(&home)) {
            SparseDefinition::True
        } else {
            current_definition(current, home)
        };
        pending.push(PendingQuery {
            rule: RELOAD_RULE,
            location,
            value,
            home,
            definition,
            message: "reload is reachable on a path without a prior same-home store",
        });
        work.query_checks += 1;
        work.edge_query_checks += usize::from(edge);
    }
}

fn verify_sparse_queries(
    cfg: &NormalizedCfg,
    phis: &[SparsePhi],
    pending: Vec<PendingQuery>,
) -> Result<(), HomeVerifyError> {
    let mut phi_users = vec![Vec::<usize>::new(); phis.len()];
    let mut false_phi = vec![false; phis.len()];
    let mut queue = VecDeque::<usize>::new();
    for (phi, node) in phis.iter().enumerate() {
        if node.inputs.len() != cfg.predecessors[node.block].len() {
            return Err(structural_error(
                "HOME.SPARSE_PHI_INPUTS",
                format!(
                    "home {:?} meet at block {} has {} inputs for {} predecessors",
                    node.home,
                    node.block,
                    node.inputs.len(),
                    cfg.predecessors[node.block].len()
                ),
            ));
        }
        let mut immediately_false = node.inputs.is_empty();
        for &input in &node.inputs {
            match input {
                SparseDefinition::False => immediately_false = true,
                SparseDefinition::True => {}
                SparseDefinition::Phi(definition) => phi_users[definition].push(phi),
            }
        }
        if immediately_false {
            false_phi[phi] = true;
            queue.push_back(phi);
        }
    }
    while let Some(phi) = queue.pop_front() {
        for &user in &phi_users[phi] {
            if !false_phi[user] {
                false_phi[user] = true;
                queue.push_back(user);
            }
        }
    }

    for query in pending {
        let initialized = match query.definition {
            SparseDefinition::False => false,
            SparseDefinition::True => true,
            SparseDefinition::Phi(phi) => !false_phi[phi],
        };
        if !initialized {
            return Err(HomeVerifyError {
                rule: query.rule,
                location: Some(query.location),
                value: Some(query.value),
                home: Some(query.home),
                message: query.message.into(),
            });
        }
    }
    Ok(())
}

fn collect_actual_stores(
    operations: &[PlannedOp],
    rematerializable_homes: &BTreeSet<SpillHome>,
    stores: &mut BTreeSet<SpillHome>,
) {
    for &operation in operations {
        if let PlannedOp::Spill { home, .. } = operation
            && !rematerializable_homes.contains(&home)
        {
            stores.insert(home);
        }
    }
}

fn verify_operation_home(
    func: &MFunction,
    plan: &SpillPlan,
    operation: PlannedOp,
    location: HomeLocation,
) -> Result<(), HomeVerifyError> {
    let value = operation_value(operation);
    let home = operation_home(operation);
    if value.0 >= func.vregs.count() {
        return Err(operation_error(
            "HOME.VALUE_RANGE",
            Some(location),
            operation,
            "operation logical value is outside the original VReg domain",
        ));
    }
    if plan.homes.of_logical(value) != home {
        return Err(operation_error(
            "HOME.CLASS_MISMATCH",
            Some(location),
            operation,
            "operation home differs from its phi-congruence home",
        ));
    }
    Ok(())
}

fn operation_value(operation: PlannedOp) -> LogicalValue {
    match operation {
        PlannedOp::Spill { value, .. }
        | PlannedOp::Reload { value, .. }
        | PlannedOp::SpillPhi { value, .. } => value,
    }
}

fn operation_home(operation: PlannedOp) -> SpillHome {
    match operation {
        PlannedOp::Spill { home, .. }
        | PlannedOp::Reload { home, .. }
        | PlannedOp::SpillPhi { home, .. } => home,
    }
}

fn is_rematerialized_logical(func: &MFunction, value: LogicalValue) -> bool {
    matches!(
        func.spill_desc(VReg(value.0)).map(|desc| &desc.kind),
        Some(SpillKind::Remat { .. })
    )
}

fn is_rematerializable_home(func: &MFunction, plan: &SpillPlan, home: SpillHome) -> bool {
    let mut immediate = None;
    for member in plan.homes.members(home) {
        let Some(SpillKind::Remat { value }) = func.spill_desc(member).map(|desc| &desc.kind)
        else {
            return false;
        };
        if immediate.is_some_and(|previous| previous != *value) {
            return false;
        }
        immediate = Some(*value);
    }
    immediate.is_some()
}

fn operation_error(
    rule: &'static str,
    location: Option<HomeLocation>,
    operation: PlannedOp,
    message: impl Into<String>,
) -> HomeVerifyError {
    HomeVerifyError {
        rule,
        location,
        value: Some(operation_value(operation)),
        home: Some(operation_home(operation)),
        message: message.into(),
    }
}

fn structural_error(rule: &'static str, message: impl Into<String>) -> HomeVerifyError {
    HomeVerifyError {
        rule,
        location: None,
        value: None,
        home: None,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{MBlock, MInst, PhiNode, SpillDesc, VRegAllocator};

    use super::super::{cfg, next_use, spill_plan};

    fn blank_plan(func: &MFunction, cfg: &NormalizedCfg) -> SpillPlan {
        let next_use = next_use::analyze(func, cfg).unwrap();
        let mut plan = spill_plan::plan(func, cfg, &next_use, 32).unwrap();
        plan.point_ops.clear();
        plan.edge_ops.clear();
        for state in plan
            .w_entry
            .iter_mut()
            .chain(&mut plan.w_exit)
            .chain(&mut plan.s_entry)
            .chain(&mut plan.s_exit)
        {
            state.clear();
        }
        plan
    }

    fn point(block: BlockId, instruction: usize) -> ProgramPoint {
        ProgramPoint {
            block,
            instruction,
            side: super::super::spill_plan::PointSide::Before,
        }
    }

    fn one_value_function(desc: SpillDesc) -> (MFunction, VReg) {
        let mut vregs = VRegAllocator::new();
        let value = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![desc]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: value,
            value: 7,
        });
        block.push(MInst::Return);
        func.push_block(block);
        (func, value)
    }

    #[test]
    fn same_point_store_precedes_reload() {
        let (mut func, value) = one_value_function(SpillDesc::transient());
        let cfg = cfg::normalize(&mut func).unwrap();
        let mut plan = blank_plan(&func, &cfg);
        let logical = plan.logical.of(value);
        let home = plan.homes.of_vreg(value);
        // Deliberately put Reload first: a plan point is parallel and lowering
        // orders stores before reloads.
        plan.point_ops.push((
            point(BlockId(0), 1),
            PlannedOp::Reload {
                value: logical,
                home,
            },
        ));
        plan.point_ops.push((
            point(BlockId(0), 1),
            PlannedOp::Spill {
                value: logical,
                home,
            },
        ));
        verify(&func, &cfg, &plan).unwrap();
    }

    #[test]
    fn later_store_does_not_dominate_point_reload() {
        let mut vregs = VRegAllocator::new();
        let value = vregs.alloc();
        let copy = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: value,
            value: 7,
        });
        block.push(MInst::Mov {
            dst: copy,
            src: value,
        });
        block.push(MInst::Return);
        func.push_block(block);
        let cfg = cfg::normalize(&mut func).unwrap();
        let mut plan = blank_plan(&func, &cfg);
        let logical = plan.logical.of(value);
        let home = plan.homes.of_vreg(value);
        plan.point_ops.push((
            point(BlockId(0), 1),
            PlannedOp::Reload {
                value: logical,
                home,
            },
        ));
        plan.point_ops.push((
            point(BlockId(0), 2),
            PlannedOp::Spill {
                value: logical,
                home,
            },
        ));
        let error = verify(&func, &cfg, &plan).unwrap_err();
        assert_eq!(error.rule, RELOAD_RULE);
    }

    fn diamond() -> (MFunction, VReg) {
        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let value = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: value,
            value: 7,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Jump { target: BlockId(3) });
        let mut join = MBlock::new(BlockId(3));
        join.push(MInst::Return);
        func.blocks = vec![entry, left, right, join];
        (func, value)
    }

    #[test]
    fn store_on_only_one_incoming_edge_does_not_dominate_join_reload() {
        let (mut func, value) = diamond();
        let cfg = cfg::normalize(&mut func).unwrap();
        let mut plan = blank_plan(&func, &cfg);
        let logical = plan.logical.of(value);
        let home = plan.homes.of_vreg(value);
        let left = cfg.block_index[&BlockId(1)];
        let join = cfg.block_index[&BlockId(3)];
        plan.edge_ops.insert(
            (left, join),
            vec![PlannedOp::Spill {
                value: logical,
                home,
            }],
        );
        plan.point_ops.push((
            point(BlockId(3), 0),
            PlannedOp::Reload {
                value: logical,
                home,
            },
        ));
        let error = verify(&func, &cfg, &plan).unwrap_err();
        assert_eq!(error.rule, RELOAD_RULE);
    }

    #[test]
    fn stores_on_every_incoming_edge_establish_join_home() {
        let (mut func, value) = diamond();
        let cfg = cfg::normalize(&mut func).unwrap();
        let mut plan = blank_plan(&func, &cfg);
        let logical = plan.logical.of(value);
        let home = plan.homes.of_vreg(value);
        let join = cfg.block_index[&BlockId(3)];
        for predecessor in [BlockId(1), BlockId(2)] {
            let predecessor = cfg.block_index[&predecessor];
            plan.edge_ops.insert(
                (predecessor, join),
                vec![PlannedOp::Spill {
                    value: logical,
                    home,
                }],
            );
        }
        plan.point_ops.push((
            point(BlockId(3), 0),
            PlannedOp::Reload {
                value: logical,
                home,
            },
        ));
        verify(&func, &cfg, &plan).unwrap();
    }

    #[test]
    fn edge_store_precedes_edge_reload() {
        let (mut func, value) = one_value_function(SpillDesc::transient());
        func.blocks[0].insts.pop();
        func.blocks[0].push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(1));
        exit.push(MInst::Return);
        func.push_block(exit);
        let cfg = cfg::normalize(&mut func).unwrap();
        let mut plan = blank_plan(&func, &cfg);
        let logical = plan.logical.of(value);
        let home = plan.homes.of_vreg(value);
        let entry = cfg.block_index[&BlockId(0)];
        let exit = cfg.block_index[&BlockId(1)];
        plan.edge_ops.insert(
            (entry, exit),
            vec![
                PlannedOp::Reload {
                    value: logical,
                    home,
                },
                PlannedOp::Spill {
                    value: logical,
                    home,
                },
            ],
        );
        verify(&func, &cfg, &plan).unwrap();
    }

    #[test]
    fn rematerialized_reload_needs_no_store() {
        let (mut func, value) = one_value_function(SpillDesc::remat(7));
        let cfg = cfg::normalize(&mut func).unwrap();
        let mut plan = blank_plan(&func, &cfg);
        let logical = plan.logical.of(value);
        let home = plan.homes.of_vreg(value);
        plan.point_ops.push((
            point(BlockId(0), 1),
            PlannedOp::Reload {
                value: logical,
                home,
            },
        ));
        verify(&func, &cfg, &plan).unwrap();
    }

    #[test]
    fn spilled_phi_rejects_a_falsely_claimed_incoming_home() {
        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let left_value = vregs.alloc();
        let right_value = vregs.alloc();
        let merged = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: left_value,
            value: 11,
        });
        entry.push(MInst::LoadImm {
            dst: right_value,
            value: 22,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Jump { target: BlockId(3) });
        let mut join = MBlock::new(BlockId(3));
        join.phis.push(PhiNode {
            dst: merged,
            sources: vec![(BlockId(1), left_value), (BlockId(2), right_value)],
        });
        join.push(MInst::Return);
        func.blocks = vec![entry, left, right, join];

        let cfg = cfg::normalize(&mut func).unwrap();
        let mut plan = blank_plan(&func, &cfg);
        let merged_logical = plan.logical.of(merged);
        let home = plan.homes.of_vreg(merged);
        let left = cfg.block_index[&BlockId(1)];
        // This claim suppresses reconstruction's implicit store on the left
        // edge, but there is no actual store establishing it.
        plan.s_exit[left].insert(plan.logical.of(left_value));
        plan.point_ops.push((
            point(BlockId(3), 0),
            PlannedOp::SpillPhi {
                value: merged_logical,
                home,
            },
        ));
        let error = verify(&func, &cfg, &plan).unwrap_err();
        assert_eq!(error.rule, SPILL_PHI_RULE);
    }

    fn loop_function() -> (MFunction, VReg) {
        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let value = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: value,
            value: 9,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut header = MBlock::new(BlockId(1));
        header.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(2),
            false_bb: BlockId(3),
        });
        let mut body = MBlock::new(BlockId(2));
        body.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(3));
        exit.push(MInst::Return);
        func.blocks = vec![entry, header, body, exit];
        (func, value)
    }

    #[test]
    fn backedge_store_does_not_hide_unstored_loop_entry_path() {
        let (mut func, value) = loop_function();
        let cfg = cfg::normalize(&mut func).unwrap();
        let mut plan = blank_plan(&func, &cfg);
        let logical = plan.logical.of(value);
        let home = plan.homes.of_vreg(value);
        let body = cfg.block_index[&BlockId(2)];
        let header = cfg.block_index[&BlockId(1)];
        plan.edge_ops.insert(
            (body, header),
            vec![PlannedOp::Spill {
                value: logical,
                home,
            }],
        );
        plan.point_ops.push((
            point(BlockId(1), 0),
            PlannedOp::Reload {
                value: logical,
                home,
            },
        ));

        let error = verify(&func, &cfg, &plan).unwrap_err();

        assert_eq!(error.rule, RELOAD_RULE);
    }

    #[test]
    fn preheader_store_establishes_home_through_loop_fixed_point() {
        let (mut func, value) = loop_function();
        let cfg = cfg::normalize(&mut func).unwrap();
        let mut plan = blank_plan(&func, &cfg);
        let logical = plan.logical.of(value);
        let home = plan.homes.of_vreg(value);
        plan.point_ops.push((
            point(BlockId(0), 2),
            PlannedOp::Spill {
                value: logical,
                home,
            },
        ));
        plan.point_ops.push((
            point(BlockId(1), 0),
            PlannedOp::Reload {
                value: logical,
                home,
            },
        ));

        verify(&func, &cfg, &plan).unwrap();
    }

    #[test]
    fn sparse_proof_does_not_materialize_block_by_home_state() {
        const BLOCKS: usize = 1024;
        const HOMES: usize = 256;

        let mut vregs = VRegAllocator::new();
        let values = (0..HOMES).map(|_| vregs.alloc()).collect::<Vec<_>>();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); HOMES]);
        let mut entry = MBlock::new(BlockId(0));
        for (index, &value) in values.iter().enumerate() {
            entry.push(MInst::LoadImm {
                dst: value,
                value: index as u64,
            });
        }
        entry.push(MInst::Jump { target: BlockId(1) });
        func.push_block(entry);
        for block in 1..BLOCKS - 1 {
            let mut body = MBlock::new(BlockId(block as u32));
            body.push(MInst::Jump {
                target: BlockId(block as u32 + 1),
            });
            func.push_block(body);
        }
        let mut exit = MBlock::new(BlockId((BLOCKS - 1) as u32));
        exit.push(MInst::Return);
        func.push_block(exit);
        let cfg = cfg::normalize(&mut func).unwrap();
        let mut plan = blank_plan(&func, &cfg);
        for &value in &values {
            let logical = plan.logical.of(value);
            let home = plan.homes.of_vreg(value);
            plan.point_ops.push((
                point(BlockId(0), HOMES),
                PlannedOp::Spill {
                    value: logical,
                    home,
                },
            ));
            plan.point_ops.push((
                point(BlockId((BLOCKS - 1) as u32), 0),
                PlannedOp::Reload {
                    value: logical,
                    home,
                },
            ));
        }

        let work = verify_with_work(&func, &cfg, &plan).unwrap();

        assert_eq!(work.homes, HOMES);
        assert_eq!(work.dominator_blocks, BLOCKS);
        assert_eq!(work.phi_nodes, 0);
        assert_eq!(work.frontier_members_visited, 0);
        assert_eq!(work.fact_updates, HOMES);
        assert_eq!(work.query_checks, HOMES);
        let sparse_operations = work.dominator_blocks
            + work.frontier_members_visited
            + work.phi_nodes
            + work.phi_inputs
            + work.fact_updates
            + work.query_checks;
        assert!(sparse_operations <= BLOCKS + 3 * HOMES);
        assert!(sparse_operations < BLOCKS * HOMES / 16);
    }
}
