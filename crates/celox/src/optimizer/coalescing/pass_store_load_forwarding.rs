use super::pass_manager::ExecutionUnitPass;
use super::shared::{
    collect_all_used_registers, def_reg, resolve_transitive_aliases, sir_value_to_u64,
};
use crate::HashMap;
use crate::ir::*;
use crate::optimizer::PassOptions;

pub(super) struct StoreLoadForwardingPass;

impl ExecutionUnitPass for StoreLoadForwardingPass {
    fn name(&self) -> &'static str {
        "store_load_forwarding"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        for block in eu.blocks.values_mut() {
            forward_and_simplify(&mut block.instructions);
        }

        // Apply aliases across the whole EU
        // (block params, terminators, all instructions)
        // then DCE
        dead_code_eliminate(eu);
    }
}

/// Per-block store-load forwarding + algebraic simplification.
/// Marks forwarded loads as dead by turning them into identity aliases.
fn forward_and_simplify(instructions: &mut [SIRInstruction<RegionedAbsoluteAddr>]) {
    // Track latest stored register for each (addr, bit_offset, width)
    #[derive(Clone, Copy, PartialEq, Eq, Hash)]
    struct StoreKey {
        addr: RegionedAbsoluteAddr,
        bit_offset: usize,
    }

    struct StoreEntry {
        src: RegisterId,
        width: usize,
    }

    let mut known_stores: HashMap<StoreKey, StoreEntry> = HashMap::default();
    let mut known_constants: HashMap<RegisterId, u64> = HashMap::default();
    let mut aliases: HashMap<RegisterId, RegisterId> = HashMap::default();

    for inst in instructions.iter_mut() {
        match inst {
            SIRInstruction::Store(addr, SIROffset::Static(off), width, src, _triggers) => {
                let key = StoreKey {
                    addr: *addr,
                    bit_offset: *off,
                };
                // Invalidate overlapping entries for same addr
                let store_end = *off + *width;
                known_stores.retain(|k, v| {
                    k.addr != *addr || {
                        let existing_end = k.bit_offset + v.width;
                        // Keep if ranges don't overlap
                        store_end <= k.bit_offset || existing_end <= *off
                    }
                });
                known_stores.insert(
                    key,
                    StoreEntry {
                        src: *src,
                        width: *width,
                    },
                );
            }
            SIRInstruction::Store(addr, SIROffset::Dynamic(_), _, _, _) => {
                // Conservatively invalidate all entries for this addr
                known_stores.retain(|k, _| k.addr != *addr);
            }
            SIRInstruction::Load(dst, addr, SIROffset::Static(off), width) => {
                let key = StoreKey {
                    addr: *addr,
                    bit_offset: *off,
                };
                if let Some(entry) = known_stores.get(&key) {
                    if entry.width == *width {
                        // Forward: alias dst to the stored register
                        aliases.insert(*dst, entry.src);
                    }
                }
            }
            SIRInstruction::Imm(dst, val) => {
                if let Some(v) = sir_value_to_u64(val) {
                    known_constants.insert(*dst, v);
                }
            }
            SIRInstruction::Binary(dst, lhs, op, rhs) => {
                let lhs_const = known_constants.get(lhs).copied();
                let rhs_const = known_constants.get(rhs).copied();

                match (op, lhs_const, rhs_const) {
                    // shift by 0 → identity
                    (BinaryOp::Shr | BinaryOp::Shl | BinaryOp::Sar, _, Some(0)) => {
                        aliases.insert(*dst, *lhs);
                    }
                    // or/add with 0 → identity
                    (BinaryOp::Or | BinaryOp::Add | BinaryOp::Xor, _, Some(0)) => {
                        aliases.insert(*dst, *lhs);
                    }
                    (BinaryOp::Or | BinaryOp::Add | BinaryOp::Xor, Some(0), _) => {
                        aliases.insert(*dst, *rhs);
                    }
                    // and with all-ones mask → identity (check if mask matches dst width)
                    (BinaryOp::And, _, Some(mask)) => {
                        // Check if mask is (1 << width) - 1 for some width
                        // If the mask covers the full width of lhs, it's identity
                        if mask == u64::MAX
                            || (mask > 0 && mask.count_ones() == mask.trailing_ones())
                        {
                            // Only alias if mask covers all bits — we can't easily
                            // know the bit width of lhs here, so only handle the
                            // common case where the And itself produces a result
                            // that is exactly the masked width.
                            // This is conservative: we skip if unsure.
                        }
                        // Actually, let's just check the specific pattern:
                        // If the And mask is all-ones for the width of the result,
                        // this is identity. But we don't have the result width
                        // readily available. Keep it simple: don't alias And here.
                        // The BitExtractPeepholePass handles the important cases.
                    }
                    _ => {}
                }
            }
            SIRInstruction::Commit(_, dst_addr, SIROffset::Static(_), _, _) => {
                // Invalidate known stores for the destination address
                known_stores.retain(|k, _| k.addr != *dst_addr);
            }
            SIRInstruction::Commit(_, dst_addr, SIROffset::Dynamic(_), _, _) => {
                known_stores.retain(|k, _| k.addr != *dst_addr);
            }
            _ => {}
        }
    }

    if aliases.is_empty() {
        return;
    }

    // Resolve transitive aliases
    let resolved = resolve_transitive_aliases(&aliases);

    // Apply aliases to all instruction operands
    for inst in instructions.iter_mut() {
        apply_aliases_to_inst(inst, &resolved);
    }
}

fn apply_aliases_to_inst(
    inst: &mut SIRInstruction<RegionedAbsoluteAddr>,
    aliases: &HashMap<RegisterId, RegisterId>,
) {
    match inst {
        SIRInstruction::Imm(_, _) => {}
        SIRInstruction::Binary(_, lhs, _, rhs) => {
            if let Some(&to) = aliases.get(lhs) {
                *lhs = to;
            }
            if let Some(&to) = aliases.get(rhs) {
                *rhs = to;
            }
        }
        SIRInstruction::Unary(_, _, src) => {
            if let Some(&to) = aliases.get(src) {
                *src = to;
            }
        }
        SIRInstruction::Load(_, _, SIROffset::Dynamic(off), _) => {
            if let Some(&to) = aliases.get(off) {
                *off = to;
            }
        }
        SIRInstruction::Load(_, _, SIROffset::Static(_), _) => {}
        SIRInstruction::Store(_, SIROffset::Dynamic(off), _, src, _) => {
            if let Some(&to) = aliases.get(off) {
                *off = to;
            }
            if let Some(&to) = aliases.get(src) {
                *src = to;
            }
        }
        SIRInstruction::Store(_, SIROffset::Static(_), _, src, _) => {
            if let Some(&to) = aliases.get(src) {
                *src = to;
            }
        }
        SIRInstruction::Commit(_, _, SIROffset::Dynamic(off), _, _) => {
            if let Some(&to) = aliases.get(off) {
                *off = to;
            }
        }
        SIRInstruction::Commit(_, _, SIROffset::Static(_), _, _) => {}
        SIRInstruction::Concat(_, args) => {
            for arg in args {
                if let Some(&to) = aliases.get(arg) {
                    *arg = to;
                }
            }
        }
        SIRInstruction::Mux(_, cond, then_val, else_val) => {
            if let Some(&to) = aliases.get(cond) {
                *cond = to;
            }
            if let Some(&to) = aliases.get(then_val) {
                *then_val = to;
            }
            if let Some(&to) = aliases.get(else_val) {
                *else_val = to;
            }
        }
        SIRInstruction::Slice(_, src, _, _) => {
            if let Some(&to) = aliases.get(src) {
                *src = to;
            }
        }
    }
}

/// Remove instructions whose defined register is never used.
fn dead_code_eliminate(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    // Iterate until no more changes (dead chains)
    loop {
        let used = collect_all_used_registers(eu);

        let mut changed = false;
        for block in eu.blocks.values_mut() {
            let before = block.instructions.len();
            block.instructions.retain(|inst| {
                if let Some(dst) = def_reg(inst) {
                    // Keep if the register is used somewhere
                    used.contains(&dst)
                } else {
                    // Store/Commit — always keep
                    true
                }
            });
            if block.instructions.len() != before {
                changed = true;
            }
        }

        if !changed {
            break;
        }
    }
}
