use cranelift::{codegen::Context, prelude::*};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Module};

use crate::MemoryLayout;
use crate::RegionedAbsoluteAddr;
use crate::cost_model::{
    CLIF_INST_THRESHOLD, FUNCTION_VALUE_THRESHOLD, STACK_FRAME_LIMIT, estimate_eu_cost,
    estimate_eu_value_count,
};
use crate::tail_call_split::MemorySpilledPlan;
use crate::tail_call_split::TailCallChunk;
use crate::{CompileOptions, CraneliftError};

use super::SIRTranslator;
use super::translator::core::get_cl_type;

fn define_simulation_function(module: &mut JITModule, ctx: &mut Context) {
    let ptr_type = module.target_config().pointer_type();

    // Add one unified memory pointer argument
    ctx.func.signature.params.push(AbiParam::new(ptr_type)); // arg0: unified_mem
    ctx.func.signature.returns.push(AbiParam::new(types::I64));
}

pub struct JitEngine {
    module: JITModule,
    pub(super) translator: SIRTranslator,
}

impl JitEngine {
    pub fn new(layout: MemoryLayout, options: &CompileOptions) -> Result<Self, CraneliftError> {
        let mut flag_builder = settings::builder();
        let cl_opts = &options.cranelift;

        flag_builder
            .set("opt_level", cl_opts.opt_level.as_cranelift_str())
            .map_err(|source| CraneliftError::setting("opt_level", source))?;
        flag_builder
            .set(
                "regalloc_algorithm",
                cl_opts.regalloc_algorithm.as_cranelift_str(),
            )
            .map_err(|source| CraneliftError::setting("regalloc_algorithm", source))?;
        flag_builder
            .set(
                "enable_alias_analysis",
                if cl_opts.enable_alias_analysis {
                    "true"
                } else {
                    "false"
                },
            )
            .map_err(|source| CraneliftError::setting("enable_alias_analysis", source))?;
        flag_builder
            .set(
                "enable_verifier",
                if cl_opts.enable_verifier {
                    "true"
                } else {
                    "false"
                },
            )
            .map_err(|source| CraneliftError::setting("enable_verifier", source))?;
        // Required for tail calls (return_call instruction)
        flag_builder
            .set("preserve_frame_pointers", "true")
            .map_err(|source| CraneliftError::setting("preserve_frame_pointers", source))?;

        let isa_builder = cranelift_native::builder()
            .map_err(|message| CraneliftError::NativeTarget { message })?;

        let isa = isa_builder
            .finish(settings::Flags::new(flag_builder))
            .map_err(|source| CraneliftError::TargetIsa { source })?;

        let builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        let module = JITModule::new(builder);
        let target_config = module.target_config();
        Ok(Self {
            module,
            translator: SIRTranslator {
                layout,
                options: options.clone(),
                target_config,
            },
        })
    }

    pub fn layout(&self) -> &MemoryLayout {
        &self.translator.layout
    }

    /// Optimize a function context, define it in the module, and emit debug output.
    fn optimize_and_define(
        &mut self,
        ctx: &mut Context,
        func_id: FuncId,
        label: &str,
        pre_clif_out: Option<&mut String>,
        post_clif_out: Option<&mut String>,
        native_out: Option<&mut String>,
    ) -> Result<(), CraneliftError> {
        let translated_values = ctx.func.dfg.num_values();
        if let Some(out) = pre_clif_out {
            out.push_str(&format!("{}\n{}\n", label, ctx.func.display()));
        }

        if native_out.is_some() {
            ctx.want_disasm = true;
        }

        let isa = self.module.isa();
        let mut ctrl_plane = cranelift::codegen::control::ControlPlane::default();
        ctx.optimize(isa, &mut ctrl_plane)
            .map_err(|source| CraneliftError::optimize(label, source))?;

        if let Some(out) = post_clif_out {
            out.push_str(&format!("{}\n{}\n", label, ctx.func.display()));
        }

        self.module
            .define_function(func_id, ctx)
            .map_err(|source| {
                CraneliftError::module(format!("failed to define {label}"), source)
            })?;

        if let Some(frame) = ctx
            .compiled_code()
            .and_then(|code| code.buffer.frame_layout())
        {
            if frame.frame_to_fp_offset > STACK_FRAME_LIMIT {
                return Err(CraneliftError::StackFrameTooLarge {
                    label: label.to_owned(),
                    bytes: frame.frame_to_fp_offset,
                    limit: STACK_FRAME_LIMIT,
                    translated_values,
                });
            }
        }

        if let Some(out) = native_out {
            if let Some(compiled) = ctx.compiled_code() {
                let data = compiled.buffer.data();
                out.push_str(&format!("{} Size: {} bytes\n", label, data.len()));
                if let Some(disasm) = &compiled.vcode {
                    out.push_str(disasm);
                } else {
                    out.push_str("(disassembly not available)");
                }
                out.push('\n');
            }
        }

        Ok(())
    }

    /// Call each EU's root through the SystemV-to-Tail ABI bridge. A nonzero
    /// status exits immediately, matching unsplit sequential EU execution.
    fn build_entry_wrapper(&mut self, entry_func_ids: &[FuncId]) -> Result<FuncId, CraneliftError> {
        let mut ctx = self.module.make_context();
        define_simulation_function(&mut self.module, &mut ctx);

        let entry_refs: Vec<_> = entry_func_ids
            .iter()
            .map(|&id| self.module.declare_func_in_func(id, &mut ctx.func))
            .collect();

        let mut builder_ctx = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
            let entry_block = builder.create_block();
            builder.append_block_params_for_function_params(entry_block);
            builder.switch_to_block(entry_block);

            let mem_ptr = builder.block_params(entry_block)[0];

            for function in entry_refs {
                let call = builder.ins().call(function, &[mem_ptr]);
                let result = builder.inst_results(call)[0];
                let ok = builder.ins().icmp_imm_s(IntCC::Equal, result, 0);
                let next = builder.create_block();
                let error = builder.create_block();
                builder.ins().brif(ok, next, &[], error, &[]);
                builder.switch_to_block(error);
                builder.ins().return_(&[result]);
                builder.switch_to_block(next);
            }
            let success = builder.ins().iconst(types::I64, 0);
            builder.ins().return_(&[success]);

            builder.seal_all_blocks();
            builder.finalize(self.module.target_config());
        }

        let func_id = self
            .module
            .declare_anonymous_function(&ctx.func.signature)
            .map_err(|source| CraneliftError::module("failed to declare entry wrapper", source))?;

        self.optimize_and_define(&mut ctx, func_id, "entry wrapper", None, None, None)?;

        Ok(func_id)
    }

    pub fn compile_units(
        &mut self,
        units: &[crate::ExecutionUnit<RegionedAbsoluteAddr>],
        pre_clif_out: Option<&mut String>,
        post_clif_out: Option<&mut String>,
        native_out: Option<&mut String>,
    ) -> Result<*const u8, CraneliftError> {
        let four_state = self.translator.options.four_state;
        let mut total_inst_cost = 0usize;
        let mut total_value_count = 0usize;
        for eu in units {
            total_inst_cost += estimate_eu_cost(eu, four_state);
            total_value_count += estimate_eu_value_count(eu, four_state);
        }
        if self.translator.options.cranelift.diagnostics.pass_timing {
            let sir_insts: usize = units
                .iter()
                .map(|eu| {
                    eu.blocks
                        .values()
                        .map(|b| b.instructions.len())
                        .sum::<usize>()
                })
                .sum();
            tracing::debug!(
                "[compile_units] {} EUs, {} SIR insts, clif_cost={total_inst_cost}/{CLIF_INST_THRESHOLD} values={total_value_count}/{FUNCTION_VALUE_THRESHOLD}",
                units.len(),
                sir_insts,
            );
        }
        if total_inst_cost > CLIF_INST_THRESHOLD || total_value_count > FUNCTION_VALUE_THRESHOLD {
            return self.compile_units_batched(units, pre_clif_out, post_clif_out, native_out);
        }

        self.compile_units_single(units, pre_clif_out, post_clif_out, native_out)
    }

    fn compile_units_single(
        &mut self,
        units: &[crate::ExecutionUnit<RegionedAbsoluteAddr>],
        pre_clif_out: Option<&mut String>,
        post_clif_out: Option<&mut String>,
        native_out: Option<&mut String>,
    ) -> Result<*const u8, CraneliftError> {
        let mut ctx = self.module.make_context();
        let mut builder_ctx = FunctionBuilderContext::new();

        define_simulation_function(&mut self.module, &mut ctx);

        {
            let builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
            self.translator.translate_units(units, builder);
        }

        if self.translator.options.cranelift.diagnostics.pass_timing {
            let num_values = ctx.func.dfg.num_values();
            let num_insts = ctx.func.dfg.num_insts();
            let num_blocks = ctx.func.dfg.num_blocks();
            tracing::debug!(
                "[compile_units_single] after translation: blocks={num_blocks} insts={num_insts} values={num_values}"
            );
        }

        let func_id = self
            .module
            .declare_anonymous_function(&ctx.func.signature)
            .map_err(|source| {
                CraneliftError::module("failed to declare master function", source)
            })?;

        self.optimize_and_define(
            &mut ctx,
            func_id,
            "=== eval_comb ===",
            pre_clif_out,
            post_clif_out,
            native_out,
        )?;

        self.module.finalize_definitions().map_err(|source| {
            CraneliftError::module("failed to finalize JIT definitions", source)
        })?;

        Ok(self.module.get_finalized_function(func_id))
    }

    /// Compile units by splitting into multiple Cranelift functions called sequentially.
    /// Each EU communicates only through shared memory, so no register passing is needed.
    fn compile_units_batched(
        &mut self,
        units: &[crate::ExecutionUnit<RegionedAbsoluteAddr>],
        mut pre_clif_out: Option<&mut String>,
        mut post_clif_out: Option<&mut String>,
        mut native_out: Option<&mut String>,
    ) -> Result<*const u8, CraneliftError> {
        let timing = self.translator.options.cranelift.diagnostics.pass_timing;
        let four_state = self.translator.options.four_state;

        // 1. Partition units into batches, each under both thresholds
        let eu_metrics: Vec<(usize, usize)> = units
            .iter()
            .map(|eu| {
                (
                    estimate_eu_cost(eu, four_state),
                    estimate_eu_value_count(eu, four_state),
                )
            })
            .collect();

        let mut batches: Vec<Vec<usize>> = Vec::new();
        let mut current_batch: Vec<usize> = Vec::new();
        let mut current_inst_cost = 0usize;
        let mut current_value_count = 0usize;

        for (i, &(eu_inst, eu_val)) in eu_metrics.iter().enumerate() {
            // If a single EU exceeds either threshold, it gets its own batch
            if eu_inst > CLIF_INST_THRESHOLD || eu_val > FUNCTION_VALUE_THRESHOLD {
                if !current_batch.is_empty() {
                    batches.push(std::mem::take(&mut current_batch));
                    current_inst_cost = 0;
                    current_value_count = 0;
                }
                batches.push(vec![i]);
                continue;
            }

            if (current_inst_cost + eu_inst > CLIF_INST_THRESHOLD
                || current_value_count + eu_val > FUNCTION_VALUE_THRESHOLD)
                && !current_batch.is_empty()
            {
                batches.push(std::mem::take(&mut current_batch));
                current_inst_cost = 0;
                current_value_count = 0;
            }

            current_batch.push(i);
            current_inst_cost += eu_inst;
            current_value_count += eu_val;
        }
        if !current_batch.is_empty() {
            batches.push(current_batch);
        }

        if timing {
            let total_inst: usize = eu_metrics.iter().map(|m| m.0).sum();
            let total_val: usize = eu_metrics.iter().map(|m| m.1).sum();
            tracing::debug!(
                "[jit-split] Splitting {} EUs (est. {} CLIF insts, {} values) into {} batches",
                units.len(),
                total_inst,
                total_val,
                batches.len()
            );
        }

        // 2. Compile each batch as a separate function
        let mut batch_func_ids: Vec<FuncId> = Vec::with_capacity(batches.len());
        let mut builder_ctx = FunctionBuilderContext::new();

        for (batch_idx, batch) in batches.iter().enumerate() {
            let batch_start = timing.then(std::time::Instant::now);

            let batch_units: Vec<_> = batch.iter().map(|&i| &units[i]).cloned().collect();

            let mut ctx = self.module.make_context();
            define_simulation_function(&mut self.module, &mut ctx);

            {
                let builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
                self.translator.translate_units(&batch_units, builder);
            }

            let func_id = self
                .module
                .declare_anonymous_function(&ctx.func.signature)
                .map_err(|source| {
                    CraneliftError::module(
                        format!("failed to declare batch {batch_idx} function"),
                        source,
                    )
                })?;

            let label = format!("=== eval_comb batch[{batch_idx}] ===");
            self.optimize_and_define(
                &mut ctx,
                func_id,
                &label,
                pre_clif_out.as_deref_mut(),
                post_clif_out.as_deref_mut(),
                native_out.as_deref_mut(),
            )?;

            batch_func_ids.push(func_id);

            if let Some(s) = batch_start {
                let batch_cost: usize = batch.iter().map(|&i| eu_metrics[i].0).sum();
                tracing::debug!(
                    "[jit-split]   batch[{batch_idx}]: {} EUs, est. {} CLIF insts, {:?}",
                    batch.len(),
                    batch_cost,
                    s.elapsed()
                );
            }
        }

        // 3. Build a wrapper function that calls each batch in sequence
        let mut ctx = self.module.make_context();
        define_simulation_function(&mut self.module, &mut ctx);

        let batch_func_refs: Vec<_> = batch_func_ids
            .iter()
            .map(|&fid| self.module.declare_func_in_func(fid, &mut ctx.func))
            .collect();

        {
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            let mem_ptr = builder.block_params(entry)[0];

            for &func_ref in &batch_func_refs {
                let call = builder.ins().call(func_ref, &[mem_ptr]);
                let result = builder.inst_results(call)[0];
                // Check for error return (non-zero = error)
                let ok = builder.ins().icmp_imm_s(IntCC::Equal, result, 0);
                let continue_block = builder.create_block();
                let error_block = builder.create_block();
                builder
                    .ins()
                    .brif(ok, continue_block, &[], error_block, &[]);

                builder.switch_to_block(error_block);
                builder.ins().return_(&[result]);

                builder.switch_to_block(continue_block);
            }

            let zero = builder.ins().iconst(types::I64, 0);
            builder.ins().return_(&[zero]);

            builder.seal_all_blocks();
            builder.finalize(self.module.target_config());
        }

        let wrapper_func_id = self
            .module
            .declare_anonymous_function(&ctx.func.signature)
            .map_err(|source| {
                CraneliftError::module("failed to declare wrapper function", source)
            })?;

        self.optimize_and_define(
            &mut ctx,
            wrapper_func_id,
            "=== eval_comb wrapper ===",
            pre_clif_out,
            post_clif_out,
            native_out,
        )?;

        self.module.finalize_definitions().map_err(|source| {
            CraneliftError::module("failed to finalize JIT definitions", source)
        })?;

        Ok(self.module.get_finalized_function(wrapper_func_id))
    }

    /// Compile a chain of tail-call chunks, returning a C-callable entry pointer.
    pub fn compile_chunks(
        &mut self,
        chunks: &[TailCallChunk],
        mut pre_clif_out: Option<&mut String>,
        mut post_clif_out: Option<&mut String>,
        mut native_out: Option<&mut String>,
    ) -> Result<*const u8, CraneliftError> {
        let ptr_type = self.module.target_config().pointer_type();
        let four_state = self.translator.options.four_state;

        // 1. Build signatures and declare all chunk functions with CallConv::Tail
        let mut chunk_func_ids = Vec::with_capacity(chunks.len());
        let mut chunk_sigs = Vec::with_capacity(chunks.len());

        for chunk in chunks {
            let mut sig = Signature::new(isa::CallConv::Tail);
            sig.params.push(AbiParam::new(ptr_type)); // mem_ptr
            for (_, reg_ty) in &chunk.incoming_live_regs {
                let width = reg_ty.width();
                let nc = width.div_ceil(64).max(1);
                if nc == 1 {
                    let cl_ty = get_cl_type(width);
                    sig.params.push(AbiParam::new(cl_ty));
                    if four_state {
                        sig.params.push(AbiParam::new(cl_ty));
                    }
                } else {
                    for _ in 0..nc {
                        sig.params.push(AbiParam::new(types::I64));
                        if four_state {
                            sig.params.push(AbiParam::new(types::I64));
                        }
                    }
                }
            }
            sig.returns.push(AbiParam::new(types::I64));

            let func_id = self
                .module
                .declare_anonymous_function(&sig)
                .map_err(|source| {
                    CraneliftError::module("failed to declare chunk function", source)
                })?;
            chunk_func_ids.push(func_id);
            chunk_sigs.push(sig);
        }

        // 2. Compile each chunk function
        for (i, chunk) in chunks.iter().enumerate() {
            let mut ctx = self.module.make_context();
            ctx.func.signature = chunk_sigs[i].clone();

            let next_func_ref = if i + 1 < chunks.len() {
                Some(
                    self.module
                        .declare_func_in_func(chunk_func_ids[i + 1], &mut ctx.func),
                )
            } else {
                None
            };

            let mut builder_ctx = FunctionBuilderContext::new();
            {
                let builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
                self.translator
                    .translate_chunk(chunk, next_func_ref, builder);
            }

            let label = format!("=== Chunk {} ===", i);
            self.optimize_and_define(
                &mut ctx,
                chunk_func_ids[i],
                &label,
                pre_clif_out.as_deref_mut(),
                post_clif_out.as_deref_mut(),
                native_out.as_deref_mut(),
            )?;
        }

        // 3. Create SystemV entry wrapper
        let entry_func_id = self.build_entry_wrapper(&[chunk_func_ids[0]])?;

        // 4. Finalize definitions
        self.module.finalize_definitions().map_err(|source| {
            CraneliftError::module("failed to finalize JIT definitions", source)
        })?;

        Ok(self.module.get_finalized_function(entry_func_id))
    }

    /// Compile a memory-spilled chunk plan, returning a C-callable entry pointer.
    ///
    /// Each chunk is a separate function taking only `(mem_ptr) -> i64`.
    /// Inter-chunk register values are passed through scratch memory.
    pub fn compile_spilled_chunks(
        &mut self,
        plan: &MemorySpilledPlan,
        mut pre_clif_out: Option<&mut String>,
        mut post_clif_out: Option<&mut String>,
        mut native_out: Option<&mut String>,
    ) -> Result<*const u8, CraneliftError> {
        let ptr_type = self.module.target_config().pointer_type();
        let scratch_base_offset = self.translator.layout.scratch_base_offset;

        // 1. Declare all chunk functions with CallConv::Tail
        let mut chunk_func_ids = Vec::with_capacity(plan.chunks.len());
        for _ in &plan.chunks {
            let mut sig = Signature::new(isa::CallConv::Tail);
            sig.params.push(AbiParam::new(ptr_type)); // mem_ptr
            sig.returns.push(AbiParam::new(types::I64));

            let func_id = self
                .module
                .declare_anonymous_function(&sig)
                .map_err(|source| {
                    CraneliftError::module("failed to declare spilled chunk function", source)
                })?;
            chunk_func_ids.push(func_id);
        }

        // 2. Compile each chunk function
        for (i, chunk) in plan.chunks.iter().enumerate() {
            let mut ctx = self.module.make_context();
            let mut sig = Signature::new(isa::CallConv::Tail);
            sig.params.push(AbiParam::new(ptr_type));
            sig.returns.push(AbiParam::new(types::I64));
            ctx.func.signature = sig;

            let chunk_func_refs: Vec<_> = chunk_func_ids
                .iter()
                .map(|&fid| self.module.declare_func_in_func(fid, &mut ctx.func))
                .collect();

            let mut builder_ctx = FunctionBuilderContext::new();
            {
                let builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
                self.translator.translate_spilled_chunk(
                    chunk,
                    &chunk_func_refs,
                    scratch_base_offset,
                    builder,
                );
            }

            let label = format!("=== SpilledChunk {} ===", i);
            self.optimize_and_define(
                &mut ctx,
                chunk_func_ids[i],
                &label,
                pre_clif_out.as_deref_mut(),
                post_clif_out.as_deref_mut(),
                native_out.as_deref_mut(),
            ).map_err(|mut error| {
                if let CraneliftError::StackFrameTooLarge { label, .. } = &mut error {
                    let planes = if self.translator.options.four_state { 2 } else { 1 };
                    let slots = |spills: &[crate::tail_call_split::SpillSlot]| spills.iter()
                        .map(|slot| slot.reg_ty.width().div_ceil(64).max(1) * planes).sum::<usize>();
                    let edge_values: usize = chunk.cross_chunk_edges.values()
                        .flat_map(|edge| &edge.param_scratch_offsets)
                        .map(|(reg, _)| chunk.eu.register_map[reg].width().div_ceil(64).max(1) * planes).sum();
                    label.push_str(&format!(" (SIR estimate={}, incoming={}, outgoing={}, edge parameters={edge_values})",
                        estimate_eu_value_count(&chunk.eu, self.translator.options.four_state),
                        slots(&chunk.incoming_spills), slots(&chunk.outgoing_spills)));
                }
                error
            })?;
        }

        // 3. Create SystemV entry wrapper
        let roots: Vec<_> = plan
            .entry_chunks
            .iter()
            .map(|&i| chunk_func_ids[i])
            .collect();
        let entry_func_id = self.build_entry_wrapper(&roots)?;

        // 4. Finalize definitions
        self.module.finalize_definitions().map_err(|source| {
            CraneliftError::module("failed to finalize JIT definitions", source)
        })?;

        Ok(self.module.get_finalized_function(entry_func_id))
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;
    use celox_state_layout::{LayoutInput, LayoutSource, MemoryLayoutMode};
    use cranelift::codegen::ir::{StackSlotData, StackSlotKind};

    struct EmptyLayout;
    impl LayoutSource<crate::AbsoluteAddr> for EmptyLayout {
        fn layout_input(&self, _: MemoryLayoutMode) -> LayoutInput<crate::AbsoluteAddr> {
            LayoutInput {
                state_objects: vec![],
                working_addresses: vec![],
                sparse_addresses: vec![],
                unpacked_arrays: Default::default(),
                requirements: Default::default(),
                ff_referenced_addresses: Default::default(),
                num_events: 0,
                runtime_event_sites: vec![],
                lane_writers: Default::default(),
                access_rank: Default::default(),
            }
        }
    }

    struct CostLayout;
    impl LayoutSource<crate::AbsoluteAddr> for CostLayout {
        fn layout_input(&self, mode: MemoryLayoutMode) -> LayoutInput<crate::AbsoluteAddr> {
            let mut input = EmptyLayout.layout_input(mode);
            input.state_objects = (0..3)
                .map(|id| celox_state_layout::StateObjectLayout {
                    address: crate::AbsoluteAddr {
                        instance_id: crate::InstanceId(0),
                        var_id: celox_design::StateObjectId(id),
                    },
                    width: 1024,
                    is_4state: true,
                })
                .collect();
            input
        }
    }

    fn translated_values(
        engine: &mut JitEngine,
        unit: &crate::ExecutionUnit<RegionedAbsoluteAddr>,
    ) -> usize {
        let mut ctx = engine.module.make_context();
        define_simulation_function(&mut engine.module, &mut ctx);
        let mut builder_ctx = FunctionBuilderContext::new();
        let builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
        engine
            .translator
            .translate_units(std::slice::from_ref(unit), builder);
        ctx.func.dfg.num_values()
    }

    #[test]
    fn operation_estimates_cover_actual_unoptimized_clif_values() {
        use crate::{
            BinaryOp, BlockId, HashMap, RegisterId, RegisterType, SIRInstruction, SIROffset,
            SIRTerminator, UnaryOp,
        };
        let mut violations = Vec::new();
        for four_state in [false, true] {
            let layout = MemoryLayout::build(&CostLayout, four_state, MemoryLayoutMode::Packed);
            let mut engine = JitEngine::new(
                layout,
                &CompileOptions {
                    four_state,
                    emit_triggers: false,
                    cranelift: Default::default(),
                },
            )
            .unwrap();
            for width in [1, 8, 64, 65, 128, 257, 512] {
                for condition_width in [1, 65, 257] {
                    let mut instructions = Vec::new();
                    let mut register_map = HashMap::default();
                    for (id, bits) in [(0, width), (1, width), (2, condition_width)] {
                        let address = crate::AbsoluteAddr {
                            instance_id: crate::InstanceId(0),
                            var_id: celox_design::StateObjectId(id as u32),
                        };
                        instructions.push(SIRInstruction::Load(
                            RegisterId(id),
                            RegionedAbsoluteAddr::from_absolute_addr(crate::STABLE_REGION, address),
                            SIROffset::Static(0),
                            bits,
                        ));
                        register_map.insert(RegisterId(id), RegisterType::Logic { width: bits });
                    }
                    let base = crate::ExecutionUnit {
                        entry_block_id: BlockId(0),
                        blocks: [(
                            BlockId(0),
                            crate::BasicBlock {
                                id: BlockId(0),
                                params: vec![],
                                instructions,
                                terminator: SIRTerminator::Return,
                            },
                        )]
                        .into_iter()
                        .collect(),
                        register_map,
                    };
                    let baseline = translated_values(&mut engine, &base);
                    let mut operations = vec![(
                        SIRInstruction::Mux(
                            RegisterId(3),
                            RegisterId(2),
                            RegisterId(0),
                            RegisterId(1),
                        ),
                        width,
                    )];
                    if condition_width == 1 {
                        for op in [BinaryOp::And, BinaryOp::Or, BinaryOp::Xor] {
                            operations.push((
                                SIRInstruction::Binary(
                                    RegisterId(3),
                                    RegisterId(0),
                                    op,
                                    RegisterId(1),
                                ),
                                width,
                            ));
                        }
                        for op in [
                            UnaryOp::Ident,
                            UnaryOp::BitNot,
                            UnaryOp::ToTwoState,
                            UnaryOp::Minus,
                            UnaryOp::LogicNot,
                            UnaryOp::Or,
                            UnaryOp::And,
                            UnaryOp::Xor,
                            UnaryOp::PopCount,
                            UnaryOp::CountLeadingZeros,
                            UnaryOp::CountTrailingZeros,
                        ] {
                            let result_width = if matches!(
                                op,
                                UnaryOp::LogicNot | UnaryOp::Or | UnaryOp::And | UnaryOp::Xor
                            ) {
                                1
                            } else {
                                width
                            };
                            operations.push((
                                SIRInstruction::Unary(RegisterId(3), op, RegisterId(0)),
                                result_width,
                            ));
                        }
                    }
                    for (instruction, result_width) in operations {
                        let mut unit = base.clone();
                        unit.register_map.insert(
                            RegisterId(3),
                            RegisterType::Logic {
                                width: result_width,
                            },
                        );
                        let estimate = crate::cost_model::estimate_clif_cost(
                            &instruction,
                            &unit.register_map,
                            four_state,
                        );
                        unit.blocks
                            .get_mut(&BlockId(0))
                            .unwrap()
                            .instructions
                            .push(instruction.clone());
                        let actual = translated_values(&mut engine, &unit) - baseline;
                        if estimate < actual {
                            violations.push(format!(
                                "{instruction:?}, width={width}, condition_width={condition_width}, four_state={four_state}: estimate {estimate} < actual {actual}"
                            ));
                        }
                    }
                }
            }
        }
        assert!(violations.is_empty(), "{}", violations.join("\n"));
    }

    #[test]
    fn outgoing_spills_only_reference_definitions_dominating_the_exit() {
        use crate::{
            BasicBlock, BlockId, HashMap, RegisterId, RegisterType, SIRInstruction, SIROffset,
            SIRTerminator, SIRValue,
        };
        for four_state in [false, true] {
            let address = crate::AbsoluteAddr {
                instance_id: crate::InstanceId(0),
                var_id: celox_design::StateObjectId(0),
            };
            let region = RegionedAbsoluteAddr::from_absolute_addr(crate::STABLE_REGION, address);
            let branch = |condition, yes, no| SIRTerminator::Branch {
                cond: RegisterId(condition),
                true_block: (BlockId(yes), vec![]),
                false_block: (BlockId(no), vec![]),
            };
            let blocks: HashMap<_, _> = [
                (
                    0,
                    (0..3)
                        .map(|index| {
                            SIRInstruction::Load(
                                RegisterId(index),
                                region,
                                SIROffset::Static(index),
                                1,
                            )
                        })
                        .collect(),
                    branch(0, 2, 1),
                ),
                (1, vec![], branch(2, 0, 4)),
                (
                    2,
                    vec![SIRInstruction::Imm(RegisterId(3), SIRValue::new(77u8))],
                    branch(1, 3, 1),
                ),
                (
                    3,
                    vec![SIRInstruction::Store(
                        region,
                        SIROffset::Static(0),
                        8,
                        RegisterId(3),
                        vec![],
                        vec![],
                    )],
                    SIRTerminator::Jump(BlockId(4), vec![]),
                ),
                (4, vec![], SIRTerminator::Return),
            ]
            .into_iter()
            .map(|(id, instructions, terminator)| {
                (
                    BlockId(id),
                    BasicBlock {
                        id: BlockId(id),
                        params: vec![],
                        instructions,
                        terminator,
                    },
                )
            })
            .collect();
            let unit = crate::ExecutionUnit {
                entry_block_id: BlockId(0),
                blocks,
                register_map: (0..4)
                    .map(|id| {
                        (
                            RegisterId(id),
                            RegisterType::Logic {
                                width: if id == 3 { 8 } else { 1 },
                            },
                        )
                    })
                    .collect(),
            };
            let threshold = crate::cost_model::estimate_eu_cost(&unit, four_state)
                - crate::cost_model::estimate_clif_cost(
                    &unit.blocks[&BlockId(3)].instructions[0],
                    &unit.register_map,
                    four_state,
                )
                - 3;
            let plan = crate::tail_call_split::split_multi_block_with_threshold(
                &[unit],
                four_state,
                threshold,
                threshold,
            )
            .unwrap();
            let first = &plan.chunks[0];
            assert!(
                first.eu.blocks.contains_key(&BlockId(1))
                    && first.eu.blocks.contains_key(&BlockId(2))
            );
            assert!(
                first
                    .outgoing_spills
                    .iter()
                    .any(|slot| slot.reg_id == RegisterId(3))
            );
            assert!(
                !first.outgoing_spills_at[&BlockId(1)]
                    .iter()
                    .any(|slot| slot.reg_id == RegisterId(3))
            );
            assert!(
                first.outgoing_spills_at[&BlockId(2)]
                    .iter()
                    .any(|slot| slot.reg_id == RegisterId(3))
            );
            let layout = MemoryLayout::build(&CostLayout, four_state, MemoryLayoutMode::Packed)
                .with_backend_scratch(plan.scratch_bytes);
            let mut engine = JitEngine::new(
                layout.clone(),
                &CompileOptions {
                    four_state,
                    emit_triggers: false,
                    cranelift: crate::CraneliftOptions::fast_compile(),
                },
            )
            .unwrap();
            let entry = engine
                .compile_spilled_chunks(&plan, None, None, None)
                .unwrap();
            let run: unsafe extern "C" fn(*mut u8) -> u64 = unsafe { std::mem::transmute(entry) };
            for input in [0u8, 1, 2, 3] {
                let mut memory = vec![0u8; layout.merged_total_size];
                let offset = layout.offsets[&address];
                memory[offset] = input;
                assert_eq!(unsafe { run(memory.as_mut_ptr()) }, 0);
                assert_eq!(memory[offset], if input == 3 { 77 } else { input });
            }
        }
    }

    #[test]
    fn excessive_actual_frame_returns_an_error_even_without_splitting() {
        for four_state in [false, true] {
            let mut options = CompileOptions {
                four_state,
                emit_triggers: false,
                cranelift: crate::CraneliftOptions::fast_compile(),
            };
            options.cranelift.tail_call_split = false;
            let layout = MemoryLayout::build(&EmptyLayout, four_state, MemoryLayoutMode::Packed);
            let mut engine = JitEngine::new(layout, &options).unwrap();
            let mut ctx = engine.module.make_context();
            define_simulation_function(&mut engine.module, &mut ctx);
            let mut builder_ctx = FunctionBuilderContext::new();
            {
                let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
                let entry = builder.create_block();
                builder.append_block_params_for_function_params(entry);
                builder.switch_to_block(entry);
                let slot = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    STACK_FRAME_LIMIT * 2,
                    3,
                ));
                let value = builder.ins().iconst(types::I64, 7);
                let pointer_type = engine.module.target_config().pointer_type();
                builder.ins().stack_store(pointer_type, value, slot, 0);
                let loaded = builder.ins().stack_load(pointer_type, types::I64, slot, 0);
                builder.ins().return_(&[loaded]);
                builder.seal_all_blocks();
                builder.finalize(engine.module.target_config());
            }
            let id = engine
                .module
                .declare_anonymous_function(&ctx.func.signature)
                .unwrap();
            let error = engine
                .optimize_and_define(&mut ctx, id, "oversized frame", None, None, None)
                .unwrap_err();
            assert!(
                matches!(error, CraneliftError::StackFrameTooLarge { bytes, limit, .. } if bytes > limit && limit == STACK_FRAME_LIMIT)
            );
        }
    }
}
