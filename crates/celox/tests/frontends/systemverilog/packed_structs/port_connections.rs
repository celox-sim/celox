use super::*;

sv_backends! {
    fn bit_member_output_connections(sim) {
        @setup {
            let source = r#"
                module Pass #(parameter W = 4)(input logic [W-1:0] a, output logic [W-1:0] y);
                    assign y = a;
                endmodule
                module Top(input logic [7:0] raw,
                           output logic [7:0] direct, sliced, concatenated, net_value);
                    typedef struct packed { logic [3:0] four; bit [3:0] two; } mixed_t;
                    mixed_t d, s, c;
                    wire mixed_t n;
                    Pass direct_two(.a(raw[3:0]), .y(d.two));
                    Pass direct_four(.a(raw[7:4]), .y(d.four));
                    Pass #(.W(2)) slice_low(.a(raw[1:0]), .y(s.two[1:0]));
                    Pass #(.W(2)) slice_high(.a(raw[3:2]), .y(s.two[3:2]));
                    assign s.four = raw[7:4];
                    Pass #(.W(8)) concat_out(.a(raw), .y({c.two, c.four}));
                    Pass net_two(.a(raw[3:0]), .y(n.two));
                    Pass net_four(.a(raw[7:4]), .y(n.four));
                    assign direct = d;
                    assign sliced = s;
                    assign concatenated = c;
                    assign net_value = n;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("struct_output_connections.sv"))], "Top")
            .four_state(true);
        let raw = sim.signal("raw");
        // Cover 0, 1, X, and Z independently in each nibble. Inspect whole
        // structs so a read-side bit conversion cannot hide an incorrect write.
        for mask in [0u8, 0x0f, 0xf0, 0xff, 0x55, 0xaa] {
            for value in [0u8, 0x0f, 0xf0, 0xff, 0x55, 0xaa] {
                sim.modify(|io| io.set_four_state(raw, value.into(), mask.into())).unwrap();
                let expected = ((value & !(mask & 15)).into(), (mask & 0xf0).into());
                for name in ["direct", "sliced", "net_value"] {
                    assert_eq!(sim.get_four_state(sim.signal(name)), expected, "{name}: {value:x}/{mask:x}");
                }
                let concat_value = ((value & 15) << 4) | ((value & !mask) >> 4);
                let concat_mask = (mask & 15) << 4;
                assert_eq!(sim.get_four_state(sim.signal("concatenated")), (concat_value.into(), concat_mask.into()));
            }
        }
    }

    fn bit_member_ports_preserve_width_and_input_conversion(sim) {
        @setup {
            let source = r#"
                module SignedSource(input logic signed [3:0] a, output logic signed [3:0] y);
                    assign y = a;
                endmodule
                module Sink(input logic signed [7:0] a, output logic signed [15:0] y);
                    assign y = a;
                endmodule
                module Top(input logic [7:0] raw, output logic [11:0] whole,
                           output logic [15:0] input_value);
                    typedef struct packed { bit signed [7:0] data; } payload_t;
                    typedef struct packed { logic [3:0] tag; payload_t payload; } packet_t;
                    packet_t written, read_value;
                    SignedSource source(.a(raw[3:0]), .y(written.payload.data));
                    assign written.tag = raw[7:4];
                    assign read_value = {4'b0, raw};
                    Sink sink(.a(read_value.payload.data), .y(input_value));
                    assign whole = written;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("struct_port_widths.sv"))], "Top")
            .four_state(true);
        let raw = sim.signal("raw");
        for mask in [0u8, 0x08, 0x80, 0xff, 0x55, 0xaa] {
            for value in [0u8, 0x07, 0x08, 0x8f, 0xff, 0x55, 0xaa] {
                sim.modify(|io| io.set_four_state(raw, value.into(), mask.into())).unwrap();
                let extended_value = (((value << 4) as i8) >> 4).cast_unsigned();
                let extended_mask = (((mask << 4) as i8) >> 4).cast_unsigned();
                let written = ((u16::from(value & 0xf0)) << 4) | u16::from(extended_value & !extended_mask);
                assert_eq!(sim.get_four_state(sim.signal("whole")), (written.into(), (u16::from(mask & 0xf0) << 4).into()));
                let input_value = ((value & !mask) as i8 as i16).cast_unsigned();
                assert_eq!(sim.get_four_state(sim.signal("input_value")), (input_value.into(), 0u8.into()));
            }
        }
    }
}

#[test]
fn rejects_overlapping_bit_member_output_drivers() {
    for other_driver in [
        "Pass second(.a(a), .y(value.two));",
        "assign value.two[0] = a[0];",
    ] {
        let source = format!(
            r#"
                module Pass(input logic [3:0] a, output logic [3:0] y); assign y = a; endmodule
                module Top(input logic [3:0] a, output logic [7:0] y);
                    struct packed {{ logic [3:0] four; bit [3:0] two; }} value;
                    Pass first(.a(a), .y(value.two));
                    {other_driver}
                    assign value.four = a;
                    assign y = value;
                endmodule
            "#,
        );
        let error = match Simulator::from_sv_sources(
            vec![(source.as_str(), Path::new("struct_output_drivers.sv"))],
            "Top",
        )
        .build_cranelift()
        {
            Ok(_) => panic!("overlapping member drivers were accepted"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("multiple variable drivers for `value`"),
            "{error}"
        );
    }
}
