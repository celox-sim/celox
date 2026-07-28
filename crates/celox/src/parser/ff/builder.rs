//! AIR-to-event builder boundary used by the FF parser.
//!
//! The parser emits semantic value, state, effect, and control operations
//! through this interface. The interface is deliberately independent of an
//! executable SIR instruction stream: a SIR builder is one consumer during
//! migration, while EIR construction consumes the same AIR walk directly.

use veryl_analyzer::ir::VarId;

use crate::ir::{
    BinaryOp, BlockId, RegisterId, RegisterType, SIRBuilder, SIRInstruction, SIROffset,
    SIRTerminator, SIRValue, UnaryOp,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FfReadSource {
    ClockSnapshot,
    ProcessLocal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FfWriteTarget {
    StagedState,
    WriteOnlyPublication,
    ProcessLocal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FfBuildOp {
    Imm(RegisterId, SIRValue),
    Binary(RegisterId, RegisterId, BinaryOp, RegisterId),
    Unary(RegisterId, UnaryOp, RegisterId),
    Read {
        destination: RegisterId,
        object: VarId,
        source: FfReadSource,
        offset: SIROffset,
        width: usize,
    },
    Write {
        object: VarId,
        target: FfWriteTarget,
        offset: SIROffset,
        width: usize,
        value: RegisterId,
    },
    Concat(RegisterId, Vec<RegisterId>),
    Slice(RegisterId, RegisterId, usize, usize),
    Mux(RegisterId, RegisterId, RegisterId, RegisterId),
    RuntimeEvent {
        site_id: u32,
        arguments: Vec<RegisterId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FfTerminator {
    Jump(BlockId, Vec<RegisterId>),
    Branch {
        condition: RegisterId,
        true_block: (BlockId, Vec<RegisterId>),
        false_block: (BlockId, Vec<RegisterId>),
    },
    Error(i64),
}

pub(crate) trait FfBuilder {
    fn alloc_logic(&mut self, width: usize) -> RegisterId;
    fn alloc_bit(&mut self, width: usize, signed: bool) -> RegisterId;
    fn register(&self, id: &RegisterId) -> &RegisterType;
    fn emit(&mut self, operation: FfBuildOp);
    fn new_block(&mut self) -> BlockId;
    fn new_block_with(&mut self, parameters: Vec<RegisterId>) -> BlockId;
    fn switch_to_block(&mut self, block: BlockId);
    fn seal_block(&mut self, terminator: FfTerminator) -> BlockId;
}

impl FfBuilder for SIRBuilder<crate::ir::RegionedVarAddr> {
    fn alloc_logic(&mut self, width: usize) -> RegisterId {
        SIRBuilder::alloc_logic(self, width)
    }

    fn alloc_bit(&mut self, width: usize, signed: bool) -> RegisterId {
        SIRBuilder::alloc_bit(self, width, signed)
    }

    fn register(&self, id: &RegisterId) -> &RegisterType {
        SIRBuilder::register(self, id)
    }

    fn emit(&mut self, operation: FfBuildOp) {
        let instruction = match operation {
            FfBuildOp::Imm(destination, value) => SIRInstruction::Imm(destination, value),
            FfBuildOp::Binary(destination, lhs, operation, rhs) => {
                SIRInstruction::Binary(destination, lhs, operation, rhs)
            }
            FfBuildOp::Unary(destination, operation, input) => {
                SIRInstruction::Unary(destination, operation, input)
            }
            FfBuildOp::Read {
                destination,
                object,
                source,
                offset,
                width,
            } => {
                let region = match source {
                    FfReadSource::ClockSnapshot => crate::ir::STABLE_REGION,
                    FfReadSource::ProcessLocal => crate::ir::WORKING_REGION,
                };
                SIRInstruction::Load(
                    destination,
                    crate::ir::RegionedVarAddr {
                        var_id: object,
                        region,
                    },
                    offset,
                    width,
                )
            }
            FfBuildOp::Write {
                object,
                target: _,
                offset,
                width,
                value,
            } => SIRInstruction::Store(
                crate::ir::RegionedVarAddr {
                    var_id: object,
                    region: crate::ir::WORKING_REGION,
                },
                offset,
                width,
                value,
                Vec::new(),
                Vec::new(),
            ),
            FfBuildOp::Concat(destination, inputs) => SIRInstruction::Concat(destination, inputs),
            FfBuildOp::Slice(destination, input, offset, width) => {
                SIRInstruction::Slice(destination, input, offset, width)
            }
            FfBuildOp::Mux(destination, condition, then_value, else_value) => {
                SIRInstruction::Mux(destination, condition, then_value, else_value)
            }
            FfBuildOp::RuntimeEvent { site_id, arguments } => SIRInstruction::RuntimeEvent {
                site_id,
                args: arguments,
            },
        };
        SIRBuilder::emit(self, instruction);
    }

    fn new_block(&mut self) -> BlockId {
        SIRBuilder::new_block(self)
    }

    fn new_block_with(&mut self, parameters: Vec<RegisterId>) -> BlockId {
        SIRBuilder::new_block_with(self, parameters)
    }

    fn switch_to_block(&mut self, block: BlockId) {
        SIRBuilder::switch_to_block(self, block);
    }

    fn seal_block(&mut self, terminator: FfTerminator) -> BlockId {
        let terminator = match terminator {
            FfTerminator::Jump(target, arguments) => SIRTerminator::Jump(target, arguments),
            FfTerminator::Branch {
                condition,
                true_block,
                false_block,
            } => SIRTerminator::Branch {
                cond: condition,
                true_block,
                false_block,
            },
            FfTerminator::Error(code) => SIRTerminator::Error(code),
        };
        SIRBuilder::seal_block(self, terminator)
    }
}
