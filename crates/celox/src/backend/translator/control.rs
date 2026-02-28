use cranelift::{codegen::ir::BlockArg, prelude::*};

use crate::HashMap;

use super::{SIRTranslator, TranslationState};

impl SIRTranslator {
    pub(super) fn translate_terminator(
        &self,
        state: &mut TranslationState,
        term: &crate::ir::SIRTerminator,
        block_map: &HashMap<crate::ir::BlockId, Block>,
        next_unit_entry: Option<Block>,
    ) {
        match term {
            crate::ir::SIRTerminator::Jump(to, params) => {
                let mut cl_args: Vec<BlockArg> = Vec::new();
                for reg in params {
                    cl_args.push(BlockArg::Value(state.regs[reg].values()[0]));
                    if self.options.four_state {
                        // Also pass the mask value
                        let mask = state.regs[reg]
                            .masks()
                            .map(|m| m[0])
                            .unwrap_or_else(|| state.builder.ins().iconst(types::I8, 0));
                        cl_args.push(BlockArg::Value(mask));
                    }
                }
                state.builder.ins().jump(block_map[to], &cl_args);
            }
            crate::ir::SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                let condition = state.regs[cond].values()[0];
                let (t_id, t_args) = true_block;
                let (f_id, f_args) = false_block;

                let mut cl_t_args: Vec<BlockArg> = Vec::new();
                for reg in t_args {
                    cl_t_args.push(BlockArg::Value(state.regs[reg].values()[0]));
                    if self.options.four_state {
                        let mask = state.regs[reg]
                            .masks()
                            .map(|m| m[0])
                            .unwrap_or_else(|| state.builder.ins().iconst(types::I8, 0));
                        cl_t_args.push(BlockArg::Value(mask));
                    }
                }
                let mut cl_f_args: Vec<BlockArg> = Vec::new();
                for reg in f_args {
                    cl_f_args.push(BlockArg::Value(state.regs[reg].values()[0]));
                    if self.options.four_state {
                        let mask = state.regs[reg]
                            .masks()
                            .map(|m| m[0])
                            .unwrap_or_else(|| state.builder.ins().iconst(types::I8, 0));
                        cl_f_args.push(BlockArg::Value(mask));
                    }
                }

                state.builder.ins().brif(
                    condition,
                    block_map[t_id],
                    &cl_t_args,
                    block_map[f_id],
                    &cl_f_args,
                );
            }
            crate::ir::SIRTerminator::Return => {
                if let Some(next_block) = next_unit_entry {
                    state.builder.ins().jump(next_block, &[]);
                } else {
                    let suucess = state.builder.ins().iconst(types::I64, 0);
                    state.builder.ins().return_(&[suucess]);
                }
            }
            crate::ir::SIRTerminator::Error(code) => {
                let error = state.builder.ins().iconst(types::I64, *code);

                state.builder.ins().return_(&[error]);
            }
        }
    }
}
