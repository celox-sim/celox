use std::{fmt, hash::Hash};

use serde::{Deserialize, Serialize};

use crate::{
    HashMap, HashSet,
    ir::{LogicPathId, VarAtomBase},
};

use super::{NodeId, SLTNodeArena};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "A: Serialize + std::hash::Hash + Eq",
    deserialize = "A: Deserialize<'de> + std::hash::Hash + Eq + Clone"
))]
pub enum LogicPathTarget<A: Hash + Eq + Clone> {
    Var(VarAtomBase<A>),
    CombCaptureEvent {
        site_id: u32,
        guard: Option<NodeId>,
        emit_on_true: bool,
        args: Vec<NodeId>,
        loop_runner: Option<NodeId>,
        fatal_error_code: Option<i64>,
        consume_enabled: bool,
    },
}

impl<A: Hash + Eq + Clone> LogicPathTarget<A> {
    pub fn var(&self) -> Option<&VarAtomBase<A>> {
        match self {
            LogicPathTarget::Var(var) => Some(var),
            LogicPathTarget::CombCaptureEvent { .. } => None,
        }
    }
}

impl<A: fmt::Display + Hash + Eq + Clone> fmt::Display for LogicPathTarget<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogicPathTarget::Var(var) => write!(f, "{var}"),
            LogicPathTarget::CombCaptureEvent { site_id, .. } => {
                write!(f, "capture_event({site_id})")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "A: Serialize + std::hash::Hash + Eq",
    deserialize = "A: Deserialize<'de> + std::hash::Hash + Eq + Clone"
))]
pub struct LogicPath<A: Hash + Eq + Clone> {
    pub target: LogicPathTarget<A>,
    pub sources: HashSet<VarAtomBase<A>>,
    pub local_inputs: Vec<(A, NodeId)>,
    pub order_before: HashSet<LogicPathId>,
    pub comb_capture_enable_sites: Vec<u32>,
    pub pre_lower_nodes: Vec<NodeId>,
    pub expr: NodeId,
}

impl<A: fmt::Display + Hash + Eq + Clone> fmt::Display for LogicPath<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.target)
    }
}

impl<A: fmt::Debug + fmt::Display + Hash + Eq + Clone> LogicPath<A> {
    pub fn map_addr<B: Hash + Eq + Clone, F>(
        &self,
        arena: &SLTNodeArena<A>,
        target_arena: &mut SLTNodeArena<B>,
        cache: &mut HashMap<NodeId, NodeId>,
        f: &F,
    ) -> LogicPath<B>
    where
        F: Fn(&A) -> B,
    {
        LogicPath {
            target: match &self.target {
                LogicPathTarget::Var(var) => LogicPathTarget::Var(VarAtomBase::new(
                    f(&var.id),
                    var.access.lsb,
                    var.access.msb,
                )),
                LogicPathTarget::CombCaptureEvent {
                    site_id,
                    guard,
                    emit_on_true,
                    args,
                    loop_runner,
                    fatal_error_code,
                    consume_enabled,
                } => LogicPathTarget::CombCaptureEvent {
                    site_id: *site_id,
                    guard: guard.map(|node| {
                        arena
                            .get(node)
                            .map_addr(node, arena, target_arena, cache, f)
                    }),
                    emit_on_true: *emit_on_true,
                    args: args
                        .iter()
                        .map(|node| {
                            arena
                                .get(*node)
                                .map_addr(*node, arena, target_arena, cache, f)
                        })
                        .collect(),
                    loop_runner: loop_runner.map(|node| {
                        arena
                            .get(node)
                            .map_addr(node, arena, target_arena, cache, f)
                    }),
                    fatal_error_code: *fatal_error_code,
                    consume_enabled: *consume_enabled,
                },
            },
            sources: self
                .sources
                .iter()
                .map(|v| VarAtomBase::new(f(&v.id), v.access.lsb, v.access.msb))
                .collect(),
            local_inputs: self
                .local_inputs
                .iter()
                .map(|(id, node)| {
                    (
                        f(id),
                        arena
                            .get(*node)
                            .map_addr(*node, arena, target_arena, cache, f),
                    )
                })
                .collect(),
            order_before: self.order_before.clone(),
            comb_capture_enable_sites: self.comb_capture_enable_sites.clone(),
            pre_lower_nodes: self
                .pre_lower_nodes
                .iter()
                .map(|node| {
                    arena
                        .get(*node)
                        .map_addr(*node, arena, target_arena, cache, f)
                })
                .collect(),
            expr: arena
                .get(self.expr)
                .map_addr(self.expr, arena, target_arena, cache, f),
        }
    }
}
