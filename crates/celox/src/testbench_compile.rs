use crate::ir::{AbsoluteAddr, RuntimeProgram};
use celox_frontend::VerylTestbenchSource;
use celox_testbench::TestbenchProgram;

pub(crate) fn project_observability(
    program: &mut RuntimeProgram,
    source: &VerylTestbenchSource,
) -> Result<(), celox_frontend::ParserError> {
    let (sites, read_variables) =
        celox_frontend::collect_testbench_observability(&program.frontend, source)?;
    program.runtime_schema.runtime_event_sites.extend(sites);
    program.runtime_schema.testbench_read_roots = read_variables;
    Ok(())
}

pub(crate) fn compile_semantic_testbench(
    program: &RuntimeProgram,
    source: &VerylTestbenchSource,
    random_seed: Option<u64>,
) -> Result<Option<TestbenchProgram<AbsoluteAddr>>, celox_frontend::ParserError> {
    celox_frontend::compile_semantic_testbench(
        &program.frontend,
        source,
        program.runtime_schema.runtime_event_sites.len(),
        random_seed,
    )
}
