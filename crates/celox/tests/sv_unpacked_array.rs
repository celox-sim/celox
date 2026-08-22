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
