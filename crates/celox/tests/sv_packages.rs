#![cfg(feature = "systemverilog")]

use celox::{ParserError, Simulator, SimulatorErrorKind};

/// A package variable is one object shared by every module. Inlining would
/// give the writer and the reader their own copies, so it is rejected until
/// packages are resolved as scopes (#1146).
#[test]
fn rejects_variables_shared_through_a_package() {
    let source = r#"
        package p; logic [7:0] shared; endpackage
        module W(input logic [7:0] a); assign p::shared = a; endmodule
        module Top(output logic [7:0] y);
            W w(.a(8'd9));
            assign y = p::shared;
        endmodule
    "#;
    let err = Simulator::from_sv_sources(vec![(source, std::path::Path::new("p.sv"))], "Top")
        .build()
        .expect_err("a package variable must be rejected");
    match err.kind() {
        SimulatorErrorKind::SIRParser(ParserError::Unsupported {
            issue: 1146,
            detail,
            ..
        }) => {
            assert!(detail.contains("`p::shared`"), "{detail}")
        }
        other => panic!("expected the package variable to be unsupported, got: {other:?}"),
    }
}
