use super::*;

// IEEE 1800-2023 11.5.1: the index operator and declaration direction
// independently determine the endpoints of the selected range.
sv_backends! {
    fn indexed_select_reads_and_writes_both_declaration_directions(sim) {
        @setup {
            let source = r#"
                module Top(input logic [15:0] data, input logic [3:0] replacement,
                           output logic [3:0] down_plus, down_minus, up_plus, up_minus,
                           output logic [15:0] down_written, output logic [0:15] up_written);
                    logic [0:15] up;
                    assign up = data;
                    assign down_plus = data[4 +: 4];
                    assign down_minus = data[7 -: 4];
                    assign up_plus = up[4 +: 4];
                    assign up_minus = up[7 -: 4];
                    always_comb begin
                        down_written = data;
                        down_written[4 +: 4] = replacement;
                        down_written[15 -: 4] = replacement;
                        up_written = data;
                        up_written[4 +: 4] = replacement;
                        up_written[15 -: 4] = replacement;
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_directions.sv"))], "Top");
        let data = sim.signal("data");
        let replacement = sim.signal("replacement");
        sim.modify(|io| { io.set(data, 0x1234u16); io.set(replacement, 0xau8); }).unwrap();
        assert_eq!(sim.get(sim.signal("down_plus")), 3u8.into());
        assert_eq!(sim.get(sim.signal("down_minus")), 3u8.into());
        assert_eq!(sim.get(sim.signal("up_plus")), 2u8.into());
        assert_eq!(sim.get(sim.signal("up_minus")), 2u8.into());
        assert_eq!(sim.get(sim.signal("down_written")), 0xa2a4u16.into());
        assert_eq!(sim.get(sim.signal("up_written")), 0x1a3au16.into());
    }

    fn indexed_select_parameterized_generate_and_ff(sim) {
        @setup {
            let source = r#"
                module Top #(parameter int WIDTH = 4)(input logic clk,
                    input logic [4*WIDTH-1:0] data, output logic [4*WIDTH-1:0] comb, q,
                    output logic [3:0] selected_size);
                    for (genvar i = 0; i < 4; i++) begin : lanes
                        assign comb[i*WIDTH +: WIDTH] = data[(3-i)*WIDTH +: WIDTH];
                        always_ff @(posedge clk) q[i*WIDTH +: WIDTH] <= data[i*WIDTH +: WIDTH];
                    end
                    localparam SELECTED_SIZE = $size(data[WIDTH +: WIDTH]);
                    assign selected_size = SELECTED_SIZE;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_generate.sv"))], "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0x1234u16)).unwrap();
        assert_eq!(sim.get(sim.signal("comb")), 0x4321u16.into());
        assert_eq!(sim.get(sim.signal("selected_size")), 4u8.into());
        sim.tick(sim.event("clk")).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 0x1234u16.into());
        sim.modify(|io| io.set(data, 0xabcd_u16)).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 0x1234u16.into());
        sim.tick(sim.event("clk")).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 0xabcdu16.into());
    }

    fn veryl_emitted_parameterized_step_generate(sim) {
        @setup {
            let veryl = r#"
                module Child #(param WIDTH: u32 = 4) (
                    data: input logic<WIDTH * 4>, y: output logic<WIDTH * 4>,
                ) {
                    for i in 0..4: lanes {
                        assign y[i step WIDTH] = data[(3-i) step WIDTH];
                    }
                }
                module Top (data: input logic<16>, wide: output logic<16>, narrow: output logic<8>) {
                    inst wide_child: Child #(WIDTH: 4) (data, y: wide);
                    inst narrow_child: Child #(WIDTH: 2) (data: data[7:0], y: narrow);
                }
            "#;
            let emitted = celox_test_suite_veryl::emit::emit_veryl_sources(&[(veryl, Path::new("step_generate.veryl"))]);
            assert!(emitted.as_sv_sources().iter().any(|(source, _)| source.contains("+:")));
        }
        @build Simulator::from_sv_sources(emitted.as_sv_sources(), "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0x12e4u16)).unwrap();
        assert_eq!(sim.get(sim.signal("wide")), 0x4e21u16.into());
        assert_eq!(sim.get(sim.signal("narrow")), 0x1bu8.into());
    }

    fn indexed_select_multidimensional_packed_reads(sim) {
        @setup {
            let source = r#"
                module Top(input logic [1:0][0:7] data, output logic [3:0] plus, minus,
                           output logic [7:0] concat_slice);
                    assign plus = data[1][2 +: 4];
                    assign minus = data[0][5 -: 4];
                    assign concat_slice = {data[1], data[0]}[4 +: 8];
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_packed.sv"))], "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0xa35cu16)).unwrap();
        assert_eq!(sim.get(sim.signal("plus")), 8u8.into());
        assert_eq!(sim.get(sim.signal("minus")), 7u8.into());
        assert_eq!(sim.get(sim.signal("concat_slice")), 0x35u8.into());
    }
}

#[test]
fn rejects_nonpositive_and_runtime_indexed_widths() {
    for width in ["0", "-1", "width"] {
        let source = format!(
            "module Top(input logic [7:0] data, input int width, output logic [7:0] y); assign y = data[0 +: {width}]; endmodule"
        );
        let error =
            Simulator::from_sv_sources(vec![(&source, Path::new("invalid_width.sv"))], "Top")
                .build_cranelift()
                .err()
                .expect("invalid indexed width must be rejected")
                .to_string();
        assert!(error.contains("indexed part-select"), "{error}");
    }
}
