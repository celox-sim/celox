//! Private memory operations for generated native scheduler machinery.
//!
//! These are encoded as reserved extern calls so they remain opaque to SIR
//! optimizations. A supporting native backend expands them into loads/stores,
//! without crossing a host ABI. They are never emitted from HDL expressions.
//!
//! All operands and read results are unsigned 64-bit words. Offsets are bytes.
//! The producer must prove each access lies in a live, writable allocation:
//! indirect allocations are runtime-owned and disjoint from simulation state;
//! state accesses address the physical state image, including four-state masks.
//! Pointers must not survive the host operation that can relocate an allocation.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeMemoryOp {
    ReadIndirect,
    WriteIndirect,
    ReadState,
    WriteState,
}

impl NativeMemoryOp {
    pub const fn function(self) -> u32 {
        match self {
            Self::ReadIndirect => u32::MAX - 2,
            Self::WriteIndirect => u32::MAX - 3,
            Self::ReadState => u32::MAX - 4,
            Self::WriteState => u32::MAX - 5,
        }
    }

    pub const fn decode(function: u32) -> Option<Self> {
        match function {
            n if n == Self::ReadIndirect.function() => Some(Self::ReadIndirect),
            n if n == Self::WriteIndirect.function() => Some(Self::WriteIndirect),
            n if n == Self::ReadState.function() => Some(Self::ReadState),
            n if n == Self::WriteState.function() => Some(Self::WriteState),
            _ => None,
        }
    }

    pub const fn arguments(self) -> usize {
        match self {
            Self::ReadIndirect | Self::WriteState => 2,
            Self::WriteIndirect => 3,
            Self::ReadState => 1,
        }
    }

    pub const fn reads(self) -> bool {
        matches!(self, Self::ReadIndirect | Self::ReadState)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BasicBlock, BlockId, ExecutionUnit, RegisterId, RegisterType, SIRInstruction,
        SIRTerminator, SIRValue,
    };

    fn unit(op: NativeMemoryOp, args: usize, result: bool, signed: bool) -> ExecutionUnit<usize> {
        let mut instructions: Vec<_> = (0..3)
            .map(|i| SIRInstruction::Imm(RegisterId(i), SIRValue::new(0u64)))
            .collect();
        instructions.push(SIRInstruction::ExternCall {
            dst: result.then_some(RegisterId(3)),
            func: op.function(),
            args: (0..args).map(RegisterId).collect(),
        });
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions,
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map: (0..4)
                .map(|i| (RegisterId(i), RegisterType::Bit { width: 64, signed }))
                .collect(),
        }
    }

    #[test]
    fn native_memory_requires_exact_word_signatures() {
        for op in [
            NativeMemoryOp::ReadIndirect,
            NativeMemoryOp::WriteIndirect,
            NativeMemoryOp::ReadState,
            NativeMemoryOp::WriteState,
        ] {
            assert_eq!(NativeMemoryOp::decode(op.function()), Some(op));
            assert!(crate::extern_abi::native_kernel_index(op.function()).is_none());
            let valid = unit(op, op.arguments(), op.reads(), false);
            valid.verify();
            let instruction = valid.blocks[&BlockId(0)].instructions.last().unwrap();
            let state = matches!(op, NativeMemoryOp::ReadState | NativeMemoryOp::WriteState);
            assert_eq!(instruction.has_opaque_state_access(), state);
            assert_eq!(!instruction.state_reads().is_none(), state);
            assert_eq!(!instruction.state_writes().is_none(), state);
            for invalid in [
                unit(op, op.arguments() - 1, op.reads(), false),
                unit(op, op.arguments(), !op.reads(), false),
                unit(op, op.arguments(), op.reads(), true),
            ] {
                let error = invalid.verify_result().unwrap_err();
                assert!(
                    error.to_string().contains("TYPE.NATIVE_MEMORY_SIGNATURE"),
                    "{error}"
                );
            }
        }
        assert_eq!(
            NativeMemoryOp::decode(crate::extern_abi::HEAP_HELPER_ID),
            None
        );
        assert_eq!(NativeMemoryOp::decode(crate::float::FLOAT_HELPER_ID), None);
    }
}
