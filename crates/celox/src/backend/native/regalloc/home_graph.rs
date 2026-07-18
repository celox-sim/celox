//! Allocation-owned live bundles and materialization homes.
//!
//! A home is valid for an explicit subset of a bundle's uses.  In particular,
//! a state home is not a VReg-wide spill attribute: each state leaf carries the
//! exact MemorySSA version observed at the use.  A later splitter can therefore
//! cut at home-validity boundaries instead of selecting stack residency first
//! and substituting reloads afterwards.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use crate::backend::native::mir::{BlockId, MFunction, VReg};

use super::cfg::NormalizedCfg;
use super::live_interval::{
    DefinitionSite, LiveIntervalError, LiveIntervals, LiveSegment, UseSite,
};
use super::reload::{
    CompositeStateRecipe, EdgeUse, PointUse, PureStep, ReloadRecipeAnalysis, ReloadRecipeError,
    ResolvedBase, ResolvedRecipe, StateFragmentRecipe, StateRecipe,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct LiveBundleId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct BundleUseId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BundleUse {
    pub id: BundleUseId,
    pub site: UseSite,
}

/// Initially one bundle covers one exact MIR live interval.  Splitting creates
/// children with the same `origin` and disjoint segment/use subsets; it never
/// changes the machine value's 32/64-bit semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LiveBundle {
    pub id: LiveBundleId,
    pub origin: VReg,
    pub definition: DefinitionSite,
    pub parent: Option<LiveBundleId>,
    pub segments: Vec<LiveSegment>,
    pub uses: Vec<BundleUse>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct RecipeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct RecipeShapeId(pub u32);

/// Closed target-level recipe DAG.  Nodes are interned and topologically
/// ordered, so shared subexpressions are represented once and cycles are
/// structurally impossible.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum RecipeNode {
    Constant(u64),
    State(StateRecipe),
    Unary {
        operation: PureStep,
        input: RecipeId,
    },
    Or64 {
        left: RecipeId,
        right: RecipeId,
    },
}

/// Memory-version-independent identity of a materialization home.  Exact
/// MemorySSA versions stay in `RecipeNode` and are selected independently for
/// every use.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum RecipeShapeNode {
    Constant(u64),
    State {
        load: super::reload::StateLoad,
        observed_start: i64,
        observed_end: i64,
    },
    Unary {
        operation: PureStep,
        input: RecipeShapeId,
    },
    Or64 {
        left: RecipeShapeId,
        right: RecipeShapeId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum HomeKind {
    /// A physical GPR selected by interval-union allocation.
    Register,
    /// A stack slot selected and later colored from spilled-range interference.
    Stack,
    /// A pure constant/unary target DAG.
    Rematerialize(RecipeShapeId),
    /// One or more physical state loads, independent of their use-local
    /// MemorySSA versions.
    State(RecipeShapeId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct UseMaterialization {
    pub use_id: BundleUseId,
    /// Exact, versioned recipe proved at this use.
    pub recipe: RecipeId,
    pub cost: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HomeCandidate {
    pub kind: HomeKind,
    /// Exact bundle uses at which this home is valid.
    pub uses: Vec<BundleUseId>,
    /// Use-local exact recipes.  Register and stack candidates have none.
    pub materializations: Vec<UseMaterialization>,
    /// Cost paid once when entering this residency (for example a stack store).
    pub creation_cost: u32,
    /// Sum of materializations at the covered uses.  Transition and frequency
    /// costs are added by the allocator, not hidden in this structural graph.
    pub materialization_cost: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HomeGraph {
    pub intervals: LiveIntervals,
    pub bundles: Vec<LiveBundle>,
    pub recipe_nodes: Vec<RecipeNode>,
    pub recipe_shape_nodes: Vec<RecipeShapeNode>,
    /// Shape identity corresponding to every exact recipe node.
    pub recipe_shapes: Vec<RecipeShapeId>,
    pub candidates: Vec<Vec<HomeCandidate>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HomeGraphError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub values: Vec<VReg>,
    pub message: String,
}

impl HomeGraphError {
    fn new(
        rule: &'static str,
        block: Option<BlockId>,
        instruction: Option<usize>,
        values: Vec<VReg>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            block,
            instruction,
            values,
            message: message.into(),
        }
    }

    fn live(error: LiveIntervalError) -> Self {
        Self::new(
            error.rule,
            error.block,
            error.instruction,
            error.values,
            error.message,
        )
    }

    fn reload(error: ReloadRecipeError) -> Self {
        Self::new(
            error.rule,
            error.block,
            error.instruction,
            error.value.into_iter().collect(),
            error.message,
        )
    }
}

impl fmt::Display for HomeGraphError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.rule)?;
        if let Some(block) = self.block {
            write!(formatter, " at {block}")?;
        }
        if let Some(instruction) = self.instruction {
            write!(formatter, "/i{instruction}")?;
        }
        if !self.values.is_empty() {
            write!(formatter, " values={:?}", self.values)?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for HomeGraphError {}

#[derive(Default)]
struct RecipeInterner {
    nodes: Vec<RecipeNode>,
    ids: HashMap<RecipeNode, RecipeId>,
    shape_nodes: Vec<RecipeShapeNode>,
    shape_ids: HashMap<RecipeShapeNode, RecipeShapeId>,
    shapes: Vec<RecipeShapeId>,
}

impl RecipeInterner {
    fn intern_shape(&mut self, node: RecipeShapeNode) -> Result<RecipeShapeId, HomeGraphError> {
        if let Some(&id) = self.shape_ids.get(&node) {
            return Ok(id);
        }
        let id = RecipeShapeId(u32::try_from(self.shape_nodes.len()).map_err(|_| {
            HomeGraphError::new(
                "HOME_GRAPH.RECIPE_SHAPE_ID_RANGE",
                None,
                None,
                Vec::new(),
                "recipe-shape DAG contains more than u32::MAX nodes",
            )
        })?);
        self.shape_nodes.push(node.clone());
        self.shape_ids.insert(node, id);
        Ok(id)
    }

    fn shape_node(&self, node: &RecipeNode) -> Result<RecipeShapeNode, HomeGraphError> {
        let input_shape = |input: RecipeId| {
            self.shapes.get(input.0 as usize).copied().ok_or_else(|| {
                HomeGraphError::new(
                    "HOME_GRAPH.RECIPE_TOPOLOGY",
                    None,
                    None,
                    Vec::new(),
                    format!("recipe input {input:?} has no preceding shape"),
                )
            })
        };
        Ok(match node {
            RecipeNode::Constant(value) => RecipeShapeNode::Constant(*value),
            RecipeNode::State(state) => {
                let (load, observed_start, observed_end) = state.home_shape_key();
                RecipeShapeNode::State {
                    load,
                    observed_start,
                    observed_end,
                }
            }
            RecipeNode::Unary { operation, input } => RecipeShapeNode::Unary {
                operation: *operation,
                input: input_shape(*input)?,
            },
            RecipeNode::Or64 { left, right } => RecipeShapeNode::Or64 {
                left: input_shape(*left)?,
                right: input_shape(*right)?,
            },
        })
    }

    fn intern(&mut self, node: RecipeNode) -> Result<RecipeId, HomeGraphError> {
        if let Some(&id) = self.ids.get(&node) {
            return Ok(id);
        }
        let shape_node = self.shape_node(&node)?;
        let shape = self.intern_shape(shape_node)?;
        let id = RecipeId(u32::try_from(self.nodes.len()).map_err(|_| {
            HomeGraphError::new(
                "HOME_GRAPH.RECIPE_ID_RANGE",
                None,
                None,
                Vec::new(),
                "recipe DAG contains more than u32::MAX nodes",
            )
        })?);
        self.nodes.push(node.clone());
        self.shapes.push(shape);
        self.ids.insert(node, id);
        Ok(id)
    }

    fn shape(&self, recipe: RecipeId) -> Result<RecipeShapeId, HomeGraphError> {
        self.shapes.get(recipe.0 as usize).copied().ok_or_else(|| {
            HomeGraphError::new(
                "HOME_GRAPH.RECIPE_NODE_RANGE",
                None,
                None,
                Vec::new(),
                format!("recipe root {recipe:?} has no shape"),
            )
        })
    }

    fn unary(&mut self, operation: PureStep, input: RecipeId) -> Result<RecipeId, HomeGraphError> {
        self.intern(RecipeNode::Unary { operation, input })
    }

    fn linear(&mut self, recipe: &ResolvedRecipe) -> Result<(RecipeId, bool), HomeGraphError> {
        let (mut root, state) = match &recipe.base {
            ResolvedBase::Constant(value) => (self.intern(RecipeNode::Constant(*value))?, false),
            ResolvedBase::State(state) => (self.intern(RecipeNode::State(state.clone()))?, true),
        };
        for &operation in &recipe.steps {
            root = self.unary(operation, root)?;
        }
        Ok((root, state))
    }

    fn fragment(&mut self, fragment: &StateFragmentRecipe) -> Result<RecipeId, HomeGraphError> {
        let value_end = fragment
            .value_bit_offset
            .checked_add(fragment.width_bits)
            .ok_or_else(|| {
                HomeGraphError::new(
                    "HOME_GRAPH.FRAGMENT_RANGE",
                    None,
                    None,
                    Vec::new(),
                    "fragment source range overflows usize",
                )
            })?;
        let state_end = fragment
            .state_bit_offset
            .checked_add(fragment.width_bits)
            .ok_or_else(|| {
                HomeGraphError::new(
                    "HOME_GRAPH.FRAGMENT_RANGE",
                    None,
                    None,
                    Vec::new(),
                    "fragment physical range overflows usize",
                )
            })?;
        let physical_bits = fragment.state.load.size.bytes() as usize * 8;
        if fragment.width_bits == 0 || value_end > 64 || state_end > physical_bits {
            return Err(HomeGraphError::new(
                "HOME_GRAPH.FRAGMENT_RANGE",
                None,
                None,
                Vec::new(),
                format!("invalid fragment {fragment:?}"),
            ));
        }

        let mut root = self.intern(RecipeNode::State(fragment.state.clone()))?;
        if fragment.state_bit_offset != 0 {
            root = self.unary(
                PureStep::ShrImm64 {
                    immediate: u8::try_from(fragment.state_bit_offset).map_err(|_| {
                        HomeGraphError::new(
                            "HOME_GRAPH.FRAGMENT_RANGE",
                            None,
                            None,
                            Vec::new(),
                            "fragment shift does not fit the machine immediate",
                        )
                    })?,
                },
                root,
            )?;
        }
        if fragment.width_bits < 64 {
            if fragment.width_bits <= 32 {
                root = self.unary(
                    PureStep::AndImm32 {
                        immediate: u32::MAX >> (32 - fragment.width_bits),
                    },
                    root,
                )?;
            } else {
                let clear = u8::try_from(64 - fragment.width_bits)
                    .expect("a validated 33..63-bit fragment has a u8 shift");
                root = self.unary(PureStep::ShlImm64 { immediate: clear }, root)?;
                root = self.unary(PureStep::ShrImm64 { immediate: clear }, root)?;
            }
        }
        if fragment.value_bit_offset != 0 {
            root = self.unary(
                PureStep::ShlImm64 {
                    immediate: u8::try_from(fragment.value_bit_offset).map_err(|_| {
                        HomeGraphError::new(
                            "HOME_GRAPH.FRAGMENT_RANGE",
                            None,
                            None,
                            Vec::new(),
                            "fragment placement does not fit the machine immediate",
                        )
                    })?,
                },
                root,
            )?;
        }
        Ok(root)
    }

    fn composite(&mut self, recipe: &CompositeStateRecipe) -> Result<RecipeId, HomeGraphError> {
        let mut fragments = recipe.fragments.iter();
        let Some(first) = fragments.next() else {
            return Err(HomeGraphError::new(
                "HOME_GRAPH.EMPTY_STATE_RECIPE",
                None,
                None,
                Vec::new(),
                "composite state recipe has no physical fragments",
            ));
        };
        let mut root = self.fragment(first)?;
        for fragment in fragments {
            let right = self.fragment(fragment)?;
            root = self.intern(RecipeNode::Or64 { left: root, right })?;
        }
        Ok(root)
    }
}

pub(super) fn build(func: &MFunction, cfg: &NormalizedCfg) -> Result<HomeGraph, HomeGraphError> {
    let graph = build_unverified(func, cfg)?;
    graph.verify(func, cfg)?;
    Ok(graph)
}

fn build_unverified(func: &MFunction, cfg: &NormalizedCfg) -> Result<HomeGraph, HomeGraphError> {
    let intervals = super::live_interval::analyze(func, cfg).map_err(HomeGraphError::live)?;
    let reloads =
        super::reload::analyze_for_home_graph(func, cfg).map_err(HomeGraphError::reload)?;
    let bundles = root_bundles(&intervals)?;
    let mut recipes = RecipeInterner::default();
    let mut candidates = Vec::with_capacity(bundles.len());
    for bundle in &bundles {
        candidates.push(bundle_candidates(bundle, &reloads, &mut recipes)?);
    }
    Ok(HomeGraph {
        intervals,
        bundles,
        recipe_nodes: recipes.nodes,
        recipe_shape_nodes: recipes.shape_nodes,
        recipe_shapes: recipes.shapes,
        candidates,
    })
}

fn root_bundles(intervals: &LiveIntervals) -> Result<Vec<LiveBundle>, HomeGraphError> {
    let mut bundles = Vec::new();
    for interval in intervals.intervals.iter().flatten() {
        let id = LiveBundleId(u32::try_from(bundles.len()).map_err(|_| {
            HomeGraphError::new(
                "HOME_GRAPH.BUNDLE_ID_RANGE",
                Some(interval.definition.block()),
                None,
                vec![interval.value],
                "root bundle count exceeds u32",
            )
        })?);
        let uses = interval
            .uses
            .iter()
            .copied()
            .enumerate()
            .map(|(index, site)| {
                Ok(BundleUse {
                    id: BundleUseId(u32::try_from(index).map_err(|_| {
                        HomeGraphError::new(
                            "HOME_GRAPH.USE_ID_RANGE",
                            Some(site.block()),
                            None,
                            vec![interval.value],
                            "bundle use count exceeds u32",
                        )
                    })?),
                    site,
                })
            })
            .collect::<Result<Vec<_>, HomeGraphError>>()?;
        bundles.push(LiveBundle {
            id,
            origin: interval.value,
            definition: interval.definition,
            parent: None,
            segments: interval.segments.clone(),
            uses,
        });
    }
    Ok(bundles)
}

fn use_point(value: VReg, site: UseSite) -> Result<UsePoint, HomeGraphError> {
    match site {
        UseSite::Instruction {
            block, instruction, ..
        } => Ok(UsePoint::Instruction(PointUse {
            block,
            instruction,
            value,
        })),
        UseSite::PhiEdge {
            predecessor,
            successor,
            ..
        } => Ok(UsePoint::Edge(EdgeUse {
            predecessor,
            successor,
            value,
        })),
    }
}

#[derive(Debug, Clone, Copy)]
enum UsePoint {
    Instruction(PointUse),
    Edge(EdgeUse),
}

fn ordinary_recipe(reloads: &ReloadRecipeAnalysis, point: UsePoint) -> Option<&ResolvedRecipe> {
    match point {
        UsePoint::Instruction(point) => reloads.resolved_recipe_at_point(point),
        UsePoint::Edge(edge) => reloads.resolved_recipe_on_edge(edge),
    }
}

fn fragment_recipe(
    reloads: &ReloadRecipeAnalysis,
    point: UsePoint,
) -> Option<&CompositeStateRecipe> {
    match point {
        UsePoint::Instruction(point) => reloads.fragment_recipe_at_point(point),
        UsePoint::Edge(edge) => reloads.fragment_recipe_on_edge(edge),
    }
}

fn bundle_candidates(
    bundle: &LiveBundle,
    reloads: &ReloadRecipeAnalysis,
    recipes: &mut RecipeInterner,
) -> Result<Vec<HomeCandidate>, HomeGraphError> {
    let all_uses = bundle.uses.iter().map(|use_| use_.id).collect::<Vec<_>>();
    let mut by_home = BTreeMap::<HomeKind, BTreeMap<BundleUseId, RecipeId>>::new();

    for use_ in &bundle.uses {
        let point = use_point(bundle.origin, use_.site)?;
        if let Some(recipe) = ordinary_recipe(reloads, point) {
            let (root, state) = recipes.linear(recipe)?;
            let shape = recipes.shape(root)?;
            let kind = if state {
                HomeKind::State(shape)
            } else {
                HomeKind::Rematerialize(shape)
            };
            insert_materialization(&mut by_home, kind, use_.id, root, &recipes.nodes)?;
        }
        if let Some(recipe) = fragment_recipe(reloads, point) {
            let root = recipes.composite(recipe)?;
            let kind = HomeKind::State(recipes.shape(root)?);
            insert_materialization(&mut by_home, kind, use_.id, root, &recipes.nodes)?;
        }
    }

    let mut candidates = vec![
        HomeCandidate {
            kind: HomeKind::Register,
            uses: all_uses.clone(),
            materializations: Vec::new(),
            creation_cost: 0,
            materialization_cost: 0,
        },
        HomeCandidate {
            kind: HomeKind::Stack,
            materialization_cost: u32::try_from(all_uses.len()).unwrap_or(u32::MAX),
            uses: all_uses,
            materializations: Vec::new(),
            creation_cost: 1,
        },
    ];
    for (kind, per_use) in by_home {
        let uses = per_use.keys().copied().collect::<Vec<_>>();
        let materializations = per_use
            .into_iter()
            .map(|(use_id, recipe)| {
                Ok(UseMaterialization {
                    use_id,
                    recipe,
                    cost: recipe_cost(&recipes.nodes, recipe)?,
                })
            })
            .collect::<Result<Vec<_>, HomeGraphError>>()?;
        let materialization_cost = materializations
            .iter()
            .fold(0_u32, |cost, item| cost.saturating_add(item.cost));
        candidates.push(HomeCandidate {
            kind,
            uses,
            materializations,
            creation_cost: 0,
            materialization_cost,
        });
    }
    Ok(candidates)
}

fn insert_materialization(
    homes: &mut BTreeMap<HomeKind, BTreeMap<BundleUseId, RecipeId>>,
    kind: HomeKind,
    use_id: BundleUseId,
    recipe: RecipeId,
    nodes: &[RecipeNode],
) -> Result<(), HomeGraphError> {
    let per_use = homes.entry(kind).or_default();
    let Some(&current) = per_use.get(&use_id) else {
        per_use.insert(use_id, recipe);
        return Ok(());
    };
    let current_key = (recipe_cost(nodes, current)?, current);
    let replacement_key = (recipe_cost(nodes, recipe)?, recipe);
    if replacement_key < current_key {
        per_use.insert(use_id, recipe);
    }
    Ok(())
}

fn recipe_cost(nodes: &[RecipeNode], root: RecipeId) -> Result<u32, HomeGraphError> {
    let mut seen = BTreeSet::<RecipeId>::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(node) = nodes.get(id.0 as usize) else {
            return Err(HomeGraphError::new(
                "HOME_GRAPH.RECIPE_NODE_RANGE",
                None,
                None,
                Vec::new(),
                format!("recipe root {id:?} references a missing node"),
            ));
        };
        match node {
            RecipeNode::Constant(_) | RecipeNode::State(_) => {}
            RecipeNode::Unary { input, .. } => stack.push(*input),
            RecipeNode::Or64 { left, right } => {
                stack.push(*left);
                stack.push(*right);
            }
        }
    }
    Ok(u32::try_from(seen.len()).unwrap_or(u32::MAX))
}

fn recipe_contains_state(nodes: &[RecipeNode], root: RecipeId) -> Result<bool, HomeGraphError> {
    let mut seen = BTreeSet::<RecipeId>::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(node) = nodes.get(id.0 as usize) else {
            return Err(HomeGraphError::new(
                "HOME_GRAPH.RECIPE_NODE_RANGE",
                None,
                None,
                Vec::new(),
                format!("recipe root {id:?} references a missing node"),
            ));
        };
        match node {
            RecipeNode::Constant(_) => {}
            RecipeNode::State(_) => return Ok(true),
            RecipeNode::Unary { input, .. } => stack.push(*input),
            RecipeNode::Or64 { left, right } => {
                stack.push(*left);
                stack.push(*right);
            }
        }
    }
    Ok(false)
}

impl HomeGraph {
    pub(super) fn verify(
        &self,
        func: &MFunction,
        cfg: &NormalizedCfg,
    ) -> Result<(), HomeGraphError> {
        self.intervals
            .verify(func, cfg)
            .map_err(HomeGraphError::live)?;
        self.verify_structure()?;
        let rebuilt = build_unverified(func, cfg)?;
        if self != &rebuilt {
            return Err(HomeGraphError::new(
                "HOME_GRAPH.MATCHES_MIR",
                None,
                None,
                Vec::new(),
                "cached bundles or homes differ from independently rebuilt MIR and MemorySSA",
            ));
        }
        Ok(())
    }

    fn candidate_error(
        bundle: &LiveBundle,
        rule: &'static str,
        message: impl Into<String>,
    ) -> HomeGraphError {
        HomeGraphError::new(
            rule,
            Some(bundle.definition.block()),
            None,
            vec![bundle.origin],
            message,
        )
    }

    fn verify_materializations(
        &self,
        bundle: &LiveBundle,
        candidate: &HomeCandidate,
        shape: RecipeShapeId,
    ) -> Result<(), HomeGraphError> {
        if self.recipe_shape_nodes.get(shape.0 as usize).is_none() {
            return Err(Self::candidate_error(
                bundle,
                "HOME_GRAPH.RECIPE_SHAPE_RANGE",
                format!("candidate references missing recipe shape {shape:?}"),
            ));
        }
        if candidate.materializations.is_empty()
            || candidate.creation_cost != 0
            || candidate
                .materializations
                .windows(2)
                .any(|pair| pair[0].use_id >= pair[1].use_id)
            || !candidate
                .materializations
                .iter()
                .map(|item| item.use_id)
                .eq(candidate.uses.iter().copied())
        {
            return Err(Self::candidate_error(
                bundle,
                "HOME_GRAPH.USE_MATERIALIZATION_SET",
                "candidate lacks one sorted exact recipe for each covered use",
            ));
        }

        let expects_state = matches!(candidate.kind, HomeKind::State(_));
        let mut expected_cost = 0_u32;
        for item in &candidate.materializations {
            let Some(&actual_shape) = self.recipe_shapes.get(item.recipe.0 as usize) else {
                return Err(Self::candidate_error(
                    bundle,
                    "HOME_GRAPH.RECIPE_NODE_RANGE",
                    format!(
                        "materialization references missing recipe {:?}",
                        item.recipe
                    ),
                ));
            };
            if actual_shape != shape {
                return Err(Self::candidate_error(
                    bundle,
                    "HOME_GRAPH.MATERIALIZATION_SHAPE",
                    format!(
                        "use {:?} has exact recipe shape {actual_shape:?}, expected {shape:?}",
                        item.use_id
                    ),
                ));
            }
            if recipe_contains_state(&self.recipe_nodes, item.recipe)? != expects_state {
                return Err(Self::candidate_error(
                    bundle,
                    "HOME_GRAPH.MATERIALIZATION_CLASS",
                    "state and pure-rematerialization candidate classes are inconsistent",
                ));
            }
            let actual_cost = recipe_cost(&self.recipe_nodes, item.recipe)?;
            if item.cost != actual_cost {
                return Err(Self::candidate_error(
                    bundle,
                    "HOME_GRAPH.MATERIALIZATION_COST",
                    "use-local materialization cost differs from its exact recipe DAG",
                ));
            }
            expected_cost = expected_cost.saturating_add(item.cost);
        }
        if candidate.materialization_cost != expected_cost {
            return Err(Self::candidate_error(
                bundle,
                "HOME_GRAPH.CANDIDATE_COST",
                "candidate materialization cost differs from its use-local recipes",
            ));
        }
        Ok(())
    }

    fn verify_structure(&self) -> Result<(), HomeGraphError> {
        if self.candidates.len() != self.bundles.len() {
            return Err(HomeGraphError::new(
                "HOME_GRAPH.BUNDLE_COVERAGE",
                None,
                None,
                Vec::new(),
                "home-candidate table does not cover every bundle",
            ));
        }
        if self.recipe_shapes.len() != self.recipe_nodes.len() {
            return Err(HomeGraphError::new(
                "HOME_GRAPH.RECIPE_SHAPE_COVERAGE",
                None,
                None,
                Vec::new(),
                "exact recipe nodes and their shape identities differ in length",
            ));
        }
        for (index, node) in self.recipe_shape_nodes.iter().enumerate() {
            let check_input = |input: RecipeShapeId| {
                if input.0 as usize >= index {
                    Err(HomeGraphError::new(
                        "HOME_GRAPH.RECIPE_SHAPE_TOPOLOGY",
                        None,
                        None,
                        Vec::new(),
                        format!("recipe-shape node {index} has non-preceding input {input:?}"),
                    ))
                } else {
                    Ok(())
                }
            };
            match node {
                RecipeShapeNode::Constant(_) | RecipeShapeNode::State { .. } => {}
                RecipeShapeNode::Unary { input, .. } => check_input(*input)?,
                RecipeShapeNode::Or64 { left, right } => {
                    check_input(*left)?;
                    check_input(*right)?;
                }
            }
        }
        for (index, node) in self.recipe_nodes.iter().enumerate() {
            let check_input = |input: RecipeId| {
                if input.0 as usize >= index {
                    Err(HomeGraphError::new(
                        "HOME_GRAPH.RECIPE_TOPOLOGY",
                        None,
                        None,
                        Vec::new(),
                        format!("recipe node {index} has non-preceding input {input:?}"),
                    ))
                } else {
                    Ok(())
                }
            };
            match node {
                RecipeNode::Constant(_) | RecipeNode::State(_) => {}
                RecipeNode::Unary { input, .. } => check_input(*input)?,
                RecipeNode::Or64 { left, right } => {
                    check_input(*left)?;
                    check_input(*right)?;
                }
            }
            let child_shape = |input: RecipeId| self.recipe_shapes[input.0 as usize];
            let expected = match node {
                RecipeNode::Constant(value) => RecipeShapeNode::Constant(*value),
                RecipeNode::State(state) => {
                    let (load, observed_start, observed_end) = state.home_shape_key();
                    RecipeShapeNode::State {
                        load,
                        observed_start,
                        observed_end,
                    }
                }
                RecipeNode::Unary { operation, input } => RecipeShapeNode::Unary {
                    operation: *operation,
                    input: child_shape(*input),
                },
                RecipeNode::Or64 { left, right } => RecipeShapeNode::Or64 {
                    left: child_shape(*left),
                    right: child_shape(*right),
                },
            };
            let shape = self.recipe_shapes[index];
            if self.recipe_shape_nodes.get(shape.0 as usize) != Some(&expected) {
                return Err(HomeGraphError::new(
                    "HOME_GRAPH.RECIPE_SHAPE_MATCH",
                    None,
                    None,
                    Vec::new(),
                    format!("exact recipe node {index} has an inconsistent home shape"),
                ));
            }
        }
        for (bundle_index, bundle) in self.bundles.iter().enumerate() {
            if bundle.id.0 as usize != bundle_index {
                return Err(HomeGraphError::new(
                    "HOME_GRAPH.BUNDLE_IDENTITY",
                    Some(bundle.definition.block()),
                    None,
                    vec![bundle.origin],
                    "bundle identity differs from its stable table index",
                ));
            }
            let all = bundle.uses.iter().map(|use_| use_.id).collect::<Vec<_>>();
            let mut has_register = false;
            let mut has_stack = false;
            let mut kinds = BTreeSet::new();
            for candidate in &self.candidates[bundle_index] {
                if !kinds.insert(candidate.kind) {
                    return Err(HomeGraphError::new(
                        "HOME_GRAPH.CANDIDATE_IDENTITY",
                        Some(bundle.definition.block()),
                        None,
                        vec![bundle.origin],
                        "bundle has duplicate candidates for one home",
                    ));
                }
                if candidate.uses.windows(2).any(|pair| pair[0] >= pair[1])
                    || candidate
                        .uses
                        .iter()
                        .any(|use_| use_.0 as usize >= bundle.uses.len())
                {
                    return Err(HomeGraphError::new(
                        "HOME_GRAPH.CANDIDATE_USE_SET",
                        Some(bundle.definition.block()),
                        None,
                        vec![bundle.origin],
                        "candidate use set is unsorted, duplicated, or outside the bundle",
                    ));
                }
                match candidate.kind {
                    HomeKind::Register => {
                        has_register = candidate.uses == all;
                        if !candidate.materializations.is_empty()
                            || candidate.creation_cost != 0
                            || candidate.materialization_cost != 0
                        {
                            return Err(Self::candidate_error(
                                bundle,
                                "HOME_GRAPH.CANDIDATE_COST",
                                "register home has materializations or storage costs",
                            ));
                        }
                    }
                    HomeKind::Stack => {
                        has_stack = candidate.uses == all;
                        let expected = u32::try_from(candidate.uses.len()).unwrap_or(u32::MAX);
                        if !candidate.materializations.is_empty()
                            || candidate.creation_cost != 1
                            || candidate.materialization_cost != expected
                        {
                            return Err(Self::candidate_error(
                                bundle,
                                "HOME_GRAPH.CANDIDATE_COST",
                                "stack home has inconsistent creation or reload costs",
                            ));
                        }
                    }
                    HomeKind::Rematerialize(shape) | HomeKind::State(shape) => {
                        self.verify_materializations(bundle, candidate, shape)?;
                    }
                }
            }
            if !has_register || !has_stack {
                return Err(HomeGraphError::new(
                    "HOME_GRAPH.MANDATORY_HOMES",
                    Some(bundle.definition.block()),
                    None,
                    vec![bundle.origin],
                    "bundle lacks complete register or stack candidates",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{BaseReg, MBlock, MInst, OpSize, SpillDesc, VRegAllocator};

    fn function(value_count: u32, descriptors: Vec<SpillDesc>, insts: Vec<MInst>) -> MFunction {
        let mut values = VRegAllocator::new();
        for _ in 0..value_count {
            values.alloc();
        }
        let mut function = MFunction::new(values, descriptors);
        let mut block = MBlock::new(BlockId(0));
        block.insts = insts;
        function.blocks.push(block);
        function
    }

    fn normalize(function: &mut MFunction) -> NormalizedCfg {
        super::super::cfg::normalize(function).unwrap()
    }

    fn candidate(
        graph: &HomeGraph,
        value: VReg,
        predicate: impl Fn(HomeKind) -> bool,
    ) -> &HomeCandidate {
        let bundle = graph
            .bundles
            .iter()
            .position(|bundle| bundle.origin == value)
            .unwrap();
        graph.candidates[bundle]
            .iter()
            .find(|candidate| predicate(candidate.kind))
            .unwrap()
    }

    #[test]
    fn two_physical_fragments_form_one_state_home_at_the_use() {
        let descriptors = vec![
            SpillDesc::transient(),
            SpillDesc::transient(),
            SpillDesc::transient().with_state_insert_fragment(VReg(0), 0, 4, 60),
            SpillDesc::transient(),
            SpillDesc::transient(),
            SpillDesc::transient().with_state_insert_fragment(VReg(0), 60, 0, 4),
        ];
        let insts = vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 32,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 64,
                size: OpSize::S64,
            },
            MInst::Or {
                dst: VReg(2),
                lhs: VReg(1),
                rhs: VReg(0),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 64,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(3),
                base: BaseReg::SimState,
                offset: 72,
                size: OpSize::S8,
            },
            MInst::ShrImm {
                dst: VReg(4),
                src: VReg(0),
                imm: 60,
            },
            MInst::Or {
                dst: VReg(5),
                lhs: VReg(3),
                rhs: VReg(4),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 72,
                src: VReg(5),
                size: OpSize::S8,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 128,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut function = function(6, descriptors, insts);
        let cfg = normalize(&mut function);
        let graph = build(&function, &cfg).unwrap();
        let state = candidate(&graph, VReg(0), |kind| {
            let HomeKind::State(shape) = kind else {
                return false;
            };
            matches!(
                graph.recipe_shape_nodes[shape.0 as usize],
                RecipeShapeNode::Or64 { .. }
            )
        });
        assert_eq!(state.uses, vec![BundleUseId(2)]);
        let HomeKind::State(shape) = state.kind else {
            unreachable!();
        };
        assert!(matches!(
            graph.recipe_shape_nodes[shape.0 as usize],
            RecipeShapeNode::Or64 { .. }
        ));
    }

    #[test]
    fn overlapping_write_removes_fragment_home_from_later_use() {
        let descriptors = vec![
            SpillDesc::transient(),
            SpillDesc::transient(),
            SpillDesc::transient().with_state_insert_fragment(VReg(0), 0, 0, 32),
            SpillDesc::transient(),
        ];
        let insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: u64::from(u32::MAX),
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 0,
            },
            MInst::Or {
                dst: VReg(2),
                lhs: VReg(1),
                rhs: VReg(0),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 80,
                src: VReg(2),
                size: OpSize::S32,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 80,
                src: VReg(1),
                size: OpSize::S32,
            },
            MInst::Mov {
                dst: VReg(3),
                src: VReg(0),
            },
            MInst::Return,
        ];
        let mut function = function(4, descriptors, insts);
        let cfg = normalize(&mut function);
        let graph = build(&function, &cfg).unwrap();
        let bundle = graph
            .bundles
            .iter()
            .position(|bundle| bundle.origin == VReg(0))
            .unwrap();
        assert!(
            graph.candidates[bundle]
                .iter()
                .all(|candidate| !matches!(candidate.kind, HomeKind::State(_)))
        );
    }

    #[test]
    fn a_hole_in_source_bit_coverage_rejects_the_state_home() {
        let descriptors = vec![
            SpillDesc::transient(),
            SpillDesc::transient().with_state_insert_fragment(VReg(0), 0, 0, 31),
            SpillDesc::transient().with_state_insert_fragment(VReg(0), 32, 0, 32),
            SpillDesc::transient(),
        ];
        let insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: u64::MAX,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 0,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 80,
                src: VReg(1),
                size: OpSize::S32,
            },
            MInst::LoadImm {
                dst: VReg(2),
                value: 0,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 88,
                src: VReg(2),
                size: OpSize::S32,
            },
            MInst::Mov {
                dst: VReg(3),
                src: VReg(0),
            },
            MInst::Return,
        ];
        let mut function = function(4, descriptors, insts);
        let cfg = normalize(&mut function);
        let graph = build(&function, &cfg).unwrap();
        let bundle = graph
            .bundles
            .iter()
            .position(|bundle| bundle.origin == VReg(0))
            .unwrap();
        assert!(
            graph.candidates[bundle]
                .iter()
                .all(|candidate| !matches!(candidate.kind, HomeKind::State(_)))
        );
    }

    #[test]
    fn disjoint_rmw_version_change_preserves_the_fragment_home() {
        let descriptors = vec![
            SpillDesc::transient(),
            SpillDesc::transient().with_state_insert_fragment(VReg(0), 0, 0, 32),
            SpillDesc::transient(),
            SpillDesc::transient().with_state_insert_fragment(VReg(2), 0, 32, 32),
            SpillDesc::transient(),
            SpillDesc::transient(),
        ];
        let insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: u64::from(u32::MAX),
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 0,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 80,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Mov {
                dst: VReg(4),
                src: VReg(0),
            },
            MInst::LoadImm {
                dst: VReg(2),
                value: 7,
            },
            MInst::LoadImm {
                dst: VReg(3),
                value: 0,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 80,
                src: VReg(3),
                size: OpSize::S64,
            },
            MInst::Mov {
                dst: VReg(5),
                src: VReg(0),
            },
            MInst::Return,
        ];
        let mut function = function(6, descriptors, insts);
        let cfg = normalize(&mut function);
        let graph = build(&function, &cfg).unwrap();
        let state = candidate(&graph, VReg(0), |kind| matches!(kind, HomeKind::State(_)));
        assert_eq!(state.uses, vec![BundleUseId(0), BundleUseId(1)]);
        assert_eq!(state.materializations.len(), 2);
        assert_ne!(
            state.materializations[0].recipe, state.materializations[1].recipe,
            "MemorySSA versions must remain use-local even when the physical home is shared"
        );
        let HomeKind::State(shape) = state.kind else {
            unreachable!();
        };
        assert!(state.materializations.iter().all(|materialization| {
            graph.recipe_shapes[materialization.recipe.0 as usize] == shape
        }));
    }

    #[test]
    fn fragment_home_validity_is_specific_to_a_phi_edge() {
        let descriptors = vec![
            SpillDesc::transient(),
            SpillDesc::transient(),
            SpillDesc::transient().with_state_insert_fragment(VReg(0), 0, 0, 16),
            SpillDesc::transient().with_state_insert_fragment(VReg(0), 16, 0, 16),
            SpillDesc::transient(),
            SpillDesc::transient(),
        ];
        let mut values = VRegAllocator::new();
        for _ in 0..descriptors.len() {
            values.alloc();
        }
        let mut function = MFunction::new(values, descriptors);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: u64::from(u32::MAX),
        });
        entry.push(MInst::LoadImm {
            dst: VReg(1),
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: VReg(1),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::LoadImm {
            dst: VReg(2),
            value: 0,
        });
        left.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 80,
            src: VReg(2),
            size: OpSize::S16,
        });
        left.push(MInst::LoadImm {
            dst: VReg(3),
            value: 0,
        });
        left.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 82,
            src: VReg(3),
            size: OpSize::S16,
        });
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Jump { target: BlockId(3) });
        let mut merge = MBlock::new(BlockId(3));
        merge.phis.push(crate::backend::native::mir::PhiNode {
            dst: VReg(4),
            sources: vec![(BlockId(1), VReg(0)), (BlockId(2), VReg(0))],
        });
        merge.push(MInst::Mov {
            dst: VReg(5),
            src: VReg(4),
        });
        merge.push(MInst::Return);
        function.blocks = vec![entry, left, right, merge];
        let cfg = normalize(&mut function);
        let graph = build(&function, &cfg).unwrap();
        let state = candidate(&graph, VReg(0), |kind| {
            let HomeKind::State(shape) = kind else {
                return false;
            };
            matches!(
                graph.recipe_shape_nodes[shape.0 as usize],
                RecipeShapeNode::Or64 { .. }
            )
        });
        assert_eq!(state.uses.len(), 1);
        let use_id = state.uses[0];
        let bundle = graph
            .bundles
            .iter()
            .find(|bundle| bundle.origin == VReg(0))
            .unwrap();
        assert!(matches!(
            bundle.uses[use_id.0 as usize].site,
            UseSite::PhiEdge {
                predecessor: BlockId(1),
                ..
            }
        ));
    }

    #[test]
    fn verifier_rejects_a_recipe_use_outside_its_bundle() {
        let descriptors = vec![SpillDesc::remat(3), SpillDesc::transient()];
        let insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 3,
            },
            MInst::Mov {
                dst: VReg(1),
                src: VReg(0),
            },
            MInst::Return,
        ];
        let mut function = function(2, descriptors, insts);
        let cfg = normalize(&mut function);
        let mut graph = build(&function, &cfg).unwrap();
        let bundle = graph
            .bundles
            .iter()
            .position(|bundle| bundle.origin == VReg(0))
            .unwrap();
        let candidate = graph.candidates[bundle]
            .iter_mut()
            .find(|candidate| matches!(candidate.kind, HomeKind::Rematerialize(_)))
            .unwrap();
        candidate.uses.push(BundleUseId(u32::MAX));
        let error = graph.verify(&function, &cfg).unwrap_err();
        assert_eq!(error.rule, "HOME_GRAPH.CANDIDATE_USE_SET");
    }
}
