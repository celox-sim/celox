//! Allocation-owned live bundles and materialization homes.
//!
//! A home is valid for an explicit subset of a bundle's uses.  In particular,
//! a state home is not a VReg-wide spill attribute: each state leaf carries the
//! exact MemorySSA version observed at the use.  A later splitter can therefore
//! cut at home-validity boundaries instead of selecting stack residency first
//! and substituting reloads afterwards.

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use crate::native::mir::{BlockId, MFunction, PackedStateHome, StateHomeId, VReg};

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
    /// A versioned packed word whose store is created only if allocation
    /// selects this home.  Unlike `State`, this is not pre-existing MemorySSA
    /// state and therefore has a one-time creation cost.
    DeferredState(PackedStateHome),
    Unary {
        operation: PureStep,
        input: RecipeId,
    },
    Or64 {
        left: RecipeId,
        right: RecipeId,
    },
}

/// Memory-snapshot-independent identity of a materialization home. Exact
/// MemorySSA snapshots stay in `RecipeNode` and are selected independently for
/// every use.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum RecipeShapeNode {
    Constant(u64),
    State {
        load: super::reload::StateLoad,
        observed_start: i64,
        observed_end: i64,
    },
    DeferredState(PackedStateHome),
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
    /// MemorySSA snapshots.
    State(RecipeShapeId),
    /// One allocator-created packed-state word. The identity is the SSA
    /// version stored there, not merely its physical address.
    DeferredState(StateHomeId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct UseMaterialization {
    pub use_id: BundleUseId,
    /// Exact, MemorySSA-snapshot-proved recipe at this use.
    pub recipe: RecipeId,
    pub cost: u32,
}

/// One non-register materialization proved at one exact bundle use. The use
/// identity is the index of the containing row in `BundleHomes::uses`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct UseHome {
    pub kind: HomeKind,
    pub recipe: RecipeId,
    pub cost: u32,
}

/// Direct use -> materialization index for one root bundle. Register
/// residency and the mandatory stack fallback are allocator mechanisms and
/// are therefore implicit instead of repeated in every use row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BundleHomes {
    pub uses: Vec<Vec<UseHome>>,
}

pub(super) const STACK_HOME_CREATION_COST: u32 = 1;
pub(super) const STACK_HOME_MATERIALIZATION_COST: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HomeGraph {
    pub intervals: LiveIntervals,
    pub bundles: Vec<LiveBundle>,
    pub recipe_nodes: Vec<RecipeNode>,
    pub recipe_shape_nodes: Vec<RecipeShapeNode>,
    /// Shape identity corresponding to every exact recipe node.
    pub recipe_shapes: Vec<RecipeShapeId>,
    pub homes: Vec<BundleHomes>,
    /// Optional allocator-created packed home owned by each root bundle.
    pub deferred_homes: Vec<Option<PackedStateHome>>,
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
            RecipeNode::DeferredState(home) => RecipeShapeNode::DeferredState(*home),
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

    fn deferred_state(&mut self, home: PackedStateHome) -> Result<RecipeId, HomeGraphError> {
        self.intern(RecipeNode::DeferredState(home))
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
    // The interval and MemorySSA producers verify their own dataflow. Verify
    // this phase's ownership and recipe-DAG invariants without recursively
    // rebuilding both complete analyses.
    graph.verify_structure()?;
    Ok(graph)
}

fn build_unverified(func: &MFunction, cfg: &NormalizedCfg) -> Result<HomeGraph, HomeGraphError> {
    let intervals = super::live_interval::analyze(func, cfg).map_err(HomeGraphError::live)?;
    let reloads =
        super::reload::analyze_for_home_graph(func, cfg).map_err(HomeGraphError::reload)?;
    let bundles = root_bundles(&intervals)?;
    let mut recipes = RecipeInterner::default();
    let mut homes = Vec::with_capacity(bundles.len());
    let mut deferred_homes = Vec::with_capacity(bundles.len());
    let mut deferred_ids = HashMap::<StateHomeId, PackedStateHome>::new();
    for bundle in &bundles {
        let deferred = func
            .spill_desc(bundle.origin)
            .and_then(|descriptor| descriptor.deferred_state_home);
        if let Some(home) = deferred {
            if home.live_on_entry || home.byte_range().is_none() {
                return Err(HomeGraphError::new(
                    "HOME_GRAPH.DEFERRED_STATE_HOME",
                    Some(bundle.definition.block()),
                    None,
                    vec![bundle.origin],
                    "deferred state home must be a finite allocator-created version",
                ));
            }
            if let Some(previous) = deferred_ids.insert(home.id, home)
                && previous != home
            {
                return Err(HomeGraphError::new(
                    "HOME_GRAPH.DEFERRED_STATE_IDENTITY",
                    Some(bundle.definition.block()),
                    None,
                    vec![bundle.origin],
                    "one deferred state-home version names two physical words",
                ));
            }
        }
        homes.push(bundle_homes(bundle, deferred, &reloads, &mut recipes)?);
        deferred_homes.push(deferred);
    }
    Ok(HomeGraph {
        intervals,
        bundles,
        recipe_nodes: recipes.nodes,
        recipe_shape_nodes: recipes.shape_nodes,
        recipe_shapes: recipes.shapes,
        homes,
        deferred_homes,
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

fn bundle_homes(
    bundle: &LiveBundle,
    deferred_home: Option<PackedStateHome>,
    reloads: &ReloadRecipeAnalysis,
    recipes: &mut RecipeInterner,
) -> Result<BundleHomes, HomeGraphError> {
    let mut uses = Vec::with_capacity(bundle.uses.len());
    for use_ in &bundle.uses {
        let mut options = Vec::new();
        let point = use_point(bundle.origin, use_.site)?;
        if let Some(recipe) = ordinary_recipe(reloads, point) {
            let (root, state) = recipes.linear(recipe)?;
            let shape = recipes.shape(root)?;
            let kind = if state {
                HomeKind::State(shape)
            } else {
                HomeKind::Rematerialize(shape)
            };
            insert_use_home(&mut options, kind, root, &recipes.nodes)?;
        }
        if let Some(recipe) = fragment_recipe(reloads, point) {
            let root = recipes.composite(recipe)?;
            let kind = HomeKind::State(recipes.shape(root)?);
            insert_use_home(&mut options, kind, root, &recipes.nodes)?;
        }
        if matches!(point, UsePoint::Instruction(_))
            && let Some(home) = deferred_home
        {
            let root = recipes.deferred_state(home)?;
            insert_use_home(
                &mut options,
                HomeKind::DeferredState(home.id),
                root,
                &recipes.nodes,
            )?;
        }
        options.sort_unstable_by_key(|option| option.kind);
        uses.push(options);
    }
    Ok(BundleHomes { uses })
}

fn insert_use_home(
    homes: &mut Vec<UseHome>,
    kind: HomeKind,
    recipe: RecipeId,
    nodes: &[RecipeNode],
) -> Result<(), HomeGraphError> {
    let replacement = UseHome {
        kind,
        recipe,
        cost: recipe_cost(nodes, recipe)?,
    };
    let Some(current) = homes.iter_mut().find(|home| home.kind == kind) else {
        homes.push(replacement);
        return Ok(());
    };
    if (replacement.cost, replacement.recipe) < (current.cost, current.recipe) {
        *current = replacement;
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
            RecipeNode::Constant(_) | RecipeNode::State(_) | RecipeNode::DeferredState(_) => {}
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
            RecipeNode::State(_) | RecipeNode::DeferredState(_) => return Ok(true),
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
        for (index, bundle) in self.bundles.iter().enumerate() {
            let descriptor = func
                .spill_desc(bundle.origin)
                .and_then(|descriptor| descriptor.deferred_state_home);
            if self.deferred_homes.get(index).copied().flatten() != descriptor {
                return Err(Self::candidate_error(
                    bundle,
                    "HOME_GRAPH.DEFERRED_STATE_MATCH",
                    "deferred state home differs from its MIR machine root",
                ));
            }
        }
        self.verify_structure()
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

    fn verify_use_home(&self, bundle: &LiveBundle, home: UseHome) -> Result<(), HomeGraphError> {
        let expected_shape = match home.kind {
            HomeKind::Rematerialize(shape) | HomeKind::State(shape) => Some(shape),
            HomeKind::DeferredState(_) => None,
            HomeKind::Register | HomeKind::Stack => {
                return Err(Self::candidate_error(
                    bundle,
                    "HOME_GRAPH.USE_HOME_CLASS",
                    "register and stack homes must not appear in the use-local recipe index",
                ));
            }
        };
        if let Some(shape) = expected_shape
            && self.recipe_shape_nodes.get(shape.0 as usize).is_none()
        {
            return Err(Self::candidate_error(
                bundle,
                "HOME_GRAPH.RECIPE_SHAPE_RANGE",
                format!("use home references missing recipe shape {shape:?}"),
            ));
        }
        let Some(&actual_shape) = self.recipe_shapes.get(home.recipe.0 as usize) else {
            return Err(Self::candidate_error(
                bundle,
                "HOME_GRAPH.RECIPE_NODE_RANGE",
                format!("use home references missing recipe {:?}", home.recipe),
            ));
        };
        if expected_shape.is_some_and(|shape| actual_shape != shape) {
            return Err(Self::candidate_error(
                bundle,
                "HOME_GRAPH.MATERIALIZATION_SHAPE",
                format!(
                    "exact recipe shape {actual_shape:?} differs from home {:?}",
                    expected_shape.expect("shape mismatch has an expected shape")
                ),
            ));
        }
        let class_matches = match home.kind {
            HomeKind::Rematerialize(_) => !recipe_contains_state(&self.recipe_nodes, home.recipe)?,
            HomeKind::State(_) => {
                recipe_contains_state(&self.recipe_nodes, home.recipe)?
                    && !matches!(
                        self.recipe_nodes.get(home.recipe.0 as usize),
                        Some(RecipeNode::DeferredState(_))
                    )
            }
            HomeKind::DeferredState(id) => matches!(
                self.recipe_nodes.get(home.recipe.0 as usize),
                Some(RecipeNode::DeferredState(candidate)) if candidate.id == id
            ),
            HomeKind::Register | HomeKind::Stack => false,
        };
        if !class_matches {
            return Err(Self::candidate_error(
                bundle,
                "HOME_GRAPH.MATERIALIZATION_CLASS",
                "materialization recipe and home classes are inconsistent",
            ));
        }
        if home.cost != recipe_cost(&self.recipe_nodes, home.recipe)? {
            return Err(Self::candidate_error(
                bundle,
                "HOME_GRAPH.MATERIALIZATION_COST",
                "use-local materialization cost differs from its exact recipe DAG",
            ));
        }
        Ok(())
    }

    fn verify_structure(&self) -> Result<(), HomeGraphError> {
        if self.homes.len() != self.bundles.len() || self.deferred_homes.len() != self.bundles.len()
        {
            return Err(HomeGraphError::new(
                "HOME_GRAPH.BUNDLE_COVERAGE",
                None,
                None,
                Vec::new(),
                "use-home and deferred-home tables do not cover every bundle",
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
                RecipeShapeNode::Constant(_)
                | RecipeShapeNode::State { .. }
                | RecipeShapeNode::DeferredState(_) => {}
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
                RecipeNode::Constant(_) | RecipeNode::State(_) | RecipeNode::DeferredState(_) => {}
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
                RecipeNode::DeferredState(home) => RecipeShapeNode::DeferredState(*home),
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
        let mut intervals = self.intervals.intervals.iter().flatten();
        for (bundle_index, bundle) in self.bundles.iter().enumerate() {
            let Some(interval) = intervals.next() else {
                return Err(HomeGraphError::new(
                    "HOME_GRAPH.BUNDLE_INTERVAL",
                    Some(bundle.definition.block()),
                    None,
                    vec![bundle.origin],
                    "bundle table contains more roots than the live-interval table",
                ));
            };
            if bundle.id.0 as usize != bundle_index {
                return Err(HomeGraphError::new(
                    "HOME_GRAPH.BUNDLE_IDENTITY",
                    Some(bundle.definition.block()),
                    None,
                    vec![bundle.origin],
                    "bundle identity differs from its stable table index",
                ));
            }
            if bundle.origin != interval.value
                || bundle.definition != interval.definition
                || bundle.parent.is_some()
                || bundle.segments != interval.segments
                || bundle.uses.len() != interval.uses.len()
                || bundle.uses.iter().enumerate().any(|(index, use_)| {
                    use_.id.0 as usize != index || use_.site != interval.uses[index]
                })
            {
                return Err(HomeGraphError::new(
                    "HOME_GRAPH.BUNDLE_INTERVAL",
                    Some(bundle.definition.block()),
                    None,
                    vec![bundle.origin],
                    "root bundle differs from its verified live interval",
                ));
            }
            let homes = &self.homes[bundle_index];
            let deferred = self.deferred_homes[bundle_index];
            if homes.uses.len() != bundle.uses.len() {
                return Err(HomeGraphError::new(
                    "HOME_GRAPH.USE_HOME_COVERAGE",
                    Some(bundle.definition.block()),
                    None,
                    vec![bundle.origin],
                    "use-home rows do not cover every bundle use exactly once",
                ));
            }
            for use_homes in &homes.uses {
                if use_homes
                    .windows(2)
                    .any(|pair| pair[0].kind >= pair[1].kind)
                {
                    return Err(HomeGraphError::new(
                        "HOME_GRAPH.USE_HOME_SET",
                        Some(bundle.definition.block()),
                        None,
                        vec![bundle.origin],
                        "one use has duplicate or unsorted physical home identities",
                    ));
                }
                for &home in use_homes {
                    if let HomeKind::DeferredState(id) = home.kind
                        && deferred.is_none_or(|candidate| candidate.id != id)
                    {
                        return Err(Self::candidate_error(
                            bundle,
                            "HOME_GRAPH.DEFERRED_STATE_OWNERSHIP",
                            "use-local deferred home is not owned by this machine root",
                        ));
                    }
                    self.verify_use_home(bundle, home)?;
                }
            }
        }
        if intervals.next().is_some() {
            return Err(HomeGraphError::new(
                "HOME_GRAPH.BUNDLE_INTERVAL",
                None,
                None,
                Vec::new(),
                "live-interval table contains a root missing from the bundle table",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::mir::{BaseReg, MBlock, MInst, OpSize, SpillDesc, VRegAllocator};

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

    fn matching_homes(
        graph: &HomeGraph,
        value: VReg,
        predicate: impl Fn(HomeKind) -> bool,
    ) -> Vec<(BundleUseId, UseHome)> {
        let bundle = graph
            .bundles
            .iter()
            .position(|bundle| bundle.origin == value)
            .unwrap();
        let mut result = Vec::new();
        for (use_index, homes) in graph.homes[bundle].uses.iter().enumerate() {
            for &home in homes {
                if predicate(home.kind) {
                    result.push((BundleUseId(u32::try_from(use_index).unwrap()), home));
                }
            }
        }
        result
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
        let states = matching_homes(&graph, VReg(0), |kind| {
            let HomeKind::State(shape) = kind else {
                return false;
            };
            matches!(
                graph.recipe_shape_nodes[shape.0 as usize],
                RecipeShapeNode::Or64 { .. }
            )
        });
        assert_eq!(states.len(), 1);
        let (use_id, state) = states[0];
        assert_eq!(use_id, BundleUseId(2));
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
            graph.homes[bundle]
                .uses
                .iter()
                .flatten()
                .all(|home| !matches!(home.kind, HomeKind::State(_)))
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
            graph.homes[bundle]
                .uses
                .iter()
                .flatten()
                .all(|home| !matches!(home.kind, HomeKind::State(_)))
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
        let states = matching_homes(&graph, VReg(0), |kind| matches!(kind, HomeKind::State(_)));
        assert_eq!(
            states.iter().map(|(use_id, _)| *use_id).collect::<Vec<_>>(),
            vec![BundleUseId(0), BundleUseId(1)]
        );
        assert_ne!(
            states[0].1.recipe, states[1].1.recipe,
            "MemorySSA snapshots must remain use-local even when the physical home is shared"
        );
        let HomeKind::State(shape) = states[0].1.kind else {
            unreachable!();
        };
        assert!(states.iter().all(|(_, home)| {
            home.kind == HomeKind::State(shape)
                && graph.recipe_shapes[home.recipe.0 as usize] == shape
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
        merge.phis.push(crate::native::mir::PhiNode {
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
        let states = matching_homes(&graph, VReg(0), |kind| {
            let HomeKind::State(shape) = kind else {
                return false;
            };
            matches!(
                graph.recipe_shape_nodes[shape.0 as usize],
                RecipeShapeNode::Or64 { .. }
            )
        });
        assert_eq!(states.len(), 1);
        let use_id = states[0].0;
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
    fn verifier_rejects_a_use_home_row_outside_its_bundle() {
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
        graph.homes[bundle].uses.push(Vec::new());
        let error = graph.verify(&function, &cfg).unwrap_err();
        assert_eq!(error.rule, "HOME_GRAPH.USE_HOME_COVERAGE");
    }
}
