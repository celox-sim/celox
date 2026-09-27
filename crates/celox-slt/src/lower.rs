//! Lower symbolic logic trees to SIR.
//!
//! Public entry points, shared lowering state, and node dispatch live here.
//! Child modules implement input/slice handling, concat and mux lowering,
//! count/scan idioms, loop construction, and cost analysis.

mod concat;
mod constants;
mod cost;
mod count_idioms;
mod inputs;
mod loops;
mod mux;
mod or_scan;
mod slices;

use constants::try_const_eval;
pub use count_idioms::matches_slt_count_idiom;
use count_idioms::{match_slt_boolean_not, normalized_slt_lane_op, slt_const_u64, slt_width};
use or_scan::match_slt_or_scan_plan;
pub use or_scan::matches_slt_or_scan_group;

use crate::{NodeId, SLTForFoldGroupState, SLTLoopBound, SLTNode, SLTNodeArena, SLTStepOp};
use celox_design::{BinaryOp, BitAccess, UnaryOp, VarAtomBase};
use celox_sir::{
    RegisterId, RegisterType, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator, SIRValue,
};
use num_bigint::{BigInt, BigUint};
use num_traits::{ToPrimitive, Zero};
use std::cell::RefCell;
use std::hash::Hash;

#[derive(Clone)]
enum SLTBitOrigin<A: Hash + Eq + Clone> {
    Node(NodeId),
    Input {
        /// One representative input node. This is provenance for memory type
        /// lookup and deliberately does not participate in origin identity:
        /// unrolled lanes have distinct nodes but the same logical input.
        node: NodeId,
        variable: A,
        signed: bool,
        index: Vec<crate::SLTIndex>,
    },
}

impl<A: Hash + Eq + Clone> PartialEq for SLTBitOrigin<A> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Node(lhs), Self::Node(rhs)) => lhs == rhs,
            (
                Self::Input {
                    variable: lhs_variable,
                    signed: lhs_signed,
                    index: lhs_index,
                    ..
                },
                Self::Input {
                    variable: rhs_variable,
                    signed: rhs_signed,
                    index: rhs_index,
                    ..
                },
            ) => lhs_variable == rhs_variable && lhs_signed == rhs_signed && lhs_index == rhs_index,
            _ => false,
        }
    }
}

impl<A: Hash + Eq + Clone> Eq for SLTBitOrigin<A> {}

impl<A: Hash + Eq + Clone> Hash for SLTBitOrigin<A> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            Self::Node(node) => {
                0u8.hash(state);
                node.hash(state);
            }
            Self::Input {
                variable,
                signed,
                index,
                ..
            } => {
                1u8.hash(state);
                variable.hash(state);
                signed.hash(state);
                index.hash(state);
            }
        }
    }
}

#[derive(Clone)]
struct SLTBitTerm<A: Hash + Eq + Clone> {
    predicate: NodeId,
    origin: Option<(SLTBitOrigin<A>, usize)>,
}

enum SLTCountPredicate {
    Node(NodeId),
    And(NodeId, NodeId),
}

enum SLTVectorExpr<A: Hash + Eq + Clone> {
    Origin(SLTBitOrigin<A>),
    /// A packed input reconstructed from a proven identity-indexed bit read.
    /// Unlike `SLTBitOrigin::Input`, this deliberately drops the dynamic lane
    /// index and loads the complete packed word at its static base offset.
    StaticInput {
        variable: A,
        access: BitAccess,
        unpacked_element_width: Option<usize>,
    },
    Broadcast(NodeId),
    LowOnes {
        bound: NodeId,
    },
    Not(Box<SLTVectorExpr<A>>),
    Binary {
        lhs: Box<SLTVectorExpr<A>>,
        op: BinaryOp,
        rhs: Box<SLTVectorExpr<A>>,
    },
}

enum SLTCountInput<A: Hash + Eq + Clone> {
    Origin(SLTBitOrigin<A>),
    Vector(SLTVectorExpr<A>),
    Predicates(Vec<SLTCountPredicate>),
}

struct SLTCountPlan<A: Hash + Eq + Clone> {
    op: UnaryOp,
    input_width: usize,
    input: SLTCountInput<A>,
    post: SLTCountPost,
}

enum SLTCountPost {
    Direct,
    /// Add a newly recovered population-count delta to an exact accumulator
    /// value that already dominates the current lowering point.
    AddTo(NodeId),
    /// Turn `clz(predicates)` into the selected last-write index.  With an
    /// all-ones default, `N - 1 - clz(0)` naturally wraps to that sentinel.
    SubtractFrom(u64),
    /// Map the count operation's zero-input result (`input_width`) back to the
    /// sentinel used by the procedural priority encoder.
    ReplaceZeroInputCount(u64),
    /// Preserve a conditional accumulator seed around the recovered count.
    Select {
        cond: NodeId,
        false_value: NodeId,
    },
}

#[derive(Default)]
struct LoweringCostCache {
    tree_costs: Vec<Option<u128>>,
    contains_div_rem: Vec<Option<bool>>,
    fanout: Vec<usize>,
    initially_materialized: Vec<bool>,
    owned_costs: Vec<Option<u128>>,
    owned_slice_lower_costs: Vec<Option<u128>>,
    contains_shared_nontrivial: Vec<Option<bool>>,
    is_speculatable_pure: Vec<Option<bool>>,
    traversal_seen: Vec<bool>,
    traversal_work: Vec<NodeId>,
    dirty: Vec<bool>,
    touched: Vec<NodeId>,
    #[cfg(test)]
    analysis_node_visits: usize,
}

impl LoweringCostCache {
    fn resize(&mut self, node_count: usize) {
        self.tree_costs.resize(node_count, None);
        self.contains_div_rem.resize(node_count, None);
        self.fanout.resize(node_count, 0);
        self.initially_materialized.resize(node_count, false);
        self.owned_costs.resize(node_count, None);
        self.owned_slice_lower_costs.resize(node_count, None);
        self.contains_shared_nontrivial.resize(node_count, None);
        self.is_speculatable_pure.resize(node_count, None);
        self.traversal_seen.resize(node_count, false);
        self.dirty.resize(node_count, false);
    }

    fn touch(&mut self, node: NodeId) {
        if !self.dirty[node.0] {
            self.dirty[node.0] = true;
            self.touched.push(node);
        }
    }

    fn reset(&mut self, node_count: usize) {
        // Most top-level roots use a tiny part of the shared arena. Reset all
        // facts for nodes written by any analysis, including nodes beyond a
        // materialization boundary, without scanning every arena-sized table.
        for node in self.touched.drain(..) {
            self.tree_costs[node.0] = None;
            self.contains_div_rem[node.0] = None;
            self.fanout[node.0] = 0;
            self.initially_materialized[node.0] = false;
            self.owned_costs[node.0] = None;
            self.owned_slice_lower_costs[node.0] = None;
            self.contains_shared_nontrivial[node.0] = None;
            self.is_speculatable_pure[node.0] = None;
            self.traversal_seen[node.0] = false;
            self.dirty[node.0] = false;
        }
        // Clear old entries before shrinking; new entries start empty.
        self.resize(node_count);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct StaticBranchProbability {
    true_weight: u128,
    total_weight: u128,
}

impl StaticBranchProbability {
    const EVEN: Self = Self {
        true_weight: 1,
        total_weight: 2,
    };

    fn inverted(self) -> Self {
        Self {
            true_weight: self.total_weight - self.true_weight,
            total_weight: self.total_weight,
        }
    }

    fn conjunction(self, rhs: Self) -> Self {
        let Some(true_weight) = self.true_weight.checked_mul(rhs.true_weight) else {
            return Self::EVEN;
        };
        let Some(total_weight) = self.total_weight.checked_mul(rhs.total_weight) else {
            return Self::EVEN;
        };
        Self {
            true_weight,
            total_weight,
        }
    }
}

struct MuxCfgPlan {
    /// Nodes used by both arms which were not already materialized.  They must
    /// be evaluated once in the dominator before the control-flow split.
    shared_nodes: Vec<NodeId>,
}

#[derive(Clone, Default)]
struct ZeroControllerFacts {
    /// The expression is the known two-state value zero independently of any
    /// runtime predicate.  Such a child imposes no constraint when an outer
    /// operation requires all of its children to be zero.
    unconditional_zero: bool,
    /// One-bit descendants whose false value is sufficient to prove this
    /// expression is all zero.
    guards: crate::HashSet<NodeId>,
}

#[derive(Clone, Copy)]
struct GuardedConcatPlan {
    guard: NodeId,
    net_benefit_scaled: u128,
}

#[derive(Default)]
struct MuxLowerStats {
    normal_seen: usize,
    slice_seen: usize,
    constant_folded: usize,
    cfg_cost: usize,
    cfg_div_rem: usize,
    cfg_slice_cost: usize,
    cfg_slice_div_rem: usize,
    shared_nodes_hoisted: usize,
    kept_four_state: usize,
    kept_impure: usize,
    kept_dynamic_env: usize,
    kept_unprofitable: usize,
    kept_deep_shared: usize,
    biased_conditions: usize,
    owned_cost_sum: u128,
    owned_cost_max: u128,
    unprofitable_cost_buckets: [usize; 7],
}

impl MuxLowerStats {
    fn record_cost(&mut self, then_cost: u128, else_cost: u128) {
        let total = then_cost.saturating_add(else_cost);
        self.owned_cost_sum = self.owned_cost_sum.saturating_add(total);
        self.owned_cost_max = self.owned_cost_max.max(total);
    }

    fn record_unprofitable(&mut self, then_cost: u128, else_cost: u128) {
        self.kept_unprofitable += 1;
        let total = then_cost.saturating_add(else_cost);
        let bucket = match total {
            0..=7 => 0,
            8..=15 => 1,
            16..=31 => 2,
            32..=63 => 3,
            64..=127 => 4,
            128..=255 => 5,
            _ => 6,
        };
        self.unprofitable_cost_buckets[bucket] += 1;
    }
}

pub struct SLTToSIRLowerer {
    four_state: bool,
    unpacked_input_element_widths: crate::HashMap<NodeId, usize>,
    cost_cache: RefCell<LoweringCostCache>,
    cache_insert_log: RefCell<Vec<NodeId>>,
    region_slice_cache: RefCell<crate::HashMap<(NodeId, BitAccess), RegisterId>>,
    region_slice_cache_insert_log: RefCell<Vec<(NodeId, BitAccess)>>,
    mux_stats: Option<RefCell<MuxLowerStats>>,
}

#[derive(Clone, Copy)]
struct LowerCacheTransaction {
    node_insertions: usize,
    region_slice_insertions: usize,
}

struct LowerEnv<'parent, A: Hash + Eq + Clone> {
    inputs: crate::HashMap<VarAtomBase<A>, RegisterId>,
    /// Lower-priority bindings from an enclosing lowering scope.  Keeping the
    /// layers separate is important for partial state targets: flattening the
    /// maps would make overlapping inner/outer ranges depend on HashMap
    /// iteration order.
    parent: Option<&'parent LowerEnv<'parent, A>>,
}

#[derive(Clone, Copy)]
struct FoldGroupLowerSpec<'arena, A: Hash + Eq + Clone> {
    loop_var: &'arena A,
    loop_width: usize,
    loop_signed: bool,
    start: &'arena BigInt,
    step: &'arena BigInt,
    trip_count: usize,
    entry_guard: NodeId,
    states: &'arena [SLTForFoldGroupState<A>],
}

impl<'arena, A: Hash + Eq + Clone> FoldGroupLowerSpec<'arena, A> {
    fn from_root(root: NodeId, arena: &'arena SLTNodeArena<A>) -> Option<Self> {
        let SLTNode::ForFoldGroup {
            loop_var,
            loop_width,
            loop_signed,
            start,
            step,
            trip_count,
            entry_guard,
            states,
        } = arena.get_checked(root)?
        else {
            return None;
        };
        Some(Self {
            loop_var,
            loop_width: *loop_width,
            loop_signed: *loop_signed,
            start,
            step,
            trip_count: *trip_count,
            entry_guard: *entry_guard,
            states,
        })
    }
}

/// A proven fixed-width first-true scan.  This is deliberately a transient
/// lowering plan rather than another SLT node: `ForFoldGroup` remains the
/// semantic representation and every near miss uses its generic counted-loop
/// lowering.
struct SLTOrScanPlan<A: Hash + Eq + Clone> {
    vector_state: usize,
    found_state: usize,
    width: usize,
    active: SLTVectorExpr<A>,
    source: SLTVectorExpr<A>,
    select_before: NodeId,
    select_first: NodeId,
}

impl SLTToSIRLowerer {
    pub fn new(four_state: bool) -> Self {
        Self {
            four_state,
            unpacked_input_element_widths: crate::HashMap::default(),
            cost_cache: RefCell::new(LoweringCostCache::default()),
            cache_insert_log: RefCell::new(Vec::new()),
            region_slice_cache: RefCell::new(crate::HashMap::default()),
            region_slice_cache_insert_log: RefCell::new(Vec::new()),
            mux_stats: tracing::enabled!(tracing::Level::DEBUG)
                .then(|| RefCell::new(MuxLowerStats::default())),
        }
    }

    pub fn with_unpacked_input_types<A: Hash + Eq + Clone>(
        mut self,
        arena: &SLTNodeArena<A>,
        element_widths: &crate::HashMap<A, usize>,
    ) -> Self {
        for (index, node) in arena.iter().enumerate() {
            let SLTNode::Input { variable, .. } = node else {
                continue;
            };
            if let Some(&element_width) = element_widths.get(variable) {
                self.unpacked_input_element_widths
                    .insert(NodeId(index), element_width);
            }
        }
        self
    }

    #[inline(always)]
    fn with_mux_stats(&self, update: impl FnOnce(&mut MuxLowerStats)) {
        if let Some(stats) = &self.mux_stats {
            update(&mut stats.borrow_mut());
        }
    }

    /// Recursively expand SLT nodes into SIR instructions
    pub fn lower<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        node: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        self.reset_cost_cache(node, arena, cache, true);
        self.lower_inner(builder, node, arena, cache, None, true)
    }

    pub fn lower_with_inputs<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        node: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        inputs: crate::HashMap<VarAtomBase<A>, RegisterId>,
    ) -> RegisterId {
        self.reset_cost_cache(node, arena, cache, false);
        let env = LowerEnv {
            inputs,
            parent: None,
        };
        self.lower_inner(builder, node, arena, cache, Some(&env), false)
    }

    /// Project a value which has already been lowered without retaining every
    /// projection at once.  Grouped folds use this after the packed result has
    /// been computed, immediately before the corresponding state Store.
    pub fn project_materialized<A>(
        &self,
        builder: &mut SIRBuilder<A>,
        value: RegisterId,
        access: BitAccess,
    ) -> RegisterId {
        let source_width = builder.register(&value).width();
        debug_assert!(access.msb < source_width);
        if access.lsb == 0 && access.msb + 1 == source_width {
            value
        } else {
            self.slice_reg(builder, value, &access)
        }
    }

    fn lower_inner<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        node: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        allow_cache: bool,
    ) -> RegisterId {
        if allow_cache {
            if let Some(reg) = cache.get(&node) {
                return *reg;
            }
        }

        if env.is_none()
            && let Some(reg) = self.try_lower_count_idiom(builder, node, arena, cache, allow_cache)
        {
            if allow_cache {
                let previous = cache.insert(node, reg);
                debug_assert!(previous.is_none());
                self.cache_insert_log.borrow_mut().push(node);
            }
            return reg;
        }

        let reg = match arena.get(node) {
            SLTNode::Input {
                variable: id,
                index,
                access,
                ..
            } => {
                if let Some(env) = env
                    && let Some(reg) =
                        self.lookup_override(builder, node, arena, cache, env, id, index, access)
                {
                    reg
                } else {
                    self.lower_input_for_node(builder, node, id, index, access, arena, cache, env)
                }
            }
            SLTNode::Constant(val, mask, width, _signed) => {
                let reg = if mask.is_zero() {
                    builder.alloc_bit(*width, false)
                } else {
                    builder.alloc_logic(*width)
                };
                builder.emit(SIRInstruction::Imm(
                    reg,
                    SIRValue::new_four_state(val.clone(), mask.clone()),
                ));
                reg
            }
            SLTNode::Binary(lhs, op, rhs) => {
                let mut l = self.lower_inner(builder, *lhs, arena, cache, env, allow_cache);
                let mut r = self.lower_inner(builder, *rhs, arena, cache, env, allow_cache);
                let width = self.get_width(node, arena);
                if matches!(
                    op,
                    BinaryOp::Eq
                        | BinaryOp::Ne
                        | BinaryOp::EqCase
                        | BinaryOp::NeCase
                        | BinaryOp::LtU
                        | BinaryOp::LtS
                        | BinaryOp::LeU
                        | BinaryOp::LeS
                        | BinaryOp::GtU
                        | BinaryOp::GtS
                        | BinaryOp::GeU
                        | BinaryOp::GeS
                        | BinaryOp::EqWildcard
                        | BinaryOp::NeWildcard
                ) {
                    let operand_width = builder
                        .register(&l)
                        .width()
                        .max(builder.register(&r).width());
                    let signed = matches!(
                        op,
                        BinaryOp::LtS | BinaryOp::LeS | BinaryOp::GtS | BinaryOp::GeS
                    ) || matches!(
                        op,
                        BinaryOp::Eq
                            | BinaryOp::Ne
                            | BinaryOp::EqCase
                            | BinaryOp::NeCase
                            | BinaryOp::EqWildcard
                            | BinaryOp::NeWildcard
                    ) && self.get_bound_signed(*lhs, arena)
                        && self.get_bound_signed(*rhs, arena);
                    l = self.cast_reg_width_ext(builder, l, operand_width, signed);
                    r = self.cast_reg_width_ext(builder, r, operand_width, signed);
                } else if matches!(
                    op,
                    BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS
                ) {
                    let signed = matches!(op, BinaryOp::DivS | BinaryOp::RemS);
                    l = self.cast_reg_width_ext(builder, l, width, signed);
                    r = self.cast_reg_width_ext(builder, r, width, signed);
                }
                let dest = builder.alloc_logic(width);
                builder.emit(SIRInstruction::Binary(dest, l, *op, r));
                dest
            }
            SLTNode::Unary(op, inner) => {
                let i = self.lower_inner(builder, *inner, arena, cache, env, allow_cache);
                let width = self.get_width(node, arena);
                let dest = if matches!(op, UnaryOp::ToTwoState) {
                    builder.alloc_bit(width, self.get_bound_signed(node, arena))
                } else {
                    builder.alloc_logic(width)
                };
                builder.emit(SIRInstruction::Unary(dest, *op, i));
                dest
            }
            SLTNode::Capture { expr, .. } => {
                self.lower_inner(builder, *expr, arena, cache, env, allow_cache)
            }
            SLTNode::Slice { expr, access } => {
                self.lower_slice_inner(builder, *expr, access, arena, cache, env, allow_cache)
            }
            SLTNode::Concat(parts) => {
                self.lower_concat_inner(builder, node, parts, arena, cache, env, allow_cache)
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => self.lower_mux_inner(
                builder,
                *cond,
                *then_expr,
                *else_expr,
                arena,
                cache,
                env,
                allow_cache,
            ),
            SLTNode::ForFold {
                loop_var,
                loop_width,
                loop_signed,
                start,
                end,
                inclusive,
                step,
                step_op,
                reverse,
                result,
                initials,
                updates,
                effects,
                continue_cond,
            } => self.lower_for_fold(
                builder,
                arena,
                cache,
                loop_var,
                *loop_width,
                *loop_signed,
                start,
                end,
                *inclusive,
                *step,
                *step_op,
                *reverse,
                result,
                initials,
                updates,
                effects,
                *continue_cond,
                env,
            ),
            SLTNode::ForFoldGroup { .. } => {
                let spec = FoldGroupLowerSpec::from_root(node, arena)
                    .expect("matched ForFoldGroup must remain present in its arena");
                self.lower_fold_group_specs(
                    builder,
                    arena,
                    cache,
                    std::slice::from_ref(&spec),
                    env,
                    allow_cache,
                )[0]
            }
        };

        if allow_cache {
            let previous = cache.insert(node, reg);
            debug_assert!(previous.is_none());
            self.cache_insert_log.borrow_mut().push(node);
        }
        reg
    }

    /// Get the width recorded by frontend construction.
    fn get_width<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> usize {
        crate::get_width(node, arena)
    }

    fn get_bound_signed<A: Hash + Eq + Clone + std::fmt::Debug>(
        &self,
        node: NodeId,
        arena: &SLTNodeArena<A>,
    ) -> bool {
        match arena.get(node) {
            SLTNode::Input { signed, .. } => *signed,
            SLTNode::Constant(_, _, _, signed) => *signed,
            SLTNode::Binary(lhs, op, rhs) => match op {
                BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::EqCase
                | BinaryOp::NeCase
                | BinaryOp::LtU
                | BinaryOp::LtS
                | BinaryOp::LeU
                | BinaryOp::LeS
                | BinaryOp::GtU
                | BinaryOp::GtS
                | BinaryOp::GeU
                | BinaryOp::GeS
                | BinaryOp::LogicAnd
                | BinaryOp::LogicOr
                | BinaryOp::EqWildcard
                | BinaryOp::NeWildcard
                | BinaryOp::DivU
                | BinaryOp::RemU => false,
                BinaryOp::DivS | BinaryOp::RemS => true,
                BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => self.get_bound_signed(*lhs, arena),
                BinaryOp::Add
                | BinaryOp::Sub
                | BinaryOp::Mul
                | BinaryOp::And
                | BinaryOp::Or
                | BinaryOp::Xor => {
                    self.get_bound_signed(*lhs, arena) && self.get_bound_signed(*rhs, arena)
                }
            },
            SLTNode::Unary(
                UnaryOp::LogicNot
                | UnaryOp::And
                | UnaryOp::Or
                | UnaryOp::Xor
                | UnaryOp::PopCount
                | UnaryOp::CountLeadingZeros
                | UnaryOp::CountTrailingZeros,
                _,
            ) => false,
            SLTNode::Unary(
                UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Minus | UnaryOp::BitNot,
                inner,
            ) => self.get_bound_signed(*inner, arena),
            SLTNode::Capture { expr, .. } => self.get_bound_signed(*expr, arena),
            SLTNode::Mux {
                then_expr,
                else_expr,
                ..
            } => {
                self.get_bound_signed(*then_expr, arena) && self.get_bound_signed(*else_expr, arena)
            }
            SLTNode::ForFold {
                loop_signed,
                result,
                ..
            } => match result {
                crate::SLTForFoldResult::State(_) => *loop_signed,
                crate::SLTForFoldResult::Transient { .. } => false,
            },
            // The grouped result has concat layout and is therefore unsigned,
            // independently of the loop counter's signedness.
            SLTNode::ForFoldGroup { .. } => false,
            // Verilog/Veryl bit- and part-select expressions are unsigned even when
            // the source signal is signed.
            SLTNode::Slice { .. } => false,
            SLTNode::Concat(_) => false,
        }
    }
}

impl Drop for SLTToSIRLowerer {
    fn drop(&mut self) {
        let Some(stats) = &self.mux_stats else {
            return;
        };
        let stats = stats.borrow();
        tracing::debug!(
            "[mux-lower-stats] normal_seen={} slice_seen={} constant_folded={} cfg_cost={} cfg_div_rem={} cfg_slice_cost={} cfg_slice_div_rem={} shared_nodes_hoisted={} kept_four_state={} kept_impure={} kept_dynamic_env={} kept_unprofitable={} kept_deep_shared={} biased_conditions={} owned_cost_sum={} owned_cost_max={} unprofitable_buckets_0_7_15_31_63_127_255_inf={:?}",
            stats.normal_seen,
            stats.slice_seen,
            stats.constant_folded,
            stats.cfg_cost,
            stats.cfg_div_rem,
            stats.cfg_slice_cost,
            stats.cfg_slice_div_rem,
            stats.shared_nodes_hoisted,
            stats.kept_four_state,
            stats.kept_impure,
            stats.kept_dynamic_env,
            stats.kept_unprofitable,
            stats.kept_deep_shared,
            stats.biased_conditions,
            stats.owned_cost_sum,
            stats.owned_cost_max,
            stats.unprofitable_cost_buckets,
        );
    }
}

#[cfg(test)]
mod tests;
