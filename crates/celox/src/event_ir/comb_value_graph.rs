use std::cell::RefCell;

use thiserror::Error;

use crate::{
    HashMap, HashSet,
    ir::{AbsoluteAddr, BinaryOp, BitAccess},
    logic_tree::{NodeId, SLTNode, SLTNodeArena},
};

use super::{CombDefinitionId, CombGraph, CombRecipeId, CombSnapshotKind};

#[derive(Debug, Error)]
pub(super) enum CombValueGraphError {
    #[error("combinational definition graph contains a cycle through {0}")]
    DefinitionCycle(CombDefinitionId),
}

#[derive(Debug, Clone, Copy, Default)]
struct ValueCost {
    instructions: u32,
    contains_div_rem: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct DemandSummary {
    fingerprint: [u64; 4],
    cost: ValueCost,
}

#[derive(Debug, Clone, Copy)]
struct MuxAssessment {
    benefit: u32,
    estimated_growth: u32,
}

#[derive(Debug, Clone)]
struct MuxCandidate {
    recipe: CombRecipeId,
    node: NodeId,
    assessment: MuxAssessment,
    deferred_dependencies: Vec<CombDefinitionId>,
}

impl DemandSummary {
    fn add_definition(&mut self, definition: CombDefinitionId, cost: ValueCost) {
        let mut hash = (definition.0 as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        for word in &mut self.fingerprint {
            hash ^= hash >> 30;
            hash = hash.wrapping_mul(0xbf58_476d_1ce4_e5b9);
            *word |= 1u64 << (hash & 63);
            hash = hash.rotate_left(17);
        }
        self.cost.add(cost);
    }

    fn merge(&mut self, other: Self) {
        for (word, other) in self.fingerprint.iter_mut().zip(other.fingerprint) {
            *word |= other;
        }
        self.cost.add(other.cost);
    }
}

impl ValueCost {
    const LIMIT: u32 = 1_000_000;

    fn node(children: impl IntoIterator<Item = Self>) -> Self {
        children.into_iter().fold(
            Self {
                instructions: 1,
                contains_div_rem: false,
            },
            |mut total, child| {
                total.instructions = total
                    .instructions
                    .saturating_add(child.instructions)
                    .min(Self::LIMIT);
                total.contains_div_rem |= child.contains_div_rem;
                total
            },
        )
    }

    fn add(&mut self, other: Self) {
        self.instructions = self
            .instructions
            .saturating_add(other.instructions)
            .min(Self::LIMIT);
        self.contains_div_rem |= other.contains_div_rem;
    }
}

#[derive(Default)]
struct RecipeInputIndex {
    definitions_by_object: HashMap<AbsoluteAddr, Vec<(CombDefinitionId, BitAccess)>>,
    previous_snapshots_by_object: HashMap<AbsoluteAddr, Vec<BitAccess>>,
    all_dependencies: Vec<CombDefinitionId>,
}

/// Sparse control-aware view of the combinational definition graph.
///
/// This deliberately does not substitute one SLT recipe into another. Recipe
/// boundaries remain graph edges, so construction is linear in the source SLT
/// arena and the definition edges. Lowering may place an arm-exclusive edge
/// inside the corresponding Mux block without constructing a context-expanded
/// symbolic arena.
pub(super) struct CombValueGraph {
    recipe_inputs: Vec<RecipeInputIndex>,
    recipe_local_costs: Vec<ValueCost>,
    definition_costs: Vec<ValueCost>,
    demand_cache: RefCell<HashMap<(CombRecipeId, NodeId), DemandSummary>>,
    planned_muxes: HashSet<(CombRecipeId, NodeId)>,
    deferred_edges: HashSet<(CombRecipeId, CombDefinitionId)>,
}

impl CombValueGraph {
    pub(super) fn build(
        graph: &CombGraph,
        arena: &SLTNodeArena<AbsoluteAddr>,
        demanded_definitions: &[bool],
        allow_sparse_control: bool,
    ) -> Result<Self, CombValueGraphError> {
        let node_costs = build_node_costs(arena);
        let recipe_local_costs = graph
            .recipes()
            .iter()
            .map(|recipe| node_costs[recipe.root.0])
            .collect::<Vec<_>>();
        let recipe_inputs = graph
            .recipes()
            .iter()
            .map(|recipe| {
                let mut index = RecipeInputIndex::default();
                for dependency in &recipe.dependencies {
                    let object = graph.definitions()[dependency.definition.0].target.object;
                    index
                        .definitions_by_object
                        .entry(object)
                        .or_default()
                        .push((
                            dependency.definition,
                            graph.definitions()[dependency.definition.0].target.access,
                        ));
                }
                for definitions in index.definitions_by_object.values_mut() {
                    definitions.sort_unstable();
                    definitions.dedup();
                }
                for snapshot in &recipe.snapshot_inputs {
                    if snapshot.kind == CombSnapshotKind::PreviousValue {
                        index
                            .previous_snapshots_by_object
                            .entry(snapshot.range.object)
                            .or_default()
                            .push(snapshot.range.access);
                    }
                }
                index.all_dependencies = index
                    .definitions_by_object
                    .values()
                    .flatten()
                    .map(|(definition, _)| *definition)
                    .collect();
                index.all_dependencies.sort_unstable();
                index.all_dependencies.dedup();
                index
            })
            .collect::<Vec<_>>();

        let mut states = vec![CostState::Unseen; graph.definitions().len()];
        for (definition, demanded) in demanded_definitions.iter().copied().enumerate() {
            if demanded {
                resolve_definition_cost(
                    CombDefinitionId(definition),
                    graph,
                    &recipe_inputs,
                    &node_costs,
                    &mut states,
                )?;
            }
        }
        let definition_costs = states
            .into_iter()
            .map(|state| match state {
                CostState::Done(cost) => cost,
                CostState::Unseen => ValueCost::default(),
                CostState::Visiting => unreachable!("cost DFS leaves no active frame"),
            })
            .collect::<Vec<_>>();
        let mut reachable_definitions = vec![false; graph.definitions().len()];
        let mut reachability_work = demanded_definitions
            .iter()
            .enumerate()
            .filter_map(|(definition, demanded)| demanded.then_some(CombDefinitionId(definition)))
            .collect::<Vec<_>>();
        while let Some(definition) = reachability_work.pop() {
            if std::mem::replace(&mut reachable_definitions[definition.0], true) {
                continue;
            }
            let recipe = graph.definitions()[definition.0].recipe;
            reachability_work.extend(recipe_inputs[recipe.0].all_dependencies.iter().copied());
        }
        let mut definition_consumer_counts = vec![0u32; graph.definitions().len()];
        for (definition, reachable) in reachable_definitions.into_iter().enumerate() {
            if !reachable {
                continue;
            }
            let recipe = graph.definitions()[definition].recipe;
            for &dependency in &recipe_inputs[recipe.0].all_dependencies {
                definition_consumer_counts[dependency.0] =
                    definition_consumer_counts[dependency.0].saturating_add(1);
            }
        }
        let mut result = Self {
            recipe_inputs,
            recipe_local_costs,
            definition_costs,
            demand_cache: RefCell::new(HashMap::default()),
            planned_muxes: HashSet::default(),
            deferred_edges: HashSet::default(),
        };
        if allow_sparse_control {
            result.plan_muxes(
                graph,
                arena,
                demanded_definitions,
                &definition_consumer_counts,
            );
        }
        result.begin_recipe_root_analysis();
        Ok(result)
    }

    pub(super) fn all_dependencies(&self, recipe: CombRecipeId) -> &[CombDefinitionId] {
        &self.recipe_inputs[recipe.0].all_dependencies
    }

    fn begin_recipe_root_analysis(&self) {
        self.demand_cache.borrow_mut().clear();
    }

    pub(super) fn eager_dependencies(
        &self,
        recipe: CombRecipeId,
    ) -> impl Iterator<Item = CombDefinitionId> + '_ {
        self.all_dependencies(recipe)
            .iter()
            .copied()
            .filter(move |dependency| !self.deferred_edges.contains(&(recipe, *dependency)))
    }

    pub(super) fn dependencies_in_subtree(
        &self,
        recipe: CombRecipeId,
        root: NodeId,
        arena: &SLTNodeArena<AbsoluteAddr>,
        materialized_nodes: &HashMap<NodeId, crate::ir::RegisterId>,
    ) -> Vec<CombDefinitionId> {
        let mut result = HashSet::default();
        let mut visited = HashSet::default();
        let mut work = vec![root];
        while let Some(node) = work.pop() {
            if materialized_nodes.contains_key(&node) || !visited.insert(node) {
                continue;
            }
            match arena.get(node) {
                SLTNode::Input {
                    variable,
                    index,
                    access,
                    ..
                } => {
                    self.append_input_dependencies(
                        recipe,
                        *variable,
                        index.is_empty().then_some(*access),
                        &mut result,
                    );
                    work.extend(index.iter().map(|index| index.node));
                }
                SLTNode::Constant(..) => {}
                SLTNode::Binary(lhs, _, rhs) => work.extend([*lhs, *rhs]),
                SLTNode::Unary(_, input) | SLTNode::Slice { expr: input, .. } => {
                    work.push(*input);
                }
                SLTNode::Mux {
                    cond,
                    then_expr,
                    else_expr,
                } => work.extend([*cond, *then_expr, *else_expr]),
                SLTNode::Concat(parts) => {
                    work.extend(parts.iter().map(|(part, _)| *part));
                }
                // Fold environments can rebind address and value inputs. Keep
                // their complete declared edge set at the fold placement.
                SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => {
                    result.extend(self.all_dependencies(recipe).iter().copied());
                }
            }
        }
        let mut result = result.into_iter().collect::<Vec<_>>();
        result.sort_unstable();
        result
    }

    pub(super) fn mux_needs_control(&self, recipe: CombRecipeId, node: NodeId) -> bool {
        self.planned_muxes.contains(&(recipe, node))
    }

    fn plan_muxes(
        &mut self,
        graph: &CombGraph,
        arena: &SLTNodeArena<AbsoluteAddr>,
        demanded_definitions: &[bool],
        definition_consumer_counts: &[u32],
    ) {
        const MIN_BENEFIT: u32 = 4_096;

        let mut candidates = Vec::new();
        let mut visited_recipes = vec![false; graph.recipes().len()];
        for (definition, demanded) in demanded_definitions.iter().copied().enumerate() {
            if !demanded {
                continue;
            }
            let recipe = graph.definitions()[definition].recipe;
            if std::mem::replace(&mut visited_recipes[recipe.0], true) {
                continue;
            }
            self.begin_recipe_root_analysis();
            let root = NodeId(graph.recipes()[recipe.0].root.0);
            let mut best = None;
            let mut visited = HashSet::default();
            let mut work = vec![root];
            while let Some(node) = work.pop() {
                if !visited.insert(node) {
                    continue;
                }
                match arena.get(node) {
                    SLTNode::Mux {
                        then_expr,
                        else_expr,
                        ..
                    } => {
                        let then_demand = self.demand_summary(recipe, *then_expr, arena);
                        let else_demand = self.demand_summary(recipe, *else_expr, arena);
                        let rough_benefit = if then_demand.fingerprint != else_demand.fingerprint {
                            then_demand
                                .cost
                                .instructions
                                .max(else_demand.cost.instructions)
                        } else {
                            0
                        };
                        if rough_benefit < MIN_BENEFIT
                            && !then_demand.cost.contains_div_rem
                            && !else_demand.cost.contains_div_rem
                        {
                            work.extend(slt_children(node, arena));
                            continue;
                        }
                        let deferred_dependencies =
                            self.deferred_dependencies_for_mux(graph, recipe, node, arena);
                        let assessment = self.deferred_work_assessment(
                            graph,
                            &deferred_dependencies,
                            definition_consumer_counts,
                            demanded_definitions,
                            MIN_BENEFIT,
                        );
                        let candidate = MuxCandidate {
                            recipe,
                            node,
                            assessment,
                            deferred_dependencies,
                        };
                        if assessment.benefit >= MIN_BENEFIT
                            && !candidate.deferred_dependencies.is_empty()
                            && best.as_ref().is_none_or(|current: &MuxCandidate| {
                                candidate_is_better(&candidate, current)
                            })
                        {
                            best = Some(candidate);
                        }
                        work.extend(slt_children(node, arena));
                    }
                    SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => {}
                    _ => work.extend(slt_children(node, arena)),
                }
            }
            if let Some(candidate) = best {
                candidates.push(candidate);
            }
        }

        // Moving two cuts whose transitive definition cones overlap destroys
        // the shared placement/CSE contract. Build those conflicts with one
        // definition-owner table and choose the highest-benefit cut in each
        // connected component. Independent cones remain independently
        // selectable.
        let mut parents = (0..candidates.len()).collect::<Vec<_>>();
        let mut cone_owner = HashMap::default();
        for (candidate_index, candidate) in candidates.iter().enumerate() {
            let mut visited = HashSet::default();
            let mut work = candidate.deferred_dependencies.clone();
            while let Some(definition) = work.pop() {
                if !visited.insert(definition) {
                    continue;
                }
                if let Some(previous) = cone_owner.insert(definition, candidate_index) {
                    union_components(&mut parents, candidate_index, previous);
                }
                let recipe = graph.definitions()[definition.0].recipe;
                work.extend(self.all_dependencies(recipe).iter().copied());
            }
        }
        let mut best_by_component = HashMap::default();
        for candidate_index in 0..candidates.len() {
            let component = find_component(&mut parents, candidate_index);
            best_by_component
                .entry(component)
                .and_modify(|best: &mut usize| {
                    if candidate_is_better(&candidates[candidate_index], &candidates[*best]) {
                        *best = candidate_index;
                    }
                })
                .or_insert(candidate_index);
        }
        let mut selected = best_by_component.into_values().collect::<Vec<_>>();
        selected.sort_unstable();
        for candidate_index in selected {
            let candidate = &candidates[candidate_index];
            self.planned_muxes
                .insert((candidate.recipe, candidate.node));
            self.deferred_edges.extend(
                candidate
                    .deferred_dependencies
                    .iter()
                    .copied()
                    .map(|dependency| (candidate.recipe, dependency)),
            );
        }
    }

    fn deferred_dependencies_for_mux(
        &self,
        graph: &CombGraph,
        recipe: CombRecipeId,
        node: NodeId,
        arena: &SLTNodeArena<AbsoluteAddr>,
    ) -> Vec<CombDefinitionId> {
        let SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } = arena.get(node)
        else {
            unreachable!("sparse-control candidate remains a Mux")
        };
        let empty = HashMap::default();
        let then_dependencies = self
            .dependencies_in_subtree(recipe, *then_expr, arena, &empty)
            .into_iter()
            .collect::<HashSet<_>>();
        let else_dependencies = self
            .dependencies_in_subtree(recipe, *else_expr, arena, &empty)
            .into_iter()
            .collect::<HashSet<_>>();

        let mut skipped = HashMap::default();
        skipped.insert(node, crate::ir::RegisterId(0));
        let mut eager = self
            .dependencies_in_subtree(
                recipe,
                NodeId(graph.recipes()[recipe.0].root.0),
                arena,
                &skipped,
            )
            .into_iter()
            .collect::<HashSet<_>>();
        eager.extend(self.dependencies_in_subtree(recipe, *cond, arena, &empty));
        eager.extend(then_dependencies.intersection(&else_dependencies).copied());
        let mut deferred = then_dependencies
            .symmetric_difference(&else_dependencies)
            .copied()
            .filter(|dependency| !eager.contains(dependency))
            .collect::<Vec<_>>();
        deferred.sort_unstable();
        deferred
    }

    fn deferred_work_assessment(
        &self,
        graph: &CombGraph,
        roots: &[CombDefinitionId],
        definition_consumer_counts: &[u32],
        demanded_definitions: &[bool],
        div_rem_cost: u32,
    ) -> MuxAssessment {
        let mut expensive_definitions = 0u32;
        let mut cheap_work = 0u32;
        let mut arm_local_definitions = 0u32;
        let mut visited = HashSet::default();
        let mut work = roots.to_vec();
        while let Some(definition) = work.pop() {
            if !visited.insert(definition) {
                continue;
            }
            // A shared or separately demanded definition retains its global
            // placement. It is a materialization frontier, not arm-local work.
            if demanded_definitions[definition.0] || definition_consumer_counts[definition.0] != 1 {
                continue;
            }
            let recipe = graph.definitions()[definition.0].recipe;
            let local_cost = self.recipe_local_costs[recipe.0];
            expensive_definitions =
                expensive_definitions.saturating_add(u32::from(local_cost.contains_div_rem));
            cheap_work = cheap_work
                .saturating_add(local_cost.instructions)
                .min(div_rem_cost - 1);
            arm_local_definitions = arm_local_definitions.saturating_add(1);
            work.extend(self.all_dependencies(recipe).iter().copied());
        }
        MuxAssessment {
            benefit: expensive_definitions
                .saturating_mul(div_rem_cost)
                .saturating_add(cheap_work),
            estimated_growth: 8u32.saturating_add(arm_local_definitions),
        }
    }

    fn append_input_dependencies(
        &self,
        recipe: CombRecipeId,
        object: AbsoluteAddr,
        static_access: Option<BitAccess>,
        result: &mut HashSet<CombDefinitionId>,
    ) {
        if let Some(access) = static_access
            && self.recipe_inputs[recipe.0]
                .previous_snapshots_by_object
                .get(&object)
                .is_some_and(|snapshots| {
                    snapshots.iter().any(|snapshot| snapshot.overlaps(&access))
                })
        {
            return;
        }
        let Some(definitions) = self.recipe_inputs[recipe.0]
            .definitions_by_object
            .get(&object)
        else {
            return;
        };
        result.extend(
            definitions
                .iter()
                .filter(|(_, target)| static_access.is_none_or(|access| target.overlaps(&access)))
                .map(|(definition, _)| *definition),
        );
    }

    fn demand_summary(
        &self,
        recipe: CombRecipeId,
        node: NodeId,
        arena: &SLTNodeArena<AbsoluteAddr>,
    ) -> DemandSummary {
        if let Some(summary) = self.demand_cache.borrow().get(&(recipe, node)).copied() {
            return summary;
        }
        let summary = match arena.get(node) {
            SLTNode::Input {
                variable,
                index,
                access,
                ..
            } => {
                let mut summary = DemandSummary::default();
                for index in index {
                    summary.merge(self.demand_summary(recipe, index.node, arena));
                }
                let mut definitions = HashSet::default();
                self.append_input_dependencies(
                    recipe,
                    *variable,
                    index.is_empty().then_some(*access),
                    &mut definitions,
                );
                for definition in definitions {
                    summary.add_definition(definition, self.definition_costs[definition.0]);
                }
                summary
            }
            SLTNode::Constant(..) => DemandSummary::default(),
            SLTNode::Binary(lhs, _, rhs) => {
                let mut summary = self.demand_summary(recipe, *lhs, arena);
                summary.merge(self.demand_summary(recipe, *rhs, arena));
                summary
            }
            SLTNode::Unary(_, input) | SLTNode::Slice { expr: input, .. } => {
                self.demand_summary(recipe, *input, arena)
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                let mut summary = self.demand_summary(recipe, *cond, arena);
                summary.merge(self.demand_summary(recipe, *then_expr, arena));
                summary.merge(self.demand_summary(recipe, *else_expr, arena));
                summary
            }
            SLTNode::Concat(parts) => {
                let mut summary = DemandSummary::default();
                for (part, _) in parts {
                    summary.merge(self.demand_summary(recipe, *part, arena));
                }
                summary
            }
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => {
                let mut summary = DemandSummary::default();
                for &definition in self.all_dependencies(recipe) {
                    summary.add_definition(definition, self.definition_costs[definition.0]);
                }
                summary
            }
        };
        self.demand_cache
            .borrow_mut()
            .insert((recipe, node), summary);
        summary
    }
}

#[derive(Debug, Clone, Copy)]
enum CostState {
    Unseen,
    Visiting,
    Done(ValueCost),
}

#[derive(Debug)]
struct CostFrame {
    definition: CombDefinitionId,
    next_dependency: usize,
    cost: ValueCost,
}

fn resolve_definition_cost(
    root: CombDefinitionId,
    graph: &CombGraph,
    recipe_inputs: &[RecipeInputIndex],
    node_costs: &[ValueCost],
    states: &mut [CostState],
) -> Result<(), CombValueGraphError> {
    if matches!(states[root.0], CostState::Done(_)) {
        return Ok(());
    }

    let make_frame = |definition: CombDefinitionId| {
        let recipe = &graph.recipes()[graph.definitions()[definition.0].recipe.0];
        CostFrame {
            definition,
            next_dependency: 0,
            cost: node_costs[recipe.root.0],
        }
    };
    states[root.0] = CostState::Visiting;
    let mut stack = vec![make_frame(root)];
    while let Some(frame) = stack.last_mut() {
        let recipe = graph.definitions()[frame.definition.0].recipe;
        let dependencies = &recipe_inputs[recipe.0].all_dependencies;
        if let Some(&dependency) = dependencies.get(frame.next_dependency) {
            frame.next_dependency += 1;
            match states[dependency.0] {
                CostState::Done(cost) => frame.cost.add(cost),
                CostState::Visiting => {
                    return Err(CombValueGraphError::DefinitionCycle(dependency));
                }
                CostState::Unseen => {
                    states[dependency.0] = CostState::Visiting;
                    stack.push(make_frame(dependency));
                }
            }
            continue;
        }

        let completed = stack.pop().expect("cost frame exists");
        states[completed.definition.0] = CostState::Done(completed.cost);
        if let Some(parent) = stack.last_mut() {
            parent.cost.add(completed.cost);
        }
    }
    Ok(())
}

fn build_node_costs(arena: &SLTNodeArena<AbsoluteAddr>) -> Vec<ValueCost> {
    let mut result = Vec::with_capacity(arena.len());
    for index in 0..arena.len() {
        let node = NodeId(index);
        let child_cost = |node: NodeId| result[node.0];
        let mut cost = match arena.get(node) {
            SLTNode::Input { index, .. } => {
                ValueCost::node(index.iter().map(|index| child_cost(index.node)))
            }
            SLTNode::Constant(..) => ValueCost::node([]),
            SLTNode::Binary(lhs, _, rhs) => ValueCost::node([child_cost(*lhs), child_cost(*rhs)]),
            SLTNode::Unary(_, input) | SLTNode::Slice { expr: input, .. } => {
                ValueCost::node([child_cost(*input)])
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => ValueCost::node([
                child_cost(*cond),
                child_cost(*then_expr),
                child_cost(*else_expr),
            ]),
            SLTNode::Concat(parts) => {
                ValueCost::node(parts.iter().map(|(part, _)| child_cost(*part)))
            }
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => ValueCost {
                instructions: ValueCost::LIMIT,
                contains_div_rem: false,
            },
        };
        if matches!(
            arena.get(node),
            SLTNode::Binary(
                _,
                BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS,
                _
            )
        ) {
            cost.contains_div_rem = true;
        }
        result.push(cost);
    }
    result
}

fn slt_children(node: NodeId, arena: &SLTNodeArena<AbsoluteAddr>) -> Vec<NodeId> {
    match arena.get(node) {
        SLTNode::Input { index, .. } => index.iter().map(|index| index.node).collect(),
        SLTNode::Constant(..) | SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => {
            Vec::new()
        }
        SLTNode::Binary(lhs, _, rhs) => vec![*lhs, *rhs],
        SLTNode::Unary(_, input) | SLTNode::Slice { expr: input, .. } => vec![*input],
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => vec![*cond, *then_expr, *else_expr],
        SLTNode::Concat(parts) => parts.iter().map(|(part, _)| *part).collect(),
    }
}

fn find_component(parents: &mut [usize], mut node: usize) -> usize {
    let mut root = node;
    while parents[root] != root {
        root = parents[root];
    }
    while parents[node] != node {
        let parent = parents[node];
        parents[node] = root;
        node = parent;
    }
    root
}

fn union_components(parents: &mut [usize], lhs: usize, rhs: usize) {
    let lhs = find_component(parents, lhs);
    let rhs = find_component(parents, rhs);
    if lhs != rhs {
        parents[rhs] = lhs;
    }
}

fn candidate_is_better(candidate: &MuxCandidate, current: &MuxCandidate) -> bool {
    candidate_rank_cmp(candidate, current).is_gt()
}

fn candidate_rank_cmp(candidate: &MuxCandidate, current: &MuxCandidate) -> std::cmp::Ordering {
    let candidate_ratio =
        u64::from(candidate.assessment.benefit) * u64::from(current.assessment.estimated_growth);
    let current_ratio =
        u64::from(current.assessment.benefit) * u64::from(candidate.assessment.estimated_growth);
    candidate_ratio.cmp(&current_ratio).then_with(|| {
        candidate
            .assessment
            .benefit
            .cmp(&current.assessment.benefit)
    })
}
