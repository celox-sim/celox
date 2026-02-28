use celox::SimulatorBuilder;

#[test]
fn test_trace_analyzer_ir() {
    let code = r#"
        module Top {
            let a: logic<32> = 1;
        }
    "#;
    let result = SimulatorBuilder::new(code, "Top")
        .trace_analyzer_ir()
        .build_with_trace();
    let trace = result.trace;

    let analyzer_ir = trace
        .format_analyzer_ir()
        .expect("analyzer_ir should be captured");
    assert!(analyzer_ir.contains("module Top"));
    assert!(analyzer_ir.contains("let var0(a): logic<32>"));
}



