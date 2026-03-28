//! Concat folding: merge Concat of consecutive Slices from the same source
//! into a single wider Slice.
//!
//! Pattern:
//!   r1 = Slice(src, off+0, 1)
//!   r2 = Slice(src, off+1, 1)
//!   r3 = Concat(r2, r1)   // MSB-first = {bit1, bit0}
//!
//! Replacement:
//!   r3 = Slice(src, off, 2)
//!
//! Also handles wider slices and non-unit widths. Consecutive means
//! the bit ranges are adjacent and from the same source register.

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
        let mut changed = false;

        for block in eu.blocks.values_mut() {
            if fold_concats(&mut block.instructions, &eu.register_map) {
                changed = true;
            }
        }

        if !changed {
            return;
        }

        // DCE: remove instructions whose defs are unused
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

/// Info about a Slice instruction.
#[derive(Clone, Copy)]
struct SliceInfo {
    src: RegisterId,
    bit_offset: usize,
    width: usize,
}

fn fold_concats(
    instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    register_map: &HashMap<RegisterId, RegisterType>,
) -> bool {
    // Build def map: register → instruction info
    let mut slice_defs: HashMap<RegisterId, SliceInfo> = HashMap::default();
    for inst in instructions.iter() {
        if let SIRInstruction::Slice(dst, src, off, width) = inst {
            slice_defs.insert(*dst, SliceInfo { src: *src, bit_offset: *off, width: *width });
        }
    }

    let mut any_changed = false;

    for inst in instructions.iter_mut() {
        let SIRInstruction::Concat(dst, args) = inst else { continue };
        if args.len() < 2 { continue; }

        // args are [MSB, ..., LSB]. Check if consecutive Slices from same source.
        // Walk LSB-first and try to merge consecutive runs.
        let mut new_args: Vec<RegisterId> = Vec::new();
        let mut i = args.len(); // walk from end (LSB)

        while i > 0 {
            i -= 1;
            let arg = args[i];
            let Some(&info) = slice_defs.get(&arg) else {
                new_args.push(arg);
                continue;
            };

            // Try to extend this slice by absorbing preceding args (higher bits)
            let merged_offset = info.bit_offset;
            let _ = merged_offset;
            let mut merged_width = info.width;
            let merged_src = info.src;

            while i > 0 {
                let prev_arg = args[i - 1];
                let Some(&prev_info) = slice_defs.get(&prev_arg) else { break };
                // Check: same source and adjacent (prev is the next higher bits)
                if prev_info.src != merged_src {
                    break;
                }
                if prev_info.bit_offset != merged_offset + merged_width {
                    break;
                }
                // Merge!
                merged_width += prev_info.width;
                i -= 1;
            }

            if merged_width > info.width {
                // Merged! Create a wider Slice.
                // We can't create a new RegisterId here easily, so we rewrite
                // the first arg's definition to be wider. Instead, just record
                // as a Slice and let the Concat use fewer args.
                // Actually: we need to create a new SIR instruction.
                // For now: just replace the Concat with a single Slice if ALL
                // args merge into one.
                new_args.push(arg); // placeholder; handled below
            } else {
                new_args.push(arg);
            }
        }

        // If we can merge ALL args into a single Slice, replace the Concat
        // Check if the entire Concat is a contiguous Slice from one source
        let mut all_slices: Vec<SliceInfo> = Vec::new();
        let mut all_from_same = true;
        let mut first_src: Option<RegisterId> = None;

        for &arg in args.iter().rev() { // LSB first
            if let Some(&info) = slice_defs.get(&arg) {
                match first_src {
                    None => first_src = Some(info.src),
                    Some(s) if s != info.src => { all_from_same = false; break; }
                    _ => {}
                }
                all_slices.push(info);
            } else {
                all_from_same = false;
                break;
            }
        }

        if all_from_same && all_slices.len() == args.len() && all_slices.len() >= 2 {
            // Check if contiguous
            let mut contiguous = true;
            for j in 1..all_slices.len() {
                if all_slices[j].bit_offset != all_slices[j-1].bit_offset + all_slices[j-1].width {
                    contiguous = false;
                    break;
                }
            }

            if contiguous {
                let src = all_slices[0].src;
                let total_offset = all_slices[0].bit_offset;
                let total_width: usize = all_slices.iter().map(|s| s.width).sum();

                // Replace Concat with Slice
                *inst = SIRInstruction::Slice(*dst, src, total_offset, total_width);

                // Update register type
                let src_type = register_map.get(&src).cloned();
                if let Some(ty) = src_type {
                    // Result type should match total_width
                    let _ = ty; // register_map is immutable here; type is already set
                }

                any_changed = true;
            }
        }
    }

    any_changed
}
