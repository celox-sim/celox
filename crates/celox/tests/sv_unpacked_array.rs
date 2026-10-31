#[cfg(feature = "systemverilog")]
#[test]
fn supports_fixed_unpacked_array_selects() {
    let source = r#"
        module Top (
            input  logic [7:0] i,
            output logic [7:0] o,
            output logic [15:0] all
        );
            logic [7:0] values [2];

            always_comb begin
                values[0] = i;
                values[1] = i + 1;
            end

            assign o = values[1];
            assign all = values;
        endmodule
    "#;
    let mut simulator = celox::Simulator::from_sv_sources(
        vec![(source, std::path::Path::new("fixed_array.sv"))],
        "Top",
    )
    .build()
    .expect("fixed unpacked array SV should build");
    let input = simulator.signal("i");
    let output = simulator.signal("o");
    let all = simulator.signal("all");

    simulator
        .modify(|io| io.set(input, 0x12u8))
        .expect("input update should succeed");
    assert_eq!(simulator.get(output), 0x13u8.into());
    assert_eq!(simulator.get(all), 0x1312u16.into());
}

#[cfg(feature = "systemverilog")]
#[test]
fn supports_fixed_unpacked_array_ports() {
    let source = r#"
        module Top (
            input logic [7:0] values [0:1],
            output logic [7:0] o
        );
            assign o = values[1];
        endmodule
    "#;
    let mut simulator = celox::Simulator::from_sv_sources(
        vec![(source, std::path::Path::new("fixed_array_port.sv"))],
        "Top",
    )
    .build()
    .expect("fixed unpacked array ports should build");
    let values = simulator.signal("values");
    let output = simulator.signal("o");

    simulator
        .modify(|io| io.set(values, 0x3412u16))
        .expect("array input update should succeed");
    assert_eq!(simulator.get(output), 0x34u8.into());
}

#[cfg(feature = "systemverilog")]
#[test]
fn supports_fixed_multidimensional_unpacked_array_selects() {
    let source = r#"
        module Top (
            input logic [7:0] i,
            output logic [7:0] o,
            output logic [47:0] all
        );
            logic [7:0] values [0:1][0:2];
            always_comb begin
                values[0][0] = i;
                values[1][2] = i + 1;
            end
            assign o = values[1][2];
            assign all = values;
        endmodule
    "#;
    let mut simulator = celox::Simulator::from_sv_sources(
        vec![(source, std::path::Path::new("fixed_multidim_array.sv"))],
        "Top",
    )
    .build()
    .expect("fixed multidimensional unpacked arrays should build");
    let input = simulator.signal("i");
    let output = simulator.signal("o");
    let all = simulator.signal("all");

    simulator
        .modify(|io| io.set(input, 0x12u8))
        .expect("input update should succeed");
    assert_eq!(simulator.get(output), 0x13u8.into());
    assert_eq!(simulator.get(all), 0x1300_0000_0012u64.into());
}

#[cfg(feature = "systemverilog")]
#[test]
fn preserves_typedef_owned_unpacked_array_dimensions() {
    let source = r#"
        module Top (
            input  logic [7:0] i,
            output logic [7:0] o,
            output logic [15:0] all
        );
            typedef logic [7:0] word_t [0:1];
            word_t values;

            always_comb begin
                values[0] = i;
                values[1] = i + 1;
            end

            assign o = values[1];
            assign all = values;
        endmodule
    "#;
    let mut simulator = celox::Simulator::from_sv_sources(
        vec![(source, std::path::Path::new("typedef_array.sv"))],
        "Top",
    )
    .build()
    .expect("typedef-owned unpacked arrays should build");
    let input = simulator.signal("i");
    let output = simulator.signal("o");
    let all = simulator.signal("all");

    simulator
        .modify(|io| io.set(input, 0x12u8))
        .expect("input update should succeed");
    assert_eq!(simulator.get(output), 0x13u8.into());
    assert_eq!(simulator.get(all), 0x1312u16.into());
}

#[cfg(feature = "systemverilog")]
#[test]
fn preserves_typedef_declarator_unpacked_array_dimensions() {
    let source = r#"
        module Top (
            input  logic [7:0] i,
            output logic [7:0] o,
            output logic [15:0] all
        );
            typedef logic [7:0] word_t;
            word_t values [0:1];

            always_comb begin
                values[0] = i;
                values[1] = i + 1;
            end

            assign o = values[1];
            assign all = values;
        endmodule
    "#;
    let mut simulator = celox::Simulator::from_sv_sources(
        vec![(source, std::path::Path::new("typedef_declarator_array.sv"))],
        "Top",
    )
    .build()
    .expect("typedef declarator unpacked arrays should build");
    let input = simulator.signal("i");
    let output = simulator.signal("o");
    let all = simulator.signal("all");

    simulator
        .modify(|io| io.set(input, 0x12u8))
        .expect("input update should succeed");
    assert_eq!(simulator.get(output), 0x13u8.into());
    assert_eq!(simulator.get(all), 0x1312u16.into());
}

#[cfg(feature = "systemverilog")]
#[test]
fn preserves_signedness_of_unpacked_array_elements() {
    let source = r#"
        module Top (
            input  logic signed [7:0] i,
            output logic lt
        );
            logic signed [7:0] values [0:1];
            assign values[0] = i;
            assign values[1] = 0;
            assign lt = values[0] < 0;
        endmodule
    "#;
    let mut simulator = celox::Simulator::from_sv_sources(
        vec![(source, std::path::Path::new("signed_array.sv"))],
        "Top",
    )
    .build()
    .expect("signed unpacked arrays should build");
    let input = simulator.signal("i");
    let output = simulator.signal("lt");

    simulator
        .modify(|io| io.set(input, 0xffu8))
        .expect("input update should succeed");
    assert_eq!(simulator.get(output), 1u8.into());
}

#[cfg(feature = "systemverilog")]
#[test]
fn preserves_width_of_zero_literal_casts() {
    let source = r#"
        module Top(output logic [8:0] y);
            assign y = {1'b1, 8'(0)};
        endmodule
    "#;
    let mut simulator = celox::Simulator::from_sv_sources(
        vec![(source, std::path::Path::new("zero_cast.sv"))],
        "Top",
    )
    .build()
    .expect("zero literal casts should build");
    assert_eq!(simulator.get(simulator.signal("y")), 0x100u16.into());
}

// Exercise physical layout and host API contracts as well as HDL behavior.
#[cfg(feature = "systemverilog")]
macro_rules! strided_backend_tests {
    ($name:ident, $build:ident) => {
        mod $name {
            #[test]
            fn ff_writes_clear_only_written_array_masks() {
                for four_state in [false, true] {
                    for (parallel, packed) in [(false, false), (true, false), (false, true)] {
                        let source = r#"
                            module Top(input logic clk, input logic [2:0] index,
                                output logic [19:0] a, b, c, d, e, untouched);
                                logic [19:0] p [6];
                                logic [19:0] q [6];
                                logic [19:0] u [6];
                                logic [19:0] matrix [2][3];
                                always_ff @(posedge clk) begin
                                    p[0] <= 20'h11;
                                    for (int k = 0; k < 6; k++) u[k] <= 20'(k + 1);
                                    q[0] <= 20'h1; q[1] <= 20'h2; q[5] <= 20'h6;
                                    for (int i = 0; i < 2; i++)
                                        for (int j = 0; j < 3; j++)
                                            matrix[i][j] <= 20'(i * 3 + j + 1);
                                end
                                assign a = p[0];
                                assign b = q[0];
                                assign c = u[index];
                                assign d = q[5];
                                assign e = matrix[1][2];
                                assign untouched = q[3];
                            endmodule
                        "#;
                        let builder = celox::Simulator::from_sv_sources(
                            vec![(source, std::path::Path::new("array_masks.sv"))],
                            "Top",
                        )
                        .four_state(four_state)
                        .parallel_partition(if parallel {
                            celox::ParallelPartition::Always
                        } else {
                            celox::ParallelPartition::Estimated
                        });
                        let trace = tempfile::tempdir().unwrap();
                        let builder = if packed {
                            builder.vcd(trace.path().join("array.vcd"))
                        } else {
                            builder
                        };
                        let mut sim = builder.$build().unwrap();
                        // These arrays must actually select the new layout.
                        assert_eq!(sim.signal("u").array_layout.is_some(), !packed);
                        let clk = sim.event("clk");
                        let index = sim.signal("index");
                        sim.modify(|io| io.set(index, 0u8)).unwrap();
                        for _ in 0..2 {
                            sim.tick(clk).unwrap();
                            for (name, value) in
                                [("a", 0x11u32), ("b", 1), ("c", 1), ("d", 6), ("e", 6)]
                            {
                                assert_eq!(
                                    sim.get_four_state(sim.signal(name)),
                                    (value.into(), 0u8.into()),
                                    "{name}, four_state={four_state}, parallel={parallel}"
                                );
                            }
                            for i in 0..6u8 {
                                sim.modify(|io| io.set(index, i)).unwrap();
                                assert_eq!(
                                    sim.get_four_state(sim.signal("c")),
                                    ((i + 1).into(), 0u8.into())
                                );
                            }
                            let expected = (0..6u32).fold(celox::BigUint::from(0u8), |value, i| {
                                value | (celox::BigUint::from(i + 1) << (i as usize * 20))
                            });
                            assert_eq!(sim.get_four_state(sim.signal("u")), (expected, 0u8.into()));
                            if four_state {
                                assert_eq!(
                                    sim.get_four_state(sim.signal("untouched")).1,
                                    0xfffffu32.into()
                                );
                            }
                            sim.modify(|io| io.set(index, 0u8)).unwrap();
                        }
                        // Raw host access must scatter into padded elements and
                        // gather the same packed value and mask again.
                        let u = sim.signal("u");
                        let value = (celox::BigUint::from(0xabcdeu32) << 100usize)
                            | celox::BigUint::from(0x12345u32);
                        let mask = celox::BigUint::from(0x101u32) << 40usize;
                        sim.modify(|io| io.set_four_state(u, value.clone(), mask.clone()))
                            .unwrap();
                        assert_eq!(
                            sim.get_four_state(u),
                            (value.clone(), if four_state { mask } else { 0u8.into() })
                        );
                        sim.modify(|io| io.set(u, 0x123456789abcdefu64)).unwrap();
                        assert_eq!(
                            sim.get_four_state(u),
                            (0x123456789abcdefu64.into(), 0u8.into())
                        );
                    }
                }
            }
        }
    };
}

#[cfg(feature = "systemverilog")]
strided_backend_tests!(native_strided, build_native);
#[cfg(all(feature = "systemverilog", feature = "cranelift"))]
strided_backend_tests!(cranelift_strided, build_cranelift);
#[cfg(all(feature = "systemverilog", feature = "wasm-runtime"))]
strided_backend_tests!(wasm_strided, build_wasm);
