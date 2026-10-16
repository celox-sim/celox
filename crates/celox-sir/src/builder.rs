use crate::HashMap;
use crate::{
    BasicBlock, BinaryOp, BlockId, RegisterId, RegisterType, SIRInstruction, SIRTerminator,
    SIRValue, UnaryOp,
};

#[derive(Clone)]
pub struct SIRBuilder<Addr> {
    next_reg_id: usize,
    registers: HashMap<RegisterId, RegisterType>,
    current_block_id: Option<BlockId>,
    blocks: HashMap<BlockId, BasicBlock<Addr>>,
    next_block_id: usize,
}

impl<Addr> Default for SIRBuilder<Addr> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Addr> SIRBuilder<Addr> {
    pub fn new() -> Self {
        Self::new_with(0)
    }
    pub fn new_with(start_reg_id: usize) -> Self {
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                instructions: vec![],
                params: vec![],
                terminator: SIRTerminator::Return,
            },
        );
        Self {
            next_reg_id: start_reg_id,
            registers: HashMap::default(),
            current_block_id: Some(BlockId(0)),
            next_block_id: 1,
            blocks,
        }
    }
    /// Issue a register with a specific type explicitly
    pub fn alloc_reg(&mut self, ty: RegisterType) -> RegisterId {
        let id = RegisterId(self.next_reg_id);
        self.registers.insert(id, ty);
        self.next_reg_id += 1;

        id
    }

    /// Helper: Issue a Logic (4-state) register
    pub fn alloc_logic(&mut self, width: usize) -> RegisterId {
        self.alloc_reg(RegisterType::Logic { width })
    }

    /// Helper: Issue a Bit (2-state) register
    pub fn alloc_bit(&mut self, width: usize, signed: bool) -> RegisterId {
        self.alloc_reg(RegisterType::Bit { width, signed })
    }
    pub fn register(&self, id: &RegisterId) -> &RegisterType {
        &self.registers[id]
    }
    pub fn drain(
        &mut self,
    ) -> (
        HashMap<BlockId, BasicBlock<Addr>>,
        HashMap<RegisterId, RegisterType>,
        usize,
    ) {
        if self.current_block_id.is_some() {
            panic!("Seal first")
        }
        let next_reg_id = self.next_reg_id;
        self.next_reg_id = 0;

        let blocks = std::mem::take(&mut self.blocks);
        let regs = std::mem::take(&mut self.registers);

        (blocks, regs, next_reg_id)
    }

    pub fn new_block(&mut self) -> BlockId {
        self.new_block_with(vec![])
    }
    pub fn new_block_with(&mut self, params: Vec<RegisterId>) -> BlockId {
        let id = BlockId(self.next_block_id);
        self.next_block_id += 1;
        let bb = BasicBlock {
            id,
            instructions: vec![],
            params,
            terminator: SIRTerminator::Return,
        };

        self.blocks.insert(id, bb);
        id
    }
    pub fn switch_to_block(&mut self, block: BlockId) {
        if self.current_block_id.is_some() {
            panic!("Attempt to switch block while previous block has unsealed instructions.");
        }
        self.current_block_id = Some(block);
    }

    pub fn seal_block(&mut self, mut terminator: SIRTerminator) -> BlockId {
        if let SIRTerminator::Branch { cond, .. } = &mut terminator {
            *cond = self.normalize_branch_condition(*cond);
        }
        let id = self
            .current_block_id
            .take()
            .expect("No active block to seal");
        self.blocks.get_mut(&id).unwrap().terminator = terminator;
        id
    }

    fn normalize_branch_condition(&mut self, cond: RegisterId) -> RegisterId {
        let cond_type = self.register(&cond).clone();
        if matches!(
            cond_type,
            RegisterType::Bit {
                width: 1,
                signed: false
            }
        ) {
            return cond;
        }

        let truth = if cond_type.width() == 1 {
            cond
        } else {
            let truth = self.alloc_logic(1);
            self.emit(SIRInstruction::Unary(truth, UnaryOp::Or, cond));
            truth
        };
        let normalized = self.alloc_bit(1, false);
        self.emit(SIRInstruction::Unary(
            normalized,
            UnaryOp::ToTwoState,
            truth,
        ));
        normalized
    }
    /// Whether a block is open for instructions.
    pub fn has_open_block(&self) -> bool {
        self.current_block_id.is_some()
    }
    /// Get the current block ID
    pub fn current_block(&self) -> BlockId {
        self.current_block_id.expect("No active block")
    }
    pub fn emit(&mut self, mut inst: SIRInstruction<Addr>) {
        if let SIRInstruction::Binary(dst, lhs, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar, rhs) =
            &mut inst
            && self.register(rhs).width() > 64
        {
            let bound = self.register(lhs).width().max(self.register(dst).width());
            *rhs = self.normalize_shift_amount(*rhs, bound);
        }
        self.blocks
            .get_mut(&self.current_block())
            .unwrap()
            .instructions
            .push(inst);
    }

    /// Backend shift instructions consume one count word. Saturate rather than
    /// discard higher words: IEEE 1800-2023 11.4.10 treats the entire count as
    /// unsigned. Counts at least as large as both operand/result widths have
    /// identical overshift behavior, so clamp them to that representable bound.
    fn normalize_shift_amount(&mut self, count: RegisterId, bound: usize) -> RegisterId {
        let width = self.register(&count).width();
        let limit = self.alloc_bit(width, false);
        self.emit(SIRInstruction::Imm(limit, SIRValue::new(bound as u64)));
        let too_large = self.alloc_logic(1);
        self.emit(SIRInstruction::Binary(
            too_large,
            count,
            BinaryOp::GtU,
            limit,
        ));
        let low = self.alloc_logic(64);
        self.emit(SIRInstruction::Slice(low, count, 0, 64));
        let saturated = self.alloc_bit(64, false);
        self.emit(SIRInstruction::Imm(saturated, SIRValue::new(bound as u64)));
        let result = self.alloc_logic(64);
        self.emit(SIRInstruction::Mux(result, too_large, saturated, low));

        if matches!(self.register(&count), RegisterType::Logic { .. }) {
            // Unknown bits in any count word require an unknown shift result.
            // A comparison with itself is 1 for a known count and X otherwise.
            // Merging the saturated result with all-X preserves that requirement,
            // even if its low word is already the saturation value.
            let known = self.alloc_logic(1);
            self.emit(SIRInstruction::Binary(known, count, BinaryOp::Eq, count));
            let unknown = self.alloc_logic(64);
            self.emit(SIRInstruction::Imm(
                unknown,
                SIRValue::new_four_state(u64::MAX, u64::MAX),
            ));
            let checked = self.alloc_logic(64);
            self.emit(SIRInstruction::Mux(checked, known, result, unknown));
            checked
        } else {
            result
        }
    }

    /// Append a control-flow unit, continuing after any normal return.
    pub fn inline_execution_unit(&mut self, unit: &crate::ExecutionUnit<Addr>)
    where
        Addr: Clone,
    {
        let ro = self.next_reg_id;
        let bo = self.next_block_id;
        let r = |id: RegisterId| RegisterId(id.0 + ro);
        let b = |id: BlockId| BlockId(id.0 + bo);
        self.next_reg_id += unit
            .register_map
            .keys()
            .map(|id| id.0 + 1)
            .max()
            .unwrap_or(0);
        self.next_block_id += unit.blocks.keys().map(|id| id.0 + 1).max().unwrap_or(0);
        let done = self.new_block();
        self.seal_block(SIRTerminator::Jump(b(unit.entry_block_id), vec![]));
        for (id, ty) in &unit.register_map {
            self.registers.insert(r(*id), ty.clone());
        }
        for (id, block) in &unit.blocks {
            let terminator = match &block.terminator {
                SIRTerminator::Return => SIRTerminator::Jump(done, vec![]),
                SIRTerminator::Error(code) => SIRTerminator::Error(*code),
                SIRTerminator::Jump(target, args) => {
                    SIRTerminator::Jump(b(*target), args.iter().copied().map(r).collect())
                }
                SIRTerminator::Branch {
                    cond,
                    true_block,
                    false_block,
                } => SIRTerminator::Branch {
                    cond: r(*cond),
                    true_block: (
                        b(true_block.0),
                        true_block.1.iter().copied().map(r).collect(),
                    ),
                    false_block: (
                        b(false_block.0),
                        false_block.1.iter().copied().map(r).collect(),
                    ),
                },
                SIRTerminator::Switch {
                    selector,
                    cases,
                    default,
                } => SIRTerminator::Switch {
                    selector: r(*selector),
                    cases: cases
                        .iter()
                        .map(|case| crate::SIRSwitchCase {
                            value: case.value.clone(),
                            target: b(case.target),
                        })
                        .collect(),
                    default: b(*default),
                },
            };
            self.blocks.insert(
                b(*id),
                BasicBlock {
                    id: b(*id),
                    params: block.params.iter().copied().map(r).collect(),
                    instructions: block
                        .instructions
                        .iter()
                        .map(|inst| crate::transform::renumber_sir_inst(inst, ro, bo))
                        .collect(),
                    terminator,
                },
            );
        }
        self.switch_to_block(done);
    }

    /// Returns the total number of blocks currently in this builder.
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Flush the current builder state into an ExecutionUnit and reset for
    /// a new one.  The current block must be unsealed (it will be sealed
    /// with `Return` automatically).  Returns `None` if the builder has
    /// only one empty block.
    pub fn flush_eu(&mut self) -> Option<crate::ExecutionUnit<Addr>> {
        // If only the initial block and it's empty, nothing to flush
        if self.blocks.len() == 1 {
            let block = self.blocks.values().next().unwrap();
            if block.instructions.is_empty() && self.current_block_id.is_some() {
                return None;
            }
        }

        // Seal current block if open
        if self.current_block_id.is_some() {
            self.seal_block(SIRTerminator::Return);
        }

        let blocks = std::mem::take(&mut self.blocks);
        let regs = std::mem::take(&mut self.registers);

        // Reset for a new EU — register and block IDs are EU-scoped.
        // INVARIANT: callers must clear any external caches keyed by
        // RegisterId (e.g. scheduler's lower_cache) after flush, since
        // the new EU reuses IDs starting from 0.
        self.next_block_id = 0;
        self.next_reg_id = 0;

        // Re-initialise with a fresh block 0
        let new_block = BasicBlock {
            id: BlockId(0),
            instructions: vec![],
            params: vec![],
            terminator: SIRTerminator::Return,
        };
        self.blocks.insert(BlockId(0), new_block);
        self.next_block_id = 1;
        self.current_block_id = Some(BlockId(0));

        Some(crate::ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map: regs,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::{SIRValue, UnaryOp};

    use super::*;

    fn branch_unit(condition_type: RegisterType) -> crate::ExecutionUnit<usize> {
        let mut builder = SIRBuilder::new();
        let cond = builder.alloc_reg(condition_type);
        builder.emit(SIRInstruction::Imm(cond, SIRValue::new(0u8)));
        let then_block = builder.new_block();
        let else_block = builder.new_block();
        builder.seal_block(SIRTerminator::Branch {
            cond,
            true_block: (then_block, vec![]),
            false_block: (else_block, vec![]),
        });
        builder.switch_to_block(then_block);
        builder.seal_block(SIRTerminator::Return);
        builder.switch_to_block(else_block);
        builder.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = builder.drain();
        crate::ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }

    #[test]
    fn inlines_sparse_control_flow_with_params_and_dynamic_offsets() {
        let input = crate::ExecutionUnit::<usize> {
            entry_block_id: BlockId(17),
            register_map: HashMap::from_iter([
                (
                    RegisterId(41),
                    RegisterType::Bit {
                        width: 1,
                        signed: false,
                    },
                ),
                (
                    RegisterId(42),
                    RegisterType::Bit {
                        width: 8,
                        signed: false,
                    },
                ),
                (
                    RegisterId(72),
                    RegisterType::Bit {
                        width: 32,
                        signed: false,
                    },
                ),
                (
                    RegisterId(90),
                    RegisterType::Bit {
                        width: 1,
                        signed: false,
                    },
                ),
                (RegisterId(91), RegisterType::Logic { width: 32 }),
            ]),
            blocks: HashMap::from_iter([
                (
                    BlockId(17),
                    BasicBlock {
                        id: BlockId(17),
                        params: vec![],
                        instructions: vec![
                            SIRInstruction::Imm(RegisterId(41), SIRValue::new(1u8)),
                            SIRInstruction::Imm(RegisterId(42), SIRValue::new(2u8)),
                        ],
                        terminator: SIRTerminator::Branch {
                            cond: RegisterId(41),
                            true_block: (BlockId(50), vec![RegisterId(41)]),
                            false_block: (BlockId(80), vec![]),
                        },
                    },
                ),
                (
                    BlockId(50),
                    BasicBlock {
                        id: BlockId(50),
                        params: vec![RegisterId(90)],
                        instructions: vec![
                            SIRInstruction::Imm(RegisterId(72), SIRValue::new(0u8)),
                            SIRInstruction::Load(
                                RegisterId(91),
                                0,
                                crate::SIROffset::Dynamic(RegisterId(72)),
                                32,
                            ),
                            SIRInstruction::Store(
                                0,
                                crate::SIROffset::Dynamic(RegisterId(72)),
                                32,
                                RegisterId(91),
                                vec![],
                                vec![],
                            ),
                        ],
                        terminator: SIRTerminator::Return,
                    },
                ),
                (
                    BlockId(80),
                    BasicBlock {
                        id: BlockId(80),
                        params: vec![],
                        instructions: vec![],
                        terminator: SIRTerminator::Switch {
                            selector: RegisterId(42),
                            cases: vec![crate::SIRSwitchCase {
                                value: 2u8.into(),
                                target: BlockId(70),
                            }],
                            default: BlockId(100),
                        },
                    },
                ),
                (
                    BlockId(70),
                    BasicBlock {
                        id: BlockId(70),
                        params: vec![],
                        instructions: vec![],
                        terminator: SIRTerminator::Return,
                    },
                ),
                (
                    BlockId(100),
                    BasicBlock {
                        id: BlockId(100),
                        params: vec![],
                        instructions: vec![],
                        terminator: SIRTerminator::Error(19),
                    },
                ),
            ]),
        };
        input.verify();
        let mut builder = SIRBuilder::new();
        let prefix = builder.alloc_bit(8, false);
        builder.emit(SIRInstruction::Imm(prefix, SIRValue::new(7u8)));
        builder.inline_execution_unit(&input);
        builder.inline_execution_unit(&input);
        builder.emit(SIRInstruction::Store(
            1,
            crate::SIROffset::Static(0),
            8,
            prefix,
            vec![],
            vec![],
        ));
        let unit = builder.flush_eu().unwrap();
        unit.verify();
        assert_eq!(
            unit.blocks
                .values()
                .filter(|block| matches!(block.terminator, SIRTerminator::Return))
                .count(),
            1
        );
        assert_eq!(
            unit.blocks
                .values()
                .filter(|block| matches!(block.terminator, SIRTerminator::Error(19)))
                .count(),
            2
        );
    }

    #[test]
    fn canonicalizes_logic_branch_condition() {
        let unit = branch_unit(RegisterType::Logic { width: 8 });
        let entry = &unit.blocks[&BlockId(0)];
        assert!(matches!(
            &entry.instructions[1..],
            [
                SIRInstruction::Unary(_, UnaryOp::Or, RegisterId(0)),
                SIRInstruction::Unary(_, UnaryOp::ToTwoState, _)
            ]
        ));
        assert!(matches!(
            entry.terminator,
            SIRTerminator::Branch {
                cond: RegisterId(2),
                ..
            }
        ));
        unit.verify();
    }

    #[test]
    fn preserves_canonical_bit_branch_condition() {
        let unit = branch_unit(RegisterType::Bit {
            width: 1,
            signed: false,
        });
        let entry = &unit.blocks[&BlockId(0)];
        assert_eq!(entry.instructions.len(), 1);
        assert!(matches!(
            entry.terminator,
            SIRTerminator::Branch {
                cond: RegisterId(0),
                ..
            }
        ));
        unit.verify();
    }
}
