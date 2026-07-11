use celox::SimulatorBuilder;

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

#[test]
fn test_compilation_trace_sim_modules() {
    let code = r#"
module ModuleA {
    var a: logic;
    assign a = 1;
}
    "#;

    let result = SimulatorBuilder::new(code, "ModuleA")
        .trace_sim_modules()
        .build_with_trace();

    let res = result.res;
    let trace = result.trace;

    res.expect("Build should succeed");

    let sim_modules = trace.sim_modules.expect("sim_modules should be captured");
    assert!(!sim_modules.is_empty(), "Modules map should not be empty");
}

#[test]
fn test_dynamic_glue_preserves_address_and_previous_value_roles() {
    let code = r#"
module Child (a: input logic<2>, y: output logic<2>) {
    assign y = a;
}
module Top (idx: input logic<3>, a: input logic<2>, out: output logic<8>) {
    var mem: logic<8>;
    inst child: Child (a, y: mem[idx +: 2]);
    assign out = mem;
}
"#;

    let result = SimulatorBuilder::new(code, "Top")
        .trace_sim_modules()
        .trace_flattened_comb_blocks()
        .build_with_trace();
    result.res.expect("dynamic glue should build");

    let sim_modules = result
        .trace
        .sim_modules
        .expect("module-level paths should be captured");
    let glue_path = sim_modules
        .values()
        .flat_map(|module| module.glue_blocks.values())
        .flatten()
        .flat_map(|block| block.output_ports.iter().map(|(_, path)| path))
        .find(|path| !path.address_sources.is_empty())
        .expect("dynamic output glue must record address sources");
    assert!(!glue_path.previous_sources.is_empty());
    assert!(glue_path.address_sources.iter().all(|address| {
        glue_path
            .sources
            .iter()
            .any(|source| source.id == address.id && source.access.overlaps(&address.access))
    }));

    let (flattened, _) = result
        .trace
        .flattened_comb_blocks
        .expect("flattened paths should be captured");
    let flattened_path = flattened
        .iter()
        .find(|path| !path.address_sources.is_empty())
        .expect("address-source roles must survive mapping and atomization");
    assert!(!flattened_path.previous_sources.is_empty());
    assert!(flattened_path.address_sources.iter().all(|address| {
        flattened_path
            .sources
            .iter()
            .any(|source| source.id == address.id && source.access.overlaps(&address.access))
    }));
}

#[test]
fn test_compilation_trace_extraction_on_error() {
    // This code has a combinational loop, so it will fail to schedule.
    let code = r#"
module BrokenLoop {
    var a: logic;
    var b: logic;
    assign a = b;
    assign b = a;
}
    "#;

    let result = SimulatorBuilder::new(code, "BrokenLoop")
        .trace_flattened_comb_blocks()
        .build_with_trace();

    let res = result.res;
    let trace = result.trace;

    // The build should fail
    assert!(res.is_err(), "Build should fail due to combinational loop");

    // But the trace should still contain the flattened logic paths
    let (blocks, _arena) = trace
        .flattened_comb_blocks
        .expect("flattened_comb_blocks should be captured");
    assert!(
        !blocks.is_empty(),
        "Flattened logic paths should be captured despite scheduling failure"
    );
}

#[test]
fn test_compilation_trace_backend_info() {
    let code = r#"
module ModuleB (
    a: input logic<32>,
    b: output logic<32>
) {
    assign b = a + 1;
}
    "#;

    let result = SimulatorBuilder::new(code, "ModuleB")
        .trace_mir()
        .trace_native()
        .build_with_trace();

    let res = result.res;
    let trace = result.trace;

    res.expect("Build should succeed");

    // MIR trace should be captured (native backend)
    let mir = trace.mir.expect("MIR should be captured");
    assert!(
        mir.contains("MIR"),
        "MIR trace should contain MIR header, found: {}",
        &mir[..mir.len().min(200)]
    );
}
