//! Global Value Numbering (GVN) pass for SIR.
//!
//! Assigns a canonical value number to each expression (opcode + operand
//! value numbers). If two instructions have the same value number, the
//! second is replaced with an alias to the first's result register.
//!
//! Works per-block for now (local value numbering). Cross-block GVN
//! requires dominator tree analysis, which can be added later.

use super::pass_manager::ExecutionUnitPass;
use super::shared::def_reg;
use crate::HashMap;
use crate::ir::*;
use crate::optimizer::PassOptions;

pub(super) struct GvnPass;

impl ExecutionUnitPass for GvnPass {
    fn name(&self) -> &'static str {
        "gvn"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        // Identify blocks that are part of loops (have back edges).
        // A block is a loop target if any Jump/Branch from a later block targets it.
        let block_ids: Vec<BlockId> = eu.blocks.keys().copied().collect();
        let mut loop_blocks: crate::HashSet<BlockId> = crate::HashSet::default();
        for block in eu.blocks.values() {
            let targets: Vec<BlockId> = match &block.terminator {
                SIRTerminator::Jump(t, _) => vec![*t],
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => {
                    vec![true_block.0, false_block.0]
                }
                _ => vec![],
            };
            for t in targets {
                // Back edge: target's index <= source's index in layout order
                let src_idx = block_ids.iter().position(|&id| id == block.id);
                let tgt_idx = block_ids.iter().position(|&id| id == t);
                if let (Some(si), Some(ti)) = (src_idx, tgt_idx) {
                    if ti <= si {
                        // All blocks in the range [ti, si] are in the loop
                        for &bid in &block_ids[ti..=si] {
                            loop_blocks.insert(bid);
                        }
                    }
                }
            }
        }

        let cfg = GvnCfg::new(eu);
        let register_map = eu.register_map.clone();
        let mut value_table: HashMap<ValueKey, RegisterId> = HashMap::default();
        let mut canonical: HashMap<RegisterId, RegisterId> = HashMap::default();
        let mut loop_dependent: crate::HashSet<RegisterId> = crate::HashSet::default();
        let mut imm_constants: HashMap<RegisterId, u64> = HashMap::default();
        let mut changed = false;

        for &root in &cfg.roots {
            gvn_dom_dfs(
                root,
                eu,
                &cfg,
                &register_map,
                &loop_blocks,
                &mut value_table,
                &mut canonical,
                &mut loop_dependent,
                &mut imm_constants,
                &mut changed,
            );
        }

        let _ = changed;
    }
}

struct GvnCfg {
    block_ids: Vec<BlockId>,
    dom_children: Vec<Vec<usize>>,
    roots: Vec<usize>,
}

impl GvnCfg {
    fn new(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> Self {
        let block_ids = rpo_blocks(eu);
        let index: HashMap<BlockId, usize> = block_ids
            .iter()
            .copied()
            .enumerate()
            .map(|(idx, block)| (block, idx))
            .collect();
        let mut preds = vec![Vec::new(); block_ids.len()];
        for (idx, block_id) in block_ids.iter().copied().enumerate() {
            let Some(block) = eu.blocks.get(&block_id) else {
                continue;
            };
            for succ in terminator_successors(&block.terminator) {
                if let Some(&succ_idx) = index.get(&succ) {
                    preds[succ_idx].push(idx);
                }
            }
        }

        let mut idom: Vec<Option<usize>> = vec![None; block_ids.len()];
        let roots = block_ids
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(idx, block)| {
                (block == eu.entry_block_id || preds[idx].is_empty()).then_some(idx)
            })
            .collect::<Vec<_>>();
        for &root in &roots {
            idom[root] = Some(root);
        }

        let mut changed = true;
        while changed {
            changed = false;
            for idx in 0..block_ids.len() {
                if roots.contains(&idx) {
                    continue;
                }
                let mut defined_preds = preds[idx]
                    .iter()
                    .copied()
                    .filter(|pred| idom[*pred].is_some());
                let Some(mut new_idom) = defined_preds.next() else {
                    continue;
                };
                for pred in defined_preds {
                    new_idom = intersect_idom(pred, new_idom, &idom);
                }
                if idom[idx] != Some(new_idom) {
                    idom[idx] = Some(new_idom);
                    changed = true;
                }
            }
        }

        let mut dom_children = vec![Vec::new(); block_ids.len()];
        for (idx, parent) in idom.iter().copied().enumerate() {
            if let Some(parent) = parent
                && parent != idx
            {
                dom_children[parent].push(idx);
            }
        }
        for children in &mut dom_children {
            children.sort_unstable();
        }

        Self {
            block_ids,
            dom_children,
            roots,
        }
    }
}

fn intersect_idom(mut a: usize, mut b: usize, idom: &[Option<usize>]) -> usize {
    while a != b {
        while a > b {
            a = idom[a].expect("idom must be known for intersect");
        }
        while b > a {
            b = idom[b].expect("idom must be known for intersect");
        }
    }
    a
}

fn rpo_blocks(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> Vec<BlockId> {
    fn visit(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        start: BlockId,
        seen: &mut crate::HashSet<BlockId>,
        out: &mut Vec<BlockId>,
    ) {
        let mut stack = vec![(start, false)];
        while let Some((block_id, expanded)) = stack.pop() {
            if !eu.blocks.contains_key(&block_id) {
                continue;
            }
            if expanded {
                out.push(block_id);
                continue;
            }
            if !seen.insert(block_id) {
                continue;
            }
            stack.push((block_id, true));
            let mut succs = terminator_successors(&eu.blocks[&block_id].terminator);
            succs.sort_unstable();
            succs.reverse();
            for succ in succs {
                if !seen.contains(&succ) {
                    stack.push((succ, false));
                }
            }
        }
    }

    let mut seen = crate::HashSet::default();
    let mut postorder = Vec::new();
    visit(eu, eu.entry_block_id, &mut seen, &mut postorder);
    let mut rest = eu.blocks.keys().copied().collect::<Vec<_>>();
    rest.sort_unstable();
    for block_id in rest {
        if !seen.contains(&block_id) {
            visit(eu, block_id, &mut seen, &mut postorder);
        }
    }
    postorder.reverse();
    postorder
}

fn terminator_successors(term: &SIRTerminator) -> Vec<BlockId> {
    match term {
        SIRTerminator::Jump(target, _) => vec![*target],
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => vec![true_block.0, false_block.0],
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn gvn_dom_dfs(
    node: usize,
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &GvnCfg,
    register_map: &HashMap<RegisterId, RegisterType>,
    loop_blocks: &crate::HashSet<BlockId>,
    value_table: &mut HashMap<ValueKey, RegisterId>,
    canonical: &mut HashMap<RegisterId, RegisterId>,
    loop_dependent: &mut crate::HashSet<RegisterId>,
    imm_constants: &mut HashMap<RegisterId, u64>,
    changed: &mut bool,
) {
    let block_id = cfg.block_ids[node];
    let mut added_values = Vec::new();
    let mut canonical_changes = Vec::new();
    let mut added_loop_deps = Vec::new();
    let mut imm_changes = Vec::new();

    if let Some(block) = eu.blocks.get_mut(&block_id) {
        let mut aliases: HashMap<RegisterId, RegisterId> = HashMap::default();
        if !loop_blocks.contains(&block_id) {
            gvn_block(
                &mut block.instructions,
                &mut aliases,
                register_map,
                &block.params,
                value_table,
                canonical,
                loop_dependent,
                imm_constants,
                &mut added_values,
                &mut canonical_changes,
                &mut added_loop_deps,
                &mut imm_changes,
            );
            if !aliases.is_empty() {
                *changed = true;
            }
        }
        if !canonical.is_empty() {
            for inst in &mut block.instructions {
                apply_aliases(inst, canonical);
            }
            apply_aliases_to_terminator(&mut block.terminator, canonical);
        }
    }

    for &child in &cfg.dom_children[node] {
        gvn_dom_dfs(
            child,
            eu,
            cfg,
            register_map,
            loop_blocks,
            value_table,
            canonical,
            loop_dependent,
            imm_constants,
            changed,
        );
    }

    for key in added_values.into_iter().rev() {
        value_table.remove(&key);
    }
    for (reg, old) in canonical_changes.into_iter().rev() {
        if let Some(old) = old {
            canonical.insert(reg, old);
        } else {
            canonical.remove(&reg);
        }
    }
    for reg in added_loop_deps.into_iter().rev() {
        loop_dependent.remove(&reg);
    }
    for (reg, old) in imm_changes.into_iter().rev() {
        if let Some(old) = old {
            imm_constants.insert(reg, old);
        } else {
            imm_constants.remove(&reg);
        }
    }
}

/// Value numbering key: identifies an expression by its operation and
/// operand value numbers.
#[derive(Clone, PartialEq, Eq, Hash)]
enum ValueKey {
    /// Constant immediate
    Imm(Vec<u64>, usize),
    /// Binary operation: (op, lhs_value_number, rhs_value_number, result_width)
    Binary(BinaryOp, RegisterId, RegisterId, usize),
    /// Unary operation: (op, src_value_number, result_width)
    Unary(UnaryOp, RegisterId, usize),
    /// Concat: list of value numbers
    Concat(Vec<RegisterId>),
    /// Slice: (src_value_number, bit_offset, width)
    Slice(RegisterId, usize, usize),
    /// Mux: (cond_value_number, then_value_number, else_value_number, result_width)
    Mux(RegisterId, RegisterId, RegisterId, usize),
}

fn gvn_block(
    instructions: &mut [SIRInstruction<RegionedAbsoluteAddr>],
    aliases: &mut HashMap<RegisterId, RegisterId>,
    register_map: &HashMap<RegisterId, RegisterType>,
    block_params: &[RegisterId],
    value_table: &mut HashMap<ValueKey, RegisterId>,
    canonical: &mut HashMap<RegisterId, RegisterId>,
    loop_dependent: &mut crate::HashSet<RegisterId>,
    imm_constants: &mut HashMap<RegisterId, u64>,
    added_values: &mut Vec<ValueKey>,
    canonical_changes: &mut Vec<(RegisterId, Option<RegisterId>)>,
    added_loop_deps: &mut Vec<RegisterId>,
    imm_changes: &mut Vec<(RegisterId, Option<u64>)>,
) {
    for &param in block_params {
        if loop_dependent.insert(param) {
            added_loop_deps.push(param);
        }
    }

    let resolve = |r: RegisterId, canonical: &HashMap<RegisterId, RegisterId>| -> RegisterId {
        canonical.get(&r).copied().unwrap_or(r)
    };

    for inst in instructions.iter() {
        let key = match inst {
            SIRInstruction::Imm(dst, val) => {
                // Track constant for Mux folding
                if let Some(v) = crate::optimizer::coalescing::shared::sir_value_to_u64(val) {
                    imm_changes.push((*dst, imm_constants.get(dst).copied()));
                    imm_constants.insert(*dst, v);
                }
                // Include mask in key for 4-state correctness:
                // Imm(0, mask=0) ≠ Imm(0, mask=0xFF)
                let mut key_data = val.payload.to_u64_digits();
                key_data.push(u64::MAX); // separator
                key_data.extend(val.mask.to_u64_digits());
                let w = register_map.get(dst).map(|t| t.width()).unwrap_or(0);
                Some(ValueKey::Imm(key_data, w))
            }
            SIRInstruction::Binary(dst, lhs, op, rhs) => {
                let l = resolve(*lhs, &canonical);
                let r = resolve(*rhs, &canonical);
                let (l, r) = if op.is_commutative() && l > r {
                    (r, l)
                } else {
                    (l, r)
                };
                let w = register_map.get(dst).map(|t| t.width()).unwrap_or(0);
                Some(ValueKey::Binary(*op, l, r, w))
            }
            SIRInstruction::Unary(dst, op, src) => {
                let s = resolve(*src, &canonical);
                let w = register_map.get(dst).map(|t| t.width()).unwrap_or(0);
                Some(ValueKey::Unary(*op, s, w))
            }
            SIRInstruction::Concat(_, args) => {
                let resolved: Vec<RegisterId> =
                    args.iter().map(|a| resolve(*a, &canonical)).collect();
                Some(ValueKey::Concat(resolved))
            }
            SIRInstruction::Slice(_, src, off, width) => {
                let s = resolve(*src, &canonical);
                Some(ValueKey::Slice(s, *off, *width))
            }
            SIRInstruction::Mux(dst, cond, then_val, else_val) => {
                let c = resolve(*cond, &canonical);
                let t = resolve(*then_val, &canonical);
                let e = resolve(*else_val, &canonical);
                // Constant fold: if cond is a known constant, alias to selected branch
                if let Some(cond_val) = imm_constants.get(&c) {
                    let selected = if *cond_val != 0 { t } else { e };
                    aliases.insert(*dst, selected);
                    set_canonical(canonical, canonical_changes, *dst, selected);
                    continue;
                }
                let w = register_map.get(dst).map(|t| t.width()).unwrap_or(0);
                Some(ValueKey::Mux(c, t, e, w))
            }
            // Load: depends on memory state, cannot be value-numbered
            // (Store-Load forwarding handles Load redundancy separately)
            SIRInstruction::Load(..) => None,
            // Store/Commit: side-effecting. Invalidate all Load-derived values.
            SIRInstruction::Store(..)
            | SIRInstruction::Commit(..)
            | SIRInstruction::RuntimeEvent { .. }
            | SIRInstruction::CombCaptureEvent { .. }
            | SIRInstruction::CombCaptureEnableIfChanged { .. } => {
                // Conservative: don't invalidate value table for pure
                // computations (Binary/Unary/Concat/Slice/Imm are
                // memory-independent). Only Loads would be affected by
                // Stores, and we already exclude Loads from GVN.
                None
            }
        };

        if let (Some(key), Some(dst)) = (key, def_reg(inst)) {
            // Check if any operand depends on a loop variable
            let uses_loop_var = match inst {
                SIRInstruction::Binary(_, lhs, _, rhs) => {
                    loop_dependent.contains(&resolve(*lhs, canonical))
                        || loop_dependent.contains(&resolve(*rhs, canonical))
                }
                SIRInstruction::Unary(_, _, src) => {
                    loop_dependent.contains(&resolve(*src, canonical))
                }
                SIRInstruction::Concat(_, args) => args
                    .iter()
                    .any(|a| loop_dependent.contains(&resolve(*a, canonical))),
                SIRInstruction::Slice(_, src, _, _) => {
                    loop_dependent.contains(&resolve(*src, canonical))
                }
                SIRInstruction::Mux(_, cond, then_val, else_val) => {
                    loop_dependent.contains(&resolve(*cond, canonical))
                        || loop_dependent.contains(&resolve(*then_val, canonical))
                        || loop_dependent.contains(&resolve(*else_val, canonical))
                }
                _ => false,
            };

            if uses_loop_var {
                // Mark result as loop-dependent; don't GVN
                if loop_dependent.insert(dst) {
                    added_loop_deps.push(dst);
                }
                set_canonical(canonical, canonical_changes, dst, dst);
            } else if let Some(&existing) = value_table.get(&key) {
                // Redundant: alias dst to existing
                aliases.insert(dst, existing);
                set_canonical(canonical, canonical_changes, dst, existing);
            } else {
                // First occurrence: record as canonical
                value_table.insert(key.clone(), dst);
                added_values.push(key);
                set_canonical(canonical, canonical_changes, dst, dst);
            }
        }
    }
}

fn set_canonical(
    canonical: &mut HashMap<RegisterId, RegisterId>,
    changes: &mut Vec<(RegisterId, Option<RegisterId>)>,
    reg: RegisterId,
    value: RegisterId,
) {
    changes.push((reg, canonical.get(&reg).copied()));
    canonical.insert(reg, value);
}

fn apply_aliases_to_terminator(
    term: &mut SIRTerminator,
    aliases: &HashMap<RegisterId, RegisterId>,
) {
    match term {
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            if let Some(&a) = aliases.get(cond) {
                *cond = a;
            }
            for arg in &mut true_block.1 {
                if let Some(&a) = aliases.get(arg) {
                    *arg = a;
                }
            }
            for arg in &mut false_block.1 {
                if let Some(&a) = aliases.get(arg) {
                    *arg = a;
                }
            }
        }
        SIRTerminator::Jump(_, args) => {
            for arg in args {
                if let Some(&a) = aliases.get(arg) {
                    *arg = a;
                }
            }
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

fn apply_aliases(
    inst: &mut SIRInstruction<RegionedAbsoluteAddr>,
    aliases: &HashMap<RegisterId, RegisterId>,
) {
    match inst {
        SIRInstruction::Imm(_, _) => {}
        SIRInstruction::Binary(_, lhs, _, rhs) => {
            if let Some(&a) = aliases.get(lhs) {
                *lhs = a;
            }
            if let Some(&a) = aliases.get(rhs) {
                *rhs = a;
            }
        }
        SIRInstruction::Unary(_, _, src) => {
            if let Some(&a) = aliases.get(src) {
                *src = a;
            }
        }
        SIRInstruction::Load(_, _, _, _) => {}
        SIRInstruction::Store(_, _, _, src, _, _) => {
            if let Some(&a) = aliases.get(src) {
                *src = a;
            }
        }
        SIRInstruction::Commit(_, _, _, _, _) => {}
        SIRInstruction::Concat(_, args) => {
            for arg in args {
                if let Some(&a) = aliases.get(arg) {
                    *arg = a;
                }
            }
        }
        SIRInstruction::Slice(_, src, _, _) => {
            if let Some(&a) = aliases.get(src) {
                *src = a;
            }
        }
        SIRInstruction::Mux(_, cond, then_val, else_val) => {
            if let Some(&a) = aliases.get(cond) {
                *cond = a;
            }
            if let Some(&a) = aliases.get(then_val) {
                *then_val = a;
            }
            if let Some(&a) = aliases.get(else_val) {
                *else_val = a;
            }
        }
        SIRInstruction::RuntimeEvent { args, .. }
        | SIRInstruction::CombCaptureEvent { args, .. } => {
            for arg in args {
                if let Some(&a) = aliases.get(arg) {
                    *arg = a;
                }
            }
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
            if let Some(&a) = aliases.get(old) {
                *old = a;
            }
            if let Some(&a) = aliases.get(new) {
                *new = a;
            }
        }
    }
}
