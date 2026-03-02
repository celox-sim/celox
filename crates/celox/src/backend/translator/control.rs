use cranelift::{codegen::ir::BlockArg, prelude::*};

use crate::HashMap;

use super::{SIRTranslator, TranslationState};
use super::core::cast_type;

/// Collect the Cranelift types of all parameters declared on a block.
/// This is used to ensure block-call arguments are cast to the exact
/// types the target block expects, avoiding Cranelift verifier errors
/// such as "arg vN has type i8, expected i32".
fn collect_block_param_types(state: &TranslationState, cl_block: Block) -> Vec<Type> {
    let dfg = &state.builder.func.dfg;
    dfg.block_params(cl_block)
        .iter()
        .map(|&v| dfg.value_type(v))
        .collect()
}

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
                let target_cl_block = block_map[to];
                // Collect expected param types before mutably borrowing the builder.
                // Block params use one Cranelift param per SIR param in 2-state mode,
                // and two (value + mask) in 4-state mode.
                let param_types = collect_block_param_types(state, target_cl_block);

                let mut cl_args: Vec<BlockArg> = Vec::new();
                for (i, reg) in params.iter().enumerate() {
                    let val = state.regs[reg].values()[0];
                    let val_idx = if self.options.four_state { i * 2 } else { i };
                    let cast_val = cast_type(state.builder, val, param_types[val_idx]);
                    cl_args.push(BlockArg::Value(cast_val));
                    if self.options.four_state {
                        // Also pass the mask value
                        let mask = state.regs[reg]
                            .masks()
                            .map(|m| m[0])
                            .unwrap_or_else(|| state.builder.ins().iconst(types::I8, 0));
                        let cast_mask = cast_type(state.builder, mask, param_types[i * 2 + 1]);
                        cl_args.push(BlockArg::Value(cast_mask));
                    }
                }
                state.builder.ins().jump(target_cl_block, &cl_args);
            }
            crate::ir::SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                let condition = state.regs[cond].values()[0];
                let (t_id, t_args) = true_block;
                let (f_id, f_args) = false_block;

                let t_param_types = collect_block_param_types(state, block_map[t_id]);
                let f_param_types = collect_block_param_types(state, block_map[f_id]);

                let mut cl_t_args: Vec<BlockArg> = Vec::new();
                for (i, reg) in t_args.iter().enumerate() {
                    let val = state.regs[reg].values()[0];
                    let val_idx = if self.options.four_state { i * 2 } else { i };
                    let cast_val = cast_type(state.builder, val, t_param_types[val_idx]);
                    cl_t_args.push(BlockArg::Value(cast_val));
                    if self.options.four_state {
                        let mask = state.regs[reg]
                            .masks()
                            .map(|m| m[0])
                            .unwrap_or_else(|| state.builder.ins().iconst(types::I8, 0));
                        let cast_mask = cast_type(state.builder, mask, t_param_types[i * 2 + 1]);
                        cl_t_args.push(BlockArg::Value(cast_mask));
                    }
                }
                let mut cl_f_args: Vec<BlockArg> = Vec::new();
                for (i, reg) in f_args.iter().enumerate() {
                    let val = state.regs[reg].values()[0];
                    let val_idx = if self.options.four_state { i * 2 } else { i };
                    let cast_val = cast_type(state.builder, val, f_param_types[val_idx]);
                    cl_f_args.push(BlockArg::Value(cast_val));
                    if self.options.four_state {
                        let mask = state.regs[reg]
                            .masks()
                            .map(|m| m[0])
                            .unwrap_or_else(|| state.builder.ins().iconst(types::I8, 0));
                        let cast_mask = cast_type(state.builder, mask, f_param_types[i * 2 + 1]);
                        cl_f_args.push(BlockArg::Value(cast_mask));
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
