//! Concat folding: merge consecutive Slices/Loads in Concat args into
//! wider operations, reducing ISel's shl+or expansion.

use super::pass_manager::ExecutionUnitPass;
use super::shared::{collect_all_used_registers, def_reg};
use crate::HashMap;
use crate::ir::*;
use crate::optimizer::PassOptions;

/// Tracks a bit-extraction source: either Slice(reg, off, w) or Load(addr, off, w).
#[derive(Clone, Copy)]
struct BitSource {
    /// For Slice: the source RegisterId. For Load: a hash of the address.
    addr: RegionedAbsoluteAddr,
    bit_offset: usize,
    width: usize,
}

pub(super) struct ConcatFoldingPass;

impl ExecutionUnitPass for ConcatFoldingPass {
    fn name(&self) -> &'static str {
        "concat_folding"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        let mut max_reg = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0);
        let mut changed = false;

        // Build Load def map: RegisterId → (addr, static_offset, width)
        let mut load_defs: HashMap<RegisterId, BitSource> = HashMap::default();
        for block in eu.blocks.values() {
            for inst in &block.instructions {
                match inst {
                    SIRInstruction::Load(dst, addr, SIROffset::Static(off), width) => {
                        load_defs.insert(
                            *dst,
                            BitSource {
                                addr: *addr,
                                bit_offset: *off,
                                width: *width,
                            },
                        );
                    }
                    SIRInstruction::Slice(dst, src, off, width) => {
                        // If src was loaded from a known addr, compute the effective addr+offset
                        if let Some(src_info) = load_defs.get(src) {
                            load_defs.insert(
                                *dst,
                                BitSource {
                                    addr: src_info.addr,
                                    bit_offset: src_info.bit_offset + *off,
                                    width: *width,
                                },
                            );
                        }
                    }
                    _ => {}
                }
            }
        }

        // Process each block
        for block in eu.blocks.values_mut() {
            let mut new_insts_to_insert: Vec<(usize, SIRInstruction<RegionedAbsoluteAddr>)> =
                Vec::new();

            for (inst_idx, inst) in block.instructions.iter_mut().enumerate() {
                let SIRInstruction::Concat(_dst, args) = inst else {
                    continue;
                };
                if args.len() < 3 {
                    continue;
                }

                // Walk LSB-first, find consecutive Load runs from same addr
                let mut new_args: Vec<RegisterId> = Vec::new();
                let mut i = args.len();
                let mut any_merged = false;

                while i > 0 {
                    i -= 1;
                    let arg = args[i];

                    if let Some(&info) = load_defs.get(&arg) {
                        let run_addr = info.addr;
                        let run_start = info.bit_offset;
                        let mut run_width = info.width;
                        let mut run_count = 1usize;

                        while i > 0 {
                            let prev = args[i - 1];
                            if let Some(&prev_info) = load_defs.get(&prev) {
                                if prev_info.addr == run_addr
                                    && prev_info.bit_offset == run_start + run_width
                                {
                                    run_width += prev_info.width;
                                    run_count += 1;
                                    i -= 1;
                                } else {
                                    break;
                                }
                            } else {
                                break;
                            }
                        }

                        if run_count >= 2 {
                            // Create a new wider Load
                            max_reg += 1;
                            let new_reg = RegisterId(max_reg);
                            eu.register_map.insert(
                                new_reg,
                                RegisterType::Bit {
                                    width: run_width,
                                    signed: false,
                                },
                            );

                            new_insts_to_insert.push((
                                inst_idx,
                                SIRInstruction::Load(
                                    new_reg,
                                    run_addr,
                                    SIROffset::Static(run_start),
                                    run_width,
                                ),
                            ));
                            new_args.push(new_reg);
                            any_merged = true;
                        } else {
                            new_args.push(arg);
                        }
                    } else {
                        new_args.push(arg);
                    }
                }

                if any_merged {
                    new_args.reverse();
                    *args = new_args;
                    changed = true;
                }
            }

            // Insert new Load instructions before Concats (reverse to preserve indices)
            for (idx, inst) in new_insts_to_insert.into_iter().rev() {
                block.instructions.insert(idx, inst);
            }
        }

        if !changed {
            return;
        }

        let used = collect_all_used_registers(eu);
        for block in eu.blocks.values_mut() {
            block.instructions.retain(|inst| {
                if let Some(d) = def_reg(inst) {
                    used.contains(&d)
                        || matches!(inst, SIRInstruction::Store(..) | SIRInstruction::Commit(..))
                } else {
                    true
                }
            });
        }
    }
}
