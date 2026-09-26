use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn test_lhs_concatenation_execution(sim) {
        @ignore_on(sv);
        @case "concatenation::test_lhs_concatenation_execution";
    }

    fn test_rhs_concatenation_execution(sim) {
        @case "concatenation::test_rhs_concatenation_execution";
    }

    fn test_rhs_mixed_concatenation_execution(sim) {
        @case "concatenation::test_rhs_mixed_concatenation_execution";
    }

    fn test_replication_concatenation_execution(sim) {
        @case "concatenation::test_replication_concatenation_execution";
    }
}

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
