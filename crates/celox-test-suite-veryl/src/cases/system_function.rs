use crate::Design;

cases! { Functions, "system_function";


    fn test_direct_comb_onehot_system_function(sim) {
        @build Design::new(r#"
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

        @build Design::new(r#"
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

        @build Design::new(r#"
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

        @build Design::new(r#"
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




    fn test_direct_comb_clog2_system_function(sim) {
        @build Design::new(r#"
module Top (
    d: input logic<8>,
    q: output logic<32>,
) {
    always_comb {
        q = $clog2(d);
    }
}
"#, "Top");

        let d = sim.signal("d");
        let q = sim.signal("q");

        for value in 0u16..256 {
            let value = value as u8;
            sim.modify(|io| io.set(d, value)).unwrap();
            let expected = if value == 0 {
                0
            } else {
                u32::BITS - (u32::from(value) - 1).leading_zeros()
            };
            assert_eq!(sim.get_as::<u32>(q), expected, "value={value}");
        }
    }



    fn test_comb_function_body_clog2_system_function(sim) {

        @build Design::new(r#"
module Top (
    d: input logic<8>,
    q: output logic<32>,
) {
    function clog2_value (
        x: input logic<8>,
    ) -> logic<32> {
        return $clog2(x);
    }

    always_comb {
        q = clog2_value(d);
    }
}
"#, "Top");

        let d = sim.signal("d");
        let q = sim.signal("q");

        for value in 0u16..256 {
            let value = value as u8;
            sim.modify(|io| io.set(d, value)).unwrap();
            let expected = if value == 0 {
                0
            } else {
                u32::BITS - (u32::from(value) - 1).leading_zeros()
            };
            assert_eq!(sim.get_as::<u32>(q), expected, "value={value}");
        }
    }



    fn test_comb_function_body_bits_size_system_functions(sim) {

        @build Design::new(r#"
module Top (
    d: input logic<8>[4],
    bits_q: output logic<32>,
    size_q: output logic<32>,
) {
    function bits_value (
        x: input logic<8>[4],
    ) -> logic<32> {
        return $bits(x);
    }

    function size_value (
        x: input logic<8>[4],
    ) -> logic<32> {
        return $size(x);
    }

    always_comb {
        bits_q = bits_value(d);
        size_q = size_value(d);
    }
}
"#, "Top");

        let bits_q = sim.signal("bits_q");
        let size_q = sim.signal("size_q");
        assert_eq!(sim.get_as::<u32>(bits_q), 32);
        assert_eq!(sim.get_as::<u32>(size_q), 4);
    }



    fn test_direct_comb_signed_system_function_sign_extends_to_context(sim) {

        @build Design::new(r#"
module Top (
    d: input logic<8>,
    q: output logic<16>,
) {
    always_comb {
        q = $signed(d);
    }
}
"#, "Top");

        let d = sim.signal("d");
        let q = sim.signal("q");

        sim.modify(|io| io.set(d, 0x80u8)).unwrap();
        assert_eq!(sim.get_as::<u16>(q), 0xff80);
    }



    fn test_direct_comb_unsigned_system_function_zero_extends_to_context(sim) {

        @build Design::new(r#"
module Top (
    d: input logic<8>,
    q: output logic<16>,
) {
    always_comb {
        q = $unsigned(d);
    }
}
"#, "Top");

        let d = sim.signal("d");
        let q = sim.signal("q");

        sim.modify(|io| io.set(d, 0x80u8)).unwrap();
        assert_eq!(sim.get_as::<u16>(q), 0x0080);
    }



    fn test_direct_comb_signed_unsigned_system_functions_affect_comparison(sim) {

        @build Design::new(r#"
module Top (
    d: input logic<8>,
    z: input logic<8>,
    signed_lt: output logic,
    unsigned_lt: output logic,
) {
    always_comb {
        signed_lt = $signed(d) <: (z as i8);
        unsigned_lt = $unsigned(d) <: (z as i8);
    }
}
"#, "Top");

        let d = sim.signal("d");
        let z = sim.signal("z");
        let signed_lt = sim.signal("signed_lt");
        let unsigned_lt = sim.signal("unsigned_lt");

        sim.modify(|io| {
            io.set(d, 0xffu8);
            io.set(z, 0x01u8);
        })
        .unwrap();
        assert_eq!(sim.get_as::<u8>(signed_lt), 1);
        assert_eq!(sim.get_as::<u8>(unsigned_lt), 0);
    }



    fn test_comb_function_body_signed_unsigned_system_functions(sim) {

        @build Design::new(r#"
module Top (
    d: input logic<8>,
    signed_q: output logic<16>,
    unsigned_q: output logic<16>,
) {
    function signed_value (
        x: input logic<8>,
    ) -> logic<16> {
        return $signed(x);
    }

    function unsigned_value (
        x: input logic<8>,
    ) -> logic<16> {
        return $unsigned(x);
    }

    always_comb {
        signed_q = signed_value(d);
        unsigned_q = unsigned_value(d);
    }
}
"#, "Top");

        let d = sim.signal("d");
        let signed_q = sim.signal("signed_q");
        let unsigned_q = sim.signal("unsigned_q");

        sim.modify(|io| io.set(d, 0x80u8)).unwrap();
        assert_eq!(sim.get_as::<u16>(signed_q), 0xff80);
        assert_eq!(sim.get_as::<u16>(unsigned_q), 0x0080);
    }



    fn test_direct_comb_bits_type_system_function(sim) {

        @build Design::new(r#"
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

        @build Design::new(r#"
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

        @build Design::new(r#"
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

        @build Design::new(r#"
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

        @build Design::new(r#"
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

        @build Design::new(r#"
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




    fn test_direct_ff_size_packed_multidimensional_system_function(sim) {
        @build Design::new(r#"
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




    fn test_direct_ff_size_packed_multidimensional_type_system_function(sim) {
        @build Design::new(r#"
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




    fn test_direct_ff_clog2_system_function(sim) {
        @build Design::new(r#"
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

        @build Design::new(r#"
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

        @build Design::new(r#"
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

        @build Design::new(r#"
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

        @build Design::new(r#"
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

        @build Design::new(r#"
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




    fn test_direct_ff_onehot_system_function(sim) {
        @build Design::new(r#"
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

        @build Design::new(r#"
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
