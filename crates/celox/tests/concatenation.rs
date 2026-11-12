use celox::Simulator;

#[test]
fn test_rhs_concatenation_dependency() {
    let code = r#"
        module Top (a: input logic<8>, b: input logic<8>) {
            var tmp: logic<16>;
            var out: logic<16>;
            always_comb {
                tmp = {a, b};
                out = tmp;
            }
        }
    "#;
    let result = Simulator::builder(code, "Top").build();
    assert!(
        result.is_ok(),
        "RHS concatenation must register all parts as sources"
    );
}
