use cranelift::{codegen::Context, prelude::*};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Module};

use crate::SimulatorOptions;
use crate::backend::memory_layout::MemoryLayout;
use crate::ir::RegionedAbsoluteAddr;
use crate::optimizer::coalescing::TailCallChunk;
use crate::optimizer::coalescing::pass_tail_call_split::MemorySpilledPlan;

use super::SIRTranslator;
use super::translator::core::get_cl_type;

fn define_simulation_function(module: &mut JITModule, ctx: &mut Context) {
    let ptr_type = module.target_config().pointer_type();

    // Add one unified memory pointer argument
    ctx.func.signature.params.push(AbiParam::new(ptr_type)); // arg0: unified_mem
    ctx.func.signature.returns.push(AbiParam::new(types::I64));
}
pub(super) struct JitEngine {
    module: JITModule,
    pub(super) translator: SIRTranslator,
}

impl JitEngine {
    pub fn new(layout: MemoryLayout, options: &SimulatorOptions) -> Result<Self, String> {
        let mut flag_builder = settings::builder();

        flag_builder
            .set("opt_level", "speed")
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
                out.push_str("Hex: ");
                for &byte in data {
                    out.push_str(&format!("{:02x} ", byte));
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

        let chunk0_func_ref = self.module.declare_func_in_func(
            first_chunk_func_id,
            &mut ctx.func,
        );

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
        let mut ctx = self.module.make_context();
        let mut builder_ctx = FunctionBuilderContext::new();

        define_simulation_function(&mut self.module, &mut ctx);

        {
            let builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
            self.translator.translate_units(units, builder);
        }

        let func_id = self
            .module
            .declare_anonymous_function(&ctx.func.signature)
            .map_err(|e| format!("Failed to declare master function: {e}"))?;

        self.optimize_and_define(&mut ctx, func_id, "=== eval_comb ===", pre_clif_out, post_clif_out, native_out)?;

        self.module
            .finalize_definitions()
            .map_err(|e| format!("Failed to finalize JIT definitions: {e}"))?;

        Ok(self.module.get_finalized_function(func_id))
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
                Some(self.module.declare_func_in_func(
                    chunk_func_ids[i + 1],
                    &mut ctx.func,
                ))
            } else {
                None
            };

            let mut builder_ctx = FunctionBuilderContext::new();
            {
                let builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
                self.translator.translate_chunk(chunk, next_func_ref, builder);
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
}
