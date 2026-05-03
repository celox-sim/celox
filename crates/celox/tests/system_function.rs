use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    #[ignore = "direct $onehot in always_comb currently evaluates incorrectly before Celox system-function lowering"]
    fn test_direct_comb_onehot_system_function(sim) {
        @build Simulator::builder(r#"
module Top (
    d: input logic<8>,
    q: output logic,
) {
    always_comb {
        q = $onehot(d);
    }
}
"#, "Top");

        let d = sim.signal("d");
        let q = sim.signal("q");

        for value in 0u16..256 {
            let value = value as u8;
            sim.modify(|io| io.set(d, value)).unwrap();
            assert_eq!(
                sim.get_as::<u8>(q),
                u8::from(value.count_ones() == 1),
                "value={value:#010b}",
            );
        }
    }

    fn test_comb_function_body_onehot_system_function(sim) {
        @ignore_on(veryl);
        @build Simulator::builder(r#"
module Top (
    d: input logic<8>,
    q: output logic,
) {
    function is_onehot (
        x: input logic<8>,
    ) -> logic {
        return $onehot(x);
    }

    always_comb {
        q = is_onehot(d);
    }
}
"#, "Top");

        let d = sim.signal("d");
        let q = sim.signal("q");

        for value in 0u16..256 {
            let value = value as u8;
            sim.modify(|io| io.set(d, value)).unwrap();
            assert_eq!(
                sim.get_as::<u8>(q),
                u8::from(value.count_ones() == 1),
                "value={value:#010b}",
            );
        }
    }

    fn test_direct_comb_bits_system_function(sim) {
        @build Simulator::builder(r#"
module Top (
    d: input logic<8>,
    q: output logic<32>,
) {
    always_comb {
        q = $bits(d);
    }
}
"#, "Top");

        let q = sim.signal("q");
        assert_eq!(sim.get_as::<u32>(q), 8);
    }

    fn test_direct_comb_size_system_function(sim) {
        @ignore_on(veryl);
        @build Simulator::builder(r#"
module Top (
    d: input logic<8>[4],
    q: output logic<32>,
) {
    always_comb {
        q = $size(d);
    }
}
"#, "Top");

        let q = sim.signal("q");
        assert_eq!(sim.get_as::<u32>(q), 4);
    }

    fn test_direct_comb_bits_type_system_function(sim) {
        @build Simulator::builder(r#"
module Top (
    q: output logic<32>,
) {
    always_comb {
        q = $bits(logic<8>);
    }
}
"#, "Top");

        let q = sim.signal("q");
        assert_eq!(sim.get_as::<u32>(q), 8);
    }

    fn test_direct_ff_bits_system_function(sim) {
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    d: input logic<8>,
    q: output logic<32>,
) {
    always_ff (clk) {
        q = $bits(d);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let q = sim.signal("q");

        sim.tick(clk).unwrap();
        assert_eq!(sim.get_as::<u32>(q), 8);
    }

    fn test_direct_ff_bits_type_system_function(sim) {
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    q: output logic<32>,
) {
    always_ff (clk) {
        q = $bits(logic<8>);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let q = sim.signal("q");

        sim.tick(clk).unwrap();
        assert_eq!(sim.get_as::<u32>(q), 8);
    }

    fn test_direct_ff_bits_array_system_function(sim) {
        @ignore_on(veryl);
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    d: input logic<8>[4],
    q: output logic<32>,
) {
    always_ff (clk) {
        q = $bits(d);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let q = sim.signal("q");

        sim.tick(clk).unwrap();
        assert_eq!(sim.get_as::<u32>(q), 32);
    }

    fn test_direct_ff_size_system_function(sim) {
        @ignore_on(veryl);
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    d: input logic<8>[4],
    q: output logic<32>,
) {
    always_ff (clk) {
        q = $size(d);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let q = sim.signal("q");

        sim.tick(clk).unwrap();
        assert_eq!(sim.get_as::<u32>(q), 4);
    }

    fn test_direct_ff_size_type_system_function(sim) {
        @ignore_on(veryl);
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    q: output logic<32>,
) {
    always_ff (clk) {
        q = $size(logic<8>);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let q = sim.signal("q");

        sim.tick(clk).unwrap();
        assert_eq!(sim.get_as::<u32>(q), 8);
    }

    #[ignore = "$size on packed multidimensional types is folded to total width by Veryl analyzer before Celox FF lowering"]
    fn test_direct_ff_size_packed_multidimensional_system_function(sim) {
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    d: input logic<10, 20>,
    q: output logic<32>,
) {
    always_ff (clk) {
        q = $size(d);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let q = sim.signal("q");

        sim.tick(clk).unwrap();
        assert_eq!(sim.get_as::<u32>(q), 10);
    }

    #[ignore = "$size on packed multidimensional type arguments is folded to total width by Veryl analyzer before Celox FF lowering"]
    fn test_direct_ff_size_packed_multidimensional_type_system_function(sim) {
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    q: output logic<32>,
) {
    always_ff (clk) {
        q = $size(logic<10, 20>);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let q = sim.signal("q");

        sim.tick(clk).unwrap();
        assert_eq!(sim.get_as::<u32>(q), 10);
    }

    #[ignore = "direct $clog2 in always_ff is folded from X payload by Veryl analyzer before Celox FF lowering"]
    fn test_direct_ff_clog2_system_function(sim) {
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    d: input logic<8>,
    q: output logic<32>,
) {
    always_ff (clk) {
        q = $clog2(d);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let d = sim.signal("d");
        let q = sim.signal("q");

        for value in 0u16..256 {
            let value = value as u8;
            sim.modify(|io| io.set(d, value)).unwrap();
            sim.tick(clk).unwrap();
            let expected = if value == 0 {
                0
            } else {
                u32::BITS - (u32::from(value) - 1).leading_zeros()
            };
            assert_eq!(sim.get_as::<u32>(q), expected, "value={value}");
        }
    }

    fn test_ff_function_body_clog2_system_function(sim) {
        @ignore_on(veryl);
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    d: input logic<8>,
    q: output logic<32>,
) {
    function clog2_value (
        x: input logic<8>,
    ) -> logic<32> {
        return $clog2(x);
    }

    always_ff (clk) {
        q = clog2_value(d);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let d = sim.signal("d");
        let q = sim.signal("q");

        for value in 0u16..256 {
            let value = value as u8;
            sim.modify(|io| io.set(d, value)).unwrap();
            sim.tick(clk).unwrap();
            let expected = if value == 0 {
                0
            } else {
                u32::BITS - (u32::from(value) - 1).leading_zeros()
            };
            assert_eq!(sim.get_as::<u32>(q), expected, "value={value}");
        }
    }

    fn test_direct_ff_signed_system_function(sim) {
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    d: input logic<8>,
    q: output logic<8>,
) {
    always_ff (clk) {
        q = $signed(d);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let d = sim.signal("d");
        let q = sim.signal("q");

        sim.modify(|io| io.set(d, 0x80u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get_as::<u8>(q), 0x80);
    }

    fn test_direct_ff_signed_system_function_sign_extends_to_context(sim) {
        @ignore_on(veryl);
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    d: input logic<8>,
    q: output logic<16>,
) {
    always_ff (clk) {
        q = $signed(d);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let d = sim.signal("d");
        let q = sim.signal("q");

        sim.modify(|io| io.set(d, 0x80u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get_as::<u16>(q), 0xff80);
    }

    fn test_direct_ff_unsigned_system_function(sim) {
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    d: input logic<8>,
    q: output logic<8>,
) {
    always_ff (clk) {
        q = $unsigned(d);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let d = sim.signal("d");
        let q = sim.signal("q");

        sim.modify(|io| io.set(d, 0x80u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get_as::<u8>(q), 0x80);
    }

    fn test_direct_ff_unsigned_system_function_zero_extends_to_context(sim) {
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    d: input logic<8>,
    q: output logic<16>,
) {
    always_ff (clk) {
        q = $unsigned(d);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let d = sim.signal("d");
        let q = sim.signal("q");

        sim.modify(|io| io.set(d, 0x80u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get_as::<u16>(q), 0x0080);
    }

    #[ignore = "direct $onehot in always_ff is folded to 1'h0 by Veryl analyzer before Celox FF lowering"]
    fn test_direct_ff_onehot_system_function(sim) {
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    d: input logic<8>,
    q: output logic,
) {
    always_ff (clk) {
        q = $onehot(d);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let d = sim.signal("d");
        let q = sim.signal("q");

        for value in 0u16..256 {
            let value = value as u8;
            sim.modify(|io| io.set(d, value)).unwrap();
            sim.tick(clk).unwrap();
            assert_eq!(
                sim.get_as::<u8>(q),
                u8::from(value.count_ones() == 1),
                "value={value:#010b}",
            );
        }
    }

    fn test_ff_function_body_onehot_system_function(sim) {
        @ignore_on(veryl);
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    d: input logic<8>,
    q: output logic,
) {
    function is_onehot (
        x: input logic<8>,
    ) -> logic {
        return $onehot(x);
    }

    always_ff (clk) {
        q = is_onehot(d);
    }
}
"#, "Top");

        let clk = sim.event("clk");
        let d = sim.signal("d");
        let q = sim.signal("q");

        for value in 0u16..256 {
            let value = value as u8;
            sim.modify(|io| io.set(d, value)).unwrap();
            sim.tick(clk).unwrap();
            assert_eq!(
                sim.get_as::<u8>(q),
                u8::from(value.count_ones() == 1),
                "value={value:#010b}",
            );
        }
    }
}

#[test]
fn test_ff_statement_runtime_event_system_functions_are_supported() {
    let code = r#"
module Top (clk: input clock, d: input logic) {
    always_ff (clk) {
        $display("display d=%0d", d);
        $write("write d=%0d", d);
        $assert(d, "assert d=%0d", d);
    }
}
"#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let d = sim.signal("d");

    sim.modify(|io| io.set(d, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "display d=1".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "write d=1".to_string(),
            },
        ]
    );
}

#[test]
fn test_unsupported_ff_statement_system_functions_are_reported() {
    let cases = [
        (
            "readmemh",
            r#"
module Top (clk: input clock) {
    var mem: logic<8>[4];
    always_ff (clk) {
        $readmemh("mem.hex", mem);
    }
}
"#,
        ),
        (
            "finish",
            r#"
module Top (clk: input clock) {
    always_ff (clk) {
        $finish();
    }
}
"#,
        ),
    ];

    for (name, code) in cases {
        let err = Simulator::builder(code, "Top")
            .build()
            .expect_err("statement system function should be unsupported in FF lowering");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("system function call"),
            "expected system function unsupported error for {name}, got: {err:?}"
        );
    }
}
