use insta::assert_snapshot;
use celox::SimulatorBuilder;

#[test]
fn test_combinational_loop_error_readability() {
    let code = r#"
        module Top (
            a: input logic,
            y: output logic
        ) {
            assign y = ~y & a;
        }
    "#;
    let res = SimulatorBuilder::new(code, "Top").build();

    assert!(res.is_err());
    let err = res.unwrap_err().to_string();
    assert_snapshot!(err);
}

#[test]
fn test_multiple_driver_error_readability() {
    let code = r#"
        module Top (
            a: input logic,
            y: output logic
        ) {
            assign y = a;
            assign y = ~a;
        }
    "#;
    let res = SimulatorBuilder::new(code, "Top").build();

    assert!(res.is_err());
    let err = res.unwrap_err().to_string();
    assert_snapshot!(err);
}



