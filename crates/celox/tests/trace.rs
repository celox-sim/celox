use celox::SimulatorBuilder;

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
module ModuleB {
    var a: logic<32>;
    var b: logic<32>;
    assign b = a + 1;
}
    "#;

    let result = SimulatorBuilder::new(code, "ModuleB")
        .trace_post_optimized_clif()
        .trace_native()
        .build_with_trace();

    let res = result.res;
    let trace = result.trace;

    res.expect("Build should succeed");

    let clif = trace.post_optimized_clif.expect("CLIF should be captured");
    assert!(clif.contains("Cranelift IR (CLIF) Dump"));
    // Cranelift IR uses iadd for integer addition
    assert!(
        clif.contains("iadd"),
        "CLIF should contain iadd, found: {}",
        clif
    );

    let native = trace.native.expect("Native should be captured");
    assert!(native.contains("Native Machine Code Dump"));
    assert!(native.contains("Hex: "));
}



