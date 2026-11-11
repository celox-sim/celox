//! Concat folding: merge consecutive Slices/Loads in Concat args into
//! wider operations, reducing ISel's shl+or expansion.

use super::pass_manager::ExecutionUnitPass;
use super::shared::{StateWritePositions, collect_all_used_registers, is_unused_definition};
use crate::HashMap;
use crate::PassOptions;
use crate::ir::*;
use std::sync::Arc;

/// Tracks a bit-extraction source: either Slice(reg, off, w) or Load(addr, off, w).
#[derive(Clone, Copy, PartialEq, Eq)]
enum BitSourceBase {
    Register(RegisterId),
    Load(RegionedAbsoluteAddr),
}

#[derive(Clone, Copy)]
struct BitSource {
    base: BitSourceBase,
    bit_offset: usize,
    width: usize,
    load_site: Option<(BlockId, usize)>,
}

pub(in crate::optimizer) struct ConcatFoldingPass {
    unpacked_element_widths: Arc<crate::HashMap<AbsoluteAddr, usize>>,
    max_load_width: usize,
}

impl Default for ConcatFoldingPass {
    fn default() -> Self {
        Self {
            unpacked_element_widths: Arc::default(),
            max_load_width: 64,
        }
    }
}

impl ConcatFoldingPass {
    pub(in crate::optimizer) fn new(
        unpacked_element_widths: Arc<crate::HashMap<AbsoluteAddr, usize>>,
        max_load_width: usize,
    ) -> Self {
        Self {
            unpacked_element_widths,
            max_load_width,
        }
    }
}

impl ExecutionUnitPass for ConcatFoldingPass {
    fn name(&self) -> &'static str {
        "concat_folding"
    }

    fn handles_calls(&self) -> bool {
        true
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        let mut max_reg = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0);
        let mut changed = fold_slices_of_concat(eu);

        // Build extract def map: RegisterId → (base, static_offset, width).
        // A Slice over a known base composes offsets, so Concat can merge
        // direct slices and slices of loads with the same code path.
        let mut extract_defs: HashMap<RegisterId, BitSource> = HashMap::default();
        for block in eu.blocks.values() {
            for (index, inst) in block.instructions.iter().enumerate() {
                match inst {
                    SIRInstruction::Load(dst, addr, SIROffset::Static(off), width) => {
                        extract_defs.insert(
                            *dst,
                            BitSource {
                                base: BitSourceBase::Load(*addr),
                                bit_offset: *off,
                                width: *width,
                                load_site: Some((block.id, index)),
                            },
                        );
                    }
                    SIRInstruction::Slice(dst, src, off, width) => {
                        if let Some(src_info) = extract_defs.get(src) {
                            extract_defs.insert(
                                *dst,
                                BitSource {
                                    base: src_info.base,
                                    bit_offset: src_info.bit_offset + *off,
                                    width: *width,
                                    load_site: src_info.load_site,
                                },
                            );
                        } else {
                            extract_defs.insert(
                                *dst,
                                BitSource {
                                    base: BitSourceBase::Register(*src),
                                    bit_offset: *off,
                                    width: *width,
                                    load_site: None,
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
            let writes = StateWritePositions::new(&block.instructions);
            let block_id = block.id;
            let mut new_insts_to_insert: Vec<(usize, SIRInstruction<RegionedAbsoluteAddr>)> =
                Vec::new();

            for (inst_idx, inst) in block.instructions.iter_mut().enumerate() {
                // A register slice keeps its SSA snapshot. A replacement Load
                // must still observe the state from every original Load.
                let available = |info: &&BitSource| match (info.base, info.load_site) {
                    (BitSourceBase::Load(address), Some((load_block, load_index))) => {
                        load_block == block_id
                            && !writes.written_between(address, load_index, inst_idx)
                    }
                    (BitSourceBase::Register(_), None) => true,
                    _ => false,
                };
                let SIRInstruction::Concat(_dst, args) = inst else {
                    continue;
                };
                if args.len() < 3 {
                    continue;
                }

                // Walk LSB-first, find consecutive runs from the same base.
                let mut new_args: Vec<RegisterId> = Vec::new();
                let mut i = args.len();
                let mut any_merged = false;

                while i > 0 {
                    i -= 1;
                    let arg = args[i];

                    if let Some(&info) = extract_defs.get(&arg).filter(available) {
                        let run_base = info.base;
                        let run_start = info.bit_offset;
                        let mut run_width = info.width;
                        let mut run_count = 1usize;

                        while i > 0 {
                            let prev = args[i - 1];
                            if let Some(&prev_info) = extract_defs.get(&prev).filter(available) {
                                if prev_info.base == run_base
                                    && prev_info.bit_offset == run_start + run_width
                                {
                                    let Some(combined_width) =
                                        run_width.checked_add(prev_info.width)
                                    else {
                                        break;
                                    };
                                    // Leave the first operand which does not fit
                                    // for the next outer iteration. Consuming the
                                    // whole run and then rejecting it below drops
                                    // every operand except `arg` from the Concat.
                                    if combined_width > self.max_load_width {
                                        break;
                                    }
                                    if let BitSourceBase::Load(address) = run_base
                                        && let Some(&element_width) = self
                                            .unpacked_element_widths
                                            .get(&address.absolute_addr())
                                    {
                                        let combined_end = run_start + combined_width;
                                        let crosses_element = run_start / element_width
                                            != combined_end.saturating_sub(1) / element_width;
                                        if crosses_element
                                            && (run_start % element_width != 0
                                                || combined_width % element_width != 0)
                                        {
                                            break;
                                        }
                                    }
                                    run_width = combined_width;
                                    run_count += 1;
                                    i -= 1;
                                } else {
                                    break;
                                }
                            } else {
                                break;
                            }
                        }

                        if run_count >= 2 && run_width <= self.max_load_width {
                            max_reg += 1;
                            let new_reg = RegisterId(max_reg);
                            eu.register_map.insert(
                                new_reg,
                                RegisterType::Bit {
                                    width: run_width,
                                    signed: false,
                                },
                            );

                            let folded_inst = match run_base {
                                BitSourceBase::Register(src) => {
                                    SIRInstruction::Slice(new_reg, src, run_start, run_width)
                                }
                                BitSourceBase::Load(addr) => {
                                    let offset = self
                                        .unpacked_element_widths
                                        .get(&addr.absolute_addr())
                                        .copied()
                                        .filter(|element_width| {
                                            run_start / *element_width
                                                != (run_start + run_width - 1) / *element_width
                                        })
                                        .map_or(SIROffset::Static(run_start), |element_width| {
                                            SIROffset::PackedElements {
                                                bit_offset: run_start,
                                                element_width,
                                            }
                                        });
                                    SIRInstruction::Load(new_reg, addr, offset, run_width)
                                }
                            };
                            new_insts_to_insert.push((inst_idx, folded_inst));
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
            block
                .instructions
                .retain(|inst| !is_unused_definition(inst, &used));
        }
    }
}

/// Fold an extraction which lies wholly inside one input of a Concat.
///
/// HDL lowering frequently packs fields for an observable output and then
/// immediately extracts the same fields for internal computation:
///
/// ```text
/// packed = Concat([sign, exponent, fraction])
/// exponent_again = Slice(packed, fraction_width, exponent_width)
/// ```
///
/// Lowering the latter through the packed word creates a shift and mask even
/// though the original SSA value is still available.  Keep the public packed
/// value, but make the internal extraction refer directly to its input.
fn fold_slices_of_concat(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) -> bool {
    let concat_defs = eu
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            SIRInstruction::Concat(dst, args) => Some((*dst, args.clone())),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let register_map = &eu.register_map;
    let mut changed = false;

    for block in eu.blocks.values_mut() {
        for instruction in &mut block.instructions {
            let SIRInstruction::Slice(dst, packed, offset, width) = instruction else {
                continue;
            };
            let Some(args) = concat_defs.get(packed) else {
                continue;
            };
            let Some(slice_end) = offset.checked_add(*width) else {
                continue;
            };

            // Concat arguments are MSB-first. Walk them in reverse so
            // `argument_start` is the argument's LSB position.
            let mut argument_start = 0usize;
            for argument in args.iter().rev().copied() {
                let Some(argument_type) = register_map.get(&argument) else {
                    break;
                };
                let argument_width = argument_type.width();
                let Some(argument_end) = argument_start.checked_add(argument_width) else {
                    break;
                };
                if *offset >= argument_start && slice_end <= argument_end {
                    let inner_offset = *offset - argument_start;
                    if inner_offset == 0
                        && *width == argument_width
                        && register_map.get(dst) == Some(argument_type)
                    {
                        *instruction = SIRInstruction::Unary(*dst, UnaryOp::Ident, argument);
                    } else {
                        *instruction = SIRInstruction::Slice(*dst, argument, inner_offset, *width);
                    }
                    changed = true;
                    break;
                }
                argument_start = argument_end;
            }
        }
    }

    changed
}

#[cfg(test)]
mod tests;
