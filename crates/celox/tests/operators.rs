use celox::Simulator;

// ============================================================
// Division / Modulo (always_comb)
// ============================================================

// ============================================================
// BitXnor (binary)
// ============================================================

// ============================================================
// Reduction BitNand / BitNor / BitXnor (unary)
// ============================================================

#[test]
fn test_nested_ternary_concat_hybrid() {
    let code = r#"
        module Top (sel: input logic, a: input logic<4>, b: input logic<4>, c: input logic<8>, o: output logic<8>) {
            always_comb {
                o = if sel ? {a, b} : (if (a == b) ? c : 8'hEE);
            }
        }
    "#;
    let result = Simulator::builder(code, "Top").build();
    assert!(
        result.is_ok(),
        "Should handle deeply nested expression structures"
    );
}
