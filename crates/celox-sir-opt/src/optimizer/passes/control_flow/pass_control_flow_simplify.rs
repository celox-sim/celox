//! Sparse conditional constant propagation and CFG cleanup for SIR.
//!
//! SIR is produced after a scheduler has already materialized a large amount
//! of mux-shaped dataflow.  A scheduler is allowed to do that, but the
//! resulting CFG still contains ordinary control-flow facts.  This pass uses
//! those facts in the usual compiler way: it propagates constants through
//! executable edges, folds only proven constant branches/muxes, removes
//! unreachable blocks, and then removes the now-dead pure definitions.
//!
//! The analysis is sparse in the SCCP sense.  It visits a block when the block
//! becomes executable or when a block argument/value lattice changes; it does
//! not enumerate paths.  Thus loops and joins are handled by a finite lattice
//! (`Unknown`, one exact constant, `Overdefined`) rather than by path cloning.

use super::pass_manager::ExecutionUnitPass;
use super::shared::{collect_all_used_registers, def_reg};
use crate::PassOptions;
use crate::ir::cfg::SirCfg;
use crate::ir::*;
use crate::{HashMap, HashSet};
use num_bigint::BigUint;
use num_traits::{One, Zero};
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

pub(in crate::optimizer) struct ControlFlowSimplifyPass;
pub(in crate::optimizer) struct PostGvnCfgCleanupPass;

#[derive(Clone, Debug, PartialEq, Eq)]
enum LatticeValue {
    Unknown,
    Constant(SIRValue),
    Overdefined,
}

#[derive(Clone)]
struct Edge {
    target: BlockId,
    arguments: Vec<RegisterId>,
}

struct Analysis {
    executable: HashSet<BlockId>,
    values: HashMap<RegisterId, LatticeValue>,
}

impl ExecutionUnitPass for ControlFlowSimplifyPass {
    fn name(&self) -> &'static str {
        "control_flow_simplify"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        if eu.blocks.is_empty() || eu.verify_result().is_err() {
            return;
        }

        let mut changed = false;
        loop {
            let analysis = analyze(eu, options.four_state);

            // Rewrite only from the final SCCP lattice.  In particular, an
            // overdefined condition never gets treated as a boolean just
            // because one predecessor happened to carry a constant value.
            let sccp_changed = apply_sccp_rewrites(eu, &analysis, options.four_state);

            // Constant propagation handles values known independently of
            // control flow.  The second proof is deliberately different: a
            // block reached only through a dominating branch edge has a known
            // predicate even when the predicate itself is dynamic.  This is
            // the CFG fact needed to discard an arm that was materialized
            // before the branch.  It uses dominance, not a single-predecessor
            // or same-block heuristic.
            let dominated_mux_changed = simplify_dominated_muxes(eu, options.four_state);
            // GVN gives repeated exact state loads one SSA name.  Use that
            // identity here to recover ordinary case semantics from a chain
            // of independently lowered equality branches: once one equality
            // is true, later comparisons against different exact constants
            // cannot be true and their pure decision blocks can be bypassed.
            let correlated_thread_changed = !options.four_state && thread_correlated_case_edges(eu);
            if !sccp_changed && !dominated_mux_changed && !correlated_thread_changed {
                break;
            }
            changed = true;
        }

        finish_cfg_rewrites(eu, changed);

        // Four-state mode is intentionally not used as a blanket early exit:
        // exact, mask-free branch constants are valid in four-state SIR too.
        // Muxes with an unknown condition never enter `exact_truth`, so their
        // X/Z merge semantics remain untouched.
    }
}

/// Revisit only CFG facts which GVN can newly expose.
///
/// The main pipeline already ran SCCP before GVN.  Re-running the complete
/// control-flow pass after every GVN rebuilds the SCCP lattice even though GVN
/// neither creates constants nor changes executable edges.  It can, however,
/// give repeated conditions and exact state loads one SSA identity.  Those
/// identities are precisely what dominated-Mux cleanup and correlated case
/// threading consume, so keep that smaller fixed point as an explicit pass.
impl ExecutionUnitPass for PostGvnCfgCleanupPass {
    fn name(&self) -> &'static str {
        "post_gvn_cfg_cleanup"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        if eu.blocks.is_empty() || eu.verify_result().is_err() {
            return;
        }

        let mut changed = false;
        loop {
            let dominated_mux_changed = simplify_dominated_muxes(eu, options.four_state);
            let correlated_thread_changed = !options.four_state && thread_correlated_case_edges(eu);
            if !dominated_mux_changed && !correlated_thread_changed {
                break;
            }
            changed = true;
        }
        finish_cfg_rewrites(eu, changed);
    }
}

fn finish_cfg_rewrites(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, changed: bool) {
    if !changed {
        return;
    }

    remove_unreachable_blocks(eu);

    // Mux replacement and threaded edges disconnect whole load/expression
    // DAGs.  The existing mark/sweep is linear in def-use edges and treats
    // loads as pure SIR values, which is exactly what is needed here.
    super::pass_vectorize_concat::remove_dead_definitions(eu);
    trim_dead_register_types(eu);

    debug_assert_eq!(eu.verify_result(), Ok(()));
}

fn remove_unreachable_blocks(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) -> bool {
    let mut reachable = HashSet::default();
    let mut work = vec![eu.entry_block_id];
    while let Some(block_id) = work.pop() {
        if !reachable.insert(block_id) {
            continue;
        }
        let Some(block) = eu.blocks.get(&block_id) else {
            continue;
        };
        match &block.terminator {
            SIRTerminator::Jump(target, _) => work.push(*target),
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => {
                work.push(true_block.0);
                work.push(false_block.0);
            }
            SIRTerminator::Switch { cases, default, .. } => {
                work.extend(cases.iter().map(|case| case.target));
                work.push(*default);
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    let unreachable = eu
        .blocks
        .keys()
        .copied()
        .filter(|block| !reachable.contains(block))
        .collect::<Vec<_>>();
    let changed = !unreachable.is_empty();
    for block in unreachable {
        eu.blocks.remove(&block);
    }
    changed
}

fn apply_sccp_rewrites(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    analysis: &Analysis,
    four_state: bool,
) -> bool {
    let mut changed = false;
    let mut executable = analysis.executable.iter().copied().collect::<Vec<_>>();
    executable.sort_unstable_by_key(|id| id.0);
    for block_id in executable {
        let Some(block) = eu.blocks.get_mut(&block_id) else {
            continue;
        };

        for instruction in &mut block.instructions {
            let replacement = def_reg(instruction)
                .and_then(|dst| match analysis.values.get(&dst) {
                    Some(LatticeValue::Constant(value))
                        if !matches!(
                            instruction,
                            SIRInstruction::Imm(_, current) if current == value
                        ) =>
                    {
                        Some(SIRInstruction::Imm(dst, value.clone()))
                    }
                    _ => None,
                })
                .or_else(|| {
                    algebraic_replacement(instruction, analysis, &eu.register_map, four_state)
                })
                .or_else(|| match instruction {
                    SIRInstruction::Mux(dst, condition, then_value, else_value) => {
                        if then_value == else_value {
                            Some(SIRInstruction::Unary(*dst, UnaryOp::Ident, *then_value))
                        } else {
                            exact_truth(analysis.values.get(condition)).map(|truth| {
                                SIRInstruction::Unary(
                                    *dst,
                                    UnaryOp::Ident,
                                    if truth { *then_value } else { *else_value },
                                )
                            })
                        }
                    }
                    _ => None,
                });
            if let Some(replacement) = replacement {
                *instruction = replacement;
                changed = true;
            }
        }

        let replacement = match &block.terminator {
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => exact_truth(analysis.values.get(cond)).map(|truth| {
                if truth {
                    SIRTerminator::Jump(true_block.0, true_block.1.clone())
                } else {
                    SIRTerminator::Jump(false_block.0, false_block.1.clone())
                }
            }),
            _ => None,
        };
        if let Some(terminator) = replacement {
            block.terminator = terminator;
            changed = true;
        }
    }

    // The executable-block set was computed from the same monotone edge
    // analysis, so these blocks cannot be reached after the folded edges are
    // installed.  Removing them before DCE is important: otherwise a later
    // native lowering still sees their definitions in the EU.
    let unreachable = eu
        .blocks
        .keys()
        .copied()
        .filter(|id| !analysis.executable.contains(id))
        .collect::<Vec<_>>();
    if !unreachable.is_empty() {
        changed = true;
        for block_id in unreachable {
            eu.blocks.remove(&block_id);
        }
    }
    changed
}

fn algebraic_replacement(
    instruction: &SIRInstruction<RegionedAbsoluteAddr>,
    analysis: &Analysis,
    types: &HashMap<RegisterId, RegisterType>,
    four_state: bool,
) -> Option<SIRInstruction<RegionedAbsoluteAddr>> {
    let SIRInstruction::Binary(dst, lhs, op, rhs) = instruction else {
        return None;
    };
    let exact = |register: RegisterId| match analysis.values.get(&register) {
        Some(LatticeValue::Constant(value)) if value.mask.is_zero() => Some(value),
        _ => None,
    };
    let is_zero = |register| exact(register).is_some_and(|value| value.payload.is_zero());
    let is_one = |register| exact(register).is_some_and(|value| value.payload.is_one());
    let same_type = |a, b| {
        types
            .get(&a)
            .zip(types.get(&b))
            .is_some_and(|(a, b)| a == b)
    };
    let identity = |source| {
        same_type(*dst, source).then_some(SIRInstruction::Unary(*dst, UnaryOp::Ident, source))
    };

    if lhs == rhs {
        match op {
            BinaryOp::And | BinaryOp::Or => return identity(*lhs),
            BinaryOp::Sub | BinaryOp::Xor if !four_state => {
                return Some(SIRInstruction::Imm(*dst, SIRValue::new(0u8)));
            }
            _ => {}
        }
    }

    match op {
        BinaryOp::LogicAnd if is_one(*lhs) => Some(SIRInstruction::Unary(*dst, UnaryOp::Or, *rhs)),
        BinaryOp::LogicAnd if is_one(*rhs) => Some(SIRInstruction::Unary(*dst, UnaryOp::Or, *lhs)),
        BinaryOp::LogicOr if is_zero(*lhs) => Some(SIRInstruction::Unary(*dst, UnaryOp::Or, *rhs)),
        BinaryOp::LogicOr if is_zero(*rhs) => Some(SIRInstruction::Unary(*dst, UnaryOp::Or, *lhs)),
        BinaryOp::Add | BinaryOp::Or | BinaryOp::Xor if is_zero(*lhs) => identity(*rhs),
        BinaryOp::Add | BinaryOp::Or | BinaryOp::Xor if is_zero(*rhs) => identity(*lhs),
        BinaryOp::Mul if is_one(*lhs) => identity(*rhs),
        BinaryOp::Mul if is_one(*rhs) => identity(*lhs),
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar if is_zero(*rhs) => identity(*lhs),
        BinaryOp::And => {
            let all_ones = |constant: RegisterId, value: RegisterId| {
                let width = types.get(dst)?.width();
                (same_type(*dst, constant)
                    && same_type(*dst, value)
                    && exact(constant)?.payload == width_mask(width))
                .then_some(SIRInstruction::Unary(*dst, UnaryOp::Ident, value))
            };
            all_ones(*lhs, *rhs).or_else(|| all_ones(*rhs, *lhs))
        }
        _ => None,
    }
}

fn simplify_dominated_muxes(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
) -> bool {
    if !eu.blocks.values().any(|block| {
        block
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Mux(..)))
    }) {
        return false;
    }
    let Ok(cfg) = SirCfg::analyze(eu) else {
        return false;
    };
    let parameter_facts = collect_edge_parameter_facts(eu);
    let mut branch_facts = HashMap::<RegisterId, Vec<(BlockId, BlockId, bool)>>::default();
    for block in eu.blocks.values() {
        if let SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } = &block.terminator
        {
            branch_facts.entry(*cond).or_default().extend([
                (block.id, true_block.0, true),
                (block.id, false_block.0, false),
            ]);
        }
    }

    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|id| id.0);
    let mut changed = false;
    for block_id in block_ids {
        let Some(block) = eu.blocks.get_mut(&block_id) else {
            continue;
        };
        let mut aliases = HashMap::<RegisterId, (RegisterId, bool)>::default();
        for instruction in &mut block.instructions {
            if let SIRInstruction::Mux(dst, condition, then_value, else_value) = instruction {
                let (root, inverted) = resolve_condition_alias(*condition, &aliases);
                let mut proven = parameter_facts
                    .get(&(block_id, root))
                    .copied()
                    .map(|truth| truth ^ inverted);
                if let Some(facts) = branch_facts.get(&root) {
                    for &(source, successor, truth) in facts {
                        if source != block_id
                            && branch_edge_dominates_block(&cfg, source, successor, block_id)
                        {
                            let truth = truth ^ inverted;
                            if proven.is_some_and(|previous| previous != truth) {
                                proven = None;
                                break;
                            }
                            proven = Some(truth);
                        }
                    }
                }
                if let Some(truth) = proven {
                    *instruction = SIRInstruction::Unary(
                        *dst,
                        UnaryOp::Ident,
                        if truth { *then_value } else { *else_value },
                    );
                    changed = true;
                }
            }

            match instruction {
                SIRInstruction::Unary(dst, UnaryOp::Ident | UnaryOp::ToTwoState, source) => {
                    if let Some(&(root, inverted)) = aliases.get(source) {
                        aliases.insert(*dst, (root, inverted));
                    } else {
                        aliases.insert(*dst, (*source, false));
                    }
                }
                SIRInstruction::Unary(dst, UnaryOp::LogicNot, source) if !four_state => {
                    if let Some(&(root, inverted)) = aliases.get(source) {
                        aliases.insert(*dst, (root, !inverted));
                    } else {
                        aliases.insert(*dst, (*source, true));
                    }
                }
                _ => {}
            }
        }
    }
    changed
}

/// Whether one particular control-flow edge proves a predicate at `block`.
///
/// Node dominance of the edge's successor alone is insufficient.  A branch
/// target can also be a loop exit / join target: the target dominates itself,
/// while a back or side edge can reach it without taking this branch edge.
/// That was enough for the old check to fold a Mux to the branch's default
/// arm, even when the loop path reached the same block with the opposite
/// predicate value.
///
/// An edge `source -> successor` dominates `successor` exactly when every
/// other predecessor of `successor` is already dominated by `successor`.
/// Such predecessors are loop-back edges, so they cannot be used to enter the
/// successor for the first time.  Combined with ordinary node dominance for a
/// descendant, this proves the edge was traversed on every path to `block`.
fn branch_edge_dominates_block(
    cfg: &SirCfg,
    source: BlockId,
    successor: BlockId,
    block: BlockId,
) -> bool {
    if source == successor || cfg.dominates(successor, source) || !cfg.dominates(successor, block) {
        return false;
    }
    let (Some(source), Some(successor)) = (cfg.block_index(source), cfg.block_index(successor))
    else {
        return false;
    };
    cfg.predecessors[successor].iter().all(|&predecessor| {
        predecessor == source || cfg.dominates(cfg.block_ids[successor], cfg.block_ids[predecessor])
    })
}

/// Prove a predicate carried through an SSA block argument.  Every incoming
/// edge must pass the branch condition itself, and every such edge must carry
/// the same truth value.  This is intentionally an all-incoming-edge proof;
/// one unproven edge makes the result unknown at the join.
fn collect_edge_parameter_facts(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<(BlockId, RegisterId), bool> {
    let mut incoming =
        HashMap::<BlockId, Vec<(Vec<RegisterId>, Option<(RegisterId, bool)>)>>::default();
    for block in eu.blocks.values() {
        match &block.terminator {
            SIRTerminator::Jump(target, arguments) => {
                incoming
                    .entry(*target)
                    .or_default()
                    .push((arguments.clone(), None));
            }
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                incoming
                    .entry(true_block.0)
                    .or_default()
                    .push((true_block.1.clone(), Some((*cond, true))));
                incoming
                    .entry(false_block.0)
                    .or_default()
                    .push((false_block.1.clone(), Some((*cond, false))));
            }
            SIRTerminator::Switch { cases, default, .. } => {
                for case in cases {
                    incoming
                        .entry(case.target)
                        .or_default()
                        .push((Vec::new(), None));
                }
                incoming
                    .entry(*default)
                    .or_default()
                    .push((Vec::new(), None));
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }

    let mut facts = HashMap::default();
    for (&target, edges) in &incoming {
        let Some(block) = eu.blocks.get(&target) else {
            continue;
        };
        for (index, &parameter) in block.params.iter().enumerate() {
            let mut proven = None;
            let mut valid = true;
            for (arguments, branch) in edges {
                let Some((condition, truth)) = branch else {
                    valid = false;
                    break;
                };
                if arguments.get(index) != Some(condition) {
                    valid = false;
                    break;
                }
                if proven.is_some_and(|previous| previous != *truth) {
                    valid = false;
                    break;
                }
                proven = Some(*truth);
            }
            if valid {
                if let Some(proven) = proven {
                    facts.insert((target, parameter), proven);
                }
            }
        }
    }
    facts
}

fn resolve_condition_alias(
    mut register: RegisterId,
    aliases: &HashMap<RegisterId, (RegisterId, bool)>,
) -> (RegisterId, bool) {
    let mut inverted = false;
    let mut steps = 0usize;
    while let Some(&(next, next_inverted)) = aliases.get(&register) {
        register = next;
        inverted ^= next_inverted;
        steps += 1;
        if steps > aliases.len() {
            break;
        }
    }
    (register, inverted)
}

#[derive(Clone, Debug)]
struct ExactCaseConstant {
    fingerprint: u64,
    width: usize,
    payload: Arc<[u64]>,
}

impl ExactCaseConstant {
    fn new(width: usize, payload: Vec<u64>) -> Self {
        Self {
            fingerprint: fxhash::hash64(&(width, &payload)),
            width,
            payload: payload.into(),
        }
    }
}

impl PartialEq for ExactCaseConstant {
    fn eq(&self, other: &Self) -> bool {
        self.fingerprint == other.fingerprint
            && self.width == other.width
            && (Arc::ptr_eq(&self.payload, &other.payload) || self.payload == other.payload)
    }
}

impl Eq for ExactCaseConstant {}

impl Hash for ExactCaseConstant {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.fingerprint);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CorrelatedFacts {
    booleans: HashMap<RegisterId, bool>,
    equalities: HashMap<RegisterId, ExactCaseConstant>,
}

impl CorrelatedFacts {
    fn meet_with(&mut self, other: &Self) {
        self.booleans
            .retain(|register, truth| other.booleans.get(register) == Some(truth));
        self.equalities
            .retain(|register, constant| other.equalities.get(register) == Some(constant));
    }
}

#[derive(Clone, Copy)]
struct CorrelatedEdge {
    target: usize,
    truth: Option<bool>,
    has_arguments: bool,
}

#[derive(Clone)]
struct CaseDecision {
    block: usize,
    selector: RegisterId,
    constant: ExactCaseConstant,
    unmatched_target: usize,
}

struct CaseChain {
    decisions: Vec<usize>,
    final_target: usize,
    constant_positions: HashMap<ExactCaseConstant, Vec<usize>>,
}

#[derive(Clone, Copy)]
struct CaseThreadPlan {
    source: usize,
    edge: usize,
    target: usize,
}

/// Thread edges carrying an exact selector value over the remaining tests in
/// a lowered case spine.
///
/// This is edge-sensitive jump threading, but it deliberately indexes an
/// entire equality spine before rewriting any edge.  Walking the suffix once
/// per taken arm is quadratic for large generated cases; the indexed form is
/// linear in CFG size plus one constant lookup per candidate edge.
fn thread_correlated_case_edges(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) -> bool {
    let Ok(cfg) = SirCfg::analyze(eu) else {
        return false;
    };
    let definitions = instruction_definition_locations(eu);
    let repeated_booleans = repeated_branch_predicates(eu, &definitions);
    let edges = correlated_edges(eu, &cfg);
    let edge_facts = analyze_correlated_facts(eu, &cfg, &definitions, &repeated_booleans, &edges);
    let transparent_targets = transparent_jump_targets(eu, &cfg);
    let uses = register_use_blocks(eu);
    let definition_blocks = register_definition_blocks(eu);

    let mut decisions = Vec::<CaseDecision>::new();
    let mut decision_for_block = vec![None; cfg.block_ids.len()];
    for (block, decision_slot) in decision_for_block.iter_mut().enumerate() {
        if cfg.sccs[cfg.scc_for_block[block]].cyclic {
            continue;
        }
        let basic_block = &eu.blocks[&cfg.block_ids[block]];
        if !basic_block.params.is_empty()
            || !basic_block
                .instructions
                .iter()
                .all(is_threadable_decision_instruction)
        {
            continue;
        }
        let SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } = &basic_block.terminator
        else {
            continue;
        };
        if !true_block.1.is_empty() || !false_block.1.is_empty() {
            continue;
        }
        let Some((selector, constant, equal_when_true)) =
            exact_equality_predicate(eu, &definitions, *cond)
        else {
            continue;
        };
        let unmatched = if equal_when_true {
            false_block.0
        } else {
            true_block.0
        };
        let Some(unmatched) = cfg.block_index(unmatched) else {
            continue;
        };
        let decision = decisions.len();
        *decision_slot = Some(decision);
        decisions.push(CaseDecision {
            block,
            selector,
            constant,
            unmatched_target: transparent_targets[unmatched],
        });
    }
    if decisions.is_empty() {
        return false;
    }

    // A case spine has one distinguished "not matched" successor per test.
    // Split at merges so each indexed chain is linear and has one unambiguous
    // final target.
    let mut next = vec![None; decisions.len()];
    let mut incoming = vec![0usize; decisions.len()];
    for (decision, node) in decisions.iter().enumerate() {
        let Some(successor) = decision_for_block[node.unmatched_target] else {
            continue;
        };
        if decisions[successor].selector == node.selector {
            next[decision] = Some(successor);
            incoming[successor] += 1;
        }
    }

    let mut chains = Vec::<CaseChain>::new();
    let mut membership = vec![None; decisions.len()];
    let mut visited = vec![false; decisions.len()];
    let starts = (0..decisions.len())
        .filter(|&decision| incoming[decision] != 1)
        .collect::<Vec<_>>();
    for start in starts {
        build_case_chain(
            start,
            &decisions,
            &next,
            &incoming,
            &mut visited,
            &mut membership,
            &mut chains,
        );
    }
    // Cyclic SCCs were excluded, so this is only a defensive fallback for an
    // unusual split graph whose predecessor was rejected as a decision.
    for start in 0..decisions.len() {
        if !visited[start] {
            build_case_chain(
                start,
                &decisions,
                &next,
                &incoming,
                &mut visited,
                &mut membership,
                &mut chains,
            );
        }
    }

    let mut plans = Vec::<(CaseThreadPlan, usize, usize)>::new();
    for source in 0..cfg.block_ids.len() {
        if cfg.sccs[cfg.scc_for_block[source]].cyclic {
            continue;
        }
        for (edge, outgoing) in edges[source].iter().enumerate() {
            if outgoing.has_arguments {
                continue;
            }
            let target = transparent_targets[outgoing.target];
            let Some(decision) = decision_for_block[target] else {
                continue;
            };
            let Some(facts) = edge_facts[source][edge].as_ref() else {
                continue;
            };
            let node = &decisions[decision];
            let Some(known) = facts.equalities.get(&node.selector) else {
                continue;
            };
            let Some((chain, position)) = membership[decision] else {
                continue;
            };
            let chain_info = &chains[chain];
            if constant_occurs_at_or_after(chain_info, known, position) {
                continue;
            }
            let final_target = chain_info.final_target;
            if final_target == outgoing.target
                || !eu.blocks[&cfg.block_ids[final_target]].params.is_empty()
            {
                continue;
            }
            plans.push((
                CaseThreadPlan {
                    source,
                    edge,
                    target: final_target,
                },
                chain,
                position,
            ));
        }
    }
    if plans.is_empty() {
        return false;
    }

    // Most generated case predicates die inside their own decision spine.
    // Summarize that common case once from tail to head; otherwise validating
    // every taken arm by re-walking its entire suffix is quadratic even though
    // chain discovery and target lookup are linear.  Suffixes with a real
    // chain-external use still take the complete SSA/rematerialization path
    // below.
    let suffix_has_external_uses = case_suffix_external_uses(eu, &cfg, &decisions, &chains, &uses);

    // A skipped definition must not become unavailable after threading.
    // Rematerialize a pure live-out DAG at each external use instead of
    // hoisting it: this repairs SSA while cutting the long predicate live
    // ranges which caused the case spine to survive in the first place.
    let mut accepted = Vec::new();
    let mut rematerialize = HashMap::<(RegisterId, BlockId), Vec<RegisterId>>::default();
    for (plan, chain, position) in plans {
        if !suffix_has_external_uses[chain][position] {
            accepted.push(plan);
            continue;
        }
        let skipped = chains[chain].decisions[position..]
            .iter()
            .map(|&decision| cfg.block_ids[decisions[decision].block])
            .collect::<HashSet<_>>();
        let mut safe = true;
        let mut edge_rematerialize = Vec::new();
        for &block_id in &skipped {
            for instruction in &eu.blocks[&block_id].instructions {
                let Some(destination) = def_reg(instruction) else {
                    continue;
                };
                for &use_block in uses.get(&destination).into_iter().flatten() {
                    if skipped.contains(&use_block) {
                        continue;
                    }
                    let Some(dag) = rematerializable_case_dag(
                        eu,
                        &cfg,
                        &definitions,
                        &definition_blocks,
                        &skipped,
                        plan.source,
                        destination,
                    ) else {
                        safe = false;
                        break;
                    };
                    edge_rematerialize.push(((destination, use_block), dag));
                }
                if !safe {
                    break;
                }
            }
            if !safe {
                break;
            }
        }
        if safe {
            accepted.push(plan);
            for (request, dag) in edge_rematerialize {
                rematerialize.entry(request).or_insert(dag);
            }
        }
    }
    if accepted.is_empty() {
        return false;
    }
    if !rematerialize.is_empty() {
        rematerialize_case_values(eu, &definitions, rematerialize);
        return true;
    }

    let mut changed = false;
    for plan in accepted {
        let block_id = cfg.block_ids[plan.source];
        let target = cfg.block_ids[plan.target];
        let Some(block) = eu.blocks.get_mut(&block_id) else {
            continue;
        };
        let rewritten = match (&mut block.terminator, plan.edge) {
            (SIRTerminator::Jump(destination, arguments), 0) if arguments.is_empty() => {
                *destination = target;
                true
            }
            (SIRTerminator::Branch { true_block, .. }, 0) if true_block.1.is_empty() => {
                true_block.0 = target;
                true
            }
            (SIRTerminator::Branch { false_block, .. }, 1) if false_block.1.is_empty() => {
                false_block.0 = target;
                true
            }
            _ => false,
        };
        changed |= rewritten;
    }
    changed
}

fn case_suffix_external_uses(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    decisions: &[CaseDecision],
    chains: &[CaseChain],
    uses: &HashMap<RegisterId, HashSet<BlockId>>,
) -> Vec<Vec<bool>> {
    chains
        .iter()
        .map(|chain| {
            let chain_blocks = chain
                .decisions
                .iter()
                .map(|&decision| cfg.block_ids[decisions[decision].block])
                .collect::<HashSet<_>>();
            let mut suffix = vec![false; chain.decisions.len()];
            let mut has_external = false;
            for (position, &decision) in chain.decisions.iter().enumerate().rev() {
                let block = &eu.blocks[&cfg.block_ids[decisions[decision].block]];
                has_external |= block.instructions.iter().any(|instruction| {
                    def_reg(instruction).is_some_and(|destination| {
                        uses.get(&destination)
                            .into_iter()
                            .flatten()
                            .any(|use_block| !chain_blocks.contains(use_block))
                    })
                });
                suffix[position] = has_external;
            }
            suffix
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn build_case_chain(
    start: usize,
    decisions: &[CaseDecision],
    next: &[Option<usize>],
    incoming: &[usize],
    visited: &mut [bool],
    membership: &mut [Option<(usize, usize)>],
    chains: &mut Vec<CaseChain>,
) {
    if visited[start] {
        return;
    }
    let chain_id = chains.len();
    let mut chain = Vec::new();
    let mut current = start;
    loop {
        if visited[current] {
            break;
        }
        visited[current] = true;
        membership[current] = Some((chain_id, chain.len()));
        chain.push(current);
        let Some(successor) = next[current] else {
            break;
        };
        if incoming[successor] != 1 || visited[successor] {
            break;
        }
        current = successor;
    }
    let final_target =
        decisions[*chain.last().expect("a case chain has a first decision")].unmatched_target;
    let mut constant_positions = HashMap::<ExactCaseConstant, Vec<usize>>::default();
    for (position, &decision) in chain.iter().enumerate() {
        constant_positions
            .entry(decisions[decision].constant.clone())
            .or_default()
            .push(position);
    }
    chains.push(CaseChain {
        decisions: chain,
        final_target,
        constant_positions,
    });
}

fn constant_occurs_at_or_after(
    chain: &CaseChain,
    constant: &ExactCaseConstant,
    position: usize,
) -> bool {
    let Some(positions) = chain.constant_positions.get(constant) else {
        return false;
    };
    let first_possible = positions.partition_point(|candidate| *candidate < position);
    first_possible < positions.len()
}

fn is_threadable_decision_instruction(instruction: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    matches!(
        instruction,
        SIRInstruction::Imm(..)
            | SIRInstruction::Binary(..)
            | SIRInstruction::Unary(..)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
    )
}

fn instruction_definition_locations(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, (BlockId, usize)> {
    let mut definitions = HashMap::default();
    for block in eu.blocks.values() {
        for (index, instruction) in block.instructions.iter().enumerate() {
            if let Some(destination) = def_reg(instruction) {
                definitions.insert(destination, (block.id, index));
            }
        }
    }
    definitions
}

fn correlated_edges(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
) -> Vec<Vec<CorrelatedEdge>> {
    cfg.block_ids
        .iter()
        .map(|block_id| match &eu.blocks[block_id].terminator {
            SIRTerminator::Jump(target, arguments) => vec![CorrelatedEdge {
                target: cfg
                    .block_index(*target)
                    .expect("SirCfg validated every jump target"),
                truth: None,
                has_arguments: !arguments.is_empty(),
            }],
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => vec![
                CorrelatedEdge {
                    target: cfg
                        .block_index(true_block.0)
                        .expect("SirCfg validated every true target"),
                    truth: Some(true),
                    has_arguments: !true_block.1.is_empty(),
                },
                CorrelatedEdge {
                    target: cfg
                        .block_index(false_block.0)
                        .expect("SirCfg validated every false target"),
                    truth: Some(false),
                    has_arguments: !false_block.1.is_empty(),
                },
            ],
            SIRTerminator::Switch { cases, default, .. } => cases
                .iter()
                .map(|case| CorrelatedEdge {
                    target: cfg
                        .block_index(case.target)
                        .expect("SirCfg validated every switch target"),
                    truth: None,
                    has_arguments: false,
                })
                .chain(std::iter::once(CorrelatedEdge {
                    target: cfg
                        .block_index(*default)
                        .expect("SirCfg validated the switch default target"),
                    truth: None,
                    has_arguments: false,
                }))
                .collect(),
            SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
        })
        .collect()
}

fn analyze_correlated_facts(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    definitions: &HashMap<RegisterId, (BlockId, usize)>,
    repeated_booleans: &HashSet<RegisterId>,
    edges: &[Vec<CorrelatedEdge>],
) -> Vec<Vec<Option<Arc<CorrelatedFacts>>>> {
    let mut incoming = vec![Vec::<(usize, usize)>::new(); cfg.block_ids.len()];
    for (source, outgoing) in edges.iter().enumerate() {
        for (edge, target) in outgoing.iter().enumerate() {
            incoming[target.target].push((source, edge));
        }
    }

    let mut entries = vec![None; cfg.block_ids.len()];
    entries[0] = Some(Arc::new(CorrelatedFacts::default()));
    let mut edge_facts = edges
        .iter()
        .map(|outgoing| vec![None; outgoing.len()])
        .collect::<Vec<_>>();
    let mut worklist = VecDeque::from([0usize]);
    let mut queued = vec![false; cfg.block_ids.len()];
    queued[0] = true;

    while let Some(source) = worklist.pop_front() {
        queued[source] = false;
        for (edge, outgoing) in edges[source].iter().enumerate() {
            let contribution = entries[source].as_ref().and_then(|facts| {
                facts_on_correlated_edge(
                    eu,
                    definitions,
                    repeated_booleans,
                    cfg.block_ids[source],
                    outgoing.truth,
                    facts,
                )
            });
            if edge_facts[source][edge] == contribution {
                continue;
            }
            edge_facts[source][edge] = contribution;
            let target = outgoing.target;
            if target == 0 {
                continue;
            }
            let mut next_entry = None::<Arc<CorrelatedFacts>>;
            for &(predecessor, incoming_edge) in &incoming[target] {
                let Some(facts) = edge_facts[predecessor][incoming_edge].as_ref() else {
                    continue;
                };
                if let Some(intersection) = next_entry.as_mut() {
                    if !Arc::ptr_eq(intersection, facts) {
                        let mut narrowed = (**intersection).clone();
                        narrowed.meet_with(facts);
                        *intersection = Arc::new(narrowed);
                    }
                } else {
                    next_entry = Some(Arc::clone(facts));
                }
            }
            if entries[target] != next_entry {
                entries[target] = next_entry;
                if !queued[target] {
                    queued[target] = true;
                    worklist.push_back(target);
                }
            }
        }
    }
    edge_facts
}

fn facts_on_correlated_edge(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, (BlockId, usize)>,
    repeated_booleans: &HashSet<RegisterId>,
    source: BlockId,
    truth: Option<bool>,
    facts: &Arc<CorrelatedFacts>,
) -> Option<Arc<CorrelatedFacts>> {
    let Some(truth) = truth else {
        return Some(Arc::clone(facts));
    };
    let SIRTerminator::Branch { cond, .. } = &eu.blocks[&source].terminator else {
        return None;
    };
    if known_correlated_condition(eu, definitions, facts, *cond).is_some_and(|known| known != truth)
    {
        return None;
    }

    let mut result = None;
    if let Some((selector, constant, equal_when_true)) =
        exact_equality_predicate(eu, definitions, *cond)
    {
        if truth == equal_when_true {
            if facts
                .equalities
                .get(&selector)
                .is_some_and(|known| known != &constant)
            {
                return None;
            }
            let mut updated = (**facts).clone();
            updated.equalities.insert(selector, constant);
            result = Some(updated);
        }
    } else {
        let (root, inverted) = resolve_thread_condition(eu, definitions, *cond);
        if repeated_booleans.contains(&root) {
            let root_truth = truth ^ inverted;
            if facts
                .booleans
                .get(&root)
                .is_some_and(|known| *known != root_truth)
            {
                return None;
            }
            let mut updated = (**facts).clone();
            updated.booleans.insert(root, root_truth);
            result = Some(updated);
        }
    }
    Some(result.map_or_else(|| Arc::clone(facts), Arc::new))
}

fn known_correlated_condition(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, (BlockId, usize)>,
    facts: &CorrelatedFacts,
    condition: RegisterId,
) -> Option<bool> {
    if let Some((selector, constant, equal_when_true)) =
        exact_equality_predicate(eu, definitions, condition)
        && let Some(known) = facts.equalities.get(&selector)
    {
        let equal = known == &constant;
        return Some(if equal_when_true { equal } else { !equal });
    }
    let (root, inverted) = resolve_thread_condition(eu, definitions, condition);
    facts.booleans.get(&root).map(|truth| *truth ^ inverted)
}

fn repeated_branch_predicates(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, (BlockId, usize)>,
) -> HashSet<RegisterId> {
    let mut counts = HashMap::<RegisterId, usize>::default();
    for block in eu.blocks.values() {
        let SIRTerminator::Branch { cond, .. } = &block.terminator else {
            continue;
        };
        if exact_equality_predicate(eu, definitions, *cond).is_some() {
            continue;
        }
        let (root, _) = resolve_thread_condition(eu, definitions, *cond);
        *counts.entry(root).or_default() += 1;
    }
    counts
        .into_iter()
        .filter_map(|(register, count)| (count > 1).then_some(register))
        .collect()
}

/// Return `(selector, constant, condition_is_true_when_equal)` for an exact
/// two-state equality predicate, following the normal boolean aliases emitted
/// by SIR lowering.
fn exact_equality_predicate(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, (BlockId, usize)>,
    mut condition: RegisterId,
) -> Option<(RegisterId, ExactCaseConstant, bool)> {
    let mut inverted = false;
    let mut seen = HashSet::default();
    let (lhs, op, rhs) = loop {
        if !seen.insert(condition) {
            return None;
        }
        let &(block, index) = definitions.get(&condition)?;
        match &eu.blocks[&block].instructions[index] {
            SIRInstruction::Unary(_, UnaryOp::LogicNot, source) => {
                condition = *source;
                inverted = !inverted;
            }
            SIRInstruction::Unary(_, UnaryOp::Ident | UnaryOp::ToTwoState, source) => {
                condition = *source;
            }
            SIRInstruction::Binary(_, lhs, op, rhs)
                if matches!(
                    op,
                    BinaryOp::Eq | BinaryOp::Ne | BinaryOp::EqWildcard | BinaryOp::NeWildcard
                ) =>
            {
                break (*lhs, *op, *rhs);
            }
            _ => return None,
        }
    };

    let left_constant = exact_case_constant(eu, definitions, lhs);
    let right_constant = exact_case_constant(eu, definitions, rhs);
    let (selector, constant) = match (left_constant, right_constant) {
        (None, Some(constant)) => (canonical_case_register(eu, definitions, lhs), constant),
        (Some(constant), None) => (canonical_case_register(eu, definitions, rhs), constant),
        _ => return None,
    };
    if eu.register_map.get(&selector)?.width() != constant.width {
        return None;
    }
    let raw_equal_when_true = matches!(op, BinaryOp::Eq | BinaryOp::EqWildcard);
    Some((selector, constant, raw_equal_when_true ^ inverted))
}

fn exact_case_constant(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, (BlockId, usize)>,
    mut register: RegisterId,
) -> Option<ExactCaseConstant> {
    let mut seen = HashSet::default();
    while seen.insert(register) {
        let &(block, index) = definitions.get(&register)?;
        match &eu.blocks[&block].instructions[index] {
            SIRInstruction::Imm(_, value) if value.mask.is_zero() => {
                return Some(ExactCaseConstant::new(
                    eu.register_map.get(&register)?.width(),
                    value.payload.to_u64_digits(),
                ));
            }
            SIRInstruction::Unary(_, UnaryOp::Ident | UnaryOp::ToTwoState, source) => {
                register = *source;
            }
            _ => return None,
        }
    }
    None
}

fn canonical_case_register(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, (BlockId, usize)>,
    mut register: RegisterId,
) -> RegisterId {
    let mut seen = HashSet::default();
    while seen.insert(register) {
        let Some(&(block, index)) = definitions.get(&register) else {
            break;
        };
        match &eu.blocks[&block].instructions[index] {
            SIRInstruction::Unary(_, UnaryOp::Ident | UnaryOp::ToTwoState, source) => {
                register = *source;
            }
            _ => break,
        }
    }
    register
}

fn resolve_thread_condition(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, (BlockId, usize)>,
    mut register: RegisterId,
) -> (RegisterId, bool) {
    let mut inverted = false;
    let mut seen = HashSet::default();
    while seen.insert(register) {
        let Some(&(block, index)) = definitions.get(&register) else {
            break;
        };
        match &eu.blocks[&block].instructions[index] {
            SIRInstruction::Unary(_, UnaryOp::Ident | UnaryOp::ToTwoState, source) => {
                register = *source;
            }
            SIRInstruction::Unary(_, UnaryOp::LogicNot, source) => {
                register = *source;
                inverted = !inverted;
            }
            _ => break,
        }
    }
    (register, inverted)
}

fn transparent_jump_targets(eu: &ExecutionUnit<RegionedAbsoluteAddr>, cfg: &SirCfg) -> Vec<usize> {
    let mut resolved = vec![None; cfg.block_ids.len()];
    for start in 0..cfg.block_ids.len() {
        if resolved[start].is_some() {
            continue;
        }
        let mut path = Vec::new();
        let mut current = start;
        let endpoint = loop {
            if let Some(endpoint) = resolved[current] {
                break endpoint;
            }
            if cfg.sccs[cfg.scc_for_block[current]].cyclic {
                resolved[current] = Some(current);
                break current;
            }
            let block = &eu.blocks[&cfg.block_ids[current]];
            let successor = match &block.terminator {
                SIRTerminator::Jump(target, arguments)
                    if block.params.is_empty()
                        && block.instructions.is_empty()
                        && arguments.is_empty() =>
                {
                    cfg.block_index(*target)
                }
                _ => None,
            };
            let Some(successor) = successor else {
                resolved[current] = Some(current);
                break current;
            };
            path.push(current);
            current = successor;
        };
        for block in path {
            resolved[block] = Some(endpoint);
        }
    }
    resolved
        .into_iter()
        .enumerate()
        .map(|(block, target)| target.unwrap_or(block))
        .collect()
}

fn register_use_blocks(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, HashSet<BlockId>> {
    let mut uses = HashMap::<RegisterId, HashSet<BlockId>>::default();
    for block in eu.blocks.values() {
        for instruction in &block.instructions {
            for register in instruction_uses(instruction) {
                uses.entry(register).or_default().insert(block.id);
            }
        }
        for register in terminator_uses(&block.terminator) {
            uses.entry(register).or_default().insert(block.id);
        }
    }
    uses
}

fn register_definition_blocks(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, BlockId> {
    let mut blocks = HashMap::default();
    for block in eu.blocks.values() {
        for &parameter in &block.params {
            blocks.insert(parameter, block.id);
        }
        for instruction in &block.instructions {
            if let Some(destination) = def_reg(instruction) {
                blocks.insert(destination, block.id);
            }
        }
    }
    blocks
}

fn rematerializable_case_dag(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    definitions: &HashMap<RegisterId, (BlockId, usize)>,
    definition_blocks: &HashMap<RegisterId, BlockId>,
    skipped: &HashSet<BlockId>,
    source: usize,
    root: RegisterId,
) -> Option<Vec<RegisterId>> {
    enum Visit {
        Enter(RegisterId),
        Exit(RegisterId),
    }

    let source = cfg.block_ids[source];
    let mut order = Vec::new();
    let mut seen = HashSet::default();
    let mut work = vec![Visit::Enter(root)];
    while let Some(visit) = work.pop() {
        match visit {
            Visit::Enter(register) => {
                let &definition_block = definition_blocks.get(&register)?;
                if !skipped.contains(&definition_block) {
                    if !cfg.dominates(definition_block, source) {
                        return None;
                    }
                    continue;
                }
                if !seen.insert(register) {
                    continue;
                }
                let &(block, index) = definitions.get(&register)?;
                let instruction = &eu.blocks[&block].instructions[index];
                if !is_threadable_decision_instruction(instruction) {
                    return None;
                }
                work.push(Visit::Exit(register));
                let operands = instruction_uses(instruction);
                for operand in operands.into_iter().rev() {
                    work.push(Visit::Enter(operand));
                }
            }
            Visit::Exit(register) => order.push(register),
        }
    }
    Some(order)
}

fn rematerialize_case_values(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, (BlockId, usize)>,
    requests: HashMap<(RegisterId, BlockId), Vec<RegisterId>>,
) {
    let mut requests = requests.into_iter().collect::<Vec<_>>();
    requests.sort_unstable_by_key(|((register, block), _)| (block.0, register.0));
    let values = requests
        .iter()
        .flat_map(|(_, dag)| dag.iter().copied())
        .collect::<HashSet<_>>();
    let mut source_values = HashMap::default();
    for register in values {
        let Some(&(block, index)) = definitions.get(&register) else {
            return;
        };
        source_values.insert(
            register,
            (
                eu.blocks[&block].instructions[index].clone(),
                eu.register_map[&register].clone(),
            ),
        );
    }

    let mut next_register = eu
        .register_map
        .keys()
        .map(|register| register.0)
        .max()
        .unwrap_or(0)
        .saturating_add(1);

    let mut first = 0usize;
    while first < requests.len() {
        let block_id = requests[first].0.1;
        let mut end = first + 1;
        while end < requests.len() && requests[end].0.1 == block_id {
            end += 1;
        }
        let mut cloned = HashMap::<RegisterId, RegisterId>::default();
        let mut instructions = Vec::new();
        let mut roots = Vec::new();
        for ((root, _), dag) in &requests[first..end] {
            for &old in dag {
                if cloned.contains_key(&old) {
                    continue;
                }
                while eu.register_map.contains_key(&RegisterId(next_register)) {
                    next_register = next_register.saturating_add(1);
                }
                let new = RegisterId(next_register);
                next_register = next_register.saturating_add(1);
                let Some((source, ty)) = source_values.get(&old) else {
                    return;
                };
                let mut instruction = source.clone();
                for (&dependency, &replacement) in &cloned {
                    replace_register_uses_in_instruction(&mut instruction, dependency, replacement);
                }
                set_instruction_destination(&mut instruction, new);
                cloned.insert(old, new);
                eu.register_map.insert(new, ty.clone());
                instructions.push(instruction);
            }
            let Some(&replacement) = cloned.get(root) else {
                return;
            };
            roots.push((*root, replacement));
        }
        let Some(block) = eu.blocks.get_mut(&block_id) else {
            return;
        };
        for (old, new) in roots {
            replace_register_uses_in_block(block, old, new);
        }
        block.instructions.splice(0..0, instructions);
        first = end;
    }
}

fn set_instruction_destination(
    instruction: &mut SIRInstruction<RegionedAbsoluteAddr>,
    destination: RegisterId,
) {
    match instruction {
        SIRInstruction::Imm(dst, _)
        | SIRInstruction::Binary(dst, _, _, _)
        | SIRInstruction::Unary(dst, _, _)
        | SIRInstruction::Concat(dst, _)
        | SIRInstruction::Slice(dst, _, _, _) => *dst = destination,
        _ => unreachable!("only a rematerializable pure instruction is cloned"),
    }
}

fn replace_register_uses_in_instruction(
    instruction: &mut SIRInstruction<RegionedAbsoluteAddr>,
    old: RegisterId,
    new: RegisterId,
) {
    let replace = |register: &mut RegisterId| {
        if *register == old {
            *register = new;
        }
    };
    let replace_offset = |offset: &mut SIROffset| match offset {
        SIROffset::Static(_) | SIROffset::PackedElements { .. } => {}
        SIROffset::Dynamic(register) => replace(register),
        SIROffset::Element {
            index,
            dynamic_bit_offset,
            ..
        } => {
            replace(index);
            if let Some(offset) = dynamic_bit_offset {
                replace(offset);
            }
        }
    };
    match instruction {
        SIRInstruction::Imm(..) => {}
        SIRInstruction::Binary(_, lhs, _, rhs) => {
            replace(lhs);
            replace(rhs);
        }
        SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, ..) => {
            replace(source);
        }
        SIRInstruction::Load(_, _, offset, _) => replace_offset(offset),
        SIRInstruction::Store(_, offset, _, source, _, _) => {
            replace_offset(offset);
            replace(source);
        }
        SIRInstruction::Commit(_, _, offset, _, _) => replace_offset(offset),
        SIRInstruction::Concat(_, arguments)
        | SIRInstruction::RuntimeEvent {
            args: arguments, ..
        }
        | SIRInstruction::CombCaptureEvent {
            args: arguments, ..
        } => {
            for argument in arguments {
                replace(argument);
            }
        }
        SIRInstruction::Mux(_, condition, then_value, else_value) => {
            replace(condition);
            replace(then_value);
            replace(else_value);
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
            replace(old);
            replace(new);
        }
    }
}

fn replace_register_uses_in_block(
    block: &mut BasicBlock<RegionedAbsoluteAddr>,
    old: RegisterId,
    new: RegisterId,
) {
    let replace = |register: &mut RegisterId| {
        if *register == old {
            *register = new;
        }
    };
    for instruction in &mut block.instructions {
        replace_register_uses_in_instruction(instruction, old, new);
    }
    match &mut block.terminator {
        SIRTerminator::Jump(_, arguments) => {
            for argument in arguments {
                replace(argument);
            }
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            replace(cond);
            for argument in &mut true_block.1 {
                replace(argument);
            }
            for argument in &mut false_block.1 {
                replace(argument);
            }
        }
        SIRTerminator::Switch { selector, .. } => replace(selector),
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

fn analyze(eu: &ExecutionUnit<RegionedAbsoluteAddr>, four_state: bool) -> Analysis {
    let mut edges = HashMap::<BlockId, Vec<Edge>>::default();
    let mut users = HashMap::<RegisterId, HashSet<BlockId>>::default();

    for (&block_id, block) in &eu.blocks {
        for instruction in &block.instructions {
            for register in instruction_uses(instruction) {
                users.entry(register).or_default().insert(block_id);
            }
        }
        for register in terminator_uses(&block.terminator) {
            users.entry(register).or_default().insert(block_id);
        }

        let outgoing = match &block.terminator {
            SIRTerminator::Jump(target, arguments) => vec![Edge {
                target: *target,
                arguments: arguments.clone(),
            }],
            SIRTerminator::Branch {
                cond: _,
                true_block,
                false_block,
            } => vec![
                Edge {
                    target: true_block.0,
                    arguments: true_block.1.clone(),
                },
                Edge {
                    target: false_block.0,
                    arguments: false_block.1.clone(),
                },
            ],
            SIRTerminator::Switch { cases, default, .. } => cases
                .iter()
                .map(|case| Edge {
                    target: case.target,
                    arguments: Vec::new(),
                })
                .chain(std::iter::once(Edge {
                    target: *default,
                    arguments: Vec::new(),
                }))
                .collect(),
            SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
        };
        edges.insert(block_id, outgoing);
    }

    let mut values = HashMap::<RegisterId, LatticeValue>::default();
    let mut executable = HashSet::default();
    let mut queued = HashSet::default();
    let mut worklist = VecDeque::new();

    let enqueue = |block: BlockId,
                   executable: &HashSet<BlockId>,
                   queued: &mut HashSet<BlockId>,
                   worklist: &mut VecDeque<BlockId>| {
        if executable.contains(&block) && queued.insert(block) {
            worklist.push_back(block);
        }
    };

    executable.insert(eu.entry_block_id);
    queued.insert(eu.entry_block_id);
    worklist.push_back(eu.entry_block_id);

    while let Some(block_id) = worklist.pop_front() {
        queued.remove(&block_id);
        let Some(block) = eu.blocks.get(&block_id) else {
            continue;
        };

        for instruction in &block.instructions {
            let Some(dst) = def_reg(instruction) else {
                continue;
            };
            let result = evaluate_instruction(instruction, &values, &eu.register_map, four_state);
            if merge_value(&mut values, dst, result) {
                if let Some(blocks) = users.get(&dst) {
                    for &user in blocks {
                        enqueue(user, &executable, &mut queued, &mut worklist);
                    }
                }
            }
        }

        let selected_edges = match &block.terminator {
            SIRTerminator::Jump(..) => vec![0usize],
            SIRTerminator::Branch { cond, .. } => match exact_truth(values.get(cond)) {
                Some(true) => vec![0],
                Some(false) => vec![1],
                None => vec![0, 1],
            },
            SIRTerminator::Switch { cases, .. } => (0..=cases.len()).collect(),
            SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
        };

        if let Some(outgoing) = edges.get(&block_id) {
            for edge_index in selected_edges {
                let Some(edge) = outgoing.get(edge_index) else {
                    continue;
                };
                let target = edge.target;
                if !eu.blocks.contains_key(&target) {
                    continue;
                }
                let new_block = executable.insert(target);
                if new_block {
                    enqueue(target, &executable, &mut queued, &mut worklist);
                }

                if let Some(target_block) = eu.blocks.get(&target) {
                    for (&parameter, &argument) in
                        target_block.params.iter().zip(edge.arguments.iter())
                    {
                        let argument_value = values
                            .get(&argument)
                            .cloned()
                            .unwrap_or(LatticeValue::Unknown);
                        if merge_value(&mut values, parameter, argument_value) {
                            if let Some(blocks) = users.get(&parameter) {
                                for &user in blocks {
                                    enqueue(user, &executable, &mut queued, &mut worklist);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Analysis { executable, values }
}

fn merge_value(
    values: &mut HashMap<RegisterId, LatticeValue>,
    register: RegisterId,
    incoming: LatticeValue,
) -> bool {
    if matches!(incoming, LatticeValue::Unknown) {
        return false;
    }
    let current = values.entry(register).or_insert(LatticeValue::Unknown);
    let next = match (&*current, incoming) {
        (LatticeValue::Unknown, value) => value,
        (_, LatticeValue::Unknown) => return false,
        (LatticeValue::Overdefined, _) => LatticeValue::Overdefined,
        (LatticeValue::Constant(old), LatticeValue::Constant(new)) if old == &new => return false,
        (LatticeValue::Constant(_), LatticeValue::Constant(_))
        | (LatticeValue::Constant(_), LatticeValue::Overdefined) => LatticeValue::Overdefined,
    };
    if *current == next {
        false
    } else {
        *current = next;
        true
    }
}

fn exact_truth(value: Option<&LatticeValue>) -> Option<bool> {
    let LatticeValue::Constant(value) = value? else {
        return None;
    };
    if !value.mask.is_zero() {
        return None;
    }
    Some(!value.payload.is_zero())
}

fn evaluate_instruction(
    instruction: &SIRInstruction<RegionedAbsoluteAddr>,
    values: &HashMap<RegisterId, LatticeValue>,
    types: &HashMap<RegisterId, RegisterType>,
    four_state: bool,
) -> LatticeValue {
    let state = |register: RegisterId| {
        values
            .get(&register)
            .cloned()
            .unwrap_or(LatticeValue::Unknown)
    };
    let unary =
        |source: RegisterId, f: fn(&SIRValue, &RegisterType) -> Option<SIRValue>| match state(
            source,
        ) {
            LatticeValue::Unknown => LatticeValue::Unknown,
            LatticeValue::Overdefined => LatticeValue::Overdefined,
            LatticeValue::Constant(value) => types
                .get(&source)
                .and_then(|ty| f(&value, ty))
                .map(LatticeValue::Constant)
                .unwrap_or(LatticeValue::Overdefined),
        };
    let binary = |lhs: RegisterId,
                  rhs: RegisterId,
                  f: &dyn Fn(&SIRValue, &SIRValue, &RegisterType) -> Option<SIRValue>,
                  dst: RegisterId| {
        match (state(lhs), state(rhs)) {
            (LatticeValue::Unknown, _) | (_, LatticeValue::Unknown) => LatticeValue::Unknown,
            (LatticeValue::Overdefined, _) | (_, LatticeValue::Overdefined) => {
                LatticeValue::Overdefined
            }
            (LatticeValue::Constant(lhs), LatticeValue::Constant(rhs)) => types
                .get(&dst)
                .and_then(|ty| f(&lhs, &rhs, ty))
                .map(LatticeValue::Constant)
                .unwrap_or(LatticeValue::Overdefined),
        }
    };

    match instruction {
        SIRInstruction::Imm(_, value) => LatticeValue::Constant(value.clone()),
        SIRInstruction::Unary(dst, op, source) => match op {
            UnaryOp::Ident => state(*source),
            UnaryOp::ToTwoState => unary(*source, |value, ty| {
                let width_mask = width_mask(ty.width());
                Some(SIRValue::new(
                    (&value.payload & (&width_mask ^ &value.mask)) & width_mask,
                ))
            }),
            UnaryOp::LogicNot => unary(*source, |value, _| {
                if value.mask.is_zero() {
                    Some(SIRValue::new(if value.payload.is_zero() {
                        1u8
                    } else {
                        0u8
                    }))
                } else {
                    None
                }
            }),
            UnaryOp::Or => unary(*source, |value, _| {
                value
                    .mask
                    .is_zero()
                    .then(|| SIRValue::new(if value.payload.is_zero() { 0u8 } else { 1u8 }))
            }),
            UnaryOp::And => unary(*source, |value, ty| {
                value.mask.is_zero().then(|| {
                    SIRValue::new(if value.payload == width_mask(ty.width()) {
                        1u8
                    } else {
                        0u8
                    })
                })
            }),
            UnaryOp::Xor => unary(*source, |value, _| {
                value.mask.is_zero().then(|| {
                    let parity = value
                        .payload
                        .to_u64_digits()
                        .into_iter()
                        .map(|digit| digit.count_ones())
                        .sum::<u32>()
                        & 1;
                    SIRValue::new(parity as u8)
                })
            }),
            UnaryOp::BitNot => unary(*source, |value, ty| {
                value.mask.is_zero().then(|| {
                    SIRValue::new(
                        width_mask(ty.width()) ^ (&value.payload & width_mask(ty.width())),
                    )
                })
            }),
            UnaryOp::Minus => unary(*source, |value, ty| {
                value.mask.is_zero().then(|| {
                    let mask = width_mask(ty.width());
                    SIRValue::new(((&mask + BigUint::one()) - &value.payload) & mask)
                })
            }),
            UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
                let _ = dst;
                LatticeValue::Overdefined
            }
        },
        SIRInstruction::Binary(dst, lhs, op, rhs) => {
            if !four_state && lhs == rhs {
                match op {
                    BinaryOp::Eq => return LatticeValue::Constant(SIRValue::new(1u8)),
                    BinaryOp::Ne | BinaryOp::Sub | BinaryOp::Xor => {
                        return LatticeValue::Constant(SIRValue::new(0u8));
                    }
                    _ => {}
                }
            }
            match op {
                BinaryOp::LogicAnd => {
                    let lhs_state = state(*lhs);
                    let rhs_state = state(*rhs);
                    if [&lhs_state, &rhs_state]
                        .into_iter()
                        .any(|value| exact_truth(Some(value)) == Some(false))
                    {
                        LatticeValue::Constant(SIRValue::new(0u8))
                    } else {
                        binary(
                            *lhs,
                            *rhs,
                            &|lhs, rhs, _| {
                                if lhs.mask.is_zero() && rhs.mask.is_zero() {
                                    Some(SIRValue::new(
                                        if !lhs.payload.is_zero() && !rhs.payload.is_zero() {
                                            1u8
                                        } else {
                                            0u8
                                        },
                                    ))
                                } else {
                                    None
                                }
                            },
                            *dst,
                        )
                    }
                }
                BinaryOp::LogicOr => {
                    let lhs_state = state(*lhs);
                    let rhs_state = state(*rhs);
                    if [&lhs_state, &rhs_state]
                        .into_iter()
                        .any(|value| exact_truth(Some(value)) == Some(true))
                    {
                        LatticeValue::Constant(SIRValue::new(1u8))
                    } else {
                        binary(
                            *lhs,
                            *rhs,
                            &|lhs, rhs, _| {
                                if lhs.mask.is_zero() && rhs.mask.is_zero() {
                                    Some(SIRValue::new(
                                        if !lhs.payload.is_zero() || !rhs.payload.is_zero() {
                                            1u8
                                        } else {
                                            0u8
                                        },
                                    ))
                                } else {
                                    None
                                }
                            },
                            *dst,
                        )
                    }
                }
                BinaryOp::Eq | BinaryOp::EqWildcard => binary(
                    *lhs,
                    *rhs,
                    &|lhs, rhs, _| {
                        (lhs.mask.is_zero() && rhs.mask.is_zero())
                            .then(|| SIRValue::new((lhs.payload == rhs.payload) as u8))
                    },
                    *dst,
                ),
                BinaryOp::Ne | BinaryOp::NeWildcard => binary(
                    *lhs,
                    *rhs,
                    &|lhs, rhs, _| {
                        (lhs.mask.is_zero() && rhs.mask.is_zero())
                            .then(|| SIRValue::new((lhs.payload != rhs.payload) as u8))
                    },
                    *dst,
                ),
                BinaryOp::And => {
                    let lhs_state = state(*lhs);
                    let rhs_state = state(*rhs);
                    if [&lhs_state, &rhs_state].into_iter().any(|value| {
                        matches!(
                            value,
                            LatticeValue::Constant(value)
                                if value.mask.is_zero() && value.payload.is_zero()
                        )
                    }) {
                        LatticeValue::Constant(SIRValue::new(0u8))
                    } else {
                        binary(
                            *lhs,
                            *rhs,
                            &|lhs, rhs, ty| {
                                if !lhs.mask.is_zero() || !rhs.mask.is_zero() {
                                    return None;
                                }
                                Some(SIRValue::new(
                                    (&lhs.payload & &rhs.payload) & width_mask(ty.width()),
                                ))
                            },
                            *dst,
                        )
                    }
                }
                BinaryOp::Or | BinaryOp::Xor => binary(
                    *lhs,
                    *rhs,
                    &|lhs, rhs, ty| {
                        if !lhs.mask.is_zero() || !rhs.mask.is_zero() {
                            return None;
                        }
                        let payload = match op {
                            BinaryOp::Or => &lhs.payload | &rhs.payload,
                            BinaryOp::Xor => &lhs.payload ^ &rhs.payload,
                            _ => unreachable!(),
                        };
                        Some(SIRValue::new(payload & width_mask(ty.width())))
                    },
                    *dst,
                ),
                BinaryOp::Mul
                    if !four_state
                        && [state(*lhs), state(*rhs)].into_iter().any(|value| {
                            matches!(
                                value,
                                LatticeValue::Constant(value)
                                    if value.mask.is_zero() && value.payload.is_zero()
                            )
                        }) =>
                {
                    LatticeValue::Constant(SIRValue::new(0u8))
                }
                BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul => binary(
                    *lhs,
                    *rhs,
                    &|lhs, rhs, ty| {
                        if !lhs.mask.is_zero() || !rhs.mask.is_zero() {
                            return None;
                        }
                        let mask = width_mask(ty.width());
                        let payload = match op {
                            BinaryOp::Add => &lhs.payload + &rhs.payload,
                            BinaryOp::Sub => (&lhs.payload + &mask + BigUint::one()) - &rhs.payload,
                            BinaryOp::Mul => &lhs.payload * &rhs.payload,
                            _ => unreachable!(),
                        } & &mask;
                        Some(SIRValue::new(payload))
                    },
                    *dst,
                ),
                _ => LatticeValue::Overdefined,
            }
        }
        SIRInstruction::Load(..) => LatticeValue::Overdefined,
        SIRInstruction::Store(..)
        | SIRInstruction::Commit(..)
        | SIRInstruction::RuntimeEvent { .. }
        | SIRInstruction::CombCaptureEvent { .. }
        | SIRInstruction::CombCaptureEnableIfChanged { .. } => LatticeValue::Overdefined,
        SIRInstruction::Concat(dst, arguments) => {
            let mut payload = BigUint::zero();
            let mut width = 0usize;
            for &argument in arguments.iter().rev() {
                match state(argument) {
                    LatticeValue::Unknown => return LatticeValue::Unknown,
                    LatticeValue::Overdefined => return LatticeValue::Overdefined,
                    LatticeValue::Constant(value) => {
                        if !value.mask.is_zero() {
                            return LatticeValue::Overdefined;
                        }
                        let Some(argument_width) = types.get(&argument).map(RegisterType::width)
                        else {
                            return LatticeValue::Overdefined;
                        };
                        payload |= (&value.payload & width_mask(argument_width)) << width;
                        width = width.saturating_add(argument_width);
                    }
                }
            }
            let Some(result_width) = types.get(dst).map(RegisterType::width) else {
                return LatticeValue::Overdefined;
            };
            LatticeValue::Constant(SIRValue::new(payload & width_mask(result_width)))
        }
        SIRInstruction::Slice(_dst, source, offset, width) => match state(*source) {
            LatticeValue::Unknown => LatticeValue::Unknown,
            LatticeValue::Overdefined => LatticeValue::Overdefined,
            LatticeValue::Constant(value) => {
                if !value.mask.is_zero() {
                    LatticeValue::Overdefined
                } else {
                    LatticeValue::Constant(SIRValue::new(
                        (&value.payload >> *offset) & width_mask(*width),
                    ))
                }
            }
        },
        SIRInstruction::Mux(_dst, condition, then_value, else_value) => {
            let then_state = state(*then_value);
            let else_state = state(*else_value);
            if *then_value == *else_value {
                return then_state;
            }
            match exact_truth(values.get(condition)) {
                Some(truth) => {
                    if truth {
                        then_state
                    } else {
                        else_state
                    }
                }
                None => {
                    if then_state == else_state {
                        then_state
                    } else if matches!(then_state, LatticeValue::Unknown)
                        || matches!(else_state, LatticeValue::Unknown)
                    {
                        LatticeValue::Unknown
                    } else {
                        LatticeValue::Overdefined
                    }
                }
            }
        }
    }
}

fn width_mask(width: usize) -> BigUint {
    if width == 0 {
        BigUint::zero()
    } else {
        (BigUint::one() << width) - BigUint::one()
    }
}

fn instruction_uses(instruction: &SIRInstruction<RegionedAbsoluteAddr>) -> Vec<RegisterId> {
    match instruction {
        SIRInstruction::Imm(..) => Vec::new(),
        SIRInstruction::Binary(_, lhs, _, rhs) => vec![*lhs, *rhs],
        SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, _, _) => {
            vec![*source]
        }
        SIRInstruction::Load(_, _, offset, _) => {
            offset.dynamic_registers().into_iter().flatten().collect()
        }
        SIRInstruction::Store(_, offset, _, source, _, _) => offset
            .dynamic_registers()
            .into_iter()
            .flatten()
            .chain(std::iter::once(*source))
            .collect(),
        SIRInstruction::Commit(_, _, offset, _, _) => {
            offset.dynamic_registers().into_iter().flatten().collect()
        }
        SIRInstruction::Concat(_, arguments)
        | SIRInstruction::RuntimeEvent {
            args: arguments, ..
        }
        | SIRInstruction::CombCaptureEvent {
            args: arguments, ..
        } => arguments.clone(),
        SIRInstruction::Mux(_, condition, then_value, else_value) => {
            vec![*condition, *then_value, *else_value]
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => vec![*old, *new],
    }
}

fn terminator_uses(terminator: &SIRTerminator) -> Vec<RegisterId> {
    match terminator {
        SIRTerminator::Jump(_, arguments) => arguments.clone(),
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => std::iter::once(*cond)
            .chain(true_block.1.iter().copied())
            .chain(false_block.1.iter().copied())
            .collect(),
        SIRTerminator::Switch { selector, .. } => vec![*selector],
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

fn trim_dead_register_types(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let used = collect_all_used_registers(eu);
    let mut live_defs = HashSet::default();
    for block in eu.blocks.values() {
        live_defs.extend(block.params.iter().copied());
        for instruction in &block.instructions {
            if let Some(register) = def_reg(instruction) {
                live_defs.insert(register);
            }
        }
    }
    eu.register_map
        .retain(|register, _| live_defs.contains(register) || used.contains(register));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::InstanceId;
    use celox_design::StateObjectId as VarId;

    fn bit(width: usize) -> RegisterType {
        RegisterType::Bit {
            width,
            signed: false,
        }
    }

    fn address() -> RegionedAbsoluteAddr {
        address_instance(0)
    }

    fn address_instance(instance: usize) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: 0,
            instance_id: InstanceId(instance),
            var_id: VarId::default(),
        }
    }

    struct CaseLadder {
        eu: ExecutionUnit<RegionedAbsoluteAddr>,
        arms: Vec<BlockId>,
        decisions: Vec<BlockId>,
        constants: Vec<RegisterId>,
        final_block: BlockId,
    }

    fn case_ladder(values: &[usize], effectful_decision: Option<usize>) -> CaseLadder {
        assert!(!values.is_empty());
        let width =
            usize::BITS as usize - values.iter().copied().max().unwrap().leading_zeros() as usize;
        let width = width.max(1);
        let selector = RegisterId(0);
        let final_block = BlockId(values.len() * 3);
        let mut next_register = 1usize;
        let mut blocks = HashMap::default();
        let mut register_map = HashMap::default();
        let mut arms = Vec::new();
        let mut decisions = Vec::new();
        let mut constants = Vec::new();
        register_map.insert(selector, bit(width));

        for (index, &value) in values.iter().enumerate() {
            let decision = BlockId(index * 3);
            let arm = BlockId(index * 3 + 1);
            let miss = BlockId(index * 3 + 2);
            let next = if index + 1 == values.len() {
                final_block
            } else {
                BlockId((index + 1) * 3)
            };
            decisions.push(decision);
            arms.push(arm);

            let constant = RegisterId(next_register);
            let comparison = RegisterId(next_register + 1);
            let condition = RegisterId(next_register + 2);
            next_register += 3;
            constants.push(constant);
            register_map.insert(constant, bit(width));
            register_map.insert(comparison, bit(1));
            register_map.insert(condition, bit(1));
            let mut instructions = Vec::new();
            if index == 0 {
                instructions.push(SIRInstruction::Load(
                    selector,
                    address_instance(1),
                    SIROffset::Static(0),
                    width,
                ));
            }
            if effectful_decision == Some(index) {
                let side_value = RegisterId(next_register);
                next_register += 1;
                register_map.insert(side_value, bit(1));
                instructions.extend([
                    SIRInstruction::Imm(side_value, SIRValue::new(1u8)),
                    SIRInstruction::Store(
                        address_instance(10_000 + index),
                        SIROffset::Static(0),
                        1,
                        side_value,
                        Vec::new(),
                        Vec::new(),
                    ),
                ]);
            }
            instructions.extend([
                SIRInstruction::Imm(constant, SIRValue::new(value)),
                SIRInstruction::Binary(comparison, selector, BinaryOp::Eq, constant),
                SIRInstruction::Unary(condition, UnaryOp::ToTwoState, comparison),
            ]);
            blocks.insert(
                decision,
                BasicBlock {
                    id: decision,
                    params: Vec::new(),
                    instructions,
                    terminator: SIRTerminator::Branch {
                        cond: condition,
                        true_block: (arm, Vec::new()),
                        false_block: (miss, Vec::new()),
                    },
                },
            );

            let arm_value = RegisterId(next_register);
            next_register += 1;
            register_map.insert(arm_value, bit(8));
            blocks.insert(
                arm,
                BasicBlock {
                    id: arm,
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Imm(arm_value, SIRValue::new((index & 0xff) as u8)),
                        SIRInstruction::Store(
                            address_instance(100 + index),
                            SIROffset::Static(0),
                            8,
                            arm_value,
                            Vec::new(),
                            Vec::new(),
                        ),
                    ],
                    terminator: SIRTerminator::Jump(next, Vec::new()),
                },
            );
            blocks.insert(
                miss,
                BasicBlock {
                    id: miss,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(next, Vec::new()),
                },
            );
        }
        blocks.insert(
            final_block,
            BasicBlock {
                id: final_block,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        );
        let eu = ExecutionUnit {
            blocks,
            entry_block_id: BlockId(0),
            register_map,
        };
        eu.verify_result().unwrap();
        CaseLadder {
            eu,
            arms,
            decisions,
            constants,
            final_block,
        }
    }

    fn constant_branch_unit() -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut register_map = HashMap::default();
        for register in 0..=4 {
            register_map.insert(RegisterId(register), bit(1));
        }
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [
                BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                        SIRInstruction::Imm(RegisterId(1), SIRValue::new(1u8)),
                        SIRInstruction::Imm(RegisterId(2), SIRValue::new(0u8)),
                    ],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(2), Vec::new()),
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Mux(
                        RegisterId(3),
                        RegisterId(0),
                        RegisterId(1),
                        RegisterId(2),
                    )],
                    terminator: SIRTerminator::Return,
                },
                BasicBlock {
                    id: BlockId(2),
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Store(
                        address(),
                        SIROffset::Static(0),
                        1,
                        RegisterId(2),
                        Vec::new(),
                        Vec::new(),
                    )],
                    terminator: SIRTerminator::Return,
                },
            ]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
            register_map,
        }
    }

    #[test]
    fn folds_constant_branch_and_drops_unreachable_arm() {
        let mut eu = constant_branch_unit();
        eu.verify_result().unwrap();

        ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert!(!eu.blocks.contains_key(&BlockId(2)));
        assert!(matches!(
            eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Jump(BlockId(1), _)
        ));
        assert!(
            !eu.blocks[&BlockId(1)]
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
        );
        assert!(!eu.register_map.contains_key(&RegisterId(2)));
    }

    #[test]
    fn folds_two_state_self_comparison_through_logical_annihilator() {
        let mut eu = constant_branch_unit();
        eu.register_map.insert(RegisterId(5), bit(1));
        eu.register_map.insert(RegisterId(6), bit(1));
        eu.blocks.get_mut(&BlockId(0)).unwrap().instructions = vec![
            SIRInstruction::Load(RegisterId(0), address_instance(1), SIROffset::Static(0), 1),
            SIRInstruction::Binary(RegisterId(5), RegisterId(0), BinaryOp::Ne, RegisterId(0)),
            SIRInstruction::Binary(
                RegisterId(6),
                RegisterId(0),
                BinaryOp::LogicAnd,
                RegisterId(5),
            ),
            SIRInstruction::Imm(RegisterId(1), SIRValue::new(1u8)),
            SIRInstruction::Imm(RegisterId(2), SIRValue::new(0u8)),
        ];
        eu.blocks.get_mut(&BlockId(0)).unwrap().terminator = SIRTerminator::Branch {
            cond: RegisterId(6),
            true_block: (BlockId(1), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        };
        eu.verify_result().unwrap();

        ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert!(!eu.blocks.contains_key(&BlockId(1)));
        assert!(matches!(
            eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Jump(BlockId(2), _)
        ));
    }

    #[test]
    fn keeps_four_state_self_comparison_dynamic() {
        let mut eu = constant_branch_unit();
        eu.register_map.insert(RegisterId(5), bit(1));
        eu.register_map.insert(RegisterId(6), bit(1));
        eu.blocks.get_mut(&BlockId(0)).unwrap().instructions = vec![
            SIRInstruction::Load(RegisterId(0), address_instance(1), SIROffset::Static(0), 1),
            SIRInstruction::Binary(RegisterId(5), RegisterId(0), BinaryOp::Ne, RegisterId(0)),
            SIRInstruction::Unary(RegisterId(6), UnaryOp::ToTwoState, RegisterId(5)),
            SIRInstruction::Imm(RegisterId(1), SIRValue::new(1u8)),
            SIRInstruction::Imm(RegisterId(2), SIRValue::new(0u8)),
        ];
        eu.blocks.get_mut(&BlockId(0)).unwrap().terminator = SIRTerminator::Branch {
            cond: RegisterId(6),
            true_block: (BlockId(1), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        };
        eu.verify_result().unwrap();
        let options = PassOptions {
            four_state: true,
            ..PassOptions::default()
        };

        ControlFlowSimplifyPass.run(&mut eu, &options);

        eu.verify_result().unwrap();
        assert!(matches!(
            eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Branch { .. }
        ));
        assert!(eu.blocks.contains_key(&BlockId(1)));
        assert!(eu.blocks.contains_key(&BlockId(2)));
    }

    #[test]
    fn materializes_proven_sccp_constants_in_sir() {
        for zero_on_left in [false, true] {
            let mut eu = constant_branch_unit();
            eu.register_map
                .insert(RegisterId(0), RegisterType::Logic { width: 1 });
            let (lhs, rhs) = if zero_on_left {
                (RegisterId(1), RegisterId(0))
            } else {
                (RegisterId(0), RegisterId(1))
            };
            eu.blocks.get_mut(&BlockId(0)).unwrap().instructions = vec![
                SIRInstruction::Load(RegisterId(0), address_instance(1), SIROffset::Static(0), 1),
                SIRInstruction::Imm(RegisterId(1), SIRValue::new(0u8)),
                SIRInstruction::Binary(RegisterId(3), lhs, BinaryOp::LogicAnd, rhs),
                SIRInstruction::Store(
                    address_instance(2),
                    SIROffset::Static(0),
                    1,
                    RegisterId(3),
                    Vec::new(),
                    Vec::new(),
                ),
            ];
            eu.blocks.get_mut(&BlockId(0)).unwrap().terminator = SIRTerminator::Return;
            eu.blocks.retain(|id, _| *id == BlockId(0));
            eu.verify_result().unwrap();
            let options = PassOptions {
                four_state: true,
                ..PassOptions::default()
            };

            ControlFlowSimplifyPass.run(&mut eu, &options);

            eu.verify_result().unwrap();
            assert!(matches!(
                eu.blocks[&BlockId(0)].instructions.as_slice(),
                [
                    SIRInstruction::Imm(RegisterId(3), value),
                    SIRInstruction::Store(_, _, 1, RegisterId(3), ..)
                ] if value == &SIRValue::new(0u8)
            ));
        }
    }

    #[test]
    fn rewrites_logical_identity_to_boolean_reduction() {
        let mut eu = constant_branch_unit();
        eu.register_map
            .insert(RegisterId(0), RegisterType::Logic { width: 8 });
        eu.register_map
            .insert(RegisterId(1), RegisterType::Logic { width: 8 });
        eu.blocks.get_mut(&BlockId(0)).unwrap().instructions = vec![
            SIRInstruction::Load(RegisterId(0), address_instance(1), SIROffset::Static(0), 8),
            SIRInstruction::Imm(RegisterId(1), SIRValue::new(0u8)),
            SIRInstruction::Binary(
                RegisterId(3),
                RegisterId(0),
                BinaryOp::LogicOr,
                RegisterId(1),
            ),
            SIRInstruction::Store(
                address_instance(2),
                SIROffset::Static(0),
                1,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ];
        eu.blocks.get_mut(&BlockId(0)).unwrap().terminator = SIRTerminator::Return;
        eu.blocks.retain(|id, _| *id == BlockId(0));
        eu.verify_result().unwrap();

        ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert!(matches!(
            eu.blocks[&BlockId(0)].instructions.as_slice(),
            [
                SIRInstruction::Load(RegisterId(0), ..),
                SIRInstruction::Unary(RegisterId(3), UnaryOp::Or, RegisterId(0)),
                SIRInstruction::Store(_, _, 1, RegisterId(3), ..)
            ]
        ));
    }

    #[test]
    fn does_not_fold_an_overdefined_join_condition() {
        let mut eu = constant_branch_unit();
        eu.blocks.get_mut(&BlockId(0)).unwrap().instructions[0] =
            SIRInstruction::Load(RegisterId(0), address(), SIROffset::Static(0), 1);
        eu.blocks.get_mut(&BlockId(1)).unwrap().instructions.clear();
        eu.blocks.get_mut(&BlockId(1)).unwrap().terminator =
            SIRTerminator::Jump(BlockId(3), Vec::new());
        eu.blocks.get_mut(&BlockId(2)).unwrap().instructions.clear();
        eu.blocks.get_mut(&BlockId(2)).unwrap().terminator =
            SIRTerminator::Jump(BlockId(3), Vec::new());
        eu.blocks.insert(
            BlockId(3),
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(1), RegisterId(2)),
                    SIRInstruction::Store(
                        address(),
                        SIROffset::Static(0),
                        1,
                        RegisterId(3),
                        Vec::new(),
                        Vec::new(),
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        );
        eu.verify_result().unwrap();

        ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert!(matches!(
            eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Branch { .. }
        ));
        assert!(
            eu.blocks[&BlockId(3)]
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
        );
    }

    #[test]
    fn uses_a_dominating_dynamic_branch_to_remove_a_mux_arm() {
        let mut eu = constant_branch_unit();
        eu.blocks.get_mut(&BlockId(0)).unwrap().instructions[0] =
            SIRInstruction::Load(RegisterId(0), address(), SIROffset::Static(0), 1);
        eu.blocks.get_mut(&BlockId(2)).unwrap().instructions.clear();
        eu.blocks.get_mut(&BlockId(2)).unwrap().terminator = SIRTerminator::Return;
        eu.verify_result().unwrap();

        ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert!(matches!(
            eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Branch { .. }
        ));
        assert!(
            !eu.blocks[&BlockId(1)]
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
        );
    }

    #[test]
    fn does_not_fold_a_branch_successor_that_is_also_a_join() {
        let mut eu = constant_branch_unit();
        eu.blocks.get_mut(&BlockId(0)).unwrap().instructions[0] =
            SIRInstruction::Load(RegisterId(0), address(), SIROffset::Static(0), 1);
        // `b2` is the false edge of the branch, but it is also reachable from
        // the true edge through `b1`.  The condition is therefore not known
        // at b2.  This is the CFG shape emitted for a guarded loop exit: a
        // direct disabled path joins the path that evaluated the loop.
        eu.blocks.get_mut(&BlockId(1)).unwrap().instructions.clear();
        eu.blocks.get_mut(&BlockId(1)).unwrap().terminator =
            SIRTerminator::Jump(BlockId(2), Vec::new());
        eu.blocks.get_mut(&BlockId(2)).unwrap().instructions = vec![
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(1), RegisterId(2)),
            SIRInstruction::Store(
                address(),
                SIROffset::Static(0),
                1,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ];
        eu.verify_result().unwrap();

        ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert!(
            eu.blocks[&BlockId(2)]
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, SIRInstruction::Mux(..))),
            "a direct branch successor is not necessarily an edge-dominated block",
        );
    }

    #[test]
    fn reapplies_sccp_after_dominance_mux_simplification() {
        let mut eu = constant_branch_unit();
        eu.blocks.get_mut(&BlockId(0)).unwrap().instructions[0] =
            SIRInstruction::Load(RegisterId(0), address(), SIROffset::Static(0), 1);
        eu.blocks.get_mut(&BlockId(1)).unwrap().terminator = SIRTerminator::Branch {
            cond: RegisterId(3),
            true_block: (BlockId(3), Vec::new()),
            false_block: (BlockId(4), Vec::new()),
        };
        eu.blocks.insert(
            BlockId(3),
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        );
        eu.blocks.insert(
            BlockId(4),
            BasicBlock {
                id: BlockId(4),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        );
        eu.verify_result().unwrap();

        ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert!(matches!(
            eu.blocks[&BlockId(1)].terminator,
            SIRTerminator::Jump(BlockId(3), _)
        ));
        assert!(!eu.blocks.contains_key(&BlockId(4)));
    }

    #[test]
    fn follows_a_branch_predicate_through_a_block_argument() {
        let mut eu = constant_branch_unit();
        eu.register_map.insert(RegisterId(5), bit(1));
        eu.blocks.get_mut(&BlockId(0)).unwrap().instructions[0] =
            SIRInstruction::Load(RegisterId(0), address(), SIROffset::Static(0), 1);
        eu.blocks.get_mut(&BlockId(0)).unwrap().terminator = SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(1), vec![RegisterId(0)]),
            false_block: (BlockId(2), Vec::new()),
        };
        eu.blocks.get_mut(&BlockId(1)).unwrap().params = vec![RegisterId(4)];
        eu.blocks.get_mut(&BlockId(1)).unwrap().instructions = vec![
            SIRInstruction::Mux(RegisterId(5), RegisterId(4), RegisterId(1), RegisterId(2)),
            SIRInstruction::Store(
                address(),
                SIROffset::Static(0),
                1,
                RegisterId(5),
                Vec::new(),
                Vec::new(),
            ),
        ];
        eu.verify_result().unwrap();

        ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert!(
            !eu.blocks[&BlockId(1)]
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
        );
    }

    #[test]
    fn threads_taken_case_arms_over_the_remaining_equality_spine() {
        let mut ladder = case_ladder(&[0, 1, 2, 3], None);

        ControlFlowSimplifyPass.run(&mut ladder.eu, &PassOptions::default());

        ladder.eu.verify_result().unwrap();
        for arm in ladder.arms {
            assert!(matches!(
                ladder.eu.blocks[&arm].terminator,
                SIRTerminator::Jump(target, _) if target == ladder.final_block
            ));
        }
    }

    #[test]
    fn repeated_constant_in_the_suffix_prevents_threading_past_its_arm() {
        let mut ladder = case_ladder(&[0, 0, 1], None);
        let first_successor = ladder.decisions[1];

        ControlFlowSimplifyPass.run(&mut ladder.eu, &PassOptions::default());

        ladder.eu.verify_result().unwrap();
        assert!(matches!(
            ladder.eu.blocks[&ladder.arms[0]].terminator,
            SIRTerminator::Jump(target, _) if target == first_successor
        ));
        assert!(matches!(
            ladder.eu.blocks[&ladder.arms[1]].terminator,
            SIRTerminator::Jump(target, _) if target == ladder.final_block
        ));
    }

    #[test]
    fn does_not_skip_an_effectful_decision_block() {
        let mut ladder = case_ladder(&[0, 1], Some(1));
        let effectful = ladder.decisions[1];

        ControlFlowSimplifyPass.run(&mut ladder.eu, &PassOptions::default());

        ladder.eu.verify_result().unwrap();
        assert!(matches!(
            ladder.eu.blocks[&ladder.arms[0]].terminator,
            SIRTerminator::Jump(target, _) if target == effectful
        ));
    }

    #[test]
    fn does_not_thread_a_case_spine_in_four_state_mode() {
        let mut ladder = case_ladder(&[0, 1, 2, 3], None);
        let second_decision = ladder.decisions[1];
        let options = PassOptions {
            four_state: true,
            ..PassOptions::default()
        };

        ControlFlowSimplifyPass.run(&mut ladder.eu, &options);

        ladder.eu.verify_result().unwrap();
        assert!(matches!(
            ladder.eu.blocks[&ladder.arms[0]].terminator,
            SIRTerminator::Jump(target, _) if target == second_decision
        ));
    }

    #[test]
    fn does_not_thread_through_a_cyclic_case_spine() {
        let mut ladder = case_ladder(&[0, 1, 2, 3], None);
        let second_decision = ladder.decisions[1];
        ladder
            .eu
            .blocks
            .get_mut(&ladder.final_block)
            .unwrap()
            .terminator = SIRTerminator::Jump(ladder.decisions[0], Vec::new());
        ladder.eu.verify_result().unwrap();

        ControlFlowSimplifyPass.run(&mut ladder.eu, &PassOptions::default());

        ladder.eu.verify_result().unwrap();
        assert!(matches!(
            ladder.eu.blocks[&ladder.arms[0]].terminator,
            SIRTerminator::Jump(target, _) if target == second_decision
        ));
    }

    #[test]
    fn rematerializes_a_case_constant_needed_after_the_threaded_suffix() {
        let mut ladder = case_ladder(&[0, 1, 2], None);
        let old_constant = ladder.constants[1];
        ladder
            .eu
            .blocks
            .get_mut(&ladder.final_block)
            .unwrap()
            .instructions
            .push(SIRInstruction::Store(
                address_instance(20_000),
                SIROffset::Static(0),
                2,
                old_constant,
                Vec::new(),
                Vec::new(),
            ));
        ladder.eu.verify_result().unwrap();

        ControlFlowSimplifyPass.run(&mut ladder.eu, &PassOptions::default());

        ladder.eu.verify_result().unwrap();
        assert!(matches!(
            ladder.eu.blocks[&ladder.arms[0]].terminator,
            SIRTerminator::Jump(target, _) if target == ladder.final_block
        ));
        let final_instructions = &ladder.eu.blocks[&ladder.final_block].instructions;
        let new_constant = match final_instructions.as_slice() {
            [
                SIRInstruction::Imm(new, value),
                SIRInstruction::Store(_, _, _, source, ..),
            ] if new == source && *new != old_constant && value == &SIRValue::new(1u8) => *new,
            instructions => panic!("unexpected rematerialized final block: {instructions:?}"),
        };
        assert_eq!(ladder.eu.register_map[&new_constant], bit(2));
    }

    #[test]
    fn rematerializes_a_live_out_predicate_dag_at_its_late_use() {
        let mut ladder = case_ladder(&[0, 1, 2], None);
        let old_predicate = match ladder.eu.blocks[&ladder.decisions[1]].terminator {
            SIRTerminator::Branch { cond, .. } => cond,
            _ => unreachable!(),
        };
        ladder
            .eu
            .blocks
            .get_mut(&ladder.final_block)
            .unwrap()
            .instructions
            .push(SIRInstruction::Store(
                address_instance(20_001),
                SIROffset::Static(0),
                1,
                old_predicate,
                Vec::new(),
                Vec::new(),
            ));
        ladder.eu.verify_result().unwrap();

        ControlFlowSimplifyPass.run(&mut ladder.eu, &PassOptions::default());

        ladder.eu.verify_result().unwrap();
        assert!(matches!(
            ladder.eu.blocks[&ladder.arms[0]].terminator,
            SIRTerminator::Jump(target, _) if target == ladder.final_block
        ));
        let final_instructions = &ladder.eu.blocks[&ladder.final_block].instructions;
        assert!(matches!(
            final_instructions.as_slice(),
            [
                SIRInstruction::Imm(constant, _),
                SIRInstruction::Binary(comparison, RegisterId(0), BinaryOp::Eq, rhs),
                SIRInstruction::Unary(condition, UnaryOp::ToTwoState, source),
                SIRInstruction::Store(_, _, 1, stored, ..),
            ] if constant == rhs
                && comparison == source
                && condition == stored
                && *condition != old_predicate
        ));
    }

    fn add_second_selector_load(ladder: &mut CaseLadder) -> RegisterId {
        let second_decision = ladder.decisions[1];
        let selector = RegisterId(0);
        let width = ladder.eu.register_map[&selector].width();
        let next = ladder
            .eu
            .register_map
            .keys()
            .map(|register| register.0)
            .max()
            .unwrap()
            + 1;
        let reloaded = RegisterId(next);
        ladder.eu.register_map.insert(reloaded, bit(width));
        let block = ladder.eu.blocks.get_mut(&second_decision).unwrap();
        block.instructions.insert(
            0,
            SIRInstruction::Load(reloaded, address_instance(1), SIROffset::Static(0), width),
        );
        let comparison = block
            .instructions
            .iter_mut()
            .find(|instruction| matches!(instruction, SIRInstruction::Binary(..)))
            .unwrap();
        let SIRInstruction::Binary(_, lhs, BinaryOp::Eq, _) = comparison else {
            unreachable!();
        };
        *lhs = reloaded;
        ladder.eu.verify_result().unwrap();
        reloaded
    }

    #[test]
    fn state_ssa_gvn_exposes_a_disjoint_store_case_for_threading() {
        let mut ladder = case_ladder(&[0, 1], None);
        let reloaded = add_second_selector_load(&mut ladder);

        super::super::pass_gvn::GvnPass.run(&mut ladder.eu, &PassOptions::default());
        PostGvnCfgCleanupPass.run(&mut ladder.eu, &PassOptions::default());

        ladder.eu.verify_result().unwrap();
        assert!(!ladder.eu.register_map.contains_key(&reloaded));
        assert!(matches!(
            ladder.eu.blocks[&ladder.arms[0]].terminator,
            SIRTerminator::Jump(target, _) if target == ladder.final_block
        ));
    }

    #[test]
    fn same_slot_write_keeps_the_reload_and_prevents_correlated_threading() {
        let mut ladder = case_ladder(&[0, 1], None);
        let reloaded = add_second_selector_load(&mut ladder);
        let selector_width = ladder.eu.register_map[&RegisterId(0)].width();
        let write = RegisterId(
            ladder
                .eu
                .register_map
                .keys()
                .map(|register| register.0)
                .max()
                .unwrap()
                + 1,
        );
        ladder.eu.register_map.insert(write, bit(selector_width));
        ladder
            .eu
            .blocks
            .get_mut(&ladder.arms[0])
            .unwrap()
            .instructions
            .extend([
                SIRInstruction::Imm(write, SIRValue::new(1u8)),
                SIRInstruction::Store(
                    address_instance(1),
                    SIROffset::Static(0),
                    selector_width,
                    write,
                    Vec::new(),
                    Vec::new(),
                ),
            ]);
        ladder.eu.verify_result().unwrap();
        let second_decision = ladder.decisions[1];

        super::super::pass_gvn::GvnPass.run(&mut ladder.eu, &PassOptions::default());
        ControlFlowSimplifyPass.run(&mut ladder.eu, &PassOptions::default());

        ladder.eu.verify_result().unwrap();
        assert!(ladder.eu.register_map.contains_key(&reloaded));
        assert!(matches!(
            ladder.eu.blocks[&ladder.arms[0]].terminator,
            SIRTerminator::Jump(target, _) if target == second_decision
        ));
    }

    #[test]
    fn threads_a_4096_case_spine_without_path_enumeration_or_recursion() {
        let values = (0..4096).collect::<Vec<_>>();
        let mut ladder = case_ladder(&values, None);

        ControlFlowSimplifyPass.run(&mut ladder.eu, &PassOptions::default());

        ladder.eu.verify_result().unwrap();
        assert!(matches!(
            ladder.eu.blocks[&ladder.arms[0]].terminator,
            SIRTerminator::Jump(target, _) if target == ladder.final_block
        ));
        assert!(matches!(
            ladder.eu.blocks[&ladder.arms[2048]].terminator,
            SIRTerminator::Jump(target, _) if target == ladder.final_block
        ));
    }
}
