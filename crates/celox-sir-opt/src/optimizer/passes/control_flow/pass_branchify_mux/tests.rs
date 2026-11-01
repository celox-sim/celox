mod cleanup;
mod controlled_joins;
mod coupled;
mod cross_block;
mod local_mux;
mod placement;
mod profitability;
mod selector;

use super::cross_block::{
    closed_cross_block_condition_slice, collect_cross_arm_defs, collect_cross_condition_defs,
    movable_cross_block_inputs, moved_defs_insertion_index,
};
use super::*;
use crate::ir::{InstanceId, RegisterType, SIRValue};
use celox_design::StateObjectId as VarId;
use num_bigint::BigUint;

fn addr(id: usize) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region: 0,
        instance_id: InstanceId(id),
        var_id: VarId::default(),
    }
}

fn unit(
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut register_map = HashMap::default();
    for reg in 0..26 {
        register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: 64,
                signed: false,
            },
        );
    }
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

fn imm(dst: usize, value: u64) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Imm(RegisterId(dst), SIRValue::new(BigUint::from(value)))
}

fn append_mul_chain(
    instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    initial: usize,
    factor: usize,
    outputs: &[usize],
) {
    let mut lhs = RegisterId(initial);
    for &output in outputs {
        instructions.push(SIRInstruction::Binary(
            RegisterId(output),
            lhs,
            crate::ir::BinaryOp::Mul,
            RegisterId(factor),
        ));
        lhs = RegisterId(output);
    }
}

fn cfg_unit(
    register_count: usize,
    one_bit_registers: &[usize],
    blocks: Vec<BasicBlock<RegionedAbsoluteAddr>>,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let one_bit_registers = one_bit_registers.iter().copied().collect::<HashSet<_>>();
    let register_map = (0..register_count)
        .map(|register| {
            (
                RegisterId(register),
                RegisterType::Bit {
                    width: if one_bit_registers.contains(&register) {
                        1
                    } else {
                        64
                    },
                    signed: false,
                },
            )
        })
        .collect();
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: blocks.into_iter().map(|block| (block.id, block)).collect(),
        register_map,
    }
}

fn store(instance: usize, source: usize) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Store(
        addr(instance),
        SIROffset::Static(0),
        64,
        RegisterId(source),
        Vec::new(),
        Vec::new(),
    )
}
