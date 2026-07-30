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
    use veryl_analyzer::ir::VarId;

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
}
