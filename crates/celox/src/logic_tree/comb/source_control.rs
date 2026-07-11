//! Verified source-value occurrences and the source-site registry they name.
//!
//! This is deliberately smaller than `SourceControlProvenance`.  It proves
//! that every recorded value occurrence names an existing source control site
//! and exactly mirrors the ordered operands of its semantic SLT node.  Regions,
//! roots, gates, decisions, source-site dominance, and source-to-flattened
//! expansion are later artifacts and are not claimed by this boundary.

#![allow(dead_code)]

use std::fmt;
use std::hash::Hash;

use serde::{Deserialize, Serialize};

use super::control::{
    CheckedControlId, SourceControlEdgeId, SourceControlPointId, SourceControlUnitId,
    SourceControlUseSite, SourceValueOccurrenceId,
};
use super::node::{NodeId, SLTLoopBound, SLTNode};
use super::node_facts::SLTNodeFacts;

/// One source control unit's entry and exit points.
///
/// This table only establishes point ownership and site existence.  The later
/// complete provenance verifier proves region structure and control legality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct SourceOccurrenceUnit {
    pub(crate) entry: SourceControlPointId,
    pub(crate) exit: SourceControlPointId,
}

/// A point that may be named by a [`SourceControlUseSite::Slot`].
///
/// Valid slots are `0..slot_count`.  A producer with `N` ordered actions uses
/// `N + 1` source positions (before, between, and after those actions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SourceOccurrencePoint {
    pub(crate) unit: SourceControlUnitId,
    pub(crate) slot_count: usize,
    pub(crate) predecessors: Vec<SourceControlEdgeId>,
    pub(crate) successors: Vec<SourceControlEdgeId>,
}

/// One exact source CFG edge usable as an edge-specific value occurrence site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct SourceOccurrenceEdge {
    pub(crate) unit: SourceControlUnitId,
    pub(crate) predecessor: SourceControlPointId,
    pub(crate) successor: SourceControlPointId,
}

/// One syntactic/semantic value occurrence before hierarchy flattening.
///
/// `semantic_node` remains a canonical SLT identity, while this record retains
/// the particular source control site at which that value occurrence exists.
/// Operand IDs are in the exact semantic-node operand order and must precede
/// their owner in the dense occurrence table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SourceValueOccurrence {
    pub(crate) semantic_node: NodeId,
    pub(crate) source_site: SourceControlUseSite,
    pub(crate) ordered_operands: Vec<SourceValueOccurrenceId>,
}

/// The producer boundary for source value occurrences.
///
/// It is not a control plan: it intentionally contains no roots, predicate
/// regions, gates, decisions, or placement claims.  Those records extend this
/// verified table when complete `SourceControlProvenance` is constructed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SourceValueOccurrenceBoundary {
    pub(crate) units: Vec<SourceOccurrenceUnit>,
    pub(crate) points: Vec<SourceOccurrencePoint>,
    pub(crate) edges: Vec<SourceOccurrenceEdge>,
    pub(crate) occurrences: Vec<SourceValueOccurrence>,
}

/// Structured failure from [`SourceValueOccurrenceBoundary::verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceOccurrenceVerifyError {
    pub(crate) invariant: &'static str,
    pub(crate) entity: &'static str,
    pub(crate) index: usize,
    pub(crate) message: String,
}

impl SourceOccurrenceVerifyError {
    fn new(
        invariant: &'static str,
        entity: &'static str,
        index: usize,
        message: impl Into<String>,
    ) -> Self {
        Self {
            invariant,
            entity,
            index,
            message: message.into(),
        }
    }
}

impl fmt::Display for SourceOccurrenceVerifyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "source occurrence verify [{}] at {} {}: {}",
            self.invariant, self.entity, self.index, self.message
        )
    }
}

impl std::error::Error for SourceOccurrenceVerifyError {}

fn canonical_ids<T: Ord>(values: &[T]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn zeroed_memberships(
    count: usize,
    entity: &'static str,
) -> Result<Vec<u8>, SourceOccurrenceVerifyError> {
    let mut memberships = Vec::new();
    memberships.try_reserve_exact(count).map_err(|error| {
        SourceOccurrenceVerifyError::new(
            "SOURCE.STORAGE_AVAILABLE",
            entity,
            count,
            format!("cannot reserve {count} membership counters: {error}"),
        )
    })?;
    memberships.resize(count, 0);
    Ok(memberships)
}

fn record_membership(counter: &mut u8) {
    // The verifier only distinguishes exactly one membership from every other
    // count.  Saturation avoids a malformed adjacency list overflowing a small
    // counter while retaining the compact O(edges) table.
    *counter = counter.saturating_add(1);
}

fn require_dense_table_representable<Id: CheckedControlId>(
    count: usize,
    entity: &'static str,
) -> Result<(), SourceOccurrenceVerifyError> {
    let Some(last_index) = count.checked_sub(1) else {
        return Ok(());
    };
    Id::checked_from_len(last_index).map(|_| ()).map_err(|_| {
        SourceOccurrenceVerifyError::new(
            "SOURCE.DENSE_ID_REPRESENTABLE",
            entity,
            last_index,
            format!(
                "{count} {entity} records do not fit the {} dense ID namespace",
                Id::KIND
            ),
        )
    })
}

impl SourceValueOccurrenceBoundary {
    /// Verify the complete relation owned by this source-occurrence boundary.
    ///
    /// The supplied facts make canonical SLT append order and width/coercion
    /// verification an explicit prerequisite. This verifier never accepts a
    /// raw, potentially malformed arena.
    pub(crate) fn verify<A>(
        &self,
        slt_facts: &SLTNodeFacts<'_, A>,
    ) -> Result<(), SourceOccurrenceVerifyError>
    where
        A: Hash + Eq + Clone,
    {
        let unit_count = self.units.len();
        let edge_count = self.edges.len();
        let occurrence_count = self.occurrences.len();

        require_dense_table_representable::<SourceControlUnitId>(unit_count, "unit")?;
        require_dense_table_representable::<SourceControlPointId>(self.points.len(), "point")?;
        require_dense_table_representable::<SourceControlEdgeId>(edge_count, "edge")?;
        require_dense_table_representable::<SourceValueOccurrenceId>(
            occurrence_count,
            "occurrence",
        )?;

        let mut predecessor_memberships = zeroed_memberships(edge_count, "edge")?;
        let mut successor_memberships = zeroed_memberships(edge_count, "edge")?;

        for (point_index, point) in self.points.iter().enumerate() {
            if point.unit.index() >= unit_count {
                return Err(SourceOccurrenceVerifyError::new(
                    "SOURCE.POINT_UNIT_EXISTS",
                    "point",
                    point_index,
                    format!(
                        "point names missing unit {} but the table contains {unit_count} units",
                        point.unit.index()
                    ),
                ));
            }
            if point.slot_count == 0 {
                return Err(SourceOccurrenceVerifyError::new(
                    "SOURCE.POINT_HAS_SLOT",
                    "point",
                    point_index,
                    "a source control point must expose at least its before/after slot",
                ));
            }
            if !canonical_ids(&point.predecessors) {
                return Err(SourceOccurrenceVerifyError::new(
                    "SOURCE.POINT_PREDECESSORS_CANONICAL",
                    "point",
                    point_index,
                    "predecessor edge IDs are not strictly increasing",
                ));
            }
            if !canonical_ids(&point.successors) {
                return Err(SourceOccurrenceVerifyError::new(
                    "SOURCE.POINT_SUCCESSORS_CANONICAL",
                    "point",
                    point_index,
                    "successor edge IDs are not strictly increasing",
                ));
            }

            for edge_id in &point.predecessors {
                let edge_index = edge_id.index();
                let Some(edge) = self.edges.get(edge_index) else {
                    return Err(SourceOccurrenceVerifyError::new(
                        "SOURCE.POINT_PREDECESSOR_EXISTS",
                        "point",
                        point_index,
                        format!("predecessor edge {edge_index} does not exist"),
                    ));
                };
                if edge.successor.index() != point_index {
                    return Err(SourceOccurrenceVerifyError::new(
                        "SOURCE.POINT_PREDECESSOR_ENDPOINT",
                        "point",
                        point_index,
                        format!(
                            "listed predecessor edge {edge_index} ends at point {}, not {point_index}",
                            edge.successor.index()
                        ),
                    ));
                }
                record_membership(&mut predecessor_memberships[edge_index]);
            }
            for edge_id in &point.successors {
                let edge_index = edge_id.index();
                let Some(edge) = self.edges.get(edge_index) else {
                    return Err(SourceOccurrenceVerifyError::new(
                        "SOURCE.POINT_SUCCESSOR_EXISTS",
                        "point",
                        point_index,
                        format!("successor edge {edge_index} does not exist"),
                    ));
                };
                if edge.predecessor.index() != point_index {
                    return Err(SourceOccurrenceVerifyError::new(
                        "SOURCE.POINT_SUCCESSOR_ENDPOINT",
                        "point",
                        point_index,
                        format!(
                            "listed successor edge {edge_index} starts at point {}, not {point_index}",
                            edge.predecessor.index()
                        ),
                    ));
                }
                record_membership(&mut successor_memberships[edge_index]);
            }
        }

        for (unit_index, unit) in self.units.iter().enumerate() {
            let Some(entry) = self.points.get(unit.entry.index()) else {
                return Err(SourceOccurrenceVerifyError::new(
                    "SOURCE.UNIT_ENTRY_EXISTS",
                    "unit",
                    unit_index,
                    format!("entry point {} does not exist", unit.entry.index()),
                ));
            };
            if entry.unit.index() != unit_index {
                return Err(SourceOccurrenceVerifyError::new(
                    "SOURCE.UNIT_ENTRY_OWNED",
                    "unit",
                    unit_index,
                    format!(
                        "entry point {} belongs to unit {}, not {unit_index}",
                        unit.entry.index(),
                        entry.unit.index()
                    ),
                ));
            }
            let Some(exit) = self.points.get(unit.exit.index()) else {
                return Err(SourceOccurrenceVerifyError::new(
                    "SOURCE.UNIT_EXIT_EXISTS",
                    "unit",
                    unit_index,
                    format!("exit point {} does not exist", unit.exit.index()),
                ));
            };
            if exit.unit.index() != unit_index {
                return Err(SourceOccurrenceVerifyError::new(
                    "SOURCE.UNIT_EXIT_OWNED",
                    "unit",
                    unit_index,
                    format!(
                        "exit point {} belongs to unit {}, not {unit_index}",
                        unit.exit.index(),
                        exit.unit.index()
                    ),
                ));
            }
        }

        for (edge_index, edge) in self.edges.iter().enumerate() {
            if edge.unit.index() >= unit_count {
                return Err(SourceOccurrenceVerifyError::new(
                    "SOURCE.EDGE_UNIT_EXISTS",
                    "edge",
                    edge_index,
                    format!(
                        "edge names missing unit {} but the table contains {unit_count} units",
                        edge.unit.index()
                    ),
                ));
            }
            let Some(predecessor) = self.points.get(edge.predecessor.index()) else {
                return Err(SourceOccurrenceVerifyError::new(
                    "SOURCE.EDGE_PREDECESSOR_EXISTS",
                    "edge",
                    edge_index,
                    format!(
                        "predecessor point {} does not exist",
                        edge.predecessor.index()
                    ),
                ));
            };
            let Some(successor) = self.points.get(edge.successor.index()) else {
                return Err(SourceOccurrenceVerifyError::new(
                    "SOURCE.EDGE_SUCCESSOR_EXISTS",
                    "edge",
                    edge_index,
                    format!("successor point {} does not exist", edge.successor.index()),
                ));
            };
            if predecessor.unit != edge.unit || successor.unit != edge.unit {
                return Err(SourceOccurrenceVerifyError::new(
                    "SOURCE.EDGE_UNIT_MATCHES_ENDPOINTS",
                    "edge",
                    edge_index,
                    format!(
                        "edge unit {} disagrees with predecessor unit {} or successor unit {}",
                        edge.unit.index(),
                        predecessor.unit.index(),
                        successor.unit.index()
                    ),
                ));
            }
            if predecessor_memberships[edge_index] != 1 {
                return Err(SourceOccurrenceVerifyError::new(
                    "SOURCE.EDGE_PREDECESSOR_MEMBERSHIP",
                    "edge",
                    edge_index,
                    format!(
                        "edge occurs {} times in successor-point predecessor lists, expected once",
                        predecessor_memberships[edge_index]
                    ),
                ));
            }
            if successor_memberships[edge_index] != 1 {
                return Err(SourceOccurrenceVerifyError::new(
                    "SOURCE.EDGE_SUCCESSOR_MEMBERSHIP",
                    "edge",
                    edge_index,
                    format!(
                        "edge occurs {} times in predecessor-point successor lists, expected once",
                        successor_memberships[edge_index]
                    ),
                ));
            }
        }

        let mut occurrence_units = Vec::<SourceControlUnitId>::new();
        occurrence_units
            .try_reserve_exact(occurrence_count)
            .map_err(|error| {
                SourceOccurrenceVerifyError::new(
                    "SOURCE.STORAGE_AVAILABLE",
                    "occurrence",
                    occurrence_count,
                    format!(
                        "cannot reserve units for {occurrence_count} source occurrences: {error}"
                    ),
                )
            })?;

        for (occurrence_index, occurrence) in self.occurrences.iter().enumerate() {
            let unit = self.site_unit(occurrence.source_site, occurrence_index)?;
            let Some(semantic_node) = slt_facts.node(occurrence.semantic_node) else {
                return Err(SourceOccurrenceVerifyError::new(
                    "SOURCE.OCCURRENCE_NODE_EXISTS",
                    "occurrence",
                    occurrence_index,
                    format!(
                        "semantic node n{} does not exist in the {}-node arena",
                        occurrence.semantic_node.0,
                        slt_facts.node_count()
                    ),
                ));
            };
            let expected_arity = semantic_operand_count(semantic_node).ok_or_else(|| {
                SourceOccurrenceVerifyError::new(
                    "SOURCE.SEMANTIC_ARITY_REPRESENTABLE",
                    "occurrence",
                    occurrence_index,
                    format!(
                        "semantic operand count of n{} overflows usize",
                        occurrence.semantic_node.0
                    ),
                )
            })?;
            if occurrence.ordered_operands.len() != expected_arity {
                return Err(SourceOccurrenceVerifyError::new(
                    "SOURCE.OCCURRENCE_OPERAND_ARITY",
                    "occurrence",
                    occurrence_index,
                    format!(
                        "n{} has {expected_arity} semantic operands but the occurrence records {}",
                        occurrence.semantic_node.0,
                        occurrence.ordered_operands.len()
                    ),
                ));
            }

            let mut semantic_error = None;
            for_each_semantic_operand(semantic_node, |position, expected_node| {
                if semantic_error.is_some() {
                    return;
                }
                if expected_node.0 >= slt_facts.node_count() {
                    semantic_error = Some(SourceOccurrenceVerifyError::new(
                        "SOURCE.SEMANTIC_CHILD_EXISTS",
                        "occurrence",
                        occurrence_index,
                        format!(
                            "semantic operand {position} names missing node n{}",
                            expected_node.0
                        ),
                    ));
                    return;
                }
                let Some(&operand_id) = occurrence.ordered_operands.get(position) else {
                    semantic_error = Some(SourceOccurrenceVerifyError::new(
                        "SOURCE.OCCURRENCE_OPERAND_ARITY",
                        "occurrence",
                        occurrence_index,
                        format!("semantic operand {position} has no occurrence record"),
                    ));
                    return;
                };
                let operand_index = operand_id.index();
                if operand_index >= occurrence_count {
                    semantic_error = Some(SourceOccurrenceVerifyError::new(
                        "SOURCE.OCCURRENCE_OPERAND_EXISTS",
                        "occurrence",
                        occurrence_index,
                        format!(
                            "operand {position} names missing occurrence {operand_index}; table length is {occurrence_count}"
                        ),
                    ));
                    return;
                }
                if operand_index >= occurrence_index {
                    semantic_error = Some(SourceOccurrenceVerifyError::new(
                        "SOURCE.OCCURRENCE_OPERAND_PRECEDES_OWNER",
                        "occurrence",
                        occurrence_index,
                        format!(
                            "operand {position} occurrence {operand_index} does not precede its owner"
                        ),
                    ));
                    return;
                }
                let Some(operand) = self.occurrences.get(operand_index) else {
                    semantic_error = Some(SourceOccurrenceVerifyError::new(
                        "SOURCE.OCCURRENCE_OPERAND_EXISTS",
                        "occurrence",
                        occurrence_index,
                        format!("operand {position} occurrence {operand_index} does not exist"),
                    ));
                    return;
                };
                if operand.semantic_node != expected_node {
                    semantic_error = Some(SourceOccurrenceVerifyError::new(
                        "SOURCE.OCCURRENCE_OPERAND_MATCHES_NODE",
                        "occurrence",
                        occurrence_index,
                        format!(
                            "operand {position} occurrence {operand_index} names n{}, expected n{}",
                            operand.semantic_node.0, expected_node.0
                        ),
                    ));
                    return;
                }
                let Some(&operand_unit) = occurrence_units.get(operand_index) else {
                    semantic_error = Some(SourceOccurrenceVerifyError::new(
                        "SOURCE.OCCURRENCE_OPERAND_UNIT_AVAILABLE",
                        "occurrence",
                        occurrence_index,
                        format!(
                            "unit of preceding operand {position} occurrence {operand_index} is unavailable"
                        ),
                    ));
                    return;
                };
                if operand_unit != unit {
                    semantic_error = Some(SourceOccurrenceVerifyError::new(
                        "SOURCE.OCCURRENCE_OPERAND_SAME_UNIT",
                        "occurrence",
                        occurrence_index,
                        format!(
                            "operand {position} occurrence {operand_index} belongs to unit {}, owner belongs to unit {}",
                            operand_unit.index(),
                            unit.index()
                        ),
                    ));
                }
            });
            if let Some(error) = semantic_error {
                return Err(error);
            }
            occurrence_units.push(unit);
        }

        // Keep the shape proof explicit even for an empty table and make it
        // impossible for a future early-continue to leave a partial unit map.
        if occurrence_units.len() != occurrence_count {
            return Err(SourceOccurrenceVerifyError::new(
                "SOURCE.OCCURRENCE_UNIT_COVERAGE",
                "occurrence",
                occurrence_units.len(),
                format!(
                    "derived {} occurrence units for {occurrence_count} occurrences",
                    occurrence_units.len()
                ),
            ));
        }

        // The point registry is intentionally not required to be reachable or
        // acyclic here. Complete SourceControlProvenance owns that proof along
        // with regions, gates, and decisions.
        Ok(())
    }

    fn site_unit(
        &self,
        site: SourceControlUseSite,
        occurrence_index: usize,
    ) -> Result<SourceControlUnitId, SourceOccurrenceVerifyError> {
        match site {
            SourceControlUseSite::Slot(site) => {
                let Some(point) = self.points.get(site.point.index()) else {
                    return Err(SourceOccurrenceVerifyError::new(
                        "SOURCE.OCCURRENCE_SITE_POINT_EXISTS",
                        "occurrence",
                        occurrence_index,
                        format!("site point {} does not exist", site.point.index()),
                    ));
                };
                if site.slot >= point.slot_count {
                    return Err(SourceOccurrenceVerifyError::new(
                        "SOURCE.OCCURRENCE_SITE_SLOT_EXISTS",
                        "occurrence",
                        occurrence_index,
                        format!(
                            "site slot {} is outside point {} slot range 0..{}",
                            site.slot,
                            site.point.index(),
                            point.slot_count
                        ),
                    ));
                }
                Ok(point.unit)
            }
            SourceControlUseSite::Edge(edge_id) => {
                let Some(edge) = self.edges.get(edge_id.index()) else {
                    return Err(SourceOccurrenceVerifyError::new(
                        "SOURCE.OCCURRENCE_SITE_EDGE_EXISTS",
                        "occurrence",
                        occurrence_index,
                        format!("site edge {} does not exist", edge_id.index()),
                    ));
                };
                Ok(edge.unit)
            }
        }
    }
}

fn semantic_operand_count<A>(node: &SLTNode<A>) -> Option<usize>
where
    A: Hash + Eq + Clone,
{
    match node {
        SLTNode::Input { index, .. } => Some(index.len()),
        SLTNode::Constant(..) => Some(0),
        SLTNode::Binary(..) => Some(2),
        SLTNode::Unary(..) | SLTNode::Slice { .. } => Some(1),
        SLTNode::Mux { .. } => Some(3),
        SLTNode::ForFold {
            start,
            end,
            initials,
            updates,
            effects,
            ..
        } => {
            let bounds = usize::from(matches!(start, SLTLoopBound::Expr(_)))
                .checked_add(usize::from(matches!(end, SLTLoopBound::Expr(_))))?;
            let effect_operands = effects.iter().try_fold(0usize, |count, effect| {
                count
                    .checked_add(usize::from(effect.guard.is_some()))?
                    .checked_add(effect.args.len())
            })?;
            bounds
                .checked_add(initials.len())?
                .checked_add(updates.len())?
                .checked_add(effect_operands)?
                .checked_add(1)
        }
        SLTNode::Concat(parts) => Some(parts.len()),
    }
}

fn for_each_semantic_operand<A>(node: &SLTNode<A>, mut visit: impl FnMut(usize, NodeId))
where
    A: Hash + Eq + Clone,
{
    let mut position = 0usize;
    let mut next = |child| {
        visit(position, child);
        position += 1;
    };
    match node {
        SLTNode::Input { index, .. } => {
            for entry in index {
                next(entry.node);
            }
        }
        SLTNode::Constant(..) => {}
        SLTNode::Binary(lhs, _, rhs) => {
            next(*lhs);
            next(*rhs);
        }
        SLTNode::Unary(_, inner) => next(*inner),
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            next(*cond);
            next(*then_expr);
            next(*else_expr);
        }
        SLTNode::ForFold {
            start,
            end,
            initials,
            updates,
            effects,
            continue_cond,
            ..
        } => {
            if let SLTLoopBound::Expr(child) = start {
                next(*child);
            }
            if let SLTLoopBound::Expr(child) = end {
                next(*child);
            }
            for initial in initials {
                next(initial.expr);
            }
            for update in updates {
                next(update.expr);
            }
            for effect in effects {
                if let Some(guard) = effect.guard {
                    next(guard);
                }
                for &argument in &effect.args {
                    next(argument);
                }
            }
            next(*continue_cond);
        }
        SLTNode::Concat(parts) => {
            for &(part, _) in parts {
                next(part);
            }
        }
        SLTNode::Slice { expr, .. } => next(*expr),
    }
}

#[cfg(test)]
mod tests {
    use num_bigint::BigUint;

    use crate::ir::{BinaryOp, UnaryOp};

    use super::super::control::SourceControlSite;
    use super::super::node::SLTNodeArena;
    use super::*;

    fn id<I: super::super::control::CheckedControlId>(index: usize) -> I {
        I::checked_from_len(index).expect("small test ID must fit")
    }

    fn constant(width: usize) -> SLTNode<u32> {
        SLTNode::Constant(BigUint::from(0u8), BigUint::from(0u8), width, false)
    }

    fn constant_value(width: usize, value: u8) -> SLTNode<u32> {
        SLTNode::Constant(BigUint::from(value), BigUint::from(0u8), width, false)
    }

    fn valid_arena() -> SLTNodeArena<u32> {
        let mut arena = SLTNodeArena::new();
        arena.alloc(constant_value(8, 0));
        arena.alloc(constant_value(8, 1));
        arena.alloc(SLTNode::Binary(NodeId(0), BinaryOp::Add, NodeId(1)));
        arena
    }

    fn valid_boundary() -> SourceValueOccurrenceBoundary {
        let unit = id::<SourceControlUnitId>(0);
        let entry = id::<SourceControlPointId>(0);
        let exit = id::<SourceControlPointId>(1);
        let edge = id::<SourceControlEdgeId>(0);
        SourceValueOccurrenceBoundary {
            units: vec![SourceOccurrenceUnit { entry, exit }],
            points: vec![
                SourceOccurrencePoint {
                    unit,
                    slot_count: 2,
                    predecessors: Vec::new(),
                    successors: vec![edge],
                },
                SourceOccurrencePoint {
                    unit,
                    slot_count: 1,
                    predecessors: vec![edge],
                    successors: Vec::new(),
                },
            ],
            edges: vec![SourceOccurrenceEdge {
                unit,
                predecessor: entry,
                successor: exit,
            }],
            occurrences: vec![
                SourceValueOccurrence {
                    semantic_node: NodeId(0),
                    source_site: SourceControlUseSite::Slot(SourceControlSite::new(entry, 1)),
                    ordered_operands: Vec::new(),
                },
                SourceValueOccurrence {
                    semantic_node: NodeId(1),
                    source_site: SourceControlUseSite::Edge(edge),
                    ordered_operands: Vec::new(),
                },
                SourceValueOccurrence {
                    semantic_node: NodeId(2),
                    source_site: SourceControlUseSite::Slot(SourceControlSite::new(exit, 0)),
                    ordered_operands: vec![
                        id::<SourceValueOccurrenceId>(0),
                        id::<SourceValueOccurrenceId>(1),
                    ],
                },
            ],
        }
    }

    fn verify_boundary(
        boundary: &SourceValueOccurrenceBoundary,
        arena: &SLTNodeArena<u32>,
    ) -> Result<(), SourceOccurrenceVerifyError> {
        let facts = SLTNodeFacts::verify(arena).expect("test arena must verify first");
        boundary.verify(&facts)
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn rejects_a_dense_table_larger_than_its_checked_id_namespace() {
        let count = usize::try_from(u64::from(u32::MAX) + 2)
            .expect("64-bit usize must represent one past the maximum table length");
        let error =
            require_dense_table_representable::<SourceValueOccurrenceId>(count, "occurrence")
                .expect_err("oversized dense table must fail");
        assert_eq!(error.invariant, "SOURCE.DENSE_ID_REPRESENTABLE");
    }

    #[test]
    fn verifies_existing_sites_and_exact_semantic_operands() {
        verify_boundary(&valid_boundary(), &valid_arena())
            .expect("valid source occurrence boundary must verify");
    }

    #[test]
    fn rejects_a_missing_slot_point() {
        let arena = valid_arena();
        let mut boundary = valid_boundary();
        boundary.occurrences[0].source_site =
            SourceControlUseSite::Slot(SourceControlSite::new(id::<SourceControlPointId>(2), 0));
        let error = verify_boundary(&boundary, &arena).expect_err("missing point must fail");
        assert_eq!(error.invariant, "SOURCE.OCCURRENCE_SITE_POINT_EXISTS");
    }

    #[test]
    fn rejects_an_out_of_range_slot() {
        let arena = valid_arena();
        let mut boundary = valid_boundary();
        boundary.occurrences[0].source_site =
            SourceControlUseSite::Slot(SourceControlSite::new(id::<SourceControlPointId>(0), 2));
        let error = verify_boundary(&boundary, &arena).expect_err("out-of-range slot must fail");
        assert_eq!(error.invariant, "SOURCE.OCCURRENCE_SITE_SLOT_EXISTS");
    }

    #[test]
    fn rejects_a_missing_edge_site() {
        let arena = valid_arena();
        let mut boundary = valid_boundary();
        boundary.occurrences[1].source_site =
            SourceControlUseSite::Edge(id::<SourceControlEdgeId>(1));
        let error = verify_boundary(&boundary, &arena).expect_err("missing edge must fail");
        assert_eq!(error.invariant, "SOURCE.OCCURRENCE_SITE_EDGE_EXISTS");
    }

    #[test]
    fn rejects_nonreciprocal_edge_membership() {
        let arena = valid_arena();
        let mut boundary = valid_boundary();
        boundary.points[1].predecessors.clear();
        let error = verify_boundary(&boundary, &arena).expect_err("unlisted edge must fail");
        assert_eq!(error.invariant, "SOURCE.EDGE_PREDECESSOR_MEMBERSHIP");
    }

    #[test]
    fn rejects_a_missing_semantic_node() {
        let arena = valid_arena();
        let mut boundary = valid_boundary();
        boundary.occurrences[0].semantic_node = NodeId(3);
        let error =
            verify_boundary(&boundary, &arena).expect_err("missing semantic node must fail");
        assert_eq!(error.invariant, "SOURCE.OCCURRENCE_NODE_EXISTS");
    }

    #[test]
    fn rejects_wrong_semantic_operand_arity() {
        let arena = valid_arena();
        let mut boundary = valid_boundary();
        boundary.occurrences[2].ordered_operands.pop();
        let error = verify_boundary(&boundary, &arena).expect_err("wrong operand arity must fail");
        assert_eq!(error.invariant, "SOURCE.OCCURRENCE_OPERAND_ARITY");
    }

    #[test]
    fn rejects_forward_occurrence_operands() {
        let arena = valid_arena();
        let mut boundary = valid_boundary();
        boundary.occurrences[2].ordered_operands[1] = id::<SourceValueOccurrenceId>(2);
        let error = verify_boundary(&boundary, &arena).expect_err("forward operand must fail");
        assert_eq!(error.invariant, "SOURCE.OCCURRENCE_OPERAND_PRECEDES_OWNER");
    }

    #[test]
    fn rejects_wrong_semantic_operand_order() {
        let arena = valid_arena();
        let mut boundary = valid_boundary();
        boundary.occurrences[2].ordered_operands.swap(0, 1);
        let error =
            verify_boundary(&boundary, &arena).expect_err("wrong semantic operand order must fail");
        assert_eq!(error.invariant, "SOURCE.OCCURRENCE_OPERAND_MATCHES_NODE");
    }

    #[test]
    fn rejects_cross_unit_operands() {
        let arena = valid_arena();
        let mut boundary = valid_boundary();
        let second_unit = id::<SourceControlUnitId>(1);
        let second_entry = id::<SourceControlPointId>(2);
        boundary.units.push(SourceOccurrenceUnit {
            entry: second_entry,
            exit: second_entry,
        });
        boundary.points.push(SourceOccurrencePoint {
            unit: second_unit,
            slot_count: 1,
            predecessors: Vec::new(),
            successors: Vec::new(),
        });
        boundary.occurrences[1].source_site =
            SourceControlUseSite::Slot(SourceControlSite::new(second_entry, 0));
        let error = verify_boundary(&boundary, &arena).expect_err("cross-unit operand must fail");
        assert_eq!(error.invariant, "SOURCE.OCCURRENCE_OPERAND_SAME_UNIT");
    }

    #[test]
    fn malformed_semantic_children_fail_at_the_prerequisite_boundary() {
        let arena: SLTNodeArena<u32> =
            SLTNodeArena::from_nodes_unchecked(vec![SLTNode::Unary(UnaryOp::Ident, NodeId(1))]);
        let error = SLTNodeFacts::verify(&arena).expect_err("missing SLT child must fail first");
        assert_eq!(error.invariant, "GRAPH.CHILD_EXISTS");
    }

    #[test]
    fn deep_occurrence_chain_verifies_iteratively() {
        const DEPTH: usize = 100_000;
        let mut arena = SLTNodeArena::new();
        arena.alloc(constant(1));
        for index in 1..DEPTH {
            arena.alloc(SLTNode::Unary(UnaryOp::Ident, NodeId(index - 1)));
        }

        let unit = id::<SourceControlUnitId>(0);
        let point = id::<SourceControlPointId>(0);
        let mut occurrences = Vec::with_capacity(DEPTH);
        occurrences.push(SourceValueOccurrence {
            semantic_node: NodeId(0),
            source_site: SourceControlUseSite::Slot(SourceControlSite::new(point, 0)),
            ordered_operands: Vec::new(),
        });
        for index in 1..DEPTH {
            occurrences.push(SourceValueOccurrence {
                semantic_node: NodeId(index),
                source_site: SourceControlUseSite::Slot(SourceControlSite::new(point, 0)),
                ordered_operands: vec![id::<SourceValueOccurrenceId>(index - 1)],
            });
        }
        let boundary = SourceValueOccurrenceBoundary {
            units: vec![SourceOccurrenceUnit {
                entry: point,
                exit: point,
            }],
            points: vec![SourceOccurrencePoint {
                unit,
                slot_count: 1,
                predecessors: Vec::new(),
                successors: Vec::new(),
            }],
            edges: Vec::new(),
            occurrences,
        };
        verify_boundary(&boundary, &arena)
            .expect("deep occurrence chain must verify without recursion");
    }
}
