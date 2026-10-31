//! Diagnostic regression for the complete SHA-256 message-schedule mux.
use celox::{DiagnosticLevel, DiagnosticPolicy, SimBackend, Simulator};

fn source(missing: bool) -> String {
    let arms = (0..if missing { 15 } else { 16 })
        .map(|i| format!("4'd{i}: q = d + 8'd{i};"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "module Top(round: input logic<6>, d: input logic<8>, q: output logic<8>) {{
            always_comb {{
                if round <: 16 {{ case round[3:0] {{ {arms} }} }}
                else {{ q = d; }}
            }}
        }}"
    )
}

fn exercise<B: SimBackend>(mut sim: Simulator<B>) {
    assert!(sim.warnings().is_empty(), "{:?}", sim.warnings());
    let round = sim.signal("round");
    let d = sim.signal("d");
    let q = sim.signal("q");
    for data in [19u8, 103] {
        for selector in (0u8..64).rev() {
            sim.modify(|io| {
                io.set(round, selector);
                io.set(d, data);
            })
            .unwrap();
            assert_eq!(
                sim.get(q),
                (data + if selector < 16 { selector } else { 0 }).into(),
                "selector={selector}, data={data}"
            );
        }
    }
}

#[test]
fn complete_binary_case_has_no_latch_diagnostic() {
    let source = source(false);
    // Warn makes a spurious diagnostic observable even if default policy changes.
    let mut policy = DiagnosticPolicy::default();
    policy.set("inferred_latch", DiagnosticLevel::Warn);
    exercise(Simulator::builder(&source, "Top").build_native().unwrap());
    #[cfg(feature = "cranelift")]
    exercise(
        Simulator::builder(&source, "Top")
            .build_cranelift()
            .unwrap(),
    );
    #[cfg(feature = "wasm-runtime")]
    exercise(Simulator::builder(&source, "Top").build_wasm().unwrap());
    exercise(
        Simulator::builder(&source, "Top")
            .diagnostic_policy(policy)
            .build_interpreter()
            .unwrap(),
    );
}

#[test]
fn missing_case_still_infers_latch() {
    let source = source(true);
    let error = Simulator::builder(&source, "Top")
        .build_native()
        .map(|_| ())
        .expect_err("missing selector 15 must be rejected");
    assert!(error.to_string().contains("inferred_latch"), "{error}");
}

#[test]
fn four_state_selector_keeps_unmatched_retention() {
    let source = source(false);
    let error = Simulator::builder(&source, "Top")
        .four_state(true)
        .build_native()
        .map(|_| ())
        .expect_err("binary labels do not cover X/Z");
    assert!(error.to_string().contains("inferred_latch"), "{error}");
}

#[test]
fn complete_labels_do_not_hide_missing_assignments_or_other_selectors() {
    for source in [
        source(false).replace("4'd15: q = d + 8'd15;", "4'd15: if d[0] { q = d; }"),
        source(false).replace("4'd15:", "5'd31:"),
        source(false).replace("4'd15:", "d[3:0]:"),
    ] {
        let error = Simulator::builder(&source, "Top")
            .build_native()
            .map(|_| ())
            .expect_err("incomplete coverage must not be proved combinational");
        assert!(error.to_string().contains("inferred_latch"), "{error}");
    }
}
