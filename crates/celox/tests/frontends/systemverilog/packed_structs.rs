use super::*;

#[path = "packed_structs/port_connections.rs"]
mod port_connections;

sv_backends! {
    fn packed_struct_layout_and_nested_fields(sim) {
        @setup {
            let source = r#"
                module Top(input logic [15:0] raw, output logic [7:0] payload,
                           output logic [3:0] tag, output logic signed [7:0] delta,
                           output logic [15:0] whole, output logic [1:0] part);
                    typedef struct packed { logic [3:0] tag; logic signed [3:0] delta; } header_t;
                    typedef struct packed { header_t header; logic [7:0] payload; } packet_t;
                    packet_t packet;
                    assign packet = raw;
                    assign payload = packet.payload;
                    assign tag = packet.header.tag;
                    assign delta = packet.header.delta;
                    assign part = packet.payload[5:4];
                    assign whole = packet;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("packed_struct_layout.sv"))], "Top");
        let raw = sim.signal("raw");
        for value in [0u16, 0x1234, 0xa8ff, 0xffff] {
            sim.modify(|io| io.set(raw, value)).unwrap();
            assert_eq!(sim.get(sim.signal("payload")), (value as u8).into());
            assert_eq!(sim.get(sim.signal("tag")), (value >> 12).into());
            assert_eq!(sim.get(sim.signal("delta")), ((((value >> 8) as u8) << 4) as i8 >> 4).cast_unsigned().into());
            assert_eq!(sim.get(sim.signal("part")), ((value >> 4) & 3).into());
            assert_eq!(sim.get(sim.signal("whole")), value.into());
        }
    }

    fn packed_struct_field_assignments_and_registers(sim) {
        @case "packed_structs::packed_struct_field_assignments_and_registers";
    }

    fn packed_struct_mixed_state_members(sim) {
        @setup {
            let source = r#"
                module Top(input bit clk, input logic [7:0] raw, input logic [3:0] data,
                           output logic [7:0] whole, signed_field, comb_result,
                           output logic [7:0] ff_result, continuous_result, compound_result);
                    typedef struct packed { logic [3:0] four; bit signed [3:0] two; } mixed_t;
                    mixed_t read_value, comb_value, ff_value, continuous_value, compound_value;
                    assign read_value = raw;
                    assign whole = read_value;
                    assign signed_field = read_value.two;
                    always_comb begin
                        comb_value = 'x;
                        comb_value.two = data;
                    end
                    always_ff @(posedge clk) begin
                        ff_value <= 'x;
                        ff_value.two <= data;
                    end
                    assign continuous_value.four = 4'h0;
                    assign continuous_value.two = data;
                    assign comb_result = comb_value;
                    assign ff_result = ff_value;
                    assign continuous_result = continuous_value;
                    always_comb begin
                        compound_value = 'x;
                        compound_value.two += 4'd1;
                    end
                    assign compound_result = compound_value;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("packed_struct_mixed_state.sv"))], "Top")
            .four_state(true);
        let raw = sim.signal("raw");
        let data = sim.signal("data");
        let clk = sim.event("clk");
        for mask in 0u8..16 {
            for value in 0u8..16 {
                sim.modify(|io| {
                    io.set_four_state(raw, (0xa0 | value).into(), (0xf0 | mask).into());
                    io.set_four_state(data, value.into(), mask.into());
                }).unwrap();
                sim.tick(clk).unwrap();
                let known = value & !mask;
                let signed = ((known << 4) as i8 >> 4).cast_unsigned();
                assert_eq!(sim.get_four_state(sim.signal("whole")), ((0xa0 | value).into(), (0xf0 | mask).into()));
                assert_eq!(sim.get_four_state(sim.signal("signed_field")), (signed.into(), 0u8.into()));
                for name in ["comb_result", "ff_result"] {
                    assert_eq!(sim.get_four_state(sim.signal(name)), ((0xf0 | known).into(), 0xf0u8.into()), "{name}: {value:x}/{mask:x}");
                }
                assert_eq!(sim.get_four_state(sim.signal("continuous_result")), (known.into(), 0u8.into()));
                assert_eq!(sim.get_four_state(sim.signal("compound_result")), (0xf1u8.into(), 0xf0u8.into()));
            }
        }
    }

    fn packed_struct_parameterized_ports(sim) {
        @setup {
            let source = r#"
                module Child #(parameter W = 8)(
                    input struct packed { logic [3:0] tag; logic [W-1:0] data; } request,
                    output struct packed { logic [W-1:0] data; logic [3:0] tag; } response);
                    assign response.data = request.data;
                    assign response.tag = request.tag;
                endmodule
                module Top(input logic [11:0] raw8, input logic [15:0] raw12,
                           output logic [11:0] out8, output logic [15:0] out12);
                    typedef struct packed { logic [3:0] tag; logic [7:0] data; } request_t;
                    request_t request;
                    assign request = raw8;
                    Child child8(.request(request), .response(out8));
                    Child #(.W(12)) child12(.request(raw12), .response(out12));
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("packed_struct_ports.sv"))], "Top");
        let raw8 = sim.signal("raw8");
        let raw12 = sim.signal("raw12");
        for value in [0u16, 0x1234, 0xa8ff, 0xffff] {
            sim.modify(|io| { io.set(raw8, value & 0xfff); io.set(raw12, value); }).unwrap();
            assert_eq!(sim.get(sim.signal("out8")), (((value & 0xff) << 4) | ((value >> 8) & 15)).into());
            assert_eq!(sim.get(sim.signal("out12")), value.rotate_left(4).into());
        }
    }

    fn packed_struct_bounds_and_type_queries(sim) {
        @setup {
            let source = r#"
                module Top(input logic [15:0] raw, output logic [3:0] asc,
                           output logic [1:0] desc, multi, output logic [31:0] widths,
                           output logic [15:0] updated);
                    typedef struct packed { logic [1:4] a; logic [7:4] d; logic [1:0][3:0] m; } original_t;
                    typedef original_t alias_t;
                    alias_t value, changed;
                    assign value = raw;
                    assign asc = value.a;
                    assign desc = value.d[6:5];
                    assign multi = value.m[1][2:1];
                    always_comb begin
                        changed = value;
                        changed.a[2:3] = 2'b01;
                    end
                    localparam BITS = $bits(alias_t) + $bits(value.a) + $bits(value.m) + $size(value.m) + $size(value.m[0]);
                    assign widths = BITS;
                    assign updated = changed;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("packed_struct_bounds.sv"))], "Top");
        let raw = sim.signal("raw");
        for value in [0u16, 0x1234, 0xa8ff, 0xffff] {
            sim.modify(|io| io.set(raw, value)).unwrap();
            assert_eq!(sim.get(sim.signal("asc")), (value >> 12).into());
            assert_eq!(sim.get(sim.signal("desc")), ((value >> 9) & 3).into());
            assert_eq!(sim.get(sim.signal("multi")), ((value >> 5) & 3).into());
            assert_eq!(sim.get(sim.signal("widths")), 34u32.into());
            assert_eq!(sim.get(sim.signal("updated")), ((value & !0x6000) | 0x2000).into());
        }
    }

    fn packed_struct_wide_four_state_layout(sim) {
        @case "packed_structs::packed_struct_wide_four_state_layout";
    }

    fn packed_struct_whole_value_signedness(sim) {
        @setup {
            let source = r#"
                module Top(input logic [7:0] raw, output logic [15:0] signed_whole,
                           unsigned_whole, signed_member, output logic [7:0] known);
                    typedef struct packed signed { logic [3:0] high, low; } signed_t;
                    typedef struct packed { logic signed [3:0] high; logic [3:0] low; } unsigned_t;
                    typedef struct packed { bit [3:0] high, low; } bits_t;
                    signed_t s;
                    unsigned_t u;
                    bits_t b;
                    assign s = raw;
                    assign u = raw;
                    assign b = raw;
                    assign signed_whole = s;
                    assign unsigned_whole = u;
                    assign signed_member = u.high;
                    assign known = b;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("packed_struct_signing.sv"))], "Top")
            .four_state(true);
        let raw = sim.signal("raw");
        for value in 0u8..=255 {
            sim.modify(|io| io.set(raw, value)).unwrap();
            assert_eq!(sim.get(sim.signal("signed_whole")), (value as i8 as i16).cast_unsigned().into());
            assert_eq!(sim.get(sim.signal("unsigned_whole")), value.into());
            assert_eq!(sim.get(sim.signal("signed_member")), ((value as i8 >> 4) as i16).cast_unsigned().into());
        }
        sim.modify(|io| io.set_four_state(raw, 0xffu8.into(), 0xf0u8.into())).unwrap();
        assert_eq!(sim.get_four_state(sim.signal("known")), (15u8.into(), 0u8.into()));
    }

    fn packed_struct_typedef_bounds_survive_generate_shadowing(sim) {
        @case "packed_structs::packed_struct_typedef_bounds_survive_generate_shadowing";
    }
}

sv_backends! {
    fn packed_struct_alias_packed_dimensions(sim) {
        @case "packed_structs::packed_struct_alias_packed_dimensions";
    }
}
