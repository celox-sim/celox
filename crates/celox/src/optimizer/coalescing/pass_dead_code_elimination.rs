use super::pass_manager::ExecutionUnitPass;
use crate::ir::{ExecutionUnit, RegionedAbsoluteAddr};
use crate::optimizer::PassOptions;

/// Final mark/sweep DCE without value-numbering or cross-region CSE.
pub(super) struct DeadCodeEliminationPass;

impl ExecutionUnitPass for DeadCodeEliminationPass {
    fn name(&self) -> &'static str {
        "dead_code_elimination"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _: &PassOptions) {
        super::pass_vectorize_concat::remove_dead_definitions(eu);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        AbsoluteAddr, BasicBlock, BlockId, InstanceId, RegisterId, RegisterType, SIRInstruction,
        SIROffset, SIRTerminator, SIRValue, STABLE_REGION,
    };
    use celox_design::StateObjectId as VarId;

    #[test]
    fn removes_only_definitions_not_reachable_from_effects() {
        let address = RegionedAbsoluteAddr::from_absolute_addr(
            STABLE_REGION,
            AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: VarId::from_raw(0),
            },
        );
        let live = RegisterId(0);
        let dead = RegisterId(1);
        let mut eu = ExecutionUnit {
            blocks: [(
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Imm(live, SIRValue::new(1u8)),
                        SIRInstruction::Imm(dead, SIRValue::new(0u8)),
                        SIRInstruction::Store(
                            address,
                            SIROffset::Static(0),
                            1,
                            live,
                            Vec::new(),
                            Vec::new(),
                        ),
                    ],
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            entry_block_id: BlockId(0),
            register_map: [
                (
                    live,
                    RegisterType::Bit {
                        width: 1,
                        signed: false,
                    },
                ),
                (
                    dead,
                    RegisterType::Bit {
                        width: 1,
                        signed: false,
                    },
                ),
            ]
            .into_iter()
            .collect(),
        };

        DeadCodeEliminationPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks[&BlockId(0)].instructions.len(), 2);
        assert!(
            eu.blocks[&BlockId(0)].instructions.iter().any(
                |instruction| matches!(instruction, SIRInstruction::Imm(reg, _) if *reg == live)
            )
        );
        assert!(
            !eu.blocks[&BlockId(0)].instructions.iter().any(
                |instruction| matches!(instruction, SIRInstruction::Imm(reg, _) if *reg == dead)
            )
        );
    }

    #[test]
    fn removes_unused_phi_inputs_and_their_producer_cones() {
        let address = RegionedAbsoluteAddr::from_absolute_addr(
            STABLE_REGION,
            AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: VarId::from_raw(0),
            },
        );
        let live_value = RegisterId(0);
        let dead_leaf = RegisterId(1);
        let dead_value = RegisterId(2);
        let live_parameter = RegisterId(3);
        let dead_parameter = RegisterId(4);
        let bit = || RegisterType::Bit {
            width: 1,
            signed: false,
        };
        let mut eu = ExecutionUnit {
            blocks: [
                (
                    BlockId(0),
                    BasicBlock {
                        id: BlockId(0),
                        params: Vec::new(),
                        instructions: vec![
                            SIRInstruction::Imm(live_value, SIRValue::new(1u8)),
                            SIRInstruction::Imm(dead_leaf, SIRValue::new(0u8)),
                            SIRInstruction::Unary(dead_value, crate::ir::UnaryOp::Ident, dead_leaf),
                        ],
                        terminator: SIRTerminator::Jump(BlockId(1), vec![live_value, dead_value]),
                    },
                ),
                (
                    BlockId(1),
                    BasicBlock {
                        id: BlockId(1),
                        params: vec![live_parameter, dead_parameter],
                        instructions: vec![SIRInstruction::Store(
                            address,
                            SIROffset::Static(0),
                            1,
                            live_parameter,
                            Vec::new(),
                            Vec::new(),
                        )],
                        terminator: SIRTerminator::Return,
                    },
                ),
            ]
            .into_iter()
            .collect(),
            entry_block_id: BlockId(0),
            register_map: [
                (live_value, bit()),
                (dead_leaf, bit()),
                (dead_value, bit()),
                (live_parameter, bit()),
                (dead_parameter, bit()),
            ]
            .into_iter()
            .collect(),
        };

        DeadCodeEliminationPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.verify_result(), Ok(()));
        assert_eq!(eu.blocks[&BlockId(1)].params, vec![live_parameter]);
        assert_eq!(
            eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Jump(BlockId(1), vec![live_value])
        );
        assert_eq!(
            eu.blocks[&BlockId(0)].instructions,
            vec![SIRInstruction::Imm(live_value, SIRValue::new(1u8))]
        );
        assert!(!eu.register_map.contains_key(&dead_leaf));
        assert!(!eu.register_map.contains_key(&dead_value));
        assert!(!eu.register_map.contains_key(&dead_parameter));
    }
}
