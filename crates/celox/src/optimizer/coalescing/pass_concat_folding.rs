//! Concat folding: merge consecutive Slices in Concat args into wider Slices.
//!
//! When a Concat has multiple consecutive args that are Slices from the same
//! source with adjacent bit ranges, they are merged into a single wider Slice.
//! This reduces ISel's shl+or expansion from N instructions to 1 per batch.

use super::pass_manager::ExecutionUnitPass;
use super::shared::{collect_all_used_registers, def_reg};
use crate::HashMap;
use crate::ir::*;
use crate::optimizer::PassOptions;

pub(super) struct ConcatFoldingPass;

impl ExecutionUnitPass for ConcatFoldingPass {
    fn name(&self) -> &'static str {
        "concat_folding"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        // Find max register ID for generating fresh ones
        let mut max_reg = 0usize;
        for &r in eu.register_map.keys() {
            max_reg = max_reg.max(r.0);
        }
        let mut next_reg = max_reg + 1;

        let mut changed = false;

        // Build Slice def map
        let mut slice_defs: HashMap<RegisterId, (RegisterId, usize, usize)> = HashMap::default();
        for block in eu.blocks.values() {
            for inst in &block.instructions {
                if let SIRInstruction::Slice(dst, src, off, width) = inst {
                    slice_defs.insert(*dst, (*src, *off, *width));
                }
            }
        }

        // Collect new Slice instructions to insert (before Concat)
        let mut new_slices: Vec<(BlockId, usize, SIRInstruction<RegionedAbsoluteAddr>)> = Vec::new();

        for (&block_id, block) in &eu.blocks {
            for (inst_idx, inst) in block.instructions.iter().enumerate() {
                let SIRInstruction::Concat(_dst, args) = inst else { continue };
                if args.len() < 3 { continue; }

                // Walk args LSB-first, detect consecutive Slice runs
                let mut new_args: Vec<RegisterId> = Vec::new();
                let mut i = args.len();
                let mut any_merged = false;

                while i > 0 {
                    i -= 1;
                    let arg = args[i];

                    if let Some(&(src, off, width)) = slice_defs.get(&arg) {
                        // Start of a potential run
                        let run_src = src;
                        let run_start_off = off;
                        let mut run_width = width;
                        let mut run_count = 1usize;

                        // Try to extend: preceding args (higher bit positions) from same source
                        while i > 0 {
                            let prev = args[i - 1];
                            if let Some(&(ps, po, pw)) = slice_defs.get(&prev) {
                                if ps == run_src && po == run_start_off + run_width {
                                    run_width += pw;
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
                            // Merged! Create a new wider Slice instruction.
                            let new_reg = RegisterId(next_reg);
                            next_reg += 1;

                            // Determine the type: Logic or Bit based on source
                            let src_type = eu.register_map.get(&run_src);
                            let new_type = match src_type {
                                Some(RegisterType::Logic { .. }) => RegisterType::Logic { width: run_width },
                                _ => RegisterType::Bit { width: run_width, signed: false },
                            };
                            eu.register_map.insert(new_reg, new_type);

                            new_slices.push((
                                block_id,
                                inst_idx, // insert before the Concat
                                SIRInstruction::Slice(new_reg, run_src, run_start_off, run_width),
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
                    // new_args was built LSB-first, but Concat expects [MSB, ..., LSB]
                    new_args.reverse();
                    // We'll replace the Concat's args below
                    // Mark for replacement
                    changed = true;
                }
            }
        }

        if !changed {
            return;
        }

        // Insert new Slice instructions and update Concat args
        // Re-process: simpler to rebuild
        let mut slice_defs2: HashMap<RegisterId, (RegisterId, usize, usize)> = slice_defs;
        for (_, _, inst) in &new_slices {
            if let SIRInstruction::Slice(dst, src, off, width) = inst {
                slice_defs2.insert(*dst, (*src, *off, *width));
            }
        }

        for (&block_id, block) in &mut eu.blocks {
            // Insert new slices before their Concat
            let mut insertions: Vec<(usize, SIRInstruction<RegionedAbsoluteAddr>)> = Vec::new();
            for &(bid, idx, ref inst) in &new_slices {
                if bid == block_id {
                    insertions.push((idx, inst.clone()));
                }
            }
            // Insert in reverse order to preserve indices
            insertions.sort_by(|a, b| b.0.cmp(&a.0));
            for (idx, inst) in insertions {
                block.instructions.insert(idx, inst);
            }

            // Update Concat args
            for inst in &mut block.instructions {
                let SIRInstruction::Concat(_, args) = inst else { continue };
                if args.len() < 3 { continue; }

                let mut new_args: Vec<RegisterId> = Vec::new();
                let mut i = args.len();

                while i > 0 {
                    i -= 1;
                    let arg = args[i];

                    if let Some(&(src, off, width)) = slice_defs2.get(&arg) {
                        let run_src = src;
                        let mut run_width = width;
                        let mut run_count = 1usize;

                        while i > 0 {
                            let prev = args[i - 1];
                            if let Some(&(ps, po, pw)) = slice_defs2.get(&prev) {
                                if ps == run_src && po == off + run_width {
                                    run_width += pw;
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
                            // Find the new_slice register for this run
                            let merged = new_slices.iter().find(|(bid, _, s)| {
                                *bid == block_id && matches!(s,
                                    SIRInstruction::Slice(_, s2, o2, w2)
                                    if *s2 == run_src && *o2 == off && *w2 == run_width
                                )
                            });
                            if let Some((_, _, SIRInstruction::Slice(reg, _, _, _))) = merged {
                                new_args.push(*reg);
                            } else {
                                new_args.push(arg); // fallback
                            }
                        } else {
                            new_args.push(arg);
                        }
                    } else {
                        new_args.push(arg);
                    }
                }

                new_args.reverse();
                *args = new_args;
            }
        }

        // DCE
        let used = collect_all_used_registers(eu);
        for block in eu.blocks.values_mut() {
            block.instructions.retain(|inst| {
                if let Some(d) = def_reg(inst) {
                    used.contains(&d) || matches!(inst, SIRInstruction::Store(..) | SIRInstruction::Commit(..))
                } else {
                    true
                }
            });
        }
    }
}
