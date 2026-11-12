//! Reconstructed Veryl upstream cases; `scripts/run-vtest.py` runs their
//! shared suite cases (`veryl_upstream::`), and the checks here need the API.

#[test]
fn zero_dimensions_report_analyzer_diagnostics() {
    for ty in ["logic<0>", "logic<8>[0]", "logic<2, 0>"] {
        let source = format!("module Top (value: output {ty}) {{ assign value = '0; }}");
        let error = celox::Simulator::builder(&source, "Top")
            .build_interpreter()
            .unwrap_err();
        let celox::SimulatorErrorKind::Analyzer(errors) = error.kind() else {
            panic!("{ty}: expected analyzer rejection, got {error:?}");
        };
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, veryl_analyzer::AnalyzerError::ZeroSize { .. })),
            "{ty}: expected ZeroSize, got {errors:?}"
        );
    }
}
