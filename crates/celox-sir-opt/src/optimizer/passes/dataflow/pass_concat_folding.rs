//! Concat folding: merge consecutive Slices/Loads in Concat args into
//! wider operations, reducing ISel's shl+or expansion.

use super::pass_manager::ExecutionUnitPass;
use super::shared::{collect_all_used_registers, def_reg};
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

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        let mut max_reg = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0);
        let mut changed = fold_slices_of_concat(eu);

        // Build extract def map: RegisterId → (base, static_offset, width).
        // A Slice over a known base composes offsets, so Concat can merge
        // direct slices and slices of loads with the same code path.
        let mut extract_defs: HashMap<RegisterId, BitSource> = HashMap::default();
        for block in eu.blocks.values() {
            for inst in &block.instructions {
                match inst {
                    SIRInstruction::Load(dst, addr, SIROffset::Static(off), width) => {
                        extract_defs.insert(
                            *dst,
                            BitSource {
                                base: BitSourceBase::Load(*addr),
                                bit_offset: *off,
                                width: *width,
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
                                },
                            );
                        } else {
                            extract_defs.insert(
                                *dst,
                                BitSource {
                                    base: BitSourceBase::Register(*src),
                                    bit_offset: *off,
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

                // Walk LSB-first, find consecutive runs from the same base.
                let mut new_args: Vec<RegisterId> = Vec::new();
                let mut i = args.len();
                let mut any_merged = false;

                while i > 0 {
                    i -= 1;
                    let arg = args[i];

                    if let Some(&info) = extract_defs.get(&arg) {
                        let run_base = info.base;
                        let run_start = info.bit_offset;
                        let mut run_width = info.width;
                        let mut run_count = 1usize;

                        while i > 0 {
                            let prev = args[i - 1];
                            if let Some(&prev_info) = extract_defs.get(&prev) {
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
mod tests {
    use super::*;
    use crate::ir::{BasicBlock, BlockId, InstanceId, SIRTerminator, STABLE_REGION};
    use celox_design::StateObjectId as VarId;

    fn test_addr() -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: STABLE_REGION,
            instance_id: InstanceId(0),
            var_id: VarId::default(),
        }
    }

    fn make_eu(
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        register_map: HashMap<RegisterId, RegisterType>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Return,
            },
        );
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }

    #[test]
    fn folds_consecutive_slices_from_same_register() {
        let mut register_map = HashMap::default();
        register_map.insert(
            RegisterId(0),
            RegisterType::Bit {
                width: 32,
                signed: false,
            },
        );
        for reg in 1..=3 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 4,
                    signed: false,
                },
            );
        }
        register_map.insert(
            RegisterId(4),
            RegisterType::Bit {
                width: 12,
                signed: false,
            },
        );

        let instructions = vec![
            SIRInstruction::Slice(RegisterId(1), RegisterId(0), 0, 4),
            SIRInstruction::Slice(RegisterId(2), RegisterId(0), 4, 4),
            SIRInstruction::Slice(RegisterId(3), RegisterId(0), 8, 4),
            SIRInstruction::Concat(
                RegisterId(4),
                vec![RegisterId(3), RegisterId(2), RegisterId(1)],
            ),
            SIRInstruction::RuntimeEvent {
                site_id: 0,
                args: vec![RegisterId(4)],
            },
        ];

        let mut eu = make_eu(instructions, register_map);
        ConcatFoldingPass::default().run(&mut eu, &PassOptions::default());
        let block = eu.blocks.get(&BlockId(0)).unwrap();

        assert!(
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Slice(_, RegisterId(0), 0, 12)))
        );
        assert!(block.instructions.iter().any(|inst| matches!(
            inst,
            SIRInstruction::Concat(RegisterId(4), args) if args.len() == 1
        )));
    }

    #[test]
    fn splits_slice_runs_at_the_fold_width_without_dropping_operands() {
        const SOURCE_WIDTH: usize = 129;
        const MAX_FOLD_WIDTH: usize = 128;
        let bit = |width| RegisterType::Bit {
            width,
            signed: false,
        };
        let register_map = HashMap::from_iter([
            (RegisterId(0), bit(SOURCE_WIDTH)),
            (RegisterId(1), bit(64)),
            (RegisterId(2), bit(64)),
            (RegisterId(3), bit(1)),
            (RegisterId(4), bit(SOURCE_WIDTH)),
        ]);
        let instructions = vec![
            SIRInstruction::Slice(RegisterId(1), RegisterId(0), 0, 64),
            SIRInstruction::Slice(RegisterId(2), RegisterId(0), 64, 64),
            SIRInstruction::Slice(RegisterId(3), RegisterId(0), 128, 1),
            SIRInstruction::Concat(
                RegisterId(4),
                vec![RegisterId(3), RegisterId(2), RegisterId(1)],
            ),
            SIRInstruction::RuntimeEvent {
                site_id: 0,
                args: vec![RegisterId(4)],
            },
        ];

        let mut eu = make_eu(instructions, register_map);
        eu.blocks
            .get_mut(&BlockId(0))
            .unwrap()
            .params
            .push(RegisterId(0));
        eu.verify_result().unwrap();
        ConcatFoldingPass::new(Arc::default(), MAX_FOLD_WIDTH)
            .run(&mut eu, &PassOptions::default());
        eu.verify_result().unwrap();

        let concat_args = eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .find_map(|instruction| match instruction {
                SIRInstruction::Concat(RegisterId(4), args) => Some(args),
                _ => None,
            })
            .unwrap();
        assert_eq!(concat_args.len(), 2);
        assert_eq!(
            concat_args
                .iter()
                .map(|argument| eu.register_map[argument].width())
                .sum::<usize>(),
            SOURCE_WIDTH,
        );
        assert!(
            eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .any(|instruction| {
                    matches!(
                        instruction,
                        SIRInstruction::Slice(_, RegisterId(0), 0, MAX_FOLD_WIDTH)
                    )
                })
        );
    }

    #[test]
    fn folds_slices_back_to_their_concat_inputs() {
        let bit = |width| RegisterType::Bit {
            width,
            signed: false,
        };
        let mut register_map = HashMap::default();
        register_map.insert(RegisterId(0), bit(52));
        register_map.insert(RegisterId(1), bit(11));
        register_map.insert(RegisterId(2), bit(1));
        register_map.insert(RegisterId(3), bit(64));
        register_map.insert(RegisterId(4), bit(52));
        register_map.insert(RegisterId(5), bit(6));
        register_map.insert(RegisterId(6), bit(1));

        let instructions = vec![
            SIRInstruction::Concat(
                RegisterId(3),
                vec![RegisterId(2), RegisterId(1), RegisterId(0)],
            ),
            SIRInstruction::Slice(RegisterId(4), RegisterId(3), 0, 52),
            SIRInstruction::Slice(RegisterId(5), RegisterId(3), 55, 6),
            SIRInstruction::Slice(RegisterId(6), RegisterId(3), 63, 1),
            SIRInstruction::RuntimeEvent {
                site_id: 0,
                args: vec![RegisterId(3), RegisterId(4), RegisterId(5), RegisterId(6)],
            },
        ];

        let mut eu = make_eu(instructions, register_map);
        ConcatFoldingPass::default().run(&mut eu, &PassOptions::default());

        let instructions = &eu.blocks[&BlockId(0)].instructions;
        assert!(instructions.iter().any(|instruction| matches!(
            instruction,
            SIRInstruction::Unary(RegisterId(4), UnaryOp::Ident, RegisterId(0))
        )));
        assert!(instructions.iter().any(|instruction| matches!(
            instruction,
            SIRInstruction::Slice(RegisterId(5), RegisterId(1), 3, 6)
        )));
        assert!(instructions.iter().any(|instruction| matches!(
            instruction,
            SIRInstruction::Unary(RegisterId(6), UnaryOp::Ident, RegisterId(2))
        )));
    }

    #[test]
    fn folds_consecutive_slices_from_same_loaded_value() {
        let addr = test_addr();
        let mut register_map = HashMap::default();
        register_map.insert(
            RegisterId(0),
            RegisterType::Bit {
                width: 32,
                signed: false,
            },
        );
        for reg in 1..=3 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 4,
                    signed: false,
                },
            );
        }
        register_map.insert(
            RegisterId(4),
            RegisterType::Bit {
                width: 12,
                signed: false,
            },
        );

        let instructions = vec![
            SIRInstruction::Load(RegisterId(0), addr, SIROffset::Static(16), 32),
            SIRInstruction::Slice(RegisterId(1), RegisterId(0), 0, 4),
            SIRInstruction::Slice(RegisterId(2), RegisterId(0), 4, 4),
            SIRInstruction::Slice(RegisterId(3), RegisterId(0), 8, 4),
            SIRInstruction::Concat(
                RegisterId(4),
                vec![RegisterId(3), RegisterId(2), RegisterId(1)],
            ),
            SIRInstruction::RuntimeEvent {
                site_id: 0,
                args: vec![RegisterId(4)],
            },
        ];

        let mut eu = make_eu(instructions, register_map);
        ConcatFoldingPass::default().run(&mut eu, &PassOptions::default());
        let block = eu.blocks.get(&BlockId(0)).unwrap();

        assert!(block.instructions.iter().any(|inst| matches!(
            inst,
            SIRInstruction::Load(_, a, SIROffset::Static(16), 12) if *a == addr
        )));
        assert!(block.instructions.iter().any(|inst| matches!(
            inst,
            SIRInstruction::Concat(RegisterId(4), args) if args.len() == 1
        )));
    }

    #[test]
    fn folding_unpacked_bit_elements_emits_explicit_packed_elements_load() {
        let addr = test_addr();
        let mut register_map = HashMap::default();
        let mut instructions = Vec::new();
        for lane in 0..64 {
            let register = RegisterId(lane);
            register_map.insert(
                register,
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
            instructions.push(SIRInstruction::Load(
                register,
                addr,
                SIROffset::Static(lane),
                1,
            ));
        }
        let result = RegisterId(64);
        register_map.insert(
            result,
            RegisterType::Bit {
                width: 64,
                signed: false,
            },
        );
        instructions.push(SIRInstruction::Concat(
            result,
            (0..64).rev().map(RegisterId).collect(),
        ));
        instructions.push(SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![result],
        });

        let mut eu = make_eu(instructions, register_map);
        let element_widths = Arc::new(HashMap::from_iter([(addr.absolute_addr(), 1)]));
        ConcatFoldingPass::new(element_widths, 64).run(&mut eu, &PassOptions::default());

        assert!(eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Load(
                    _,
                    address,
                    SIROffset::PackedElements {
                        bit_offset: 0,
                        element_width: 1,
                    },
                    64,
                ) if *address == addr
            )
        }));
    }
}
