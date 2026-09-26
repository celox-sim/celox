#[allow(unused_macros)]
mod test_utils;

use celox::{CodegenError, ParserError, SimulatorError, SimulatorErrorKind};
use veryl_test_suite::CompilationRejected;

#[test]
fn negative_suite_cases_accept_only_source_diagnostics() {
    let diagnostic = SimulatorError::from(ParserError::IllegalContext {
        feature: "output port connection",
        detail: "nonconstant destination".into(),
        source_location: None,
    });
    assert!(test_utils::suite::classify_build_error(diagnostic).is::<CompilationRejected>());

    for error in [
        SimulatorError::new(SimulatorErrorKind::Codegen(CodegenError::Cancelled)),
        SimulatorError::from(ParserError::TopNotFound { name: "Top".into() }),
        SimulatorError::from(ParserError::Unsupported {
            issue: 1,
            phase: celox::LoweringPhase::SimulatorParser,
            feature: "unimplemented adapter feature",
            detail: "not a language rejection".into(),
            source_location: None,
        }),
        SimulatorError::from(ParserError::Unsupported {
            issue: 64,
            phase: celox::LoweringPhase::SimulatorParser,
            feature: "systemverilog analysis",
            detail: "Unsupported SystemVerilog construct: indexed part-select".into(),
            source_location: None,
        }),
    ] {
        assert!(!test_utils::suite::classify_build_error(error).is::<CompilationRejected>());
    }
}
