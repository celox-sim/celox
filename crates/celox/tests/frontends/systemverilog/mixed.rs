use super::*;

sv_backends! {
    fn avoids_authored_interface_names_in_probes(sim) {
        @setup {
            let veryl = "module Top(a: input logic<8>, y: output logic<8>) { inst bus: $sv::Bus; assign bus.req = a; assign y = bus.rsp; }";
            let sv = "interface __celox_interface_Bus; logic x; endinterface module __celox_interface_Bus_0; endmodule interface Bus; logic [7:0] req, rsp; assign rsp = req + 1; endinterface";
        }
        @build Simulator::from_mixed_sources(vec![(veryl, Path::new("top.veryl"))], vec![(sv, Path::new("bus.sv"))], "Top");
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 8u8)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 9u8.into());
    }

    fn reserves_authored_interface_names_for_generated_modules(sim) {
        @setup {
            let veryl = "module Child #(param K: u32 = 1)(a: input logic<8>, y: output logic<8>) { inst bus: $sv::Bus; assign bus.req = a; assign y = bus.rsp + K; }";
            let sv = "interface __celox_sv_interface_0; logic x; endinterface interface __celox_foreign_Child_0; logic x; endinterface interface Bus; logic [7:0] req, rsp; assign rsp = req + 1; endinterface module Top(input logic [7:0] a, output logic [7:0] y); Child #(.K(3)) child(a,y); endmodule";
        }
        @build Simulator::from_mixed_sources(vec![(veryl, Path::new("child.veryl"))], vec![(sv, Path::new("top.sv"))], "Top");
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 2u8)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 6u8.into());
    }

    fn preserves_explicitly_namespaced_same_named_children(sim) {
        @setup {
            let veryl = "module Child(a: input logic<8>, y: output logic<8>) { assign y = a + 10; } module Top(a: input logic<8>, y: output logic<8>, native_y: output logic<8>) { inst sv_child: $sv::Child(a,y); inst native_child: Child(a, y: native_y); }";
            let sv = "module Child(input logic [7:0] a, output logic [7:0] y); assign y = a + 1; endmodule";
        }
        @build Simulator::from_mixed_sources(vec![(veryl, Path::new("top.veryl"))], vec![(sv, Path::new("child.sv"))], "Top");
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 4u8)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 5u8.into());
        assert_eq!(sim.get(sim.signal("native_y")), 14u8.into());
    }

    fn preserves_a_veryl_top_with_an_sv_namesake(sim) {
        @setup {
            let veryl = "module Top(a: input logic<8>, y: output logic<8>) { inst child: $sv::Child(a,y); }";
            let sv = "module Top(input logic [7:0] a, output logic [7:0] y); assign y = 0; endmodule module Child(input logic [7:0] a, output logic [7:0] y); assign y = a + 1; endmodule";
        }
        @build Simulator::from_mixed_sources(vec![(veryl, Path::new("top.veryl"))], vec![(sv, Path::new("child.sv"))], "Top");
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 4u8)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 5u8.into());
    }

    fn reports_sv_warnings_of_a_veryl_top(sim) {
        @setup {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("short.hex");
            std::fs::write(&file, "aa").unwrap();
            let veryl = "module Top(y: output logic<8>) { inst child: $sv::Child(y); }";
            let sv = format!(r#"
                module Child(output logic [7:0] y);
                    logic [7:0] mem [0:1];
                    initial $readmemh("{}", mem);
                    assign y = mem[0];
                endmodule
            "#, file.display());
        }
        @build Simulator::from_mixed_sources(vec![(veryl, Path::new("top.veryl"))], vec![(&sv, Path::new("child.sv"))], "Top");
        assert_eq!(sim.get(sim.signal("y")), 0xaau8.into());
        let warnings: Vec<_> = sim.warnings().iter().map(ToString::to_string).collect();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            matches!(
                &sim.warnings()[0],
                celox::CompilationWarning::SystemVerilog(
                    celox::SvFrontendWarning::MemoryFileWordCount { .. }
                )
            ),
            "{warnings:?}"
        );
    }

    fn resolves_veryl_interface_parameter_types_after_prior_overrides(sim) {
        @setup {
            let veryl = "interface Bus #(param W: u32 = 8, param V: logic<W> = 4) { var req: logic<V>; var rsp: logic<V>; modport slave { req: input, rsp: output, } } module Child(bus: modport Bus::slave) { assign bus.rsp = bus.req; }";
            let sv = "module Top(input logic [259:0] a, output logic [259:0] y); Bus #(.V(16'd260), .W(16)) bus(); assign bus.req = a; Child child(.bus(bus)); assign y = bus.rsp; endmodule";
        }
        @build Simulator::from_mixed_sources(vec![(veryl, Path::new("bus.veryl"))], vec![(sv, Path::new("top.sv"))], "Top");
        let a = sim.signal("a");
        let value = (BigUint::from(1u8) << 259usize) + 1u8;
        sim.modify(|io| io.set_wide(a, value.clone())).unwrap();
        assert_eq!(sim.get(sim.signal("y")), value);
    }

    fn preserves_sv_interface_constant_range_selection(sim) {
        @setup {
            let veryl = "module Top(y: output logic<9>, wide: output logic<17>) { inst bus: $sv::Bus #(W: 18); inst high: $sv::Bus #(W: 30); assign bus.req = 9'h1ff; assign y = bus.req; assign high.req = 17'h1ffff; assign wide = high.req; }";
            let sv = "interface Bus #(parameter logic [31:0] W = 8); function automatic integer low_width(input logic [31:0] x); return x[3:0] + 1; endfunction localparam N = low_width(W); logic [N-1:0] req; endinterface";
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))], vec![(sv, Path::new("bus.sv"))], "Top",
        );
        assert_eq!(sim.get(sim.signal("y")), 7u8.into());
        assert_eq!(sim.get(sim.signal("wide")), 0x7fffu32.into());
    }

    fn preserves_parameter_dependent_veryl_interface_constants(sim) {
        @setup {
            let veryl = r#"
                interface Bus #(param W: u32 = 8) {
                    const N: u32 = W * 2;
                    const M: u32 = N + 0;
                    var req: logic<M>; var rsp: logic<M>;
                    modport slave { req: input, rsp: output, }
                }
                module Child(bus: modport Bus::slave) { assign bus.rsp = bus.req + 1; }
            "#;
            let sv = "module Top(input logic [31:0] a, output logic [31:0] y); Bus #(.W(16)) bus(); assign bus.req = a; Child child(.bus(bus)); assign y = bus.rsp; endmodule";
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("bus.veryl"))], vec![(sv, Path::new("top.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x1234_5678u32)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x1234_5679u32.into());
    }

    fn preserves_unsigned_128_bit_parameters_into_sv(sim) {
        @setup {
            let veryl = "module Top(y: output logic<128>) { inst child: $sv::Child #(K: 128'h8000_0000_0000_0000_0000_0000_0000_0001)(y); }";
            let sv = "module Child #(parameter logic [127:0] K = 0)(output logic [127:0] y); assign y = K; endmodule";
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))], vec![(sv, Path::new("child.sv"))], "Top",
        );
        assert_eq!(sim.get(sim.signal("y")), (BigUint::from(1u8) << 127usize) + 1u8);
    }

    fn preserves_unsigned_payload_and_signed_extension_into_veryl(sim) {
        @setup {
            let veryl = "module Child #(param K: logic<256> = 0, param S: signed logic<256> = 0)(y: output logic<256>, negative: output logic<256>) { assign y = K; assign negative = S; }";
            let sv = r#"
                module Top(output logic [255:0] y, negative);
                    localparam logic [127:0] HIGH = 128'h8000_0000_0000_0000_0000_0000_0000_0001;
                    Child #(.K(HIGH), .S(-128'sd1)) child(.y(y), .negative(negative));
                endmodule
            "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("child.veryl"))], vec![(sv, Path::new("top.sv"))], "Top",
        );
        assert_eq!(sim.get(sim.signal("y")), (BigUint::from(1u8) << 127usize) + 1u8);
        assert_eq!(sim.get(sim.signal("negative")), (BigUint::from(1u8) << 256usize) - 1u8);
    }

    fn reserves_interface_wrapper_internal_identifiers(sim) {
        @setup {
            let veryl = r#"
                module Top(a: input logic<8>, y0: output logic<8>, y1: output logic<8>) {
                    inst bus: $sv::Bus [2];
                    assign bus[0].req = a; assign bus[1].req = a + 1;
                    assign y0 = bus[0].h; assign y1 = bus[1].h;
                }
            "#;
            let sv = r#"
                interface Bus;
                    logic [7:0] req, h, h_0, g_2, elements_3;
                    assign h_0 = req + 1;
                    assign g_2 = h_0 + 1;
                    assign elements_3 = g_2 + 1;
                    assign h = elements_3 + 1;
                endinterface
            "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))], vec![(sv, Path::new("bus.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 3u8)).unwrap();
        assert_eq!(sim.get(sim.signal("y0")), 7u8.into());
        assert_eq!(sim.get(sim.signal("y1")), 8u8.into());
    }

    fn preserves_lexical_interface_timescales_in_mixed_behavior(sim) {
        @setup {
            let veryl = "module Top(a: input logic<8>, y: output logic<8>) { inst bus: $sv::Bus; assign bus.req = a; assign y = bus.rsp; }";
            let sv = "interface Bus; timeunit 1ns; timeprecision 1ps; logic [7:0] req, rsp; assign rsp = req + $timeunit(); endinterface";
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))], vec![(sv, Path::new("bus.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 20u8)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 11u8.into());
    }

    fn selects_timescale_roots_for_foreign_request_aliases(sim) {
        @setup {
            let veryl = "module Top(y: output logic<8>) { inst child: $sv::Child #(K: 1)(y); }";
            let sv = r#"
                module Child #(parameter K = 0)(output logic [7:0] y);
                    timeunit 1ns; timeprecision 1ps;
                    assign y = -$timeunit($root.Child) + K;
                endmodule
                module Unused; timeunit 1fs; timeprecision 1fs; endmodule
            "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))], vec![(sv, Path::new("child.sv"))], "Top",
        );
        assert_eq!(sim.get(sim.signal("y")), 10u8.into());
    }

    fn generated_foreign_declarations_preserve_explicit_timescales(sim) {
        @setup {
            let veryl = "module Child(a: input logic<8>, y: output logic<8>) { assign y = a + 1; }";
            let sv = r#"
                module Top(input logic [7:0] a, output logic [7:0] y);
                    timeunit 1ns; timeprecision 1ps;
                    Child child(.a(a), .y(y));
                endmodule
            "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("child.veryl"))], vec![(sv, Path::new("top.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 20u8)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 21u8.into());
    }

    fn preserves_parameter_types_dependent_on_sv_interface_headers(sim) {
        @setup {
            let veryl = "module Top(a: input logic<16>, y: output logic<16>) { inst bus: $sv::Bus #(W: 16, V: 16'h0101, S: 16'sh8001); assign bus.req = a; assign y = bus.result; }";
            let sv = r#"
                interface Bus #(parameter W = 8, parameter logic [W-1:0] V = 1, parameter logic signed [W-1:0] S = -1);
                    logic [V-1:0] req;
                    logic [W-1:0] result;
                    assign result = req ^ S;
                endinterface
            "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))], vec![(sv, Path::new("bus.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0xabcdu16)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x2bccu16.into());
    }

    fn preserves_sv_constant_function_header_dependencies(sim) {
        @setup {
            let veryl = "module Top(a: input logic<32>, y: output logic<32>) { inst bus: $sv::Bus #(W: 16); assign bus.req = a; assign y = bus.rsp; }";
            let sv = r#"
                interface Bus #(parameter W = 8, parameter N = twice());
                    function automatic integer width_value(); return W; endfunction
                    function automatic integer twice(); return width_value() * 2; endfunction
                    localparam M = twice();
                    logic [N-1:0] req;
                    logic [M-1:0] rsp;
                    assign rsp = req + N;
                endinterface
            "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))], vec![(sv, Path::new("bus.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x1234_5678u32)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x1234_5698u32.into());
    }

    fn preserves_sv_named_default_constant_function_header_dependencies(sim) {
        @setup {
            let veryl = "module Top(a: input logic<32>, y: output logic<32>) { inst bus: $sv::Bus #(W: 16); assign bus.req = a; assign y = bus.rsp; }";
            let sv = r#"
                interface Bus #(parameter W = 8, parameter N = twice(.factor()));
                    function automatic integer width_value(); return W; endfunction
                    function automatic integer twice(input integer factor = 2); return width_value() * factor; endfunction
                    localparam M = twice(.factor(2));
                    logic [N-1:0] req;
                    logic [M-1:0] rsp;
                    assign rsp = req + N;
                endinterface
            "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))], vec![(sv, Path::new("bus.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x1234_5678u32)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x1234_5698u32.into());
    }

    fn avoids_foreign_specialization_name_collisions(sim) {
        @setup {
            let veryl = "module Child #(param K: u32 = 1)(a: input logic<8>, y: output logic<8>) { assign y = a + K; }";
            let sv = r#"
                module __celox_foreign_Child_0(input logic [7:0] a, output logic [7:0] y);
                    assign y = a + 42;
                endmodule
                module Top(input logic [7:0] a, output logic [7:0] specialized, authored);
                    Child #(.K(5)) child(.a(a), .y(specialized));
                    __celox_foreign_Child_0 other(.a(a), .y(authored));
                endmodule
            "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("child.veryl"))], vec![(sv, Path::new("top.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 1u8)).unwrap();
        assert_eq!(sim.get(sim.signal("specialized")), 6u8.into());
        assert_eq!(sim.get(sim.signal("authored")), 43u8.into());
    }

    fn avoids_interface_behavior_name_collisions(sim) {
        @setup {
            let veryl = "module Top(a: input logic<8>, y: output logic<8>, authored: output logic<8>) { inst bus: $sv::Bus; assign bus.req = a; assign y = bus.rsp; inst child: $sv::__celox_sv_interface_0(a, y: authored); }";
            let sv = "interface Bus; logic [7:0] req, rsp; assign rsp = req + 1; endinterface module __celox_sv_interface_0(input logic [7:0] a, output logic [7:0] y); assign y = a + 42; endmodule";
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))], vec![(sv, Path::new("bus.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 3u8)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 4u8.into());
        assert_eq!(sim.get(sim.signal("authored")), 45u8.into());
    }

    fn respecializes_systemverilog_children_of_foreign_specializations(sim) {
        @setup {
            let veryl = "module Mid #(param W: u32 = 8) (a: input logic<W>, y: output logic<W>) { inst child: $sv::Leaf #(W: W)(a, y); }";
            let sv = r#"
                module Leaf #(parameter W = 8)(input logic [W-1:0] a, output logic [W-1:0] y);
                    assign y = a + W;
                endmodule
                module Top(input logic [15:0] a, output logic [15:0] y, output logic [7:0] narrow);
                    Mid #(.W(16)) wide(.a(a), .y(y));
                    Mid normal(.a(a[7:0]), .y(narrow));
                endmodule
            "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("mid.veryl"))], vec![(sv, Path::new("top.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x1234u16)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x1244u16.into());
        assert_eq!(sim.get(sim.signal("narrow")), 0x3cu8.into());
    }

    fn attaches_interface_behavior_in_foreign_specializations(sim) {
        @setup {
            let veryl = "module Mid #(param W: u32 = 8) (a: input logic<W>, y: output logic<W>) { inst bus: $sv::Bus #(W: W); assign bus.req = a; assign y = bus.rsp; }";
            let sv = r#"
                interface Bus #(parameter W = 8);
                    logic [W-1:0] req, rsp;
                    assign rsp = req + W;
                endinterface
                module Top(input logic [15:0] a, output logic [15:0] y, output logic [7:0] narrow);
                    Mid #(.W(16)) wide(.a(a), .y(y));
                    Mid normal(.a(a[7:0]), .y(narrow));
                endmodule
            "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("mid.veryl"))], vec![(sv, Path::new("top.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x1234u16)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x1244u16.into());
        assert_eq!(sim.get(sim.signal("narrow")), 0x3cu8.into());
    }

    fn respecializes_generic_interface_children_of_foreign_modules(sim) {
        @setup {
            let veryl = r#"
                interface Bus #(param W: u32 = 8) {
                    var req: logic<W>; var rsp: logic<W>;
                    modport slave { req: input, rsp: output, }
                }
                module Mid #(param W: u32 = 8)(a: input logic<W>, y: output logic<W>) {
                    inst bus: Bus #(W: W);
                    assign bus.req = a;
                    inst child: $sv::Leaf(bus);
                    assign y = bus.rsp;
                }
            "#;
            let sv = r#"
                module Leaf(interface bus); assign bus.rsp = bus.req + bus.W; endmodule
                module Top(input logic [15:0] a, output logic [15:0] y, output logic [7:0] narrow);
                    Mid #(.W(16)) wide(.a(a), .y(y));
                    Mid normal(.a(a[7:0]), .y(narrow));
                endmodule
            "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("mid.veryl"))], vec![(sv, Path::new("top.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x1234u16)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x1244u16.into());
        assert_eq!(sim.get(sim.signal("narrow")), 0x3cu8.into());
    }

    fn prepares_multiple_alternating_parameterized_boundaries(sim) {
        @setup {
            let veryl = r#"
                module Mid #(param W: u32 = 8)(a: input logic<W>, y: output logic<W>) {
                    inst child: $sv::Hop #(W: W)(a, y);
                }
                module Final #(param W: u32 = 8)(a: input logic<W>, y: output logic<W>) {
                    inst child: $sv::Leaf #(W: W)(a, y);
                }
            "#;
            let sv = r#"
                module Hop #(parameter W = 8)(input logic [W-1:0] a, output logic [W-1:0] y);
                    Final #(.W(W)) child(.a(a), .y(y));
                endmodule
                module Leaf #(parameter W = 8)(input logic [W-1:0] a, output logic [W-1:0] y);
                    assign y = a + W;
                endmodule
                module Top(input logic [31:0] a, output logic [31:0] y, output logic [15:0] narrow);
                    Mid #(.W(32)) wide(.a(a), .y(y));
                    Mid #(.W(16)) other(.a(a[15:0]), .y(narrow));
                endmodule
            "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("mid.veryl"))], vec![(sv, Path::new("top.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x123456u32)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x123476u32.into());
        assert_eq!(sim.get(sim.signal("narrow")), 0x3466u16.into());
    }

    fn resolves_sv_only_interface_default_dependencies(sim) {
        @setup {
            let veryl = "module Top(a: input logic<32>, y: output logic<32>) { inst bus: $sv::Bus #(W: 24); assign bus.req = a; assign y = bus.rsp; }";
            let sv = r#"
                package P;
                    parameter WIDTH = 8;
                    function automatic integer increment(); return 1; endfunction
                endpackage
                interface Bus #(parameter W = P::WIDTH, parameter N = W + P::WIDTH, parameter C = P::increment());
                    localparam EXTRA = P::WIDTH;
                    localparam SIZE = N;
                    localparam TOTAL = SIZE;
                    logic [TOTAL-1:0] req, rsp;
                    assign rsp = req + C + EXTRA;
                endinterface
            "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))], vec![(sv, Path::new("bus.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x1234_5678u32)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x1234_5681u32.into());
    }

    fn preserves_signed_and_wide_veryl_parameters_from_systemverilog(sim) {
        @setup {
    let veryl = r#"
        module Child #(param NEG: signed logic<128> = 0, param HIGH: logic<96> = 0)
            (negative: output logic<128>, positive: output logic<96>) {
            assign negative = NEG;
            assign positive = HIGH;
        }
    "#;
    let sv = r#"
        module Top(output logic [127:0] negative, output logic [95:0] positive);
            Child #(.NEG(-128'sd1), .HIGH(96'h1_0000_0000_0000_0001)) child(.negative(negative), .positive(positive));
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("child.veryl"))], vec![(sv, Path::new("top.sv"))], "Top",
        );
        assert_eq!(sim.get(sim.signal("negative")), (BigUint::from(1u8) << 128usize) - 1u8);
        assert_eq!(sim.get(sim.signal("positive")), (BigUint::from(1u8) << 64usize) + 1u8);
    }

    fn avoids_specialization_alias_collisions_with_source_modules(sim) {
        @setup {
    let veryl = r#"
        module Top(a: input logic<8>, specialized: output logic<8>, authored: output logic<8>) {
            inst child: $sv::Child #(K: 5) (a, y: specialized);
            inst other: $sv::__celox_sv_instance_0 (a, y: authored);
        }
    "#;
    let sv = r#"
        module Child #(parameter K = 1)(input logic [7:0] a, output logic [7:0] y);
            assign y = a + K;
        endmodule
        module __celox_sv_instance_0(input logic [7:0] a, output logic [7:0] y);
            assign y = a + 42;
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))], vec![(sv, Path::new("child.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 1u8)).unwrap();
        assert_eq!(sim.get(sim.signal("specialized")), 6u8.into());
        assert_eq!(sim.get(sim.signal("authored")), 43u8.into());
    }

    fn preserves_dollar_signs_in_ordinary_systemverilog_ports(sim) {
        @setup {
    let veryl = "module Top(a: input logic<8>, y: output logic<8>) { inst child: $sv::Child (a, y); }";
    let sv = "module Child(input logic [7:0] data$in, output logic [7:0] y); assign y = data$in ^ 8'h55; endmodule";
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))], vec![(sv, Path::new("child.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x33u8)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x66u8.into());
    }

    fn preserves_dependent_veryl_module_parameter_defaults(sim) {
        @setup {
    let veryl = r#"
        package Defaults { function increment() -> u32 { return 16; } }
        module Child #(param W: u32 = 8, param N: u32 = W * 2, param C: u32 = Defaults::increment())
            (a: input logic<N>, y: output logic<N>) { assign y = a + C; }
    "#;
    let sv = "module Top(input logic [31:0] a, output logic [31:0] y); Child #(.W(16)) child(.a(a), .y(y)); endmodule";
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("child.veryl"))], vec![(sv, Path::new("top.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x1234_5678u32)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x1234_5688u32.into());
    }

    fn preserves_dependent_veryl_interface_parameter_defaults(sim) {
        @setup {
    let veryl = r#"
        interface Bus #(param W: u32 = 8, param N: u32 = W * 2) {
            var req: logic<N>; var rsp: logic<N>;
            modport slave { req: input, rsp: output, }
        }
        module Child(bus: modport Bus::slave) { assign bus.rsp = bus.req + 1; }
    "#;
    let sv = r#"
        module Top(input logic [31:0] a, output logic [31:0] y);
            Bus #(.W(16)) bus();
            assign bus.req = a;
            Child child(.bus(bus));
            assign y = bus.rsp;
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("child.veryl"))], vec![(sv, Path::new("top.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x1234_5678u32)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x1234_5679u32.into());
    }

    fn preserves_interface_function_writes_in_systemverilog_behavior(sim) {
        @setup {
    let veryl = "module Top (a: input logic<8>, y: output logic<8>) { inst bus: $sv::Bus; assign bus.req = a; assign y = bus.rsp; }";
    let sv = r#"
        interface Bus;
            logic [7:0] req, rsp;
            function automatic void update(input logic [7:0] value);
                rsp = value + 8'd1;
            endfunction
            function automatic void dispatch(input logic [7:0] value);
                update(value);
            endfunction
            always_comb dispatch(req);
        endinterface
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))],
            vec![(sv, Path::new("bus.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 41u8)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 42u8.into());
        sim.modify(|io| io.set(a, 127u8)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 128u8.into());
    }

    fn preserves_wide_signed_systemverilog_interface_parameters(sim) {
        @setup {
    let veryl = "module Top (y: output logic<64>) { inst bus: $sv::Bus #(V: 64'sh1_0000_0001); assign y = bus.value; }";
    let sv = "interface Bus #(parameter longint signed V = -1); logic [63:0] value; assign value = V; endinterface";
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))],
            vec![(sv, Path::new("bus.sv"))], "Top",
        );
        assert_eq!(sim.get(sim.signal("y")), 0x1_0000_0001u64.into());
    }

    fn overrides_parameters_of_a_systemverilog_top_in_a_mixed_design(sim) {
        @setup {
    let veryl = "module Child (a: input logic<16>, y: output logic<16>) { assign y = a + 1; }";
    let sv = "module Top #(parameter W = 8)(input logic [W-1:0] a, output logic [W-1:0] y); Child child(.a(a), .y(y)); endmodule";
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("child.veryl"))],
            vec![(sv, Path::new("top.sv"))], "Top",
        ).param("W", 16);
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x1234u16)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x1235u16.into());
    }

    fn specializes_systemverilog_children_from_veryl_parameters(sim) {
        @setup {
    let veryl = r#"
        module Top (a: input logic<16>, y: output logic<16>) {
            inst child: $sv::Child #(W: 16) (a, y);
        }
    "#;
    let sv = r#"
        module Child #(parameter W = 8)(input logic [W-1:0] a, output logic [W-1:0] y);
            assign y = a + 1;
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))],
            vec![(sv, Path::new("child.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x1234u16)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x1235u16.into());
    }

    fn binds_a_veryl_interface_to_a_generic_systemverilog_port(sim) {
        @setup {
    let veryl = r#"
        interface Bus #(param W: u32 = 8) {
            var req: logic<W>;
            var rsp: logic<W>;
            modport slave { req: input, rsp: output, }
        }
        module Top (a: input logic<16>, y: output logic<16>) {
            inst bus: Bus #(W: 16);
            assign bus.req = a;
            inst child: $sv::Child (bus);
            assign y = bus.rsp;
        }
    "#;
    let sv = "module Child(interface bus); assign bus.rsp = bus.req + 1; endmodule";
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("bus.veryl"))],
            vec![(sv, Path::new("child.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x1234u16)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x1235u16.into());
    }

    fn forwards_systemverilog_interface_parameters_from_veryl(sim) {
        @setup {
    let veryl = r#"
        module Top (a: input logic<16>, y: output logic<16>) {
            inst bus: $sv::Bus #(W: 16);
            assign bus.req = a;
            inst child: $sv::Child (bus);
            assign y = bus.rsp;
        }
    "#;
    let sv = r#"
        interface Bus #(parameter W = 8);
            logic [W-1:0] req, rsp;
            modport slave(input req, output rsp);
        endinterface
        module Child(Bus.slave bus); assign bus.rsp = bus.req + 1; endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))],
            vec![(sv, Path::new("child.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x1234u16)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x1235u16.into());
    }

    fn specializes_veryl_children_from_systemverilog_parameters(sim) {
        @setup {
    let veryl = r#"
        module Child #(param W: u32 = 8) (a: input logic<W>, y: output logic<W>) {
            assign y = a + 1;
        }
    "#;
    let sv = r#"
        module Top(input logic [15:0] a, output logic [15:0] y);
            Child #(.W(16)) child(.a(a), .y(y));
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("child.veryl"))],
            vec![(sv, Path::new("top.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x1234u16)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x1235u16.into());
    }

    fn specializes_veryl_modports_to_the_connected_systemverilog_interface(sim) {
        @setup {
    let veryl = r#"
        module Child (bus: modport $sv::Bus::slave) {
            assign bus.rsp = bus.req + 1;
        }
    "#;
    let sv = r#"
        interface Bus #(parameter W = 8);
            logic [W-1:0] req, rsp;
            modport slave(input req, output rsp);
        endinterface
        module Top(input logic [15:0] a, output logic [15:0] y);
            Bus #(.W(16)) bus();
            assign bus.req = a;
            Child child(.bus(bus));
            assign y = bus.rsp;
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("child.veryl"))],
            vec![(sv, Path::new("top.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x1234u16)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x1235u16.into());
    }

    fn runs_parameterized_systemverilog_interface_arrays_instantiated_in_veryl(sim) {
        @setup {
    let veryl = r#"
        module Top (a: input logic<16>, y0: output logic<16>, y1: output logic<16>) {
            inst bus: $sv::Bus [2] #(W: 16);
            assign bus[0].req = a;
            assign bus[1].req = a + 1;
            assign y0 = bus[0].rsp;
            assign y1 = bus[1].rsp;
        }
    "#;
    let sv = r#"
        interface Bus #(parameter W = 8);
            logic [W-1:0] req, rsp;
            assign rsp = req + 1;
        endinterface
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))],
            vec![(sv, Path::new("bus.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x1234u16)).unwrap();
        assert_eq!(sim.get(sim.signal("y0")), 0x1235u16.into());
        assert_eq!(sim.get(sim.signal("y1")), 0x1236u16.into());
    }

    fn runs_systemverilog_interface_logic_instantiated_in_veryl(sim) {
        @setup {
    let veryl = r#"
        module Top (a: input logic<8>, y: output logic<8>) {
            inst bus: $sv::Bus;
            assign bus.req = a;
            assign y = bus.rsp;
        }
    "#;
    let sv = r#"
        interface Bus;
            logic [7:0] req, rsp;
            assign rsp = req + 8'd1;
        endinterface
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))],
            vec![(sv, Path::new("bus.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 41u8)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 42u8.into());
    }

    fn connects_a_veryl_interface_from_systemverilog(sim) {
        @setup {
    let veryl = r#"
        interface Bus {
            var req: logic<8>;
            var rsp: logic<8>;
            modport slave { req: input, rsp: output, }
        }
        module Child (bus: modport Bus::slave) {
            assign bus.rsp = bus.req + 8'd1;
        }
    "#;
    let sv = r#"
        module Top(input logic [7:0] a, output logic [7:0] y);
            Bus bus();
            assign bus.req = a;
            Child child(.bus(bus));
            assign y = bus.rsp;
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("bus.veryl"))],
            vec![(sv, Path::new("top.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 41u8)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 42u8.into());
    }

    fn passes_a_veryl_interface_to_a_systemverilog_module(sim) {
        @setup {
    let veryl = r#"
        interface Bus {
            var req: logic<8>;
            var rsp: logic<8>;
            modport slave { req: input, rsp: output, }
        }
        module Top (a: input logic<8>, y: output logic<8>) {
            inst bus: Bus;
            assign bus.req = a;
            inst slave: $sv::Slave (bus);
            assign y = bus.rsp;
        }
    "#;
    let sv = "module Slave(Bus.slave bus); assign bus.rsp = bus.req + 8'd1; endmodule";
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("bus.veryl"))],
            vec![(sv, Path::new("slave.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 41u8)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 42u8.into());
    }

    fn passes_a_systemverilog_interface_to_a_veryl_module(sim) {
        @setup {
    let veryl = r#"
        module Child (bus: modport $sv::Bus::slave) {
            assign bus.rsp = bus.req + 8'd1;
        }
    "#;
    let sv = r#"
        interface Bus;
            logic [7:0] req, rsp;
            modport slave(input req, output rsp);
        endinterface
        module Top(input logic [7:0] a, output logic [7:0] y);
            Bus bus();
            assign bus.req = a;
            Child child(.bus(bus));
            assign y = bus.rsp;
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("child.veryl"))],
            vec![(sv, Path::new("top.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 41u8)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 42u8.into());
    }

    fn connects_a_systemverilog_interface_from_veryl(sim) {
        @setup {
    let veryl = r#"
        module Top (a: input logic<8>, y: output logic<8>) {
            inst bus: $sv::Bus;
            assign bus.req = a;
            inst slave: $sv::Slave (bus);
            assign y = bus.rsp;
        }
    "#;
    let sv = r#"
        interface Bus;
            logic [7:0] req, rsp;
            modport slave(input req, output rsp);
        endinterface
        module Slave(Bus.slave bus);
            assign bus.rsp = bus.req + 8'd1;
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))],
            vec![(sv, Path::new("bus.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 41u8)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 42u8.into());
    }

    fn simulates_systemverilog_top_with_veryl_child_and_systemverilog_grandchild(sim) {
        @setup {
    let veryl = r#"
        module VerylChild (a: input logic<8>, y: output logic<8>) {
            var intermediate: logic<8>;
            assign intermediate = a + 8'd3;
            inst leaf: $sv::SvLeaf (a: intermediate, y);
        }
    "#;
    let sv = r#"
        module Top(input logic [7:0] a, output logic [7:0] y);
            VerylChild child(.y(y), .a(a));
        endmodule
        module SvLeaf(input logic [7:0] a, output logic [7:0] y);
            assign y = a ^ 8'h55;
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("child.veryl"))],
            vec![(sv, Path::new("top.sv"))], "Top",
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0x20u8)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x76u8.into());
    }

    fn preserves_positional_veryl_to_systemverilog_port_order(sim) {
        @setup {
    let veryl = r#"
        module Top (
            a: input logic,
            b: input logic,
            y: output logic<2>,
        ) {
            inst child: $sv::PositionalPorts (b, a, y);
        }
    "#;
    let sv = r#"
        module PositionalPorts(input logic a, input logic b, output logic [1:0] y);
            assign y = {a, b};
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("positional_ports.veryl"))],
            vec![(sv, Path::new("positional_ports.sv"))],
            "Top",
        );

    let a = sim.signal("a");
    let b = sim.signal("b");
    sim.modify(|io| {
        io.set(a, 1u8);
        io.set(b, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
    }

    fn preserves_out_of_order_named_veryl_to_systemverilog_ports(sim) {
        @setup {
    let veryl = r#"
        module Top (
            xa: input logic,
            xb: input logic,
            y: output logic<2>,
        ) {
            inst child: $sv::NamedPorts (
                b: xb,
                a: xa,
                y,
            );
        }
    "#;
    let sv = r#"
        module NamedPorts(input logic a, input logic b, output logic [1:0] y);
            assign y = {a, b};
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("named_ports.veryl"))],
            vec![(sv, Path::new("named_ports.sv"))],
            "Top",
        );

    let xa = sim.signal("xa");
    let xb = sim.signal("xb");
    sim.modify(|io| {
        io.set(xa, 1u8);
        io.set(xb, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 2u8.into());
    }

    fn ignores_comments_when_parsing_veryl_to_systemverilog_ports(sim) {
        @setup {
    let veryl = r#"
        module Top (
            a: input logic,
            b: input logic,
            y: output logic<2>,
        ) {
            inst child: $sv::CommentedPorts (
                a, // forwarded; comma, and terminator text must be ignored
                b,
                y,
            );
        }
    "#;
    let sv = r#"
        module CommentedPorts(input logic a, input logic b, output logic [1:0] y);
            assign y = {a, b};
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("commented_ports.veryl"))],
            vec![(sv, Path::new("commented_ports.sv"))],
            "Top",
        );

    let a = sim.signal("a");
    let b = sim.signal("b");
    sim.modify(|io| {
        io.set(a, 1u8);
        io.set(b, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 2u8.into());
    }

    fn converts_veryl_logic_to_systemverilog_bit_input(sim) {
        @setup {
    let veryl = r#"
        module Top (
            a: input logic,
            y: output logic,
        ) {
            inst child: $sv::BitInput (a, y);
        }
    "#;
    let sv = r#"
        module BitInput(input bit a, output logic y); assign y = a; endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))],
            vec![(sv, Path::new("bit_input.sv"))],
            "Top",
        ).four_state(true);

    let a = sim.signal("a");
    sim.modify(|io| {
        io.set_four_state(a, BigUint::from(1u8), BigUint::from(1u8));
    }).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0u8.into());
    }

    fn simulates_veryl_top_with_systemverilog_child(sim) {
        @setup {
    let veryl = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            y: output logic<8>,
        ) {
            inst u_xor: $sv::Xor8 (
                a,
                b,
                y,
            );
        }
    "#;
    let sv = r#"
        module Xor8(input logic [7:0] a, input logic [7:0] b, output logic [7:0] y);
            always_comb begin
                y = a ^ b;
            end
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
        vec![(veryl, Path::new("top.veryl"))],
        vec![(sv, Path::new("xor8.sv"))],
        "Top",
    );

    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");

    sim.modify(|io| {
        io.set(a, 0xf0u8);
        io.set(b, 0x33u8);
    })
    .unwrap();

    assert_eq!(sim.get(y), 0xc3u8.into());
    }

    fn simulates_veryl_top_with_nested_systemverilog_hierarchy(sim) {
        @setup {
    let veryl = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            y: output logic<8>,
        ) {
            inst u_wrapper: $sv::Wrapper (
                a,
                b,
                y,
            );
        }
    "#;
    let sv = r#"
        module Xor8(input logic [7:0] a, input logic [7:0] b, output logic [7:0] y);
            assign y = a ^ b;
        endmodule

        module Wrapper(input logic [7:0] a, input logic [7:0] b, output logic [7:0] y);
            Xor8 u_xor(.a(a), .b(b), .y(y));
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
        vec![(veryl, Path::new("top.veryl"))],
        vec![(sv, Path::new("nested.sv"))],
        "Top",
    );

    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");

    sim.modify(|io| {
        io.set(a, 0xa5u8);
        io.set(b, 0x3cu8);
    })
    .unwrap();

    assert_eq!(sim.get(y), 0x99u8.into());
    }

    fn zero_extends_signed_veryl_selections_into_systemverilog_inputs(sim) {
        @setup {
    let veryl = r#"
        module Top (
            a: input signed logic<8>,
            part_y: output logic<16>,
            bit_y: output logic<16>,
        ) {
            inst child: $sv::SelectionInputs (
                part_value: a[7:0],
                bit_value: a[7],
                part_y,
                bit_y,
            );
        }
    "#;
    let sv = r#"
        module SelectionInputs(
            input logic [15:0] part_value,
            input logic [15:0] bit_value,
            output logic [15:0] part_y,
            output logic [15:0] bit_y
        );
            assign part_y = part_value;
            assign bit_y = bit_value;
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("signed_selection.veryl"))],
            vec![(sv, Path::new("selection_inputs.sv"))],
            "Top",
        );

    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 0x80u8)).unwrap();
    assert_eq!(sim.get(sim.signal("part_y")), 0x0080u16.into());
    assert_eq!(sim.get(sim.signal("bit_y")), 1u16.into());
    }
}

#[test]
fn bounds_recursive_alternating_parameter_specializations() {
    let veryl = "module Mid #(param W: u32 = 0) { inst child: $sv::Hop #(W: W + 1); }";
    let sv = "module Hop #(parameter W = 0); Mid #(.W(W)) child(); endmodule module Top; Mid #(.W(1)) child(); endmodule";
    let error = Simulator::from_mixed_sources(
        vec![(veryl, Path::new("mid.veryl"))],
        vec![(sv, Path::new("top.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap_err();
    assert!(
        error.to_string().contains("specialization limit")
            || error.to_string().contains("too many specializations"),
        "{error}"
    );
}

#[test]
fn rejects_missing_children_of_a_systemverilog_top_in_a_mixed_design() {
    let veryl = "module Unused {}";
    let sv = "module Top(output logic y); Missing child(.y(y)); endmodule";
    let error = Simulator::from_mixed_sources(
        vec![(veryl, Path::new("unused.veryl"))],
        vec![(sv, Path::new("top.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap_err();
    assert!(error.to_string().contains("Missing"), "{error}");
}

#[test]
fn rejects_cycles_that_cross_language_boundaries() {
    let veryl = "module Child { inst parent: $sv::Top; }";
    let sv = "module Top; Child child(); endmodule";
    let error = Simulator::from_mixed_sources(
        vec![(veryl, Path::new("child.veryl"))],
        vec![(sv, Path::new("top.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap_err();
    assert!(
        error.to_string().contains("recursive mixed-language"),
        "{error}"
    );
}

// These assert the intended simulation results, so implementing the missing
// bridge behavior only requires removing the ignore attributes.
mod pending_interface_functions {
    use super::*;

    sv_backends! {
        #[ignore = "SV interface function bodies are not imported into the Veryl frontend"]
        fn calls_systemverilog_interface_function_from_veryl(sim) {
            @setup {
                let veryl = r#"
                    module Top (b: input logic<8>, k: input logic<8>, y: output logic<8>) {
                        inst bus: $sv::Bus;
                        assign bus.bias = b;
                        assign y = bus.transform(k);
                    }
                "#;
                let sv = r#"
                    interface Bus;
                        logic [7:0] bias;
                        function automatic logic [7:0] transform(input logic [7:0] value);
                            return bias + value;
                        endfunction
                    endinterface
                "#;
            }
            @build Simulator::from_mixed_sources(
                vec![(veryl, Path::new("top.veryl"))],
                vec![(sv, Path::new("bus.sv"))], "Top",
            );
            let b = sim.signal("b");
            let k = sim.signal("k");
            let y = sim.signal("y");
            for (bias, value, expected) in [(0u8, 0u8, 0u8), (16, 5, 21), (255, 2, 1), (3, 7, 10)] {
                sim.modify(|io| { io.set(b, bias); io.set(k, value); }).unwrap();
                assert_eq!(sim.get(y), expected.into());
            }
        }

        #[ignore = "Veryl interface function bodies are not exported to the SV frontend"]
        fn calls_veryl_interface_function_from_systemverilog(sim) {
            @setup {
                let veryl = r#"
                    interface Bus {
                        var bias: logic<8>;
                        function transform (value: input logic<8>) -> logic<8> {
                            return bias + value;
                        }
                    }
                "#;
                let sv = r#"
                    module Top(input logic [7:0] b, k, output logic [7:0] y);
                        Bus bus();
                        assign bus.bias = b;
                        assign y = bus.transform(k);
                    endmodule
                "#;
            }
            @build Simulator::from_mixed_sources(
                vec![(veryl, Path::new("bus.veryl"))],
                vec![(sv, Path::new("top.sv"))], "Top",
            );
            let b = sim.signal("b");
            let k = sim.signal("k");
            let y = sim.signal("y");
            for (bias, value, expected) in [(0u8, 0u8, 0u8), (16, 5, 21), (255, 2, 1), (3, 7, 10)] {
                sim.modify(|io| { io.set(b, bias); io.set(k, value); }).unwrap();
                assert_eq!(sim.get(y), expected.into());
            }
        }

        #[ignore = "SV modport function imports and captured members are not bridged to Veryl"]
        fn imports_systemverilog_interface_function_into_veryl_modport(sim) {
            @setup {
                let veryl = r#"
                    module Child (bus: modport $sv::Bus::reader, k: input logic<8>, y: output logic<8>) {
                        assign y = bus.transform(k);
                    }
                "#;
                let sv = r#"
                    interface Bus;
                        logic [7:0] bias;
                        function automatic logic [7:0] transform(input logic [7:0] value);
                            return bias + value;
                        endfunction
                        modport reader(import transform);
                    endinterface
                    module Top(input logic [7:0] b, k, output logic [7:0] y);
                        Bus bus();
                        assign bus.bias = b;
                        Child child(.bus(bus), .k(k), .y(y));
                    endmodule
                "#;
            }
            @build Simulator::from_mixed_sources(
                vec![(veryl, Path::new("child.veryl"))],
                vec![(sv, Path::new("top.sv"))], "Top",
            );
            let b = sim.signal("b");
            let k = sim.signal("k");
            let y = sim.signal("y");
            for (bias, value, expected) in [(0u8, 0u8, 0u8), (16, 5, 21), (255, 2, 1), (3, 7, 10)] {
                sim.modify(|io| { io.set(b, bias); io.set(k, value); }).unwrap();
                assert_eq!(sim.get(y), expected.into());
            }
        }

        #[ignore = "Veryl modport function imports and captured members are not bridged to SV"]
        fn imports_veryl_interface_function_into_systemverilog_modport(sim) {
            @setup {
                let veryl = r#"
                    interface Bus {
                        var bias: logic<8>;
                        function transform (value: input logic<8>) -> logic<8> {
                            return bias + value;
                        }
                        modport reader { transform: import, }
                    }
                    module Top (b: input logic<8>, k: input logic<8>, y: output logic<8>) {
                        inst bus: Bus;
                        assign bus.bias = b;
                        inst child: $sv::Child (bus, k, y);
                    }
                "#;
                let sv = r#"
                    module Child(Bus.reader bus, input logic [7:0] k, output logic [7:0] y);
                        assign y = bus.transform(k);
                    endmodule
                "#;
            }
            @build Simulator::from_mixed_sources(
                vec![(veryl, Path::new("top.veryl"))],
                vec![(sv, Path::new("child.sv"))], "Top",
            );
            let b = sim.signal("b");
            let k = sim.signal("k");
            let y = sim.signal("y");
            for (bias, value, expected) in [(0u8, 0u8, 0u8), (16, 5, 21), (255, 2, 1), (3, 7, 10)] {
                sim.modify(|io| { io.set(b, bias); io.set(k, value); }).unwrap();
                assert_eq!(sim.get(y), expected.into());
            }
        }

        #[ignore = "SV modport exports and calls from a Veryl importer are not implemented"]
        fn calls_systemverilog_exported_function_from_veryl_modport(sim) {
            @setup {
                let veryl = r#"
                    module Child (bus: modport $sv::Bus::client, k: input logic<8>, y: output logic<8>) {
                        assign y = bus.transform(k);
                    }
                "#;
                // IEEE 1800-2023 25.7: a function supplied by another module
                // needs a full prototype on the importing modport.
                let sv = r#"
                    interface Bus;
                        logic [7:0] bias;
                        modport server(input bias, export transform);
                        modport client(import function logic [7:0] transform(input logic [7:0] value));
                    endinterface
                    module Provider(Bus.server bus);
                        function automatic logic [7:0] bus.transform(input logic [7:0] value);
                            return bus.bias + value;
                        endfunction
                    endmodule
                    module Top(input logic [7:0] b, k, output logic [7:0] y);
                        Bus bus();
                        assign bus.bias = b;
                        Provider provider(.bus(bus));
                        Child child(.bus(bus), .k(k), .y(y));
                    endmodule
                "#;
            }
            @build Simulator::from_mixed_sources(
                vec![(veryl, Path::new("child.veryl"))],
                vec![(sv, Path::new("top.sv"))], "Top",
            );
            let b = sim.signal("b");
            let k = sim.signal("k");
            let y = sim.signal("y");
            for (bias, value, expected) in [(0u8, 0u8, 0u8), (16, 5, 21), (255, 2, 1), (3, 7, 10)] {
                sim.modify(|io| { io.set(b, bias); io.set(k, value); }).unwrap();
                assert_eq!(sim.get(y), expected.into());
            }
        }
    }
}
