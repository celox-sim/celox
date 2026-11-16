use celox::Simulator;

#[test]
fn state_cast_is_explicit_in_sir() {
    let result = Simulator::builder(
        r#"
module Top (
    a: input logic<130>,
    y: output logic<130>,
) {
    type U130 = bit<130>;
    assign y = a as U130;
}
"#,
        "Top",
    )
    .four_state(true)
    .trace_post_optimized_sir()
    .build_with_trace();
    let sir = result
        .trace
        .format_post_optimized_sir()
        .expect("post-optimized SIR");
    assert!(
        sir.contains("ToTwoState"),
        "the state-conversion boundary disappeared from SIR:\n{sir}"
    );
}
