//! Concat folding: merge consecutive Slices/Loads in Concat args into
//! wider operations, reducing ISel's shl+or expansion.

use super::pass_manager::ExecutionUnitPass;
use super::shared::{collect_all_used_registers, def_reg};
use crate::HashMap;
use crate::ir::*;
use crate::optimizer::PassOptions;

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

pub(super) struct ConcatFoldingPass;

const MAX_FOLDED_LOAD_WIDTH: usize = 64;

impl ExecutionUnitPass for ConcatFoldingPass {
    fn name(&self) -> &'static str {
        "concat_folding"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        let mut max_reg = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0);
        let mut changed = false;

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

                        if run_count >= 2 && run_width <= MAX_FOLDED_LOAD_WIDTH {
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
                                BitSourceBase::Load(addr) => SIRInstruction::Load(
                                    new_reg,
                                    addr,
                                    SIROffset::Static(run_start),
                                    run_width,
                                ),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{BasicBlock, BlockId, InstanceId, SIRTerminator, STABLE_REGION};
    use veryl_analyzer::ir::VarId;

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
        ConcatFoldingPass.run(&mut eu, &PassOptions::default());
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
        ConcatFoldingPass.run(&mut eu, &PassOptions::default());
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
}
