use cranelift::{codegen::Context, prelude::*};
use cranelift_frontend::FunctionBuilder;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::Module;

use crate::SimulatorOptions;
use crate::backend::memory_layout::MemoryLayout;
use crate::ir::RegionedAbsoluteAddr;

use super::SIRTranslator;

fn define_simulation_function(module: &mut JITModule, ctx: &mut Context) {
    let ptr_type = module.target_config().pointer_type();

    // Add one unified memory pointer argument
    ctx.func.signature.params.push(AbiParam::new(ptr_type)); // arg0: unified_mem
    ctx.func.signature.returns.push(AbiParam::new(types::I64));
    // In HDL, we usually don't return values from these blocks
    // as they update memory directly.
}
pub(super) struct JitEngine {
    module: JITModule,
    pub(super) translator: SIRTranslator,
}

impl JitEngine {
    pub fn new(layout: MemoryLayout, options: &SimulatorOptions) -> Result<Self, String> {
        // 1. Create a flag builder to set compiler options
        let mut flag_builder = settings::builder();

        // 2. Set optimization level to "speed"
        // Other options: "none", "speed_and_size"
        flag_builder
            .set("opt_level", "speed")
            .map_err(|e| e.to_string())?;

        // 3. Detect the host's native Instruction Set Architecture (ISA)
        let isa_builder = cranelift_native::builder().map_err(|e| e.to_string())?;

        // 4. Combine the flags and the ISA builder to create the final TargetIsa
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

    pub fn compile_units(
        &mut self,
        units: &[crate::ir::ExecutionUnit<RegionedAbsoluteAddr>],
        mut pre_clif_out: Option<&mut String>,
        mut post_clif_out: Option<&mut String>,
        mut native_out: Option<&mut String>,
    ) -> Result<*const u8, String> {
        // 1. Create function context
        let mut ctx = self.module.make_context();
        let mut builder_ctx = FunctionBuilderContext::new();

        // 2. Define function argument (unified memory pointer)
        define_simulation_function(&mut self.module, &mut ctx);

        // 3. Execute translation
        // Here all units are integrated into a single CFG (Control Flow Graph)
        {
            let builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
            self.translator.translate_units(units, builder);
        }

        if let Some(out) = pre_clif_out.as_mut() {
            out.push_str(&format!("{}\n", ctx.func.display()));
        }

        let isa = self.module.isa();
        let mut ctrl_plane = cranelift::codegen::control::ControlPlane::default();
        ctx.optimize(isa, &mut ctrl_plane)
            .map_err(|e| format!("Optimization failed: {e}"))?;

        if let Some(out) = post_clif_out.as_mut() {
            out.push_str(&format!("{}\n", ctx.func.display()));
        }

        // 4. Register to JIT module and compile
        let func_id = self
            .module
            .declare_anonymous_function(&ctx.func.signature)
            .map_err(|e| format!("Failed to declare master function: {e}"))?;

        self.module
            .define_function(func_id, &mut ctx)
            .map_err(|e| format!("Failed to define master function: {e}"))?;

        if let Some(out) = native_out.as_mut() {
            // Get the compiled code metadata from the context after define_function
            if let Some(compiled) = ctx.compiled_code() {
                out.push_str(&format!("Size: {} bytes\n", compiled.buffer.data().len()));
                out.push_str("Hex: ");
                for &byte in compiled.buffer.data() {
                    out.push_str(&format!("{:02x} ", byte));
                }
                out.push('\n');
            }
        }

        // Finalize symbols and write to executable memory
        self.module
            .finalize_definitions()
            .map_err(|e| format!("Failed to finalize JIT definitions: {e}"))?;

        // 5. Return the start address of the generated machine code
        Ok(self.module.get_finalized_function(func_id))
    }
}
