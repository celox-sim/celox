use cranelift::{codegen::ir::BlockArg, prelude::*};

use crate::HashMap;

use super::core::cast_type;
use super::{SIRTranslator, TranslationState};

/// Collect the Cranelift types of all parameters declared on a block.
/// This is used to ensure block-call arguments are cast to the exact
/// types the target block expects, avoiding Cranelift verifier errors
/// such as "arg vN has type i8, expected i16" or "expected i32".
pub(super) fn collect_block_param_types(state: &TranslationState, cl_block: Block) -> Vec<Type> {
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
                let param_types = collect_block_param_types(state, target_cl_block);

                let mut cl_args: Vec<BlockArg> = Vec::new();
                let mut param_type_idx = 0;
                for reg in params.iter() {
                    let values = state.regs[reg].load_value_chunks(state.builder);
                    let masks = if self.options.four_state {
                        Some(match state.regs[reg].load_mask_chunks(state.builder) {
                            Some(masks) => masks,
                            None => {
                                let mut zeros = Vec::with_capacity(values.len());
                                for value in &values {
                                    let ty = state.builder.func.dfg.value_type(*value);
                                    zeros.push(state.builder.ins().iconst(ty, 0));
                                }
                                zeros
                            }
                        })
                    } else {
                        None
                    };
                    for (chunk_idx, value) in values.iter().enumerate() {
                        let cast_val = cast_type(state.builder, *value, param_types[param_type_idx]);
                        cl_args.push(BlockArg::Value(cast_val));
                        param_type_idx += 1;
                        if let Some(masks) = &masks {
                            let cast_mask =
                                cast_type(state.builder, masks[chunk_idx], param_types[param_type_idx]);
                            cl_args.push(BlockArg::Value(cast_mask));
                            param_type_idx += 1;
                        }
                    }
                }
                debug_assert_eq!(
                    param_type_idx,
                    param_types.len(),
                    "SIR Jump arg count does not match target block param count"
                );
                state.builder.ins().jump(target_cl_block, &cl_args);
            }
            crate::ir::SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                let condition = state.regs[cond].first_value(state.builder);
                let (t_id, t_args) = true_block;
                let (f_id, f_args) = false_block;

                let t_param_types = collect_block_param_types(state, block_map[t_id]);
                let f_param_types = collect_block_param_types(state, block_map[f_id]);
                let mut cl_t_args: Vec<BlockArg> = Vec::new();
                let mut t_param_type_idx = 0;
                for reg in t_args.iter() {
                    let values = state.regs[reg].load_value_chunks(state.builder);
                    let masks = if self.options.four_state {
                        Some(match state.regs[reg].load_mask_chunks(state.builder) {
                            Some(masks) => masks,
                            None => {
                                let mut zeros = Vec::with_capacity(values.len());
                                for value in &values {
                                    let ty = state.builder.func.dfg.value_type(*value);
                                    zeros.push(state.builder.ins().iconst(ty, 0));
                                }
                                zeros
                            }
                        })
                    } else {
                        None
                    };
                    for (chunk_idx, value) in values.iter().enumerate() {
                        let cast_val =
                            cast_type(state.builder, *value, t_param_types[t_param_type_idx]);
                        cl_t_args.push(BlockArg::Value(cast_val));
                        t_param_type_idx += 1;
                        if let Some(masks) = &masks {
                            let cast_mask = cast_type(
                                state.builder,
                                masks[chunk_idx],
                                t_param_types[t_param_type_idx],
                            );
                            cl_t_args.push(BlockArg::Value(cast_mask));
                            t_param_type_idx += 1;
                        }
                    }
                }
                debug_assert_eq!(
                    t_param_type_idx,
                    t_param_types.len(),
                    "SIR Branch true-arg count does not match target block param count"
                );
                let mut cl_f_args: Vec<BlockArg> = Vec::new();
                let mut f_param_type_idx = 0;
                for reg in f_args.iter() {
                    let values = state.regs[reg].load_value_chunks(state.builder);
                    let masks = if self.options.four_state {
                        Some(match state.regs[reg].load_mask_chunks(state.builder) {
                            Some(masks) => masks,
                            None => {
                                let mut zeros = Vec::with_capacity(values.len());
                                for value in &values {
                                    let ty = state.builder.func.dfg.value_type(*value);
                                    zeros.push(state.builder.ins().iconst(ty, 0));
                                }
                                zeros
                            }
                        })
                    } else {
                        None
                    };
                    for (chunk_idx, value) in values.iter().enumerate() {
                        let cast_val =
                            cast_type(state.builder, *value, f_param_types[f_param_type_idx]);
                        cl_f_args.push(BlockArg::Value(cast_val));
                        f_param_type_idx += 1;
                        if let Some(masks) = &masks {
                            let cast_mask = cast_type(
                                state.builder,
                                masks[chunk_idx],
                                f_param_types[f_param_type_idx],
                            );
                            cl_f_args.push(BlockArg::Value(cast_mask));
                            f_param_type_idx += 1;
                        }
                    }
                }
                debug_assert_eq!(
                    f_param_type_idx,
                    f_param_types.len(),
                    "SIR Branch false-arg count does not match target block param count"
                );

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
                    let success = state.builder.ins().iconst(types::I64, 0);
                    state.builder.ins().return_(&[success]);
                }
            }
            crate::ir::SIRTerminator::Error(code) => {
                let error = state.builder.ins().iconst(types::I64, *code);

                state.builder.ins().return_(&[error]);
            }
        }
    }
}
