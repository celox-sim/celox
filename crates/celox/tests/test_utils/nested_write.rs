// Shared semantic assertions for small tests and CodSpeed scaling cases.
pub const NESTED_WRITE: &str = r#"
pub module AdcGroupNestedWriteMre (
    i_primary       : input  logic [8],
    i_overflow      : input  logic [8],
    i_primary_active: input  logic [2],
    o_pop           : output logic [8],
) {
    always_comb {
        for bank in 0..2 {
            for lane in 0..4 {
                let index: u32 = bank * 4 + lane;
                o_pop[index] = 0;
                if i_primary[index] {
                    o_pop[index] = 1;
                }
                if i_overflow[index] && !i_primary_active[bank] {
                    o_pop[index] = 1;
                }
            }
        }
    }
}
"#;

pub fn deep_nested_write(width: usize) -> String {
    let banks = if width.is_multiple_of(2) { 2 } else { 1 };
    let lanes = width / banks;
    NESTED_WRITE
        .replace("logic [8]", &format!("logic [{width}]"))
        .replace("0..2", &format!("0..{banks}"))
        .replace("0..4", &format!("0..{lanes}"))
        .replace("bank * 4", &format!("bank * {lanes}"))
}

macro_rules! complete {
    ($build:ident, $width:expr, $four_state:expr) => {{
        let width = $width;
        let four_state = $four_state;
        let source = $crate::nested_write::deep_nested_write(width);
        let mut sim = celox::Simulator::builder(&source, "AdcGroupNestedWriteMre")
            .four_state(four_state)
            .$build()
            .expect("deep fully assigned writes must compile");
        let primary = sim.signal("i_primary");
        let overflow = sim.signal("i_overflow");
        let active = sim.signal("i_primary_active");
        let out = sim.signal("o_pop");
        let requests = (celox::BigUint::from(1u8) << (width - 1)) | celox::BigUint::from(1u8);
        let extra = celox::BigUint::from(1u8) << (width / 2);
        sim.modify(|io| {
            io.set_wide(primary, requests.clone());
            io.set_wide(overflow, extra.clone());
            io.set(active, 0u8);
        })
        .unwrap();
        assert_eq!(sim.get(out), requests | extra);
        sim.modify(|io| {
            io.set(primary, 0u8);
            io.set(overflow, 0u8);
        })
        .unwrap();
        assert_eq!(sim.get(out), celox::BigUint::from(0u8));
    }};
}
pub(crate) use complete;

macro_rules! incomplete {
    ($build:ident, $width:expr, $four_state:expr) => {{
        use celox::{DiagnosticLevel, DiagnosticPolicy};
        use miette::Diagnostic;
        let four_state = $four_state;
        let width = $width;
        let lanes = width / 2;
        let source = $crate::nested_write::deep_nested_write(width)
            .replace(&format!("0..{lanes}"), &format!("0..{}", lanes - 1));
        let mut policy = DiagnosticPolicy::default();
        policy.set("inferred_latch", DiagnosticLevel::Warn);
        let mut sim = celox::Simulator::builder(&source, "AdcGroupNestedWriteMre")
            .diagnostic_policy(policy)
            .four_state(four_state)
            .$build()
            .unwrap();
        assert!(sim.warnings().iter().any(|warning| {
            warning
                .code()
                .is_some_and(|code| code.to_string() == "inferred_latch")
        }));
        let primary = sim.signal("i_primary");
        let out = sim.signal("o_pop");
        let overflow = sim.signal("i_overflow");
        let active = sim.signal("i_primary_active");
        // Unwritten bank tails retain their externally seeded values across activations.
        let retained =
            (celox::BigUint::from(1u8) << (width - 1)) | (celox::BigUint::from(1u8) << (lanes - 1));
        sim.modify(|io| {
            io.set_wide(out, retained.clone());
            io.set(primary, 1u8);
            io.set(overflow, 0u8);
            io.set(active, 0u8);
        })
        .unwrap();
        assert_eq!(sim.get(out), &retained | celox::BigUint::from(1u8));
        sim.modify(|io| {
            io.set(primary, 0u8);
        })
        .unwrap();
        assert_eq!(sim.get(out), retained);
    }};
}
pub(crate) use incomplete;

pub fn strictly_reject(width: usize) {
    let lanes = width / 2;
    let source =
        deep_nested_write(width).replace(&format!("0..{lanes}"), &format!("0..{}", lanes - 1));
    let error = celox::Simulator::builder(&source, "AdcGroupNestedWriteMre")
        .build_interpreter()
        .expect_err("incomplete writes must infer a latch");
    assert!(error.to_string().contains("inferred_latch"), "{error}");
}
