use celox_analysis::{
    graph::StronglyConnectedComponents,
    interval::{DisjointIntervalError, DisjointIntervalMap, ExactInterval},
};

use crate::{
    ir::AbsoluteAddr,
    logic_tree::{LogicPath, LogicPathTarget, NodeId, SLTNodeArena, SLTNodeFacts},
};

use super::{
    CombConvergenceId, CombDefinition, CombDefinitionId, CombRecipeId, CombRecipeNodeId,
    ObjectRange,
};

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum CombSnapshotKind {
    /// No combinational definition covers this part of a current-value input.
    EventEntry,
    /// AIR/SLT explicitly requests the value from before combinational
    /// evaluation of this event.
    PreviousValue,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct CombSnapshotInput {
    pub range: ObjectRange,
    pub kind: CombSnapshotKind,
    pub used_for_address: bool,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct CombDependency {
    pub definition: CombDefinitionId,
    /// The complete range requested by the LogicPath. The defining range can
    /// be narrower when one input is assembled from several definitions.
    pub requested: ObjectRange,
    pub used_for_address: bool,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct CombLocalInput {
    pub object: AbsoluteAddr,
    pub value: CombRecipeNodeId,
    pub width: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombCaptureRecipe {
    pub site_id: u32,
    pub guard: Option<CombRecipeNodeId>,
    pub emit_on_true: bool,
    pub arguments: Vec<CombRecipeNodeId>,
    pub loop_runner: Option<CombRecipeNodeId>,
    pub fatal_error_code: Option<i64>,
    pub consume_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CombRecipeTarget {
    Definition(CombDefinitionId),
    Capture(CombCaptureRecipe),
}

/// One intact SLT/LogicPath recipe plus its resolved event-level bindings.
///
/// The recipe node IDs refer to the immutable flattened SLT arena owned by the
/// containing compilation. EIR records all current/snapshot bindings here, so
/// later lowering never has to infer them from SIR memory operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombRecipe {
    pub root: CombRecipeNodeId,
    pub pre_evaluate: Vec<CombRecipeNodeId>,
    pub local_inputs: Vec<CombLocalInput>,
    pub dependencies: Vec<CombDependency>,
    pub snapshot_inputs: Vec<CombSnapshotInput>,
    pub order_before: Vec<CombRecipeId>,
    pub semantic_region: Option<u64>,
    pub convergence: Option<CombConvergenceId>,
    pub target: CombRecipeTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombConvergenceRegion {
    pub recipes: Vec<CombRecipeId>,
}

/// Shared combinational definition graph used by every event projection.
#[derive(Debug, Clone, Default)]
pub struct CombGraph {
    slt_node_count: usize,
    recipes: Vec<CombRecipe>,
    definitions: Vec<CombDefinition>,
    convergence_regions: Vec<CombConvergenceRegion>,
}

impl CombGraph {
    pub fn recipes(&self) -> &[CombRecipe] {
        &self.recipes
    }

    pub fn definitions(&self) -> &[CombDefinition] {
        &self.definitions
    }

    pub fn convergence_regions(&self) -> &[CombConvergenceRegion] {
        &self.convergence_regions
    }

    pub fn slt_node_count(&self) -> usize {
        self.slt_node_count
    }

    /// Import all flattened LogicPaths. Construction is `O((P + E) log P)`
    /// time and `O(P + E + N)` space for `P` paths, `E` resolved dependencies,
    /// and `N` SLT nodes. Numerical RTL widths are never expanded.
    pub(crate) fn import(
        paths: &[LogicPath<AbsoluteAddr>],
        arena: &SLTNodeArena<AbsoluteAddr>,
        facts: &SLTNodeFacts<'_, AbsoluteAddr>,
    ) -> Result<Self, CombImportError> {
        let mut definition_by_path = vec![None; paths.len()];
        let mut definitions = Vec::new();
        for (path_index, path) in paths.iter().enumerate() {
            let root_width = facts
                .require_lowerable(path.expr, "EIR combinational recipe")
                .map_err(|error| {
                    CombImportError::new(
                        CombImportInvariant::SltGraph,
                        Some(path_index),
                        error.to_string(),
                    )
                })?;
            let Some(target) = path.target.var() else {
                continue;
            };
            let Some(target_width) = range_width(target.access) else {
                return Err(CombImportError::new(
                    CombImportInvariant::DefinitionRange,
                    Some(path_index),
                    "definition has an invalid or overflowing target range",
                ));
            };
            if root_width != target_width {
                return Err(CombImportError::new(
                    CombImportInvariant::DefinitionWidth,
                    Some(path_index),
                    format!("SLT root width {root_width} differs from target width {target_width}"),
                ));
            }
            let id = CombDefinitionId(definitions.len());
            definition_by_path[path_index] = Some(id);
            definitions.push(CombDefinition {
                target: ObjectRange::new(target.id, target.access),
                recipe: CombRecipeId(path_index),
            });
        }

        let intervals = definitions
            .iter()
            .enumerate()
            .map(|(definition, item)| ExactInterval {
                object: item.target.object,
                start: item.target.access.lsb,
                length: item
                    .target
                    .width()
                    .expect("definition ranges were checked above"),
                value: CombDefinitionId(definition),
            });
        let definition_index = DisjointIntervalMap::try_new(intervals).map_err(|error| {
            let (first, second) = match error {
                DisjointIntervalError::Overlap { first, second } => (first, Some(second)),
                DisjointIntervalError::Empty { value }
                | DisjointIntervalError::Overflow { value } => (value, None),
            };
            let message = second.map_or_else(
                || format!("definition {first} has an invalid interval"),
                |second| format!("definitions {first} and {second} overlap"),
            );
            CombImportError::new(CombImportInvariant::DefinitionRange, None, message)
        })?;

        let mut recipes = Vec::with_capacity(paths.len());
        // Only value-definition edges define convergence. Source-order edges
        // remain explicit scheduling constraints but must not turn an
        // anti-dependence cycle into a fixed-point combinational SCC.
        let mut value_successors = vec![Vec::new(); paths.len()];
        for (path_index, path) in paths.iter().enumerate() {
            let target = match &path.target {
                LogicPathTarget::Var(_) => CombRecipeTarget::Definition(
                    definition_by_path[path_index].expect("variable targets reserve definitions"),
                ),
                LogicPathTarget::CombCaptureEvent {
                    site_id,
                    guard,
                    emit_on_true,
                    args,
                    loop_runner,
                    fatal_error_code,
                    consume_enabled,
                } => CombRecipeTarget::Capture(CombCaptureRecipe {
                    site_id: *site_id,
                    guard: guard.map(recipe_node),
                    emit_on_true: *emit_on_true,
                    arguments: args.iter().copied().map(recipe_node).collect(),
                    loop_runner: loop_runner.map(recipe_node),
                    fatal_error_code: *fatal_error_code,
                    consume_enabled: *consume_enabled,
                }),
            };

            validate_auxiliary_roots(path_index, path, facts)?;

            let mut dependencies = Vec::new();
            let mut snapshot_inputs = Vec::new();
            for source in &path.sources {
                let requested = ObjectRange::new(source.id, source.access);
                let used_for_address = path.address_sources.iter().any(|address| {
                    address.id == source.id && address.access.overlaps(&source.access)
                });
                let Some(length) = range_width(source.access) else {
                    return Err(CombImportError::new(
                        CombImportInvariant::SourceRange,
                        Some(path_index),
                        "current-value source has an invalid range",
                    ));
                };
                let reaching = definition_index
                    .overlapping(&source.id, source.access.lsb, length)
                    .map_err(|_| {
                        CombImportError::new(
                            CombImportInvariant::SourceRange,
                            Some(path_index),
                            "current-value source range overflows",
                        )
                    })?
                    .collect::<Vec<_>>();
                for &definition in &reaching {
                    dependencies.push(CombDependency {
                        definition,
                        requested,
                        used_for_address,
                    });
                    value_successors[definitions[definition.0].recipe.0].push(path_index);
                }
                append_uncovered_snapshot_ranges(
                    requested,
                    &reaching,
                    &definitions,
                    used_for_address,
                    &mut snapshot_inputs,
                );
            }
            for source in &path.previous_sources {
                if range_width(source.access).is_none() {
                    return Err(CombImportError::new(
                        CombImportInvariant::SourceRange,
                        Some(path_index),
                        "previous-value source has an invalid range",
                    ));
                }
                snapshot_inputs.push(CombSnapshotInput {
                    range: ObjectRange::new(source.id, source.access),
                    kind: CombSnapshotKind::PreviousValue,
                    used_for_address: false,
                });
            }

            dependencies.sort_unstable();
            dependencies.dedup();
            snapshot_inputs.sort_unstable();
            snapshot_inputs.dedup();

            let mut order_before = path
                .order_before
                .iter()
                .map(|ordered| {
                    if ordered.0 >= paths.len() {
                        Err(CombImportError::new(
                            CombImportInvariant::OrderEdge,
                            Some(path_index),
                            format!("order edge names absent recipe {}", ordered.0),
                        ))
                    } else {
                        Ok(CombRecipeId(ordered.0))
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            order_before.sort_unstable();
            order_before.dedup();

            let local_inputs = path
                .local_inputs
                .iter()
                .map(|(object, node)| {
                    Ok(CombLocalInput {
                        object: *object,
                        value: recipe_node(*node),
                        width: facts
                            .require_width(*node, "EIR local input")
                            .map_err(|error| {
                                CombImportError::new(
                                    CombImportInvariant::SltGraph,
                                    Some(path_index),
                                    error.to_string(),
                                )
                            })?,
                    })
                })
                .collect::<Result<Vec<_>, CombImportError>>()?;

            recipes.push(CombRecipe {
                root: recipe_node(path.expr),
                pre_evaluate: path
                    .pre_lower_nodes
                    .iter()
                    .copied()
                    .map(recipe_node)
                    .collect(),
                local_inputs,
                dependencies,
                snapshot_inputs,
                order_before,
                semantic_region: path.semantic_region,
                convergence: None,
                target,
            });
        }
        for outgoing in &mut value_successors {
            outgoing.sort_unstable();
            outgoing.dedup();
        }

        let sccs = StronglyConnectedComponents::analyze(&value_successors).map_err(|error| {
            CombImportError::new(
                CombImportInvariant::DependencyGraph,
                None,
                format!("{error:?}"),
            )
        })?;
        let mut convergence_regions = Vec::new();
        for component in sccs.components {
            if !component.cyclic {
                continue;
            }
            let convergence = CombConvergenceId(convergence_regions.len());
            let members = component
                .nodes
                .into_iter()
                .map(CombRecipeId)
                .collect::<Vec<_>>();
            for recipe in &members {
                recipes[recipe.0].convergence = Some(convergence);
            }
            convergence_regions.push(CombConvergenceRegion { recipes: members });
        }

        let graph = Self {
            slt_node_count: arena.len(),
            recipes,
            definitions,
            convergence_regions,
        };
        graph.verify()?;
        Ok(graph)
    }

    pub fn verify(&self) -> Result<(), CombImportError> {
        for (definition_index, definition) in self.definitions.iter().enumerate() {
            let definition_id = CombDefinitionId(definition_index);
            let Some(recipe) = self.recipes.get(definition.recipe.0) else {
                return Err(CombImportError::new(
                    CombImportInvariant::DefinitionRecipe,
                    None,
                    format!("{definition_id} names absent {}", definition.recipe),
                ));
            };
            if recipe.target != CombRecipeTarget::Definition(definition_id) {
                return Err(CombImportError::new(
                    CombImportInvariant::DefinitionRecipe,
                    Some(definition.recipe.0),
                    format!("{definition_id} and its recipe do not name each other"),
                ));
            }
            if definition.target.width().is_none() {
                return Err(CombImportError::new(
                    CombImportInvariant::DefinitionRange,
                    Some(definition.recipe.0),
                    "definition range is invalid",
                ));
            }
        }

        let mut convergence_owner = vec![None; self.recipes.len()];
        for (index, convergence) in self.convergence_regions.iter().enumerate() {
            let id = CombConvergenceId(index);
            if convergence.recipes.is_empty() {
                return Err(CombImportError::new(
                    CombImportInvariant::ConvergenceRegion,
                    None,
                    format!("{id} is empty"),
                ));
            }
            for recipe in &convergence.recipes {
                let Some(item) = self.recipes.get(recipe.0) else {
                    return Err(CombImportError::new(
                        CombImportInvariant::ConvergenceRegion,
                        None,
                        format!("{id} names absent {recipe}"),
                    ));
                };
                if item.convergence != Some(id) || convergence_owner[recipe.0].replace(id).is_some()
                {
                    return Err(CombImportError::new(
                        CombImportInvariant::ConvergenceRegion,
                        Some(recipe.0),
                        "convergence ownership is inconsistent",
                    ));
                }
            }
        }

        for (recipe_index, recipe) in self.recipes.iter().enumerate() {
            let node_valid = |node: CombRecipeNodeId| node.0 < self.slt_node_count;
            if !node_valid(recipe.root)
                || recipe.pre_evaluate.iter().any(|node| !node_valid(*node))
                || recipe
                    .local_inputs
                    .iter()
                    .any(|input| input.width == 0 || !node_valid(input.value))
            {
                return Err(CombImportError::new(
                    CombImportInvariant::RecipeNode,
                    Some(recipe_index),
                    "recipe names an absent or zero-width SLT node",
                ));
            }
            if recipe
                .dependencies
                .iter()
                .any(|dependency| dependency.definition.0 >= self.definitions.len())
                || recipe
                    .order_before
                    .iter()
                    .any(|ordered| ordered.0 >= self.recipes.len())
            {
                return Err(CombImportError::new(
                    CombImportInvariant::DependencyGraph,
                    Some(recipe_index),
                    "recipe names an absent definition or order target",
                ));
            }
            if let CombRecipeTarget::Capture(capture) = &recipe.target
                && (capture.guard.is_some_and(|node| !node_valid(node))
                    || capture.arguments.iter().any(|node| !node_valid(*node))
                    || capture.loop_runner.is_some_and(|node| !node_valid(node)))
            {
                return Err(CombImportError::new(
                    CombImportInvariant::RecipeNode,
                    Some(recipe_index),
                    "capture recipe names an absent SLT node",
                ));
            }
            if recipe.convergence != convergence_owner[recipe_index] {
                return Err(CombImportError::new(
                    CombImportInvariant::ConvergenceRegion,
                    Some(recipe_index),
                    "recipe convergence ID has no matching region membership",
                ));
            }
        }
        Ok(())
    }
}

fn recipe_node(node: NodeId) -> CombRecipeNodeId {
    CombRecipeNodeId(node.0)
}

fn range_width(access: crate::ir::BitAccess) -> Option<usize> {
    access.msb.checked_sub(access.lsb)?.checked_add(1)
}

fn validate_auxiliary_roots(
    path_index: usize,
    path: &LogicPath<AbsoluteAddr>,
    facts: &SLTNodeFacts<'_, AbsoluteAddr>,
) -> Result<(), CombImportError> {
    for node in path
        .pre_lower_nodes
        .iter()
        .copied()
        .chain(path.local_inputs.iter().map(|(_, node)| *node))
    {
        facts
            .require_lowerable(node, "EIR combinational auxiliary recipe")
            .map_err(|error| {
                CombImportError::new(
                    CombImportInvariant::SltGraph,
                    Some(path_index),
                    error.to_string(),
                )
            })?;
    }
    if let LogicPathTarget::CombCaptureEvent {
        guard,
        args,
        loop_runner,
        ..
    } = &path.target
    {
        for node in guard
            .iter()
            .copied()
            .chain(args.iter().copied())
            .chain(loop_runner.iter().copied())
        {
            facts
                .require_lowerable(node, "EIR capture recipe")
                .map_err(|error| {
                    CombImportError::new(
                        CombImportInvariant::SltGraph,
                        Some(path_index),
                        error.to_string(),
                    )
                })?;
        }
    }
    Ok(())
}

fn append_uncovered_snapshot_ranges(
    requested: ObjectRange,
    reaching: &[CombDefinitionId],
    definitions: &[CombDefinition],
    used_for_address: bool,
    snapshots: &mut Vec<CombSnapshotInput>,
) {
    let mut covered = reaching
        .iter()
        .map(|definition| definitions[definition.0].target.access)
        .map(|access| {
            (
                access.lsb.max(requested.access.lsb),
                access.msb.min(requested.access.msb),
            )
        })
        .collect::<Vec<_>>();
    covered.sort_unstable();

    let mut cursor = Some(requested.access.lsb);
    for (start, end) in covered {
        let Some(current) = cursor else {
            break;
        };
        if current < start {
            snapshots.push(CombSnapshotInput {
                range: ObjectRange::new(
                    requested.object,
                    crate::ir::BitAccess::new(current, start - 1),
                ),
                kind: CombSnapshotKind::EventEntry,
                used_for_address,
            });
        }
        cursor = end.checked_add(1).map(|after_end| current.max(after_end));
    }
    if let Some(cursor) = cursor
        && cursor <= requested.access.msb
    {
        snapshots.push(CombSnapshotInput {
            range: ObjectRange::new(
                requested.object,
                crate::ir::BitAccess::new(cursor, requested.access.msb),
            ),
            kind: CombSnapshotKind::EventEntry,
            used_for_address,
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombImportInvariant {
    SltGraph,
    DefinitionRange,
    DefinitionWidth,
    DefinitionRecipe,
    SourceRange,
    RecipeNode,
    OrderEdge,
    DependencyGraph,
    ConvergenceRegion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombImportError {
    pub invariant: CombImportInvariant,
    pub recipe: Option<usize>,
    pub message: String,
}

impl CombImportError {
    fn new(
        invariant: CombImportInvariant,
        recipe: Option<usize>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            invariant,
            recipe,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CombImportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "EIR comb import {:?}", self.invariant)?;
        if let Some(recipe) = self.recipe {
            write!(formatter, " at recipe{recipe}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for CombImportError {}

#[cfg(test)]
mod tests {
    use num_bigint::BigUint;
    use veryl_analyzer::ir::VarId;

    use super::*;
    use crate::{
        HashSet,
        ir::{BitAccess, InstanceId, LogicPathId, VarAtomBase},
        logic_tree::{SLTNode, SLTNodeArena},
    };

    fn object(var: u32) -> AbsoluteAddr {
        AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(var),
        }
    }

    fn path(
        target: AbsoluteAddr,
        expr: NodeId,
        sources: impl IntoIterator<Item = VarAtomBase<AbsoluteAddr>>,
    ) -> LogicPath<AbsoluteAddr> {
        LogicPath {
            semantic_region: Some(0),
            target: LogicPathTarget::Var(VarAtomBase::new(target, 0, 7)),
            sources: sources.into_iter().collect(),
            previous_sources: HashSet::default(),
            address_sources: HashSet::default(),
            local_inputs: Vec::new(),
            order_before: HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            pre_lower_nodes: Vec::new(),
            expr,
        }
    }

    fn import(paths: &[LogicPath<AbsoluteAddr>], arena: &SLTNodeArena<AbsoluteAddr>) -> CombGraph {
        let facts = SLTNodeFacts::verify(arena).unwrap();
        CombGraph::import(paths, arena, &facts).unwrap()
    }

    #[test]
    fn resolves_current_definition_and_previous_snapshot_separately() {
        let a = object(1);
        let b = object(2);
        let mut arena = SLTNodeArena::new();
        let constant = arena
            .alloc(SLTNode::Constant(
                BigUint::from(3u8),
                BigUint::from(0u8),
                8,
                false,
            ))
            .unwrap();
        let read_a = arena
            .alloc(SLTNode::Input {
                variable: a,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let first = path(a, constant, []);
        let mut second = path(b, read_a, [VarAtomBase::new(a, 0, 7)]);
        second.previous_sources.insert(VarAtomBase::new(b, 0, 7));

        let graph = import(&[first, second], &arena);
        assert_eq!(graph.recipes[1].dependencies.len(), 1);
        assert_eq!(
            graph.recipes[1].dependencies[0].definition,
            CombDefinitionId(0)
        );
        assert_eq!(
            graph.recipes[1].snapshot_inputs,
            vec![CombSnapshotInput {
                range: ObjectRange::new(b, BitAccess::new(0, 7)),
                kind: CombSnapshotKind::PreviousValue,
                used_for_address: false,
            }]
        );
    }

    #[test]
    fn marks_a_definition_cycle_as_one_convergence_region() {
        let a = object(1);
        let b = object(2);
        let mut arena = SLTNodeArena::new();
        let read_b = arena
            .alloc(SLTNode::Input {
                variable: b,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let read_a = arena
            .alloc(SLTNode::Input {
                variable: a,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let first = path(a, read_b, [VarAtomBase::new(b, 0, 7)]);
        let second = path(b, read_a, [VarAtomBase::new(a, 0, 7)]);

        let graph = import(&[first, second], &arena);
        assert_eq!(graph.convergence_regions.len(), 1);
        assert_eq!(
            graph.convergence_regions[0].recipes,
            vec![CombRecipeId(0), CombRecipeId(1)]
        );
    }

    #[test]
    fn preserves_source_order_edges_between_recipes() {
        let a = object(1);
        let b = object(2);
        let mut arena = SLTNodeArena::new();
        let constant = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(0u8),
                8,
                false,
            ))
            .unwrap();
        let mut first = path(a, constant, []);
        first.order_before.insert(LogicPathId(1));
        let second = path(b, constant, []);

        let graph = import(&[first, second], &arena);
        assert_eq!(graph.recipes[0].order_before, vec![CombRecipeId(1)]);
    }

    #[test]
    fn source_order_cycle_is_not_a_combinational_convergence_region() {
        let a = object(1);
        let b = object(2);
        let mut arena = SLTNodeArena::new();
        let constant = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(0u8),
                8,
                false,
            ))
            .unwrap();
        let mut first = path(a, constant, []);
        let mut second = path(b, constant, []);
        first.order_before.insert(LogicPathId(1));
        second.order_before.insert(LogicPathId(0));

        let graph = import(&[first, second], &arena);
        assert!(graph.convergence_regions.is_empty());
    }
}
