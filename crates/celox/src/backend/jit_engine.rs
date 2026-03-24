use cranelift::{codegen::Context, prelude::*};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Module};

use crate::SimulatorOptions;
use crate::backend::memory_layout::MemoryLayout;
use crate::ir::RegionedAbsoluteAddr;
use crate::optimizer::coalescing::TailCallChunk;
use crate::optimizer::coalescing::cost_model::{
    CLIF_INST_THRESHOLD, VREG_VALUE_THRESHOLD, estimate_eu_cost, estimate_eu_value_count,
};
use crate::optimizer::coalescing::pass_tail_call_split::MemorySpilledPlan;

use super::SIRTranslator;
use super::translator::core::get_cl_type;

fn define_simulation_function(module: &mut JITModule, ctx: &mut Context) {
    let ptr_type = module.target_config().pointer_type();

    // Add one unified memory pointer argument
    ctx.func.signature.params.push(AbiParam::new(ptr_type)); // arg0: unified_mem
    ctx.func.signature.returns.push(AbiParam::new(types::I64));
}
pub(crate) struct JitEngine {
    module: JITModule,
    pub(super) translator: SIRTranslator,
}

impl JitEngine {
    pub fn new(layout: MemoryLayout, options: &SimulatorOptions) -> Result<Self, String> {
        let mut flag_builder = settings::builder();
        let cl_opts = &options.cranelift_options;

        flag_builder
            .set("opt_level", cl_opts.opt_level.as_cranelift_str())
            .map_err(|e| e.to_string())?;
        flag_builder
            .set(
                "regalloc_algorithm",
                cl_opts.regalloc_algorithm.as_cranelift_str(),
            )
            .map_err(|e| e.to_string())?;
        flag_builder
            .set(
                "enable_alias_analysis",
                if cl_opts.enable_alias_analysis {
                    "true"
                } else {
                    "false"
                },
            )
            .map_err(|e| e.to_string())?;
        flag_builder
            .set(
                "enable_verifier",
                if cl_opts.enable_verifier {
                    "true"
                } else {
                    "false"
                },
            )
            .map_err(|e| e.to_string())?;
        // Required for tail calls (return_call instruction)
        flag_builder
            .set("preserve_frame_pointers", "true")
            .map_err(|e| e.to_string())?;

        let isa_builder = cranelift_native::builder().map_err(|e| e.to_string())?;

        let isa = isa_builder
            .finish(settings::Flags::new(flag_builder))
            .map_err(|e| e.to_string())?;

        let builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        let module = JITModule::new(builder);
        Ok(Self {
            module,
            translator: SIRTranslator {
                layout,
                options: options.clone(),
            },
        })
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
    ) -> Result<(), String> {
        if let Some(out) = pre_clif_out {
            out.push_str(&format!("{}\n{}\n", label, ctx.func.display()));
        }

        if native_out.is_some() {
            ctx.want_disasm = true;
        }

        let isa = self.module.isa();
        let mut ctrl_plane = cranelift::codegen::control::ControlPlane::default();
        ctx.optimize(isa, &mut ctrl_plane)
            .map_err(|e| format!("{label} optimization failed: {e:#?}"))?;

        if let Some(out) = post_clif_out {
            out.push_str(&format!("{}\n{}\n", label, ctx.func.display()));
        }

        self.module
            .define_function(func_id, ctx)
            .map_err(|e| format!("Failed to define {label}: {e}"))?;

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

    /// Create a SystemV entry wrapper that calls the first chunk function via a
    /// regular `call` (not tail-call), bridging from SystemV to Tail calling convention.
    fn build_entry_wrapper(&mut self, first_chunk_func_id: FuncId) -> Result<FuncId, String> {
        let mut ctx = self.module.make_context();
        define_simulation_function(&mut self.module, &mut ctx);

        let chunk0_func_ref = self
            .module
            .declare_func_in_func(first_chunk_func_id, &mut ctx.func);

        let mut builder_ctx = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
            let entry_block = builder.create_block();
            builder.append_block_params_for_function_params(entry_block);
            builder.switch_to_block(entry_block);

            let mem_ptr = builder.block_params(entry_block)[0];

            let call = builder.ins().call(chunk0_func_ref, &[mem_ptr]);
            let result = builder.inst_results(call)[0];
            builder.ins().return_(&[result]);

            builder.seal_all_blocks();
            builder.finalize();
        }

        let isa = self.module.isa();
        let mut ctrl_plane = cranelift::codegen::control::ControlPlane::default();
        ctx.optimize(isa, &mut ctrl_plane)
            .map_err(|e| format!("Entry wrapper optimization failed: {e:#?}"))?;

        let func_id = self
            .module
            .declare_anonymous_function(&ctx.func.signature)
            .map_err(|e| format!("Failed to declare entry wrapper: {e}"))?;

        self.module
            .define_function(func_id, &mut ctx)
            .map_err(|e| format!("Failed to define entry wrapper: {e}"))?;

        Ok(func_id)
    }

    pub fn compile_units(
        &mut self,
        units: &[crate::ir::ExecutionUnit<RegionedAbsoluteAddr>],
        pre_clif_out: Option<&mut String>,
        post_clif_out: Option<&mut String>,
        native_out: Option<&mut String>,
    ) -> Result<*const u8, String> {
        let four_state = self.translator.options.four_state;
        let mut total_inst_cost = 0usize;
        let mut total_value_count = 0usize;
        for eu in units {
            total_inst_cost += estimate_eu_cost(eu, four_state);
            total_value_count += estimate_eu_value_count(eu, four_state);
        }
        if std::env::var("CELOX_PASS_TIMING").is_ok() {
            let sir_insts: usize = units
                .iter()
                .map(|eu| {
                    eu.blocks
                        .values()
                        .map(|b| b.instructions.len())
                        .sum::<usize>()
                })
                .sum();
            eprintln!(
                "[compile_units] {} EUs, {} SIR insts, clif_cost={total_inst_cost}/{CLIF_INST_THRESHOLD} values={total_value_count}/{VREG_VALUE_THRESHOLD}",
                units.len(),
                sir_insts,
            );
        }
        if total_inst_cost > CLIF_INST_THRESHOLD || total_value_count > VREG_VALUE_THRESHOLD {
            return self.compile_units_batched(units, pre_clif_out, post_clif_out, native_out);
        }

        self.compile_units_single(units, pre_clif_out, post_clif_out, native_out)
    }

    fn compile_units_single(
        &mut self,
        units: &[crate::ir::ExecutionUnit<RegionedAbsoluteAddr>],
        pre_clif_out: Option<&mut String>,
        post_clif_out: Option<&mut String>,
        native_out: Option<&mut String>,
    ) -> Result<*const u8, String> {
        let mut ctx = self.module.make_context();
        let mut builder_ctx = FunctionBuilderContext::new();

        define_simulation_function(&mut self.module, &mut ctx);

        {
            let builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
            self.translator.translate_units(units, builder);
        }

        if std::env::var("CELOX_PASS_TIMING").is_ok() {
            let num_values = ctx.func.dfg.num_values();
            let num_insts = ctx.func.dfg.num_insts();
            let num_blocks = ctx.func.dfg.num_blocks();
            eprintln!(
                "[compile_units_single] after translation: blocks={num_blocks} insts={num_insts} values={num_values}"
            );
        }

        let func_id = self
            .module
            .declare_anonymous_function(&ctx.func.signature)
            .map_err(|e| format!("Failed to declare master function: {e}"))?;

        self.optimize_and_define(
            &mut ctx,
            func_id,
            "=== eval_comb ===",
            pre_clif_out,
            post_clif_out,
            native_out,
        )?;

        self.module
            .finalize_definitions()
            .map_err(|e| format!("Failed to finalize JIT definitions: {e}"))?;

        Ok(self.module.get_finalized_function(func_id))
    }

    /// Compile units by splitting into multiple Cranelift functions called sequentially.
    /// Each EU communicates only through shared memory, so no register passing is needed.
    fn compile_units_batched(
        &mut self,
        units: &[crate::ir::ExecutionUnit<RegionedAbsoluteAddr>],
        mut pre_clif_out: Option<&mut String>,
        mut post_clif_out: Option<&mut String>,
        mut native_out: Option<&mut String>,
    ) -> Result<*const u8, String> {
        let timing = std::env::var("CELOX_PASS_TIMING").is_ok();
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
            if eu_inst > CLIF_INST_THRESHOLD || eu_val > VREG_VALUE_THRESHOLD {
                if !current_batch.is_empty() {
                    batches.push(std::mem::take(&mut current_batch));
                    current_inst_cost = 0;
                    current_value_count = 0;
                }
                batches.push(vec![i]);
                continue;
            }

            if (current_inst_cost + eu_inst > CLIF_INST_THRESHOLD
                || current_value_count + eu_val > VREG_VALUE_THRESHOLD)
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
            eprintln!(
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
                .map_err(|e| format!("Failed to declare batch {batch_idx} function: {e}"))?;

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
                eprintln!(
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
                let ok = builder.ins().icmp_imm(IntCC::Equal, result, 0);
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
            builder.finalize();
        }

        let wrapper_func_id = self
            .module
            .declare_anonymous_function(&ctx.func.signature)
            .map_err(|e| format!("Failed to declare wrapper function: {e}"))?;

        self.optimize_and_define(
            &mut ctx,
            wrapper_func_id,
            "=== eval_comb wrapper ===",
            pre_clif_out,
            post_clif_out,
            native_out,
        )?;

        self.module
            .finalize_definitions()
            .map_err(|e| format!("Failed to finalize JIT definitions: {e}"))?;

        Ok(self.module.get_finalized_function(wrapper_func_id))
    }

    /// Compile a chain of tail-call chunks, returning a C-callable entry pointer.
    pub fn compile_chunks(
        &mut self,
        chunks: &[TailCallChunk],
        mut pre_clif_out: Option<&mut String>,
        mut post_clif_out: Option<&mut String>,
        mut native_out: Option<&mut String>,
    ) -> Result<*const u8, String> {
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
                .map_err(|e| format!("Failed to declare chunk function: {e}"))?;
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
        let entry_func_id = self.build_entry_wrapper(chunk_func_ids[0])?;

        // 4. Finalize definitions
        self.module
            .finalize_definitions()
            .map_err(|e| format!("Failed to finalize JIT definitions: {e}"))?;

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
    ) -> Result<*const u8, String> {
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
                .map_err(|e| format!("Failed to declare spilled chunk function: {e}"))?;
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
            )?;
        }

        // 3. Create SystemV entry wrapper
        let entry_func_id = self.build_entry_wrapper(chunk_func_ids[0])?;

        // 4. Finalize definitions
        self.module
            .finalize_definitions()
            .map_err(|e| format!("Failed to finalize JIT definitions: {e}"))?;

        Ok(self.module.get_finalized_function(entry_func_id))
    }

    /// Compile a batch evaluation function that runs eval_comb in a tight loop.
    #[allow(dead_code)]
    ///
    /// Generated function signature:
    ///   `fn(mem_ptr: *mut u8, inputs: *const u8, outputs: *mut u8, count: i64) -> i64`
    ///
    /// For each iteration `i` in `0..count`:
    ///   1. Copies `inputs[i * input_byte_size .. ]` to `mem[input_offset]`
    ///   2. Runs eval_comb logic inline
    ///   3. Copies `mem[output_offset .. ]` to `outputs[i * output_byte_size .. ]`
    pub fn compile_batch_eval(
        &mut self,
        units: &[crate::ir::ExecutionUnit<RegionedAbsoluteAddr>],
        input_offset: usize,
        input_byte_size: usize,
        output_offset: usize,
        output_byte_size: usize,
    ) -> Result<*const u8, String> {
        let ptr_type = self.module.target_config().pointer_type();
        let mut ctx = self.module.make_context();

        // Signature: (mem_ptr, inputs_ptr, outputs_ptr, count) -> i64
        ctx.func.signature.params.push(AbiParam::new(ptr_type)); // mem_ptr
        ctx.func.signature.params.push(AbiParam::new(ptr_type)); // inputs_ptr
        ctx.func.signature.params.push(AbiParam::new(ptr_type)); // outputs_ptr
        ctx.func.signature.params.push(AbiParam::new(types::I64)); // count
        ctx.func.signature.returns.push(AbiParam::new(types::I64));

        let mut builder_ctx = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);

            let counter_var = builder.declare_var(types::I64);

            // Entry block
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);

            let mem_ptr = builder.block_params(entry)[0];
            let inputs_ptr = builder.block_params(entry)[1];
            let outputs_ptr = builder.block_params(entry)[2];
            let count = builder.block_params(entry)[3];

            let zero = builder.ins().iconst(types::I64, 0);
            builder.def_var(counter_var, zero);

            let loop_header = builder.create_block();
            let loop_body = builder.create_block();
            let loop_tail = builder.create_block();
            let exit_block = builder.create_block();

            builder.ins().jump(loop_header, &[]);

            // Loop header: check i < count
            builder.switch_to_block(loop_header);
            let i = builder.use_var(counter_var);
            let done = builder
                .ins()
                .icmp(IntCC::UnsignedGreaterThanOrEqual, i, count);
            builder.ins().brif(done, exit_block, &[], loop_body, &[]);

            // Loop body: copy input to memory, then inline eval_comb
            builder.switch_to_block(loop_body);
            let i = builder.use_var(counter_var);

            let input_byte_off = builder.ins().imul_imm(i, input_byte_size as i64);
            let src_addr = builder.ins().iadd(inputs_ptr, input_byte_off);
            let dst_addr = builder.ins().iadd_imm(mem_ptr, input_offset as i64);
            Self::emit_memcpy(&mut builder, src_addr, dst_addr, input_byte_size);

            // Inline eval_comb units; last unit jumps to loop_tail
            self.translator
                .translate_units_into(units, &mut builder, mem_ptr, Some(loop_tail));

            // Loop tail: copy output from memory, increment counter
            builder.switch_to_block(loop_tail);
            let i = builder.use_var(counter_var);

            let src_addr = builder.ins().iadd_imm(mem_ptr, output_offset as i64);
            let output_byte_off = builder.ins().imul_imm(i, output_byte_size as i64);
            let dst_addr = builder.ins().iadd(outputs_ptr, output_byte_off);
            Self::emit_memcpy(&mut builder, src_addr, dst_addr, output_byte_size);

            let next_i = builder.ins().iadd_imm(i, 1);
            builder.def_var(counter_var, next_i);
            builder.ins().jump(loop_header, &[]);

            // Exit
            builder.switch_to_block(exit_block);
            let success = builder.ins().iconst(types::I64, 0);
            builder.ins().return_(&[success]);

            builder.seal_all_blocks();
            builder.finalize();
        }

        let func_id = self
            .module
            .declare_anonymous_function(&ctx.func.signature)
            .map_err(|e| format!("Failed to declare batch eval function: {e}"))?;

        self.optimize_and_define(&mut ctx, func_id, "=== batch_eval ===", None, None, None)?;

        self.module
            .finalize_definitions()
            .map_err(|e| format!("Failed to finalize batch eval: {e}"))?;

        Ok(self.module.get_finalized_function(func_id))
    }

    /// Emit a memcpy of `size` bytes from `src` to `dst` using typed loads/stores.
    fn emit_memcpy(builder: &mut FunctionBuilder, src: Value, dst: Value, size: usize) {
        let mut offset = 0i32;
        let mut remaining = size;
        while remaining > 0 {
            let (ty, step) = if remaining >= 8 {
                (types::I64, 8)
            } else if remaining >= 4 {
                (types::I32, 4)
            } else if remaining >= 2 {
                (types::I16, 2)
            } else {
                (types::I8, 1)
            };
            let val = builder.ins().load(ty, MemFlags::new(), src, offset);
            builder.ins().store(MemFlags::new(), val, dst, offset);
            offset += step as i32;
            remaining -= step;
        }
    }
}
