use crate::ir::{AbsoluteAddr, RuntimeProgram};
use celox_frontend_veryl::VerylTestbenchSource;
use celox_testbench::TestbenchProgram;

pub(crate) fn project_observability(program: &mut RuntimeProgram, source: &VerylTestbenchSource) {
    let (sites, read_variables) = celox_frontend_veryl::collect_testbench_observability(source);
    program.runtime_schema.runtime_event_sites.extend(sites);
    program.runtime_schema.testbench_read_roots = read_variables
        .into_iter()
        .filter_map(|var_id| {
            program
                .frontend
                .root_variable(var_id)
                .map(|(address, _)| address)
        })
        .collect();
}

pub(crate) fn compile_semantic_testbench(
    program: &RuntimeProgram,
    source: &VerylTestbenchSource,
) -> Option<TestbenchProgram<AbsoluteAddr>> {
    celox_frontend_veryl::compile_semantic_testbench(
        &program.frontend,
        source,
        program.runtime_schema.runtime_event_sites.len(),
    )
}
