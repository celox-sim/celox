use crate::HashMap;
use crate::ir::{BasicBlock, BlockId, RegisterId, RegisterType, SIRInstruction, SIRTerminator};

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

    pub fn seal_block(&mut self, terminator: SIRTerminator) -> BlockId {
        let id = self
            .current_block_id
            .take()
            .expect("No active block to seal");
        self.blocks.get_mut(&id).unwrap().terminator = terminator;
        id
    }
    pub fn set_block_params(&mut self, block: BlockId, params: Vec<RegisterId>) {
        self.blocks.get_mut(&block).unwrap().params = params;
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

    /// Flush the current blocks into an ExecutionUnit but keep the register file
    /// and register id allocation alive for subsequent chunks.
    #[allow(dead_code)]
    pub fn flush_chunk_preserve_regs(&mut self) -> Option<crate::ir::ExecutionUnit<Addr>>
    where
        Addr: Clone,
    {
        if self.blocks.len() == 1 {
            let block = self.blocks.values().next().unwrap();
            if block.instructions.is_empty() && self.current_block_id.is_some() {
                return None;
            }
        }

        if self.current_block_id.is_some() {
            self.seal_block(SIRTerminator::Return);
        }

        let blocks = std::mem::take(&mut self.blocks);
        let regs = self.registers.clone();

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
