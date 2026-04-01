//! Global Value Numbering (GVN) pass for SIR.
//!
//! Assigns a canonical value number to each expression (opcode + operand
//! value numbers). If two instructions have the same value number, the
//! second is replaced with an alias to the first's result register.
//!
//! Works per-block for now (local value numbering). Cross-block GVN
//! requires dominator tree analysis, which can be added later.

use super::pass_manager::ExecutionUnitPass;
use super::shared::{collect_all_used_registers, def_reg, resolve_transitive_aliases};
use crate::HashMap;
use crate::ir::*;
use crate::optimizer::PassOptions;

pub(super) struct GvnPass;

impl ExecutionUnitPass for GvnPass {
    fn name(&self) -> &'static str {
        "gvn"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        let mut aliases: HashMap<RegisterId, RegisterId> = HashMap::default();

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

        let register_map = &eu.register_map;
        for block in eu.blocks.values_mut() {
            // Skip loop blocks — expressions may evaluate differently across iterations
            if loop_blocks.contains(&block.id) {
                continue;
            }
            gvn_block(&mut block.instructions, &mut aliases, register_map, &[]);
        }

        if aliases.is_empty() {
            return;
        }

        // Resolve transitive aliases (A→B, B→C → A→C)
        let aliases = resolve_transitive_aliases(&aliases);

        // Apply aliases to all instructions, block params, and terminators
        for block in eu.blocks.values_mut() {
            for inst in &mut block.instructions {
                apply_aliases(inst, &aliases);
            }
            match &mut block.terminator {
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
                _ => {}
            }
        }

        // DCE: remove instructions whose defs are no longer used
        let used = collect_all_used_registers(eu);
        for block in eu.blocks.values_mut() {
            block.instructions.retain(|inst| {
                if let Some(d) = def_reg(inst) {
                    // Keep if the register is used, or if the instruction has side effects
                    used.contains(&d)
                        || matches!(inst, SIRInstruction::Store(..) | SIRInstruction::Commit(..))
                } else {
                    true // Keep side-effecting instructions (Store, Commit)
                }
            });
        }
    }
}

/// Value numbering key: identifies an expression by its operation and
/// operand value numbers.
#[derive(Clone, PartialEq, Eq, Hash)]
enum ValueKey {
    /// Constant immediate
    Imm(Vec<u64>),
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
) {
    // Map from ValueKey → canonical RegisterId (first occurrence)
    let mut value_table: HashMap<ValueKey, RegisterId> = HashMap::default();
    // Map from RegisterId → canonical RegisterId (for looking up operands)
    let mut canonical: HashMap<RegisterId, RegisterId> = HashMap::default();
    // Registers that transitively depend on block params (loop variables).
    // Expressions depending on these must not be GVN'd.
    let mut loop_dependent: crate::HashSet<RegisterId> = block_params.iter().copied().collect();
    // Track known constant values for Mux constant folding
    let mut imm_constants: HashMap<RegisterId, u64> = HashMap::default();

    let resolve = |r: RegisterId, canonical: &HashMap<RegisterId, RegisterId>| -> RegisterId {
        canonical.get(&r).copied().unwrap_or(r)
    };

    for inst in instructions.iter() {
        let key = match inst {
            SIRInstruction::Imm(dst, val) => {
                // Track constant for Mux folding
                if let Some(v) = crate::optimizer::coalescing::shared::sir_value_to_u64(val) {
                    imm_constants.insert(*dst, v);
                }
                // Include mask in key for 4-state correctness:
                // Imm(0, mask=0) ≠ Imm(0, mask=0xFF)
                let mut key_data = val.payload.to_u64_digits();
                key_data.push(u64::MAX); // separator
                key_data.extend(val.mask.to_u64_digits());
                Some(ValueKey::Imm(key_data))
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
                    canonical.insert(*dst, selected);
                    continue;
                }
                let w = register_map.get(dst).map(|t| t.width()).unwrap_or(0);
                Some(ValueKey::Mux(c, t, e, w))
            }
            // Load: depends on memory state, cannot be value-numbered
            // (Store-Load forwarding handles Load redundancy separately)
            SIRInstruction::Load(..) => None,
            // Store/Commit: side-effecting. Invalidate all Load-derived values.
            SIRInstruction::Store(..) | SIRInstruction::Commit(..) => {
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
                    loop_dependent.contains(&resolve(*lhs, &canonical))
                        || loop_dependent.contains(&resolve(*rhs, &canonical))
                }
                SIRInstruction::Unary(_, _, src) => {
                    loop_dependent.contains(&resolve(*src, &canonical))
                }
                SIRInstruction::Concat(_, args) => args
                    .iter()
                    .any(|a| loop_dependent.contains(&resolve(*a, &canonical))),
                SIRInstruction::Slice(_, src, _, _) => {
                    loop_dependent.contains(&resolve(*src, &canonical))
                }
                SIRInstruction::Mux(_, cond, then_val, else_val) => {
                    loop_dependent.contains(&resolve(*cond, &canonical))
                        || loop_dependent.contains(&resolve(*then_val, &canonical))
                        || loop_dependent.contains(&resolve(*else_val, &canonical))
                }
                _ => false,
            };

            if uses_loop_var {
                // Mark result as loop-dependent; don't GVN
                loop_dependent.insert(dst);
                canonical.insert(dst, dst);
            } else if let Some(&existing) = value_table.get(&key) {
                // Redundant: alias dst to existing
                aliases.insert(dst, existing);
                canonical.insert(dst, existing);
            } else {
                // First occurrence: record as canonical
                value_table.insert(key, dst);
                canonical.insert(dst, dst);
            }
        }
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
        SIRInstruction::Store(_, _, _, src, _) => {
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
    }
}
