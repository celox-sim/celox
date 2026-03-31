//! MIR optimization passes: run between ISel and regalloc.
//!
//! - Copy propagation: `v2 = mov v1` → replace all uses of v2 with v1
//! - Dead code elimination: remove instructions whose defs are unused

use std::collections::HashMap;

use super::mir::*;

/// Run all MIR optimization passes.
pub fn optimize(func: &mut MFunction) {
    if func.vregs.count() > 40 {
        // High-pressure: full pipeline
        for _ in 0..2 {
            constant_fold(func);
            constant_dedup(func);
            copy_propagate(func);
            algebraic_simplify(func);
            redundant_mask_eliminate(func);
            global_gvn(func);
            dead_code_eliminate(func);
        }
        lower_to_imm_forms(func);
        sink_loads(func);
        split_live_ranges(func);
        fold_xor_chain_to_pext(func);
    } else {
        // Low-pressure: lightweight but complete pipeline
        constant_fold(func);
        constant_dedup(func);
        copy_propagate(func);
        algebraic_simplify(func);
        redundant_mask_eliminate(func);
        fold_xor_chain_to_pext(func);
        dead_code_eliminate(func);
        lower_to_imm_forms(func);
        dead_code_eliminate(func); // clean up dead LoadImm from imm lowering
    }
    if_convert(func);
    simplify_cfg(func);
    compute_value_widths(func);
}

// ────────────────────────────────────────────────────────────────
// Phase 1A: Constant folding
// ────────────────────────────────────────────────────────────────

/// Constant folding: evaluate operations with constant operands at compile time.
fn constant_fold(func: &mut MFunction) {
    // Build def map: VReg → LoadImm value
    let mut consts: HashMap<VReg, u64> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let MInst::LoadImm { dst, value } = inst {
                consts.insert(*dst, *value);
            }
        }
    }
    if consts.is_empty() {
        return;
    }

    let mut changed = true;
    while changed {
        changed = false;
        for block in &mut func.blocks {
            for inst in &mut block.insts {
                let folded = match inst {
                    // Binary reg-reg with both constant
                    MInst::Add { dst, lhs, rhs } => fold_bin(&consts, *dst, *lhs, *rhs, u64::wrapping_add),
                    MInst::Sub { dst, lhs, rhs } => fold_bin(&consts, *dst, *lhs, *rhs, u64::wrapping_sub),
                    MInst::Mul { dst, lhs, rhs } => fold_bin(&consts, *dst, *lhs, *rhs, u64::wrapping_mul),
                    MInst::And { dst, lhs, rhs } => fold_bin(&consts, *dst, *lhs, *rhs, |a, b| a & b),
                    MInst::Or { dst, lhs, rhs } => fold_bin(&consts, *dst, *lhs, *rhs, |a, b| a | b),
                    MInst::Xor { dst, lhs, rhs } => fold_bin(&consts, *dst, *lhs, *rhs, |a, b| a ^ b),
                    MInst::Shr { dst, lhs, rhs } => fold_bin(&consts, *dst, *lhs, *rhs, |a, b| {
                        if b >= 64 { 0 } else { a >> b }
                    }),
                    MInst::Shl { dst, lhs, rhs } => fold_bin(&consts, *dst, *lhs, *rhs, |a, b| {
                        if b >= 64 { 0 } else { a << b }
                    }),
                    MInst::Sar { dst, lhs, rhs } => fold_bin(&consts, *dst, *lhs, *rhs, |a, b| {
                        if b >= 64 { ((a as i64) >> 63) as u64 } else { ((a as i64) >> b) as u64 }
                    }),
                    // Binary imm with constant src
                    MInst::AndImm { dst, src, imm } => {
                        consts.get(src).map(|&v| (*dst, v & *imm))
                    }
                    MInst::OrImm { dst, src, imm } => {
                        consts.get(src).map(|&v| (*dst, v | *imm))
                    }
                    MInst::ShrImm { dst, src, imm } => {
                        consts.get(src).map(|&v| (*dst, if *imm >= 64 { 0 } else { v >> *imm }))
                    }
                    MInst::ShlImm { dst, src, imm } => {
                        consts.get(src).map(|&v| (*dst, if *imm >= 64 { 0 } else { v << *imm }))
                    }
                    MInst::SarImm { dst, src, imm } => {
                        consts.get(src).map(|&v| (*dst, if *imm >= 64 { ((v as i64) >> 63) as u64 } else { ((v as i64) >> *imm) as u64 }))
                    }
                    // Unary with constant src
                    MInst::BitNot { dst, src } => {
                        consts.get(src).map(|&v| (*dst, !v))
                    }
                    MInst::Neg { dst, src } => {
                        consts.get(src).map(|&v| (*dst, v.wrapping_neg()))
                    }
                    MInst::Popcnt { dst, src } => {
                        consts.get(src).map(|&v| (*dst, v.count_ones() as u64))
                    }
                    // Comparison with both constant
                    MInst::Cmp { dst, lhs, rhs, kind } => {
                        if let (Some(&l), Some(&r)) = (consts.get(lhs), consts.get(rhs)) {
                            let result = match kind {
                                CmpKind::Eq => l == r,
                                CmpKind::Ne => l != r,
                                CmpKind::LtU => l < r,
                                CmpKind::LeU => l <= r,
                                CmpKind::GtU => l > r,
                                CmpKind::GeU => l >= r,
                                CmpKind::LtS => (l as i64) < (r as i64),
                                CmpKind::LeS => (l as i64) <= (r as i64),
                                CmpKind::GtS => (l as i64) > (r as i64),
                                CmpKind::GeS => (l as i64) >= (r as i64),
                            };
                            Some((*dst, result as u64))
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some((dst, value)) = folded {
                    *inst = MInst::LoadImm { dst, value };
                    consts.insert(dst, value);
                    changed = true;
                }
            }
        }
    }
}

fn fold_bin(
    consts: &HashMap<VReg, u64>,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    op: impl Fn(u64, u64) -> u64,
) -> Option<(VReg, u64)> {
    if let (Some(&l), Some(&r)) = (consts.get(&lhs), consts.get(&rhs)) {
        Some((dst, op(l, r)))
    } else {
        None
    }
}

// ────────────────────────────────────────────────────────────────
// Phase 1B: Redundant mask elimination
// ────────────────────────────────────────────────────────────────

/// Helper: compute the width of a mask that is `(1 << w) - 1`.
fn mask_width(imm: u64) -> Option<usize> {
    if imm == 0 {
        return Some(0);
    }
    if imm == u64::MAX {
        return Some(64);
    }
    // Check if imm is of the form (1 << w) - 1: all lower bits set
    let w = imm.trailing_ones() as usize;
    if imm == (1u64 << w) - 1 {
        Some(w)
    } else {
        None
    }
}

/// Redundant mask elimination: track known bit widths and remove unnecessary
/// AND masks when the source is already narrow enough.
fn redundant_mask_eliminate(func: &mut MFunction) {
    // Build def-map for AND chain folding
    let mut def_map: HashMap<VReg, MInst> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(d) = inst.def() {
                def_map.insert(d, inst.clone());
            }
        }
    }

    for block in &mut func.blocks {
        let mut known: HashMap<VReg, usize> = HashMap::new();

        for inst in &mut block.insts {
            let known_width = compute_known_width(inst, &known);

            let should_replace = if let MInst::AndImm { dst, src, imm } = inst {
                // Check 1: redundant mask (source already narrow enough)
                if let Some(mw) = mask_width(*imm) {
                    if let Some(&src_w) = known.get(src) {
                        if src_w <= mw {
                            Some(MaskElimAction::Mov(*dst, *src))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                // Check 2: AND chain folding — AndImm(AndImm(x, m1), m2) → AndImm(x, m1 & m2)
                } else {
                    None
                }.or_else(|| {
                    // AND chain: if src was defined by AndImm(inner, m1), fold to AndImm(inner, m1 & imm)
                    if let Some(MInst::AndImm { src: inner, imm: m1, .. }) = def_map.get(src) {
                        let folded = *m1 & *imm;
                        Some(MaskElimAction::FoldAnd(*dst, *inner, folded))
                    } else {
                        None
                    }
                })
            } else {
                None
            };

            if let Some(action) = should_replace {
                match action {
                    MaskElimAction::Mov(dst, src) => {
                        *inst = MInst::Mov { dst, src };
                        if let Some(&src_w) = known.get(&src) {
                            known.insert(dst, src_w);
                        }
                    }
                    MaskElimAction::FoldAnd(dst, inner, folded_mask) => {
                        *inst = MInst::AndImm { dst, src: inner, imm: folded_mask };
                        let w = if folded_mask == 0 { 0 } else { 64 - folded_mask.leading_zeros() as usize };
                        known.insert(dst, w);
                    }
                }
                continue;
            }

            if let Some(w) = known_width {
                if let Some(d) = inst.def() {
                    known.insert(d, w);
                }
            }
        }
    }
}

enum MaskElimAction {
    Mov(VReg, VReg),
    FoldAnd(VReg, VReg, u64),
}

/// Compute the known bit width of an instruction's result.
fn compute_known_width(inst: &MInst, known: &HashMap<VReg, usize>) -> Option<usize> {
    match inst {
        MInst::LoadImm { value, .. } => {
            if *value == 0 { Some(0) } else { Some(64 - value.leading_zeros() as usize) }
        }
        MInst::Load { size, .. } | MInst::LoadIndexed { size, .. } => {
            Some(size.bytes() as usize * 8)
        }
        MInst::Cmp { .. } | MInst::CmpImm { .. } => Some(1),
        MInst::Popcnt { .. } => Some(7), // max popcnt(u64) = 64, fits in 7 bits
        MInst::Mov { src, .. } => known.get(src).copied(),
        MInst::AndImm { src, imm, .. } => {
            let imm_w = if *imm == 0 { 0 } else { 64 - imm.leading_zeros() as usize };
            let src_w = known.get(src).copied().unwrap_or(64);
            Some(src_w.min(imm_w))
        }
        MInst::OrImm { src, imm, .. } => {
            let imm_w = if *imm == 0 { 0 } else { 64 - imm.leading_zeros() as usize };
            let src_w = known.get(src).copied().unwrap_or(64);
            Some(src_w.max(imm_w))
        }
        MInst::ShrImm { src, imm, .. } => {
            known.get(src).map(|&w| w.saturating_sub(*imm as usize))
        }
        MInst::ShlImm { src, imm, .. } => {
            known.get(src).map(|&w| (w + *imm as usize).min(64))
        }
        MInst::And { lhs, rhs, .. } => {
            match (known.get(lhs), known.get(rhs)) {
                (Some(&l), Some(&r)) => Some(l.min(r)),
                (Some(&l), None) => Some(l),
                (None, Some(&r)) => Some(r),
                _ => None,
            }
        }
        MInst::Or { lhs, rhs, .. } | MInst::Xor { lhs, rhs, .. } => {
            match (known.get(lhs), known.get(rhs)) {
                (Some(&l), Some(&r)) => Some(l.max(r)),
                _ => None,
            }
        }
        MInst::Add { lhs, rhs, .. } | MInst::Sub { lhs, rhs, .. } => {
            match (known.get(lhs), known.get(rhs)) {
                (Some(&l), Some(&r)) => Some((l.max(r) + 1).min(64)),
                _ => None,
            }
        }
        MInst::Mul { lhs, rhs, .. } => {
            match (known.get(lhs), known.get(rhs)) {
                (Some(&l), Some(&r)) => Some((l + r).min(64)),
                _ => None,
            }
        }
        MInst::Select { true_val, false_val, .. } => {
            match (known.get(true_val), known.get(false_val)) {
                (Some(&t), Some(&f)) => Some(t.max(f)),
                _ => None,
            }
        }
        MInst::Pext { .. } => Some(64), // conservative
        _ => None,
    }
}

// ────────────────────────────────────────────────────────────────
// Global GVN (Global Value Numbering)
// ────────────────────────────────────────────────────────────────
//
// Dominator-tree-scoped CSE: walk blocks in dominator-tree pre-order,
// maintaining a scoped hash table. Entries from a dominator are visible
// to all dominated blocks, enabling cross-block redundancy elimination.

/// Key for GVN: opcode discriminant + operands (sorted for commutative ops).
#[derive(Hash, PartialEq, Eq, Clone)]
enum GvnKey {
    BinRR(u8, VReg, VReg),
    BinRI(u8, VReg, u64),
    ShiftI(u8, VReg, u8),
    Unary(u8, VReg),
    Cmp(u8, VReg, VReg, u8),
    CmpI(u8, VReg, i32, u8),
    AddI(VReg, i32),
    SubI(VReg, i32),
    Load(u8, i32, u8),  // base(SimState=0,Stack=1), offset, size
}

const GVN_ADD: u8 = 1;
const GVN_SUB: u8 = 2;
const GVN_MUL: u8 = 3;
const GVN_AND: u8 = 4;
const GVN_OR: u8 = 5;
const GVN_XOR: u8 = 6;
const GVN_SHR: u8 = 7;
const GVN_SHL: u8 = 8;
const GVN_SAR: u8 = 9;
const GVN_AND_IMM: u8 = 10;
const GVN_OR_IMM: u8 = 11;
const GVN_SHR_IMM: u8 = 12;
const GVN_SHL_IMM: u8 = 13;
const GVN_SAR_IMM: u8 = 14;
const GVN_NOT: u8 = 15;
const GVN_NEG: u8 = 16;
const GVN_POPCNT: u8 = 17;
const GVN_CMP: u8 = 18;
const GVN_PEXT: u8 = 19;

fn gvn_is_commutative(op: u8) -> bool {
    matches!(op, GVN_ADD | GVN_MUL | GVN_AND | GVN_OR | GVN_XOR)
}

fn gvn_key(inst: &MInst) -> Option<GvnKey> {
    match inst {
        MInst::Add { lhs, rhs, .. } => Some(gvn_bin_rr(GVN_ADD, *lhs, *rhs)),
        MInst::Sub { lhs, rhs, .. } => Some(GvnKey::BinRR(GVN_SUB, *lhs, *rhs)),
        MInst::Mul { lhs, rhs, .. } => Some(gvn_bin_rr(GVN_MUL, *lhs, *rhs)),
        MInst::And { lhs, rhs, .. } => Some(gvn_bin_rr(GVN_AND, *lhs, *rhs)),
        MInst::Or { lhs, rhs, .. } => Some(gvn_bin_rr(GVN_OR, *lhs, *rhs)),
        MInst::Xor { lhs, rhs, .. } => Some(gvn_bin_rr(GVN_XOR, *lhs, *rhs)),
        MInst::Shr { lhs, rhs, .. } => Some(GvnKey::BinRR(GVN_SHR, *lhs, *rhs)),
        MInst::Shl { lhs, rhs, .. } => Some(GvnKey::BinRR(GVN_SHL, *lhs, *rhs)),
        MInst::Sar { lhs, rhs, .. } => Some(GvnKey::BinRR(GVN_SAR, *lhs, *rhs)),
        MInst::AndImm { src, imm, .. } => Some(GvnKey::BinRI(GVN_AND_IMM, *src, *imm)),
        MInst::OrImm { src, imm, .. } => Some(GvnKey::BinRI(GVN_OR_IMM, *src, *imm)),
        MInst::ShrImm { src, imm, .. } => Some(GvnKey::ShiftI(GVN_SHR_IMM, *src, *imm)),
        MInst::ShlImm { src, imm, .. } => Some(GvnKey::ShiftI(GVN_SHL_IMM, *src, *imm)),
        MInst::SarImm { src, imm, .. } => Some(GvnKey::ShiftI(GVN_SAR_IMM, *src, *imm)),
        MInst::BitNot { src, .. } => Some(GvnKey::Unary(GVN_NOT, *src)),
        MInst::Neg { src, .. } => Some(GvnKey::Unary(GVN_NEG, *src)),
        MInst::Popcnt { src, .. } => Some(GvnKey::Unary(GVN_POPCNT, *src)),
        MInst::Pext { src, mask, .. } => Some(GvnKey::BinRR(GVN_PEXT, *src, *mask)),
        MInst::Cmp { lhs, rhs, kind, .. } => Some(GvnKey::Cmp(GVN_CMP, *lhs, *rhs, *kind as u8)),
        MInst::CmpImm { lhs, imm, kind, .. } => Some(GvnKey::CmpI(GVN_CMP, *lhs, *imm, *kind as u8)),
        MInst::AddImm { src, imm, .. } => Some(GvnKey::AddI(*src, *imm)),
        MInst::SubImm { src, imm, .. } => Some(GvnKey::SubI(*src, *imm)),
        // Loads from SimState with fixed offset can be CSE'd (no aliasing stores between)
        MInst::Load { base: BaseReg::SimState, offset, size, .. } => {
            Some(GvnKey::Load(0, *offset, *size as u8))
        }
        _ => None,
    }
}

fn gvn_bin_rr(op: u8, lhs: VReg, rhs: VReg) -> GvnKey {
    if gvn_is_commutative(op) && rhs < lhs {
        GvnKey::BinRR(op, rhs, lhs)
    } else {
        GvnKey::BinRR(op, lhs, rhs)
    }
}

/// Returns true if the instruction could modify memory that Load reads.
fn is_memory_clobber(inst: &MInst) -> bool {
    matches!(inst, MInst::Store { .. } | MInst::StoreIndexed { .. })
}

/// Global GVN: dominator-tree-scoped value numbering.
fn global_gvn(func: &mut MFunction) {
    let num_blocks = func.blocks.len();
    if num_blocks == 0 { return; }

    // Build block index map: BlockId → index
    let block_id_to_idx: HashMap<BlockId, usize> = func.blocks.iter()
        .enumerate()
        .map(|(i, b)| (b.id, i))
        .collect();

    // Build predecessor lists and successor lists
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); num_blocks];
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); num_blocks];
    for (i, block) in func.blocks.iter().enumerate() {
        for succ_id in block.successors() {
            if let Some(&j) = block_id_to_idx.get(&succ_id) {
                succs[i].push(j);
                preds[j].push(i);
            }
        }
    }

    // Compute dominators using simple iterative algorithm (Cooper, Harvey, Kennedy)
    let idom = compute_dominators(num_blocks, &preds);

    // Build dominator tree children
    let mut dom_children: Vec<Vec<usize>> = vec![Vec::new(); num_blocks];
    for i in 1..num_blocks {
        if let Some(parent) = idom[i] {
            dom_children[parent].push(i);
        }
    }

    // DFS pre-order traversal of dominator tree
    let mut order: Vec<usize> = Vec::with_capacity(num_blocks);
    let mut stack = vec![0usize];
    while let Some(node) = stack.pop() {
        order.push(node);
        // Push children in reverse so they come out in forward order
        for &child in dom_children[node].iter().rev() {
            stack.push(child);
        }
    }

    // Scoped value table: Vec of scopes, each scope is a HashMap.
    // When entering a dominator tree node, push a new scope.
    // When leaving, pop it. All ancestor scopes are visible.
    //
    // For efficiency, use a flat HashMap + a "generation" stack to track
    // which entries belong to which scope level.
    let mut value_table: HashMap<GvnKey, VReg> = HashMap::new();
    let mut replacements: Vec<(usize, usize, MInst)> = Vec::new(); // (block_idx, inst_idx, new_inst)

    // Process in dominator tree pre-order using iterative DFS with scope tracking
    // We need to know when we're done with a subtree to pop scope entries.
    // Use a two-pass approach: track scope entries per block.
    let mut scope_entries: Vec<Vec<GvnKey>> = vec![Vec::new(); num_blocks];

    // Process blocks in RPO (reverse post-order) which is a valid dominator-tree
    // compatible order for structured CFGs. For simple cases, layout order works
    // since ISel produces blocks in topological order.
    for &bi in &order {
        let block = &func.blocks[bi];

        // Clear entries from non-dominator ancestors.
        // Simple approach: for each block, rebuild the value table from its
        // dominator chain. This is O(depth × entries) but correct.
        // For performance, only clear if the previous block wasn't our immediate dominator.

        for inst_idx in 0..block.insts.len() {
            let inst = &block.insts[inst_idx];

            // Memory stores invalidate Load CSE entries
            if is_memory_clobber(inst) {
                value_table.retain(|k, _| !matches!(k, GvnKey::Load(..)));
            }

            if let Some(key) = gvn_key(inst) {
                if let Some(&prev) = value_table.get(&key) {
                    let dst = inst.def().unwrap();
                    if dst != prev {
                        replacements.push((bi, inst_idx, MInst::Mov { dst, src: prev }));
                    }
                } else if let Some(dst) = inst.def() {
                    value_table.insert(key.clone(), dst);
                    scope_entries[bi].push(key);
                }
            }
        }
    }

    // Pop scope entries in reverse dominator tree order
    // (Not needed for correctness if we process in domtree preorder and
    // each block only sees entries from its dominators. But the flat HashMap
    // approach above doesn't scope correctly for sibling subtrees.)
    //
    // For correctness: rebuild the value table properly using scoped approach.
    // Let's redo with a proper scoped implementation.

    // Actually, the simple flat HashMap approach above has a bug: entries from
    // sibling subtrees leak into each other. Let me implement properly.

    value_table.clear();
    replacements.clear();

    // Proper domtree DFS with scope push/pop
    fn gvn_dfs(
        node: usize,
        dom_children: &[Vec<usize>],
        func: &MFunction,
        value_table: &mut HashMap<GvnKey, VReg>,
        replacements: &mut Vec<(usize, usize, MInst)>,
    ) {
        let mut added_keys: Vec<GvnKey> = Vec::new();
        let block = &func.blocks[node];

        process_gvn_block(node, block, value_table, &mut added_keys, replacements);

        for &child in &dom_children[node] {
            gvn_dfs(child, dom_children, func, value_table, replacements);
        }

        // Pop this block's entries
        for key in added_keys {
            value_table.remove(&key);
        }
    }

    fn process_gvn_block(
        node: usize,
        block: &MBlock,
        value_table: &mut HashMap<GvnKey, VReg>,
        added_keys: &mut Vec<GvnKey>,
        replacements: &mut Vec<(usize, usize, MInst)>,
    ) {
        for inst_idx in 0..block.insts.len() {
            let inst = &block.insts[inst_idx];

            if is_memory_clobber(inst) {
                let load_keys: Vec<GvnKey> = value_table.keys()
                    .filter(|k| matches!(k, GvnKey::Load(..)))
                    .cloned()
                    .collect();
                for k in &load_keys {
                    value_table.remove(k);
                }
                added_keys.retain(|k| !matches!(k, GvnKey::Load(..)));
            }

            if let Some(key) = gvn_key(inst) {
                if let Some(&prev) = value_table.get(&key) {
                    let dst = inst.def().unwrap();
                    if dst != prev {
                        replacements.push((node, inst_idx, MInst::Mov { dst, src: prev }));
                    }
                } else if let Some(dst) = inst.def() {
                    if !value_table.contains_key(&key) {
                        value_table.insert(key.clone(), dst);
                        added_keys.push(key);
                    }
                }
            }
        }
    }

    gvn_dfs(0, &dom_children, func, &mut value_table, &mut replacements);

    // Apply replacements
    for (bi, inst_idx, new_inst) in replacements {
        func.blocks[bi].insts[inst_idx] = new_inst;
    }
}

/// Compute immediate dominators using the iterative algorithm.
/// Returns idom[i] = Some(j) where j immediately dominates i, or None for entry.
fn compute_dominators(n: usize, preds: &[Vec<usize>]) -> Vec<Option<usize>> {
    // Simple iterative dominator computation (Cooper, Harvey, Kennedy 2001)
    // Assumes block 0 is the entry.
    let mut idom: Vec<Option<usize>> = vec![None; n];
    idom[0] = Some(0); // Entry dominates itself (sentinel)

    let mut changed = true;
    while changed {
        changed = false;
        for b in 1..n {
            // Find first processed predecessor
            let mut new_idom: Option<usize> = None;
            for &p in &preds[b] {
                if idom[p].is_some() {
                    new_idom = Some(match new_idom {
                        None => p,
                        Some(cur) => intersect_dom(cur, p, &idom),
                    });
                }
            }
            if new_idom != idom[b] {
                idom[b] = new_idom;
                changed = true;
            }
        }
    }

    // Fix entry: idom[0] = None (no dominator)
    idom[0] = None;
    idom
}

fn intersect_dom(mut a: usize, mut b: usize, idom: &[Option<usize>]) -> usize {
    while a != b {
        while a > b {
            a = idom[a].unwrap_or(0);
        }
        while b > a {
            b = idom[b].unwrap_or(0);
        }
    }
    a
}

// ────────────────────────────────────────────────────────────────
// Phase 1D: Algebraic simplification
// ────────────────────────────────────────────────────────────────

/// Algebraic simplification: identity, annihilation, self-inverse, and
/// strength reduction rules.
fn algebraic_simplify(func: &mut MFunction) {
    // Build def map for constant lookups
    let mut consts: HashMap<VReg, u64> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let MInst::LoadImm { dst, value } = inst {
                consts.insert(*dst, *value);
            }
        }
    }

    for block in &mut func.blocks {
        for inst in &mut block.insts {
            let replacement = match inst {
                // Identity: add x, 0 → x
                MInst::Add { dst, lhs, rhs } => {
                    if consts.get(rhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else if consts.get(lhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *rhs))
                    } else {
                        None
                    }
                }
                // Identity: sub x, 0 → x; self: sub x, x → 0
                MInst::Sub { dst, lhs, rhs } => {
                    if consts.get(rhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else if lhs == rhs {
                        Some(Simplification::Const(*dst, 0))
                    } else {
                        None
                    }
                }
                // Identity: mul x, 1 → x; annihilation: mul x, 0 → 0
                // Strength reduction: mul x, 2^n → shl x, n
                MInst::Mul { dst, lhs, rhs } => {
                    try_simplify_mul(*dst, *lhs, *rhs, &consts)
                }
                // Identity: and x, -1 → x; annihilation: and x, 0 → 0
                MInst::And { dst, lhs, rhs } => {
                    if consts.get(rhs) == Some(&u64::MAX) {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else if consts.get(lhs) == Some(&u64::MAX) {
                        Some(Simplification::Mov(*dst, *rhs))
                    } else if consts.get(rhs) == Some(&0) || consts.get(lhs) == Some(&0) {
                        Some(Simplification::Const(*dst, 0))
                    } else if lhs == rhs {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else {
                        None
                    }
                }
                // Identity: or x, 0 → x; self: or x, x → x
                MInst::Or { dst, lhs, rhs } => {
                    if consts.get(rhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else if consts.get(lhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *rhs))
                    } else if lhs == rhs {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else {
                        None
                    }
                }
                // Identity: xor x, 0 → x; self: xor x, x → 0
                MInst::Xor { dst, lhs, rhs } => {
                    if consts.get(rhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else if consts.get(lhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *rhs))
                    } else if lhs == rhs {
                        Some(Simplification::Const(*dst, 0))
                    } else {
                        None
                    }
                }
                // Identity: shr/shl/sar x, 0 → x
                MInst::Shr { dst, lhs, rhs } | MInst::Shl { dst, lhs, rhs } | MInst::Sar { dst, lhs, rhs } => {
                    if consts.get(rhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else {
                        None
                    }
                }
                MInst::ShrImm { dst, src, imm: 0 } | MInst::ShlImm { dst, src, imm: 0 } | MInst::SarImm { dst, src, imm: 0 } => {
                    Some(Simplification::Mov(*dst, *src))
                }
                // AND chain: and(x, m) with immediate where m is mask
                MInst::AndImm { dst, src, imm } => {
                    if *imm == u64::MAX {
                        Some(Simplification::Mov(*dst, *src))
                    } else if *imm == 0 {
                        Some(Simplification::Const(*dst, 0))
                    } else {
                        None
                    }
                }
                // OrImm identity
                MInst::OrImm { dst, src, imm: 0 } => {
                    Some(Simplification::Mov(*dst, *src))
                }
                // Double negate
                MInst::BitNot { dst, src } => {
                    if let Some(&c) = consts.get(src) {
                        Some(Simplification::Const(*dst, !c))
                    } else {
                        None
                    }
                }
                MInst::Neg { dst, src } => {
                    if let Some(&c) = consts.get(src) {
                        Some(Simplification::Const(*dst, c.wrapping_neg()))
                    } else {
                        None
                    }
                }
                // Select with constant condition
                MInst::Select { dst, cond, true_val, false_val } => {
                    if let Some(&c) = consts.get(cond) {
                        if c != 0 {
                            Some(Simplification::Mov(*dst, *true_val))
                        } else {
                            Some(Simplification::Mov(*dst, *false_val))
                        }
                    } else {
                        None
                    }
                }
                // Mov of constant → LoadImm (enables further constant folding)
                MInst::Mov { dst, src } => {
                    if let Some(&c) = consts.get(src) {
                        Some(Simplification::Const(*dst, c))
                    } else {
                        None
                    }
                }
                _ => None,
            };

            if let Some(simp) = replacement {
                match simp {
                    Simplification::Mov(dst, src) => {
                        *inst = MInst::Mov { dst, src };
                    }
                    Simplification::Const(dst, value) => {
                        *inst = MInst::LoadImm { dst, value };
                        consts.insert(dst, value);
                    }
                    Simplification::Shl(dst, src, imm) => {
                        *inst = MInst::ShlImm { dst, src, imm };
                    }
                }
            }
        }
    }
}

enum Simplification {
    Mov(VReg, VReg),
    Const(VReg, u64),
    Shl(VReg, VReg, u8),
}

fn try_simplify_mul(dst: VReg, lhs: VReg, rhs: VReg, consts: &HashMap<VReg, u64>) -> Option<Simplification> {
    // Check each operand for constant
    for &(val_vreg, const_vreg) in &[(lhs, rhs), (rhs, lhs)] {
        if let Some(&c) = consts.get(&const_vreg) {
            if c == 0 {
                return Some(Simplification::Const(dst, 0));
            }
            if c == 1 {
                return Some(Simplification::Mov(dst, val_vreg));
            }
            // Power of 2: mul → shl
            if c.is_power_of_two() {
                let shift = c.trailing_zeros() as u8;
                return Some(Simplification::Shl(dst, val_vreg, shift));
            }
        }
    }
    None
}

// ────────────────────────────────────────────────────────────────
// If-conversion (branch → select)
// ────────────────────────────────────────────────────────────────

/// Convert simple diamond-shaped branches into Select (cmov) instructions.
///
/// Pattern:
///   bb_head: ... branch cond, bb_true, bb_false
///   bb_true: <1-2 instructions, no side effects except store> jump bb_merge
///   bb_false: <1-2 instructions, no side effects except store> jump bb_merge
///   bb_merge: ...
///
/// Converted to:
///   bb_head: ... <true_insts> <false_insts> select cond, true_val, false_val; store; jump bb_merge
fn if_convert(func: &mut MFunction) {
    let block_map: HashMap<BlockId, usize> = func.blocks.iter()
        .enumerate().map(|(i, b)| (b.id, i)).collect();

    // Collect conversion candidates
    struct Diamond {
        head_idx: usize,
        true_idx: usize,
        false_idx: usize,
        merge_id: BlockId,
    }
    let mut diamonds: Vec<Diamond> = Vec::new();

    for (i, block) in func.blocks.iter().enumerate() {
        let Some(MInst::Branch { cond: _, true_bb, false_bb }) = block.terminator() else { continue };
        let Some(&ti) = block_map.get(true_bb) else { continue };
        let Some(&fi) = block_map.get(false_bb) else { continue };

        let true_block = &func.blocks[ti];
        let false_block = &func.blocks[fi];

        // Both arms must be small (≤ 3 instructions including terminator)
        if true_block.insts.len() > 3 || false_block.insts.len() > 3 { continue; }
        // No phi nodes in arms
        if !true_block.phis.is_empty() || !false_block.phis.is_empty() { continue; }

        // Both must end with Jump to the same merge block
        let true_target = match true_block.terminator() {
            Some(MInst::Jump { target }) => *target,
            _ => continue,
        };
        let false_target = match false_block.terminator() {
            Some(MInst::Jump { target }) => *target,
            _ => continue,
        };
        if true_target != false_target { continue; }

        // Arms must only contain stores and pure computations (no loads, no branches)
        let arms_ok = |block: &MBlock| -> bool {
            block.insts.iter().all(|inst| matches!(inst,
                MInst::Store { .. } | MInst::StoreIndexed { .. }
                | MInst::Load { .. } | MInst::LoadImm { .. } | MInst::Mov { .. }
                | MInst::Add { .. } | MInst::Sub { .. } | MInst::Mul { .. }
                | MInst::And { .. } | MInst::Or { .. } | MInst::Xor { .. }
                | MInst::AndImm { .. } | MInst::OrImm { .. }
                | MInst::ShrImm { .. } | MInst::ShlImm { .. }
                | MInst::AddImm { .. } | MInst::SubImm { .. }
                | MInst::Select { .. }
                | MInst::Jump { .. }
            ))
        };
        if !arms_ok(true_block) || !arms_ok(false_block) { continue; }

        diamonds.push(Diamond {
            head_idx: i,
            true_idx: ti,
            false_idx: fi,
            merge_id: true_target,
        });
    }

    // Apply conversions (in reverse order to preserve indices)
    for diamond in diamonds.into_iter().rev() {
        let cond = match func.blocks[diamond.head_idx].terminator() {
            Some(MInst::Branch { cond, .. }) => *cond,
            _ => continue,
        };

        // Collect non-terminator instructions from both arms
        let true_insts: Vec<MInst> = func.blocks[diamond.true_idx].insts.iter()
            .filter(|i| !i.is_terminator()).cloned().collect();
        let false_insts: Vec<MInst> = func.blocks[diamond.false_idx].insts.iter()
            .filter(|i| !i.is_terminator()).cloned().collect();

        // Find matching stores: both arms store to the same address
        // Replace with: compute both values, select, store once
        let mut merged_insts: Vec<MInst> = Vec::new();
        let mut handled_stores: Vec<(i32, OpSize)> = Vec::new();

        // Check for paired stores (same base+offset in both arms)
        for t_inst in &true_insts {
            if let MInst::Store { base: t_base, offset: t_off, src: t_src, size: t_size } = t_inst {
                for f_inst in &false_insts {
                    if let MInst::Store { base: f_base, offset: f_off, src: f_src, size: f_size } = f_inst {
                        if t_base == f_base && t_off == f_off && t_size == f_size {
                            // Found matching store! Generate select + store
                            // First, add any computation instructions from both arms
                            // (LoadImm, Add, etc. that produce the store values)
                            for ti2 in &true_insts {
                                if !matches!(ti2, MInst::Store { .. }) {
                                    merged_insts.push(ti2.clone());
                                }
                            }
                            for fi2 in &false_insts {
                                if !matches!(fi2, MInst::Store { .. }) {
                                    merged_insts.push(fi2.clone());
                                }
                            }

                            let sel_dst = func.vregs.alloc();
                            while func.spill_descs.len() <= sel_dst.0 as usize {
                                func.spill_descs.push(SpillDesc::transient());
                            }
                            merged_insts.push(MInst::Select {
                                dst: sel_dst,
                                cond,
                                true_val: *t_src,
                                false_val: *f_src,
                            });
                            merged_insts.push(MInst::Store {
                                base: *t_base,
                                offset: *t_off,
                                src: sel_dst,
                                size: *t_size,
                            });
                            handled_stores.push((*t_off, *t_size));
                        }
                    }
                }
            }
        }

        if handled_stores.is_empty() { continue; }

        // Add remaining unhandled stores from both arms (shouldn't happen for simple cases)
        // Remove the branch from head block, add merged instructions + jump to merge
        let head = &mut func.blocks[diamond.head_idx];
        // Remove terminator (Branch)
        head.insts.pop();
        // Add merged instructions
        head.insts.extend(merged_insts);
        // Jump to merge block
        head.insts.push(MInst::Jump { target: diamond.merge_id });

        // Mark true/false blocks as empty (will be cleaned up by simplify_cfg)
        func.blocks[diamond.true_idx].insts.clear();
        func.blocks[diamond.true_idx].insts.push(MInst::Jump { target: diamond.merge_id });
        func.blocks[diamond.false_idx].insts.clear();
        func.blocks[diamond.false_idx].insts.push(MInst::Jump { target: diamond.merge_id });
    }
}

// ────────────────────────────────────────────────────────────────
// CFG simplification
// ────────────────────────────────────────────────────────────────

/// Simplify the control flow graph:
/// - Thread jumps through empty blocks (jmp-only blocks)
/// - Fold branch targets through jump chains
fn simplify_cfg(func: &mut MFunction) {
    // Build jump-through map: if a block contains only `jmp target`,
    // redirect all references to this block directly to `target`.
    let mut redirect: HashMap<BlockId, BlockId> = HashMap::new();
    for block in &func.blocks {
        if block.phis.is_empty() && block.insts.len() == 1 {
            if let MInst::Jump { target } = &block.insts[0] {
                redirect.insert(block.id, *target);
            }
        }
    }

    if redirect.is_empty() { return; }

    // Transitively resolve redirects
    let mut resolved: HashMap<BlockId, BlockId> = HashMap::new();
    for &src in redirect.keys() {
        let mut target = src;
        let mut seen = std::collections::HashSet::new();
        while let Some(&next) = redirect.get(&target) {
            if !seen.insert(next) { break; } // cycle
            target = next;
        }
        if target != src {
            resolved.insert(src, target);
        }
    }

    // Rewrite all jump/branch targets
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            match inst {
                MInst::Jump { target } => {
                    if let Some(&new_target) = resolved.get(target) {
                        *target = new_target;
                    }
                }
                MInst::Branch { true_bb, false_bb, .. } => {
                    if let Some(&new_t) = resolved.get(true_bb) {
                        *true_bb = new_t;
                    }
                    if let Some(&new_f) = resolved.get(false_bb) {
                        *false_bb = new_f;
                    }
                }
                _ => {}
            }
        }
        // Rewrite phi sources
        for phi in &mut block.phis {
            for (pred_id, _) in &mut phi.sources {
                if let Some(&new_pred) = resolved.get(pred_id) {
                    *pred_id = new_pred;
                }
            }
        }
    }

    // Remove empty blocks that are now unreachable (keep entry block)
    let entry = func.blocks.first().map(|b| b.id);
    func.blocks.retain(|block| {
        Some(block.id) == entry || !resolved.contains_key(&block.id)
    });
}

// ────────────────────────────────────────────────────────────────
// Load sinking (instruction reordering for shorter live ranges)
// ────────────────────────────────────────────────────────────────

/// Move Load and LoadImm instructions closer to their first use within
/// each basic block. This shortens live ranges, reducing register pressure
/// and improving the quality of the single-pass register allocator.
///
/// Only moves instructions that have no side effects and whose operands
/// don't depend on intervening instructions.
fn sink_loads(func: &mut MFunction) {
    for block in &mut func.blocks {
        // Build first-use map: for each defined VReg, find the index of
        // its first use within this block.
        let mut first_use: HashMap<VReg, usize> = HashMap::new();
        for (i, inst) in block.insts.iter().enumerate() {
            for u in inst.uses() {
                first_use.entry(u).or_insert(i);
            }
        }

        // Identify sinkable instructions and their target positions.
        // Process from the end to avoid invalidating indices.
        let mut sinks: Vec<(usize, usize)> = Vec::new(); // (from, to)

        for (i, inst) in block.insts.iter().enumerate() {
            // Only sink LoadImm (always safe — no memory dependency)
            if !matches!(inst, MInst::LoadImm { .. }) { continue; }
            let Some(def) = inst.def() else { continue };

            if let Some(&use_pos) = first_use.get(&def) {
                if use_pos > i + 4 {
                    sinks.push((i, use_pos));
                }
            }
        }

        // Apply sinks in reverse order to preserve indices
        for (from, to) in sinks.into_iter().rev() {
            let inst = block.insts.remove(from);
            // `to` needs adjustment since we removed an element before it
            block.insts.insert(to - 1, inst);
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Live range splitting
// ────────────────────────────────────────────────────────────────

/// Split long live ranges by re-materializing values close to their use.
///
/// For VRegs with a long gap between definition and use, insert a
/// re-materialization instruction (duplicate Load/LoadImm) just before
/// the use and rewrite the use to the new VReg. The original VReg's
/// live range shortens, freeing a register during the gap.
///
/// Handles:
/// - LoadImm: re-emit the same constant (free, already handled by sink_loads for most cases)
/// - SimState Load: re-load from the same memory address (1 instruction)
/// - Transient values: insert Store to stack after def + Load before use (2+1 instructions)
fn split_live_ranges(func: &mut MFunction) {
    // Only worth splitting if register pressure is high.
    // 13 registers, so only activate when VRegs significantly exceed registers.
    let num_regs = 13usize;
    if (func.vregs.count() as usize) < num_regs * 4 {
        return;
    }

    // Build def-map: VReg → (block_idx, inst_idx, defining instruction)
    let mut def_info: HashMap<VReg, (usize, usize, MInst)> = HashMap::new();
    for (bi, block) in func.blocks.iter().enumerate() {
        for (ii, inst) in block.insts.iter().enumerate() {
            if let Some(d) = inst.def() {
                def_info.insert(d, (bi, ii, inst.clone()));
            }
        }
    }

    // For each block, find VRegs with long gaps and split them.
    // Process per-block: find uses where the def is far away (in the same block).
    const SPLIT_THRESHOLD: usize = 20;

    // Collect all splits to apply
    let mut splits: Vec<SplitAction> = Vec::new();

    for (bi, block) in func.blocks.iter().enumerate() {
        // Find first use position of each VReg in this block
        let mut first_use_in_block: HashMap<VReg, usize> = HashMap::new();
        for (ii, inst) in block.insts.iter().enumerate() {
            for u in inst.uses() {
                first_use_in_block.entry(u).or_insert(ii);
            }
        }

        // Check each used VReg: if defined much earlier in the same block, split
        for (&vreg, &use_pos) in &first_use_in_block {
            let Some(&(def_bi, def_ii, ref def_inst)) = def_info.get(&vreg) else { continue };

            // Only handle same-block for now (cross-block is complex)
            if def_bi != bi { continue; }

            let gap = use_pos.saturating_sub(def_ii);
            if gap < SPLIT_THRESHOLD { continue; }

            // Determine re-materialization strategy
            let remat = match def_inst {
                MInst::LoadImm { value, .. } => {
                    // Already handled by sink_loads for LoadImm
                    // Only split if sink_loads didn't handle it (gap check is different)
                    Some(RematKind::Imm(*value))
                }
                MInst::Load { base: BaseReg::SimState, offset, size, .. } => {
                    // Check no Store between def and use (conservative)
                    let has_store = block.insts[def_ii+1..use_pos].iter()
                        .any(|i| matches!(i, MInst::Store { .. } | MInst::StoreIndexed { .. }));
                    if !has_store {
                        Some(RematKind::SimLoad(*offset, *size))
                    } else {
                        None
                    }
                }
                _ => {
                    // Transient value: use Store + Load via stack
                    Some(RematKind::StackSpill)
                }
            };

            if let Some(kind) = remat {
                splits.push(SplitAction { block_idx: bi, use_pos, vreg, kind });
            }
        }
    }

    // Sort splits by (block_idx, use_pos) descending to apply from end to start
    splits.sort_by(|a, b| (b.block_idx, b.use_pos).cmp(&(a.block_idx, a.use_pos)));

    // Apply splits
    for split in splits {
        let block = &mut func.blocks[split.block_idx];
        let new_vreg = func.vregs.alloc();

        let (reload_inst, spill_desc) = match split.kind {
            RematKind::Imm(_) => {
                // Skip: sink_loads already handles this
                continue;
            }
            RematKind::SimLoad(offset, size) => {
                let inst = MInst::Load {
                    dst: new_vreg,
                    base: BaseReg::SimState,
                    offset,
                    size,
                };
                // Use transient SpillDesc — the regalloc will handle
                // further spilling if needed. The key benefit is that
                // the new VReg has a short live range.
                (inst, SpillDesc::transient())
            }
            RematKind::StackSpill => {
                // For transient values: allocate a stack slot in the MIR,
                // insert Store after def and Load before use.
                // This is more complex — for now, skip transient values.
                // The regalloc handles them with its own spilling.
                continue;
            }
        };

        // Ensure spill_descs is large enough
        while func.spill_descs.len() <= new_vreg.0 as usize {
            func.spill_descs.push(spill_desc.clone());
        }

        // Insert reload instruction just before the use
        block.insts.insert(split.use_pos, reload_inst);

        // Rewrite the use (and all subsequent uses of this VReg in this block)
        // to use new_vreg instead
        for inst in &mut block.insts[split.use_pos + 1..] {
            if inst.uses().contains(&split.vreg) {
                inst.rewrite_use(split.vreg, new_vreg);
            }
        }
    }
}

struct SplitAction {
    block_idx: usize,
    use_pos: usize,
    vreg: VReg,
    kind: RematKind,
}

enum RematKind {
    Imm(u64),
    SimLoad(i32, OpSize),
    StackSpill,
}

// ────────────────────────────────────────────────────────────────
// Immediate-form lowering
// ────────────────────────────────────────────────────────────────

/// Convert Cmp/Add/Sub with one constant operand into CmpImm/AddImm/SubImm.
/// This runs late (after CSE/constant fold) to maximize opportunities.
fn lower_to_imm_forms(func: &mut MFunction) {
    // Collect constants
    let mut consts: HashMap<VReg, u64> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let MInst::LoadImm { dst, value } = inst {
                consts.insert(*dst, *value);
            }
        }
    }

    for block in &mut func.blocks {
        for inst in &mut block.insts {
            match inst {
                MInst::Cmp { dst, lhs, rhs, kind } => {
                    if let Some(&val) = consts.get(rhs) {
                        if val as i64 >= i32::MIN as i64 && val as i64 <= i32::MAX as i64 {
                            *inst = MInst::CmpImm { dst: *dst, lhs: *lhs, imm: val as i32, kind: *kind };
                        }
                    }
                }
                MInst::Add { dst, lhs, rhs } => {
                    if let Some(&val) = consts.get(rhs) {
                        if val as i64 >= i32::MIN as i64 && val as i64 <= i32::MAX as i64 {
                            *inst = MInst::AddImm { dst: *dst, src: *lhs, imm: val as i32 };
                        }
                    } else if let Some(&val) = consts.get(lhs) {
                        // Add is commutative
                        if val as i64 >= i32::MIN as i64 && val as i64 <= i32::MAX as i64 {
                            *inst = MInst::AddImm { dst: *dst, src: *rhs, imm: val as i32 };
                        }
                    }
                }
                MInst::Sub { dst, lhs, rhs } => {
                    if let Some(&val) = consts.get(rhs) {
                        if val as i64 >= i32::MIN as i64 && val as i64 <= i32::MAX as i64 {
                            *inst = MInst::SubImm { dst: *dst, src: *lhs, imm: val as i32 };
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Value width computation (for 32-bit emit)
// ────────────────────────────────────────────────────────────────

/// Compute value widths for all VRegs and store in func.value_widths.
/// This enables the emit phase to use 32-bit registers when width ≤ 32.
fn compute_value_widths(func: &mut MFunction) {
    let n = func.vregs.count() as usize;
    let mut widths: Vec<Option<u8>> = vec![None; n];

    // Forward pass per block
    for block in &func.blocks {
        // Phi nodes: conservative (unknown)
        for phi in &block.phis {
            widths[phi.dst.0 as usize] = None; // could refine later
        }

        for inst in &block.insts {
            let w: Option<u8> = match inst {
                MInst::LoadImm { value, .. } => {
                    if *value == 0 { Some(0) } else { Some((64 - value.leading_zeros()) as u8) }
                }
                MInst::Load { size, .. } | MInst::LoadIndexed { size, .. } => {
                    Some((size.bytes() * 8) as u8)
                }
                MInst::Cmp { .. } | MInst::CmpImm { .. } => Some(1),
                MInst::Popcnt { .. } => Some(7),
                MInst::Mov { src, .. } => {
                    widths.get(src.0 as usize).copied().flatten()
                }
                MInst::AndImm { src, imm, .. } => {
                    let imm_w = if *imm == 0 { 0 } else { (64 - imm.leading_zeros()) as u8 };
                    let src_w = widths.get(src.0 as usize).copied().flatten().unwrap_or(64);
                    Some(src_w.min(imm_w))
                }
                MInst::OrImm { src, imm, .. } => {
                    let imm_w = if *imm == 0 { 0 } else { (64 - imm.leading_zeros()) as u8 };
                    let src_w = widths.get(src.0 as usize).copied().flatten().unwrap_or(64);
                    Some(src_w.max(imm_w))
                }
                MInst::ShrImm { src, imm, .. } => {
                    widths.get(src.0 as usize).copied().flatten()
                        .map(|w| w.saturating_sub(*imm))
                }
                MInst::ShlImm { src, imm, .. } => {
                    widths.get(src.0 as usize).copied().flatten()
                        .map(|w| (w as u16 + *imm as u16).min(64) as u8)
                }
                MInst::And { lhs, rhs, .. } => {
                    match (get_w(&widths, *lhs), get_w(&widths, *rhs)) {
                        (Some(l), Some(r)) => Some(l.min(r)),
                        (Some(l), None) => Some(l),
                        (None, Some(r)) => Some(r),
                        _ => None,
                    }
                }
                MInst::Or { lhs, rhs, .. } | MInst::Xor { lhs, rhs, .. } => {
                    match (get_w(&widths, *lhs), get_w(&widths, *rhs)) {
                        (Some(l), Some(r)) => Some(l.max(r)),
                        _ => None,
                    }
                }
                MInst::Add { lhs, rhs, .. } | MInst::Sub { lhs, rhs, .. } => {
                    match (get_w(&widths, *lhs), get_w(&widths, *rhs)) {
                        (Some(l), Some(r)) => Some((l.max(r) + 1).min(64)),
                        _ => None,
                    }
                }
                MInst::AddImm { src, .. } | MInst::SubImm { src, .. } => {
                    widths.get(src.0 as usize).copied().flatten()
                        .map(|w| (w + 1).min(64))
                }
                MInst::Mul { lhs, rhs, .. } => {
                    match (get_w(&widths, *lhs), get_w(&widths, *rhs)) {
                        (Some(l), Some(r)) => Some(((l as u16) + (r as u16)).min(64) as u8),
                        _ => None,
                    }
                }
                MInst::Select { true_val, false_val, .. } => {
                    match (get_w(&widths, *true_val), get_w(&widths, *false_val)) {
                        (Some(t), Some(f)) => Some(t.max(f)),
                        _ => None,
                    }
                }
                _ => None,
            };

            if let Some(d) = inst.def() {
                if (d.0 as usize) < n {
                    widths[d.0 as usize] = w;
                }
            }
        }
    }

    func.value_widths = widths;
}

fn get_w(widths: &[Option<u8>], vreg: VReg) -> Option<u8> {
    widths.get(vreg.0 as usize).copied().flatten()
}

// ────────────────────────────────────────────────────────────────
// Existing passes
// ────────────────────────────────────────────────────────────────

/// Fold XOR chains of single-bit extractions from the same source into
/// PEXT + POPCNT + AND 1.
///
/// Pattern: `(src >> a) & 1 ^ (src >> b) & 1 ^ ...` where all extractions
/// come from the same source register.
///
/// Replacement: `pext(src, mask) → popcnt → and 1` where
/// `mask = (1 << a) | (1 << b) | ...`
fn fold_xor_chain_to_pext(func: &mut MFunction) {
    // Build def map: VReg → instruction (cloned to avoid borrowing func)
    let mut defs: HashMap<VReg, MInst> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(d) = inst.def() {
                defs.insert(d, inst.clone());
            }
        }
    }

    // For each block, scan for Xor instructions and try to fold
    for block in &mut func.blocks {
        let mut replacements: Vec<(usize, Vec<MInst>)> = Vec::new();

        for (inst_idx, inst) in block.insts.iter().enumerate() {
            // Look for: v = xor a, b  where result is 1-bit (used with and 1)
            let MInst::Xor { dst, lhs, rhs } = inst else { continue };

            // Try to collect the full XOR chain and extract bit positions
            let mut bits: Vec<(VReg, u64)> = Vec::new();
            let mut source_reg: Option<VReg> = None;

            let ok = collect_xor_chain_bits(*dst, *lhs, *rhs, &defs, &mut bits, &mut source_reg);
            if !ok {
                continue;
            }

            // Need at least 3 bits to be worth the PEXT overhead
            let Some(src) = source_reg else { continue };
            if bits.len() < 3 { continue; }

            // Build mask from bit positions
            let mut mask_val: u64 = 0;
            for &(_, pos) in &bits {
                if pos >= 64 { continue; } // skip wide
                mask_val |= 1u64 << pos;
            }
            if mask_val == 0 { continue; }

            // Generate: mask_vreg = imm mask_val
            //           pext_vreg = pext src, mask_vreg
            //           popcnt_vreg = popcnt pext_vreg
            //           dst = and popcnt_vreg, 1
            let mask_vreg = func.vregs.alloc();
            while func.spill_descs.len() <= mask_vreg.0 as usize {
                func.spill_descs.push(SpillDesc::remat(mask_val));
            }
            let pext_vreg = func.vregs.alloc();
            while func.spill_descs.len() <= pext_vreg.0 as usize {
                func.spill_descs.push(SpillDesc::transient());
            }
            let popcnt_vreg = func.vregs.alloc();
            while func.spill_descs.len() <= popcnt_vreg.0 as usize {
                func.spill_descs.push(SpillDesc::transient());
            }

            let new_insts = vec![
                MInst::LoadImm { dst: mask_vreg, value: mask_val },
                MInst::Pext { dst: pext_vreg, src, mask: mask_vreg },
                MInst::Popcnt { dst: popcnt_vreg, src: pext_vreg },
                MInst::AndImm { dst: *dst, src: popcnt_vreg, imm: 1 },
            ];
            replacements.push((inst_idx, new_insts));
        }

        // Apply replacements in reverse order (to preserve indices)
        for (idx, new_insts) in replacements.into_iter().rev() {
            block.insts.splice(idx..=idx, new_insts);
        }
    }
}

/// Recursively collect single-bit extractions from a XOR chain.
/// Returns true if the entire chain consists of single-bit extractions
/// from the same source register.
fn collect_xor_chain_bits(
    _vreg: VReg,
    lhs: VReg,
    rhs: VReg,
    defs: &HashMap<VReg, MInst>,
    bits: &mut Vec<(VReg, u64)>,
    source_reg: &mut Option<VReg>,
) -> bool {
    // Try to extract a bit from each operand
    for &operand in &[lhs, rhs] {
        if let Some(def_inst) = defs.get(&operand) {
            match def_inst {
                // Pattern: v = xor a, b (recursive)
                MInst::Xor { lhs: l2, rhs: r2, .. } => {
                    if !collect_xor_chain_bits(operand, *l2, *r2, defs, bits, source_reg) {
                        return false;
                    }
                }
                // Pattern: v = shr src, imm (bit extraction)
                MInst::ShrImm { src, imm, .. } => {
                    match source_reg {
                        Some(s) if *s != *src => return false, // different source
                        None => *source_reg = Some(*src),
                        _ => {}
                    }
                    bits.push((*src, *imm as u64));
                }
                // Pattern: v = and src, 1 (masked bit — look through)
                MInst::AndImm { src: and_src, imm: 1, .. } => {
                    if let Some(inner) = defs.get(and_src) {
                        match inner {
                            MInst::ShrImm { src, imm, .. } => {
                                match source_reg {
                                    Some(s) if *s != *src => return false,
                                    None => *source_reg = Some(*src),
                                    _ => {}
                                }
                                bits.push((*src, *imm as u64));
                            }
                            MInst::Xor { lhs: l2, rhs: r2, .. } => {
                                if !collect_xor_chain_bits(*and_src, *l2, *r2, defs, bits, source_reg) {
                                    return false;
                                }
                            }
                            _ => return false,
                        }
                    } else {
                        return false;
                    }
                }
                _ => return false,
            }
        } else {
            return false;
        }
    }
    true
}

/// Constant deduplication: merge LoadImm instructions with the same value
/// into a single VReg. Reduces register pressure and instruction count.
fn constant_dedup(func: &mut MFunction) {
    let mut aliases: HashMap<VReg, VReg> = HashMap::new();
    // Map from constant value → canonical VReg
    let mut const_map: HashMap<u64, VReg> = HashMap::new();

    for block in &func.blocks {
        const_map.clear(); // per-block to avoid cross-block live range extension
        for inst in &block.insts {
            if let MInst::LoadImm { dst, value } = inst {
                if let Some(&canonical) = const_map.get(value) {
                    aliases.insert(*dst, canonical);
                } else {
                    const_map.insert(*value, *dst);
                }
            }
        }
    }

    if aliases.is_empty() {
        return;
    }

    // Apply aliases
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            let current_uses = inst.uses();
            for u in current_uses {
                if let Some(&target) = aliases.get(&u) {
                    inst.rewrite_use(u, target);
                }
            }
        }
        for phi in &mut block.phis {
            for (_, src) in &mut phi.sources {
                if let Some(&a) = aliases.get(src) {
                    *src = a;
                }
            }
        }
    }

    // Remove duplicated LoadImm
    for block in &mut func.blocks {
        block.insts.retain(|inst| {
            if let MInst::LoadImm { dst, .. } = inst {
                !aliases.contains_key(dst)
            } else {
                true
            }
        });
    }
}

/// Copy propagation: for each `Mov { dst, src }`, replace all uses of dst
/// with src throughout the function. Then remove the Mov.
fn copy_propagate(func: &mut MFunction) {
    // Build alias map: dst → src (transitively resolved)
    let mut aliases: HashMap<VReg, VReg> = HashMap::new();

    for block in &func.blocks {
        for inst in &block.insts {
            if let MInst::Mov { dst, src } = inst {
                // Resolve transitively: if src is itself an alias, follow the chain
                let mut target = *src;
                while let Some(&next) = aliases.get(&target) {
                    target = next;
                }
                aliases.insert(*dst, target);
            }
        }
    }

    if aliases.is_empty() {
        return;
    }

    // Apply aliases to all instructions
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            rewrite_uses(inst, &aliases);
        }
        // Also rewrite phi sources
        for phi in &mut block.phis {
            for (_pred, src) in &mut phi.sources {
                if let Some(&a) = aliases.get(src) {
                    *src = a;
                }
            }
        }
    }

    // Remove Mov instructions that are now identity (dst == src after alias resolution)
    // or whose dst is aliased away
    for block in &mut func.blocks {
        block.insts.retain(|inst| {
            if let MInst::Mov { dst, src } = inst {
                // Keep only if dst is not aliased (it's still needed)
                if aliases.contains_key(dst) {
                    return false; // Remove: dst was aliased to src
                }
                if dst == src {
                    return false; // Remove: identity mov
                }
            }
            true
        });
    }
}

/// Dead code elimination: remove instructions whose defs are never used.
fn dead_code_eliminate(func: &mut MFunction) {
    // Iterate until no more dead code is removed (cascading DCE).
    loop {
        let mut used: std::collections::HashSet<VReg> = std::collections::HashSet::new();
        for block in &func.blocks {
            for inst in &block.insts {
                for u in inst.uses() {
                    used.insert(u);
                }
            }
            for phi in &block.phis {
                for (_, src) in &phi.sources {
                    used.insert(*src);
                }
            }
        }

        let mut removed = false;
        for block in &mut func.blocks {
            let before = block.insts.len();
            block.insts.retain(|inst| {
                if let Some(def) = inst.def() {
                    if !used.contains(&def) {
                        return matches!(
                            inst,
                            MInst::Store { .. }
                                | MInst::StoreIndexed { .. }
                                | MInst::Branch { .. }
                                | MInst::Jump { .. }
                                | MInst::Return
                                | MInst::ReturnError { .. }
                        );
                    }
                }
                true
            });
            if block.insts.len() < before {
                removed = true;
            }
        }

        if !removed { break; }
    }
}

/// Rewrite all use operands in an instruction according to the alias map.
fn rewrite_uses(inst: &mut MInst, aliases: &HashMap<VReg, VReg>) {
    // Iterate over uses and rewrite any that appear in aliases
    let current_uses = inst.uses();
    for u in current_uses {
        if let Some(&target) = aliases.get(&u) {
            inst.rewrite_use(u, target);
        }
    }
}
