use crate::ir::{AbsoluteAddr, FrontendLookup, RuntimeSchema};
use celox_frontend_veryl::VerylTestbenchSource;
use celox_testbench::TestbenchProgram;

pub(crate) fn project_observability(
    lookup: &FrontendLookup,
    runtime_schema: &mut RuntimeSchema<AbsoluteAddr>,
    source: &VerylTestbenchSource,
) -> Result<(), celox_frontend_veryl::ParserError> {
    let (sites, read_variables) =
        celox_frontend_veryl::collect_testbench_observability(lookup, source)?;
    runtime_schema.runtime_event_sites.extend(sites);
    runtime_schema.testbench_read_roots.extend(read_variables);
    Ok(())
}

pub(crate) fn compile_semantic_testbench(
    lookup: &FrontendLookup,
    runtime_event_site_count: usize,
    source: &VerylTestbenchSource,
    random_seed: Option<u64>,
) -> Result<Option<TestbenchProgram<AbsoluteAddr>>, celox_frontend_veryl::ParserError> {
    celox_frontend_veryl::compile_semantic_testbench(
        lookup,
        source,
        runtime_event_site_count,
        random_seed,
    )
}

/// Compile every expression at the ordinary pre-layout SIR boundary. Runtime
/// bindings carry entry indices and result slots, never executable bytecode.
pub(crate) fn lower_expressions(
    sir: &mut crate::ir::SirProgram,
    runtime: &mut crate::ir::RuntimeProgram,
    program: &mut TestbenchProgram<AbsoluteAddr>,
) -> Result<(), String> {
    use celox_design::{ProcessKind, ProcessSlots, RegionedStateAddr, STABLE_REGION};
    use celox_sir::{BlockId, ExecutionUnit, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator};
    use celox_testbench::{ExprOpcode, ExpressionKernel, StateLocation};
    let control = ProcessSlots {
        kind: ProcessKind::Callable,
        requires_timed_simulation: false,
        const_delays: false,
        program: None,
        resume: runtime.design.allocate_testbench_slot(16, false),
        status: runtime.design.allocate_testbench_slot(8, false),
        delay: runtime.design.allocate_testbench_slot(64, false),
    };
    for slot in [control.resume, control.status, control.delay] {
        runtime.runtime_schema.testbench_read_roots.insert(slot);
    }
    program.try_visit_expressions(|expression| {
        let mut builder = SIRBuilder::new();
        let value = celox_testbench::lowering::Expressions {
            builder: &mut builder,
        }
        .compile(expression)?;
        let width = builder.register(&value.register).width();
        let result = runtime.design.allocate_testbench_slot(width, value.string);
        runtime.runtime_schema.testbench_read_roots.insert(result);
        builder.emit(SIRInstruction::Store(
            result,
            SIROffset::Static(0),
            width,
            value.register,
            vec![],
            vec![],
        ));
        builder.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = builder.drain();
        let mut unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
        .into_map_addr(&mut |address| {
            RegionedStateAddr::from_absolute_addr(STABLE_REGION, address)
        });
        memory::normalize(&mut unit, &runtime.design);
        unit.verify_result().map_err(|error| error.to_string())?;
        let mut writes = Vec::new();
        for op in expression.ops() {
            match op {
                ExprOpcode::StoreU64 {
                    location,
                    byte_size,
                } => writes.push((*location, *byte_size)),
                ExprOpcode::StoreString { location } => {
                    writes.push((*location, celox_heap::HANDLE_BYTES))
                }
                _ => {}
            }
        }
        let constant = if width <= 64
            && !value.string
            && writes.is_empty()
            && !unit
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
                .any(|inst| {
                    matches!(
                        inst,
                        SIRInstruction::Load(..) | SIRInstruction::ExternCall { .. }
                    )
                }) {
            celox_sir_opt::optimizer::optimize_merged_chain(
                &mut unit,
                std::sync::Arc::new(Default::default()),
                |_, _, _| true,
                false,
                false,
                &Default::default(),
                || false,
            )
            .map_err(|error| error.to_string())?;
            let constants = celox_sir::analysis::collect_unique_exact_u64_constants(&unit);
            unit.blocks
                .values()
                .flat_map(|block| &block.instructions)
                .find_map(|inst| match inst {
                    SIRInstruction::Store(address, _, _, value, _, _)
                        if address.absolute_addr() == result =>
                    {
                        constants.get(value).map(|value| value.value)
                    }
                    _ => None,
                })
        } else {
            None
        };
        let entry = sir.processes.len();
        sir.processes.push(unit);
        runtime.runtime_schema.processes.push(control);
        runtime
            .runtime_schema
            .process_origins
            .push(Default::default());
        runtime.runtime_schema.process_frames.push(None);
        expression.set_kernel(ExpressionKernel {
            constant,
            entry,
            result: StateLocation {
                address: result,
                byte_offset: 0,
            },
            width,
            is_string: value.string,
            writes,
        });
        Ok(())
    })
}

mod process;
pub(crate) use process::lower as lower_processes;

mod memory;
