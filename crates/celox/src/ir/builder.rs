use crate::HashMap;
use crate::ir::{
    BasicBlock, BlockId, RegisterId, RegisterType, SIRInstruction, SIRTerminator, UnaryOp,
};

#[derive(Clone)]
pub(crate) struct SIRBuilder<Addr> {
    next_reg_id: usize,
    registers: HashMap<RegisterId, RegisterType>,
    current_block_id: Option<BlockId>,
    blocks: HashMap<BlockId, BasicBlock<Addr>>,
    next_block_id: usize,
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
    /// Get the current block ID
    pub fn current_block(&self) -> BlockId {
        self.current_block_id.expect("No active block")
    }
    pub fn emit(&mut self, inst: SIRInstruction<Addr>) {
        self.blocks
            .get_mut(&self.current_block())
            .unwrap()
            .instructions
            .push(inst);
    }

    /// Returns the total number of blocks currently in this builder.
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Flush the current builder state into an ExecutionUnit and reset for
    /// a new one.  The current block must be unsealed (it will be sealed
    /// with `Return` automatically).  Returns `None` if the builder has
    /// only one empty block.
    pub fn flush_eu(&mut self) -> Option<crate::ir::ExecutionUnit<Addr>> {
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

        Some(crate::ir::ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map: regs,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::ir::{SIRValue, UnaryOp};

    use super::*;

    fn branch_unit(condition_type: RegisterType) -> crate::ir::ExecutionUnit<usize> {
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
        crate::ir::ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
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
