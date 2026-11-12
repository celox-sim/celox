//! Symbolic execution of combinational processes into SLT.
//!
//! A process is executed statement by statement over a [`SymbolicStore`]
//! that maps each written variable to the SLT value it holds at the current
//! point. Branches fork the store and merge it with multiplexers; `break`,
//! `continue`, and `return` set hidden flag variables under which the rest of
//! a block runs. Loops whose iteration count is known are unrolled; counted
//! loops with run-time bounds become [`SLTNode::ForFold`] folds. Subroutine
//! calls are inlined: their arguments and locals are hidden variables.

mod assign;
mod case;
mod comb_process;
mod expr;
mod hoist;
mod initial_process;
mod inline_calls;
mod loops;
mod merge;
mod path;
mod rewrite;
mod stmt;
mod store;
mod system_tasks;

use std::borrow::Cow;

use super::procedural::*;
use super::*;
use celox_slt::{
    RangeStore, SLTForEffect, SLTForFoldResult, SLTForUpdate, SLTLoopBound, SymbolicStore,
};
use num_traits::{ToPrimitive, Zero};

pub(super) use comb_process::*;
pub(super) use loops::RUNTIME_LOOP_CONDITION;
use rewrite::*;

type Sources = HashSet<VarAtomBase<SourceVarId>>;
pub(super) type Store = SymbolicStore<SourceVarId, NodeId>;
type Value = (NodeId, Sources);

#[derive(Clone)]
enum Frame {
    Loop {
        break_flag: SourceVarId,
        continue_flag: SourceVarId,
    },
    /// A named block a `disable` can leave (9.6.2).
    Block { id: u64, exit_flag: SourceVarId },
    Function {
        return_flag: SourceVarId,
        return_var: Option<String>,
    },
}

pub(super) struct Comb<'p, 'a> {
    pub m: &'p mut ProcModule<'a>,
    pub arena: &'p mut SLTNodeArena<SourceVarId>,
    /// Constant values of `arena` nodes.
    consts: ConstCache,
    /// The subroutine bodies being executed.
    call_stack: inline_calls::CallStack,
    unrolled: usize,
    /// Whether nonblocking assignments are allowed (they are not in
    /// combinational processes).
    allow_nonblocking: bool,
    /// The conditions that hold on the current path.
    path: path::PathConditions,
    /// The runtime events of the process.
    pub events: system_tasks::RuntimeEvents,
    /// The run-time loop folds being executed.
    folds: loops::Folds,
    /// Whether the process is a continuous assignment, which drives fixed bits.
    pub continuous: bool,
    /// Whether the process is an `initial` block, which defines initial state.
    pub initial: bool,
}

fn range_error(error: impl std::fmt::Display) -> sv::AnalyzerError {
    unsupported(format!("procedural lowering: {error}"))
}

fn full(width: usize) -> BitAccess {
    BitAccess::new(0, width.saturating_sub(1))
}

impl<'p, 'a> Comb<'p, 'a> {
    pub fn new(m: &'p mut ProcModule<'a>, arena: &'p mut SLTNodeArena<SourceVarId>) -> Self {
        Self {
            m,
            arena,
            consts: ConstCache::default(),
            call_stack: inline_calls::CallStack::default(),
            unrolled: 0,
            allow_nonblocking: false,
            path: path::PathConditions::default(),
            events: system_tasks::RuntimeEvents::default(),
            folds: loops::Folds::default(),
            continuous: false,
            initial: false,
        }
    }

    fn alloc(&mut self, node: SLTNode<SourceVarId>) -> Result<NodeId, sv::AnalyzerError> {
        self.arena.alloc(node).map_err(slt_error)
    }

    fn width(&self, node: NodeId) -> usize {
        celox_slt::get_width(node, self.arena)
    }

    fn constant(&mut self, value: u64, width: usize) -> Result<NodeId, sv::AnalyzerError> {
        slt_constant(self.arena, BigUint::from(value), width, false)
    }

    fn slice(&mut self, node: NodeId, access: BitAccess) -> Result<NodeId, sv::AnalyzerError> {
        let width = self.width(node);
        if access.lsb == 0 && access.msb + 1 == width {
            return Ok(node);
        }
        self.alloc(SLTNode::Slice { expr: node, access })
    }

    fn concat_lsb_first(
        &mut self,
        parts: Vec<(NodeId, usize)>,
    ) -> Result<NodeId, sv::AnalyzerError> {
        if parts.len() == 1 {
            return Ok(parts[0].0);
        }
        self.alloc(SLTNode::Concat(parts.into_iter().rev().collect()))
    }
}
