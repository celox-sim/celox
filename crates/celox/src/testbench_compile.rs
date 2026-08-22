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
    runtime_schema.testbench_read_roots = read_variables;
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
