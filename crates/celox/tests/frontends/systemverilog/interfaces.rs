use super::*;

// Verilator 5.052 faults internally on this generic interface relay; keep the
// generic binding regression in the Celox harness and the concrete form shared.
sv_backends! {
    fn binds_implicit_named_generic_interface_ports(sim) {
        @setup {
            let source = r#"
    interface Bus;
      logic [7:0] data;
      modport writer(output data);
      modport reader(input data);
    endinterface
    module Writer(Bus.writer iface, input logic [7:0] x);
      assign iface.data = x;
    endmodule
    module Reader(interface iface, output logic [7:0] y);
      assign y = iface.data;
    endmodule
    module Relay(Bus iface, output logic [7:0] y);
      Reader r(.iface, .y);
    endmodule
    module Top(input logic [7:0] x, output logic [7:0] y);
      Bus iface();
      Writer w(.iface, .x);
      Relay r(.iface, .y);
    endmodule
  "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("implicit_generic_interface.sv"))], "Top");
        let x = sim.signal("x");
        let y = sim.signal("y");
        for value in [0u8, 0x41, 0xff] {
            sim.modify(|io| io.set(x, value)).unwrap();
            assert_eq!(u64::try_from(sim.get(y)).unwrap(), u64::from(value));
        }
    }
}

// `.*` connects each port no connection names as `.name` does (IEEE
// 1800-2023 23.3.2.4), on a module with interface ports as on any other:
// beside interface ports connected by name, and the interface ports
// themselves.
sv_backends! {
    fn wildcard_connections_beside_named_interface_ports(sim) {
        @setup {
            let source = r#"
    interface Bus;
      logic [7:0] wdata, rdata;
      modport src(output wdata, input rdata);
    endinterface
    module Leaf(input logic [7:0] din, output logic [7:0] dout, Bus.src port);
      assign port.wdata = din;
      assign dout = port.rdata;
    endmodule
    module Top(input logic [7:0] din, output logic [7:0] dout);
      Bus bus();
      assign bus.rdata = bus.wdata + 8'd1;
      Leaf leaf(.port(bus), .*);
    endmodule
  "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("wildcard_beside_interface.sv"))], "Top");
        let din = sim.signal("din");
        let dout = sim.signal("dout");
        for value in [0u8, 0x21, 0xff] {
            sim.modify(|io| io.set(din, value)).unwrap();
            assert_eq!(u64::try_from(sim.get(dout)).unwrap(), u64::from(value.wrapping_add(1)));
        }
    }
    fn wildcard_connections_bind_interface_ports(sim) {
        @setup {
            let source = r#"
    interface Bus;
      logic [7:0] data;
      modport writer(output data);
      modport reader(input data);
    endinterface
    module Writer(Bus.writer bus, input logic [7:0] x);
      assign bus.data = x;
    endmodule
    module Reader(Bus.reader bus, output logic [7:0] y, output logic [7:0] z);
      assign y = bus.data;
      assign z = ~bus.data;
    endmodule
    module Top(input logic [7:0] x, output logic [7:0] y, output logic [7:0] inverted);
      Bus bus();
      Writer w(.*);
      if (1) begin : inner
        Reader r(.z(inverted), .*);
      end
    endmodule
  "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("wildcard_interface.sv"))], "Top");
        let x = sim.signal("x");
        let y = sim.signal("y");
        let inverted = sim.signal("inverted");
        for value in [0u8, 0x41, 0xff] {
            sim.modify(|io| io.set(x, value)).unwrap();
            assert_eq!(u64::try_from(sim.get(y)).unwrap(), u64::from(value));
            assert_eq!(u64::try_from(sim.get(inverted)).unwrap(), u64::from(!value));
        }
    }
}

#[test]
fn wildcard_connections_need_an_interface_of_the_port_name() {
    // An interface port cannot stay unconnected (23.3.3.4), `.name()` does
    // not fall back to the wildcard, and an implicit connection cannot bind a
    // generic interface port (25.3.3).
    let interface = "interface Bus; logic [7:0] data; endinterface\n";
    for (child, instance) in [
        (
            "module Child(Bus port); endmodule",
            "Bus other(); Child child(.*);",
        ),
        (
            "module Child(Bus bus); endmodule",
            "Bus bus(); Child child(.bus(), .*);",
        ),
        (
            "module Child(interface bus); endmodule",
            "Bus bus(); Child child(.*);",
        ),
    ] {
        let source = format!("{interface}{child}\nmodule Top; {instance} endmodule");
        assert!(
            Simulator::from_sv_sources(
                vec![(source.as_str(), Path::new("wildcard_rejections.sv"))],
                "Top"
            )
            .build_cranelift()
            .is_err(),
            "{source}"
        );
    }
}

// Interface objects are not IEEE 20.7 data arrays. This opt-in compatibility
// behavior belongs to Celox's harness rather than the portable language suite.
sv_backends! {
    fn queries_interface_array_sizes_as_an_explicit_extension(sim) {
        @setup {
            let source = r#"
                interface Lane; logic value; endinterface
                module Child #(parameter N=1)(output wire [7:0] y); assign y=N; endmodule
                module Reader(Lane ports[3:2], output wire [7:0] y);
                    assign y=$size(ports);
                endmodule
                module Top(output wire [7:0] local_count, param_count, formal_count,
                    first_count, second_count, selected_count, generated_count, shadow_count, member_shadow_count,
                    output wire [1:0] generated);
                    Lane lanes[3:2]();
                    Lane matrix[1:0][4:2]();
                    localparam N=$size(lanes);
                    assign local_count=N;
                    Child #(.N($size(lanes))) c(.y(param_count));
                    Reader r(.ports(lanes), .y(formal_count));
                    assign first_count=$size(matrix,1);
                    assign second_count=$size(matrix,2);
                    assign selected_count=$size(matrix[0]);
                    for(genvar i=0;i<$size(lanes);i=i+1) begin: bits_out
                        assign generated[i]=1'b1;
                    end
                    if(1) begin: bank
                        Lane local_lanes[5:7]();
                        assign generated_count=$size(local_lanes);
                    end
                    function automatic int shadow();
                        int lanes[5];
                        return $size(lanes);
                    endfunction
                    assign shadow_count=shadow();
                    function automatic int shadow_member();
                        struct packed {logic [4:0] local_lanes;} bank;
                        return $size(bank.local_lanes);
                    endfunction
                    assign member_shadow_count=shadow_member();
                endmodule
            "#;
            let mut policy=celox::DiagnosticPolicy::default();
            policy.set("interface_array_query",celox::DiagnosticLevel::Warn);
        }
        @build Simulator::from_sv_sources(vec![(source,Path::new("interface_array_size.sv"))],"Top")
            .diagnostic_policy(policy);
        for (name,expected) in [("local_count",2u8),("param_count",2),("formal_count",2),
            ("first_count",2),("second_count",3),("selected_count",3),
            ("generated_count",3),("shadow_count",5),("member_shadow_count",5),("generated",3)] {
            assert_eq!(sim.get(sim.signal(name)),BigUint::from(expected),"{name}");
        }
        assert!(!sim.warnings().is_empty());
    }
}

#[test]
fn rejects_invalid_interface_array_selections() {
    for (selection, expected) in [
        ("lanes[4:3]", "outside its declared range"),
        ("lanes[0:1]", "reversed interface array selection"),
        (
            "lanes[1 +: 0]",
            "nonpositive interface array selection width",
        ),
        ("lanes[index +: 2]", "nonconstant interface array selection"),
        ("lanes[0][0]", "too many interface array selections"),
        ("lanes[3:1]", "interface array dimension mismatch"),
        (
            "lanes[128'h7fffffffffffffffffffffffffffffff +: 2]",
            "selection bound overflow",
        ),
    ] {
        let source = format!(
            r#"
            interface Lane;
                logic value;
                modport reader(input value);
            endinterface
            module Reader(Lane.reader ports[2], output wire y);
                assign y=ports[0].value ^ ports[1].value;
            endmodule
            module Top(input int index, output wire y);
                Lane lanes[3:0]();
                for(genvar n=0;n<4;n++) begin: g
                    assign lanes[n].value=1'b0;
                end
                Reader r(.ports({selection}),.y(y));
            endmodule
        "#
        );
        let error = Simulator::from_sv_sources(
            vec![(source.as_str(), Path::new("invalid_interface_slice.sv"))],
            "Top",
        )
        .build_interpreter()
        .expect_err(selection)
        .to_string();
        assert!(error.contains(expected), "{selection}: {error}");
    }
}

#[test]
fn rejects_interface_array_slices_in_multiple_dimensions() {
    let source = r#"
        interface Lane; logic value; endinterface
        module Reader(Lane lanes[1][1], output wire y);
            assign y=lanes[0][0].value;
        endmodule
        module Top(output wire y);
            Lane lanes[2][2]();
            Reader r(.lanes(lanes[0:0][0:0]),.y(y));
        endmodule
    "#;
    let error =
        Simulator::from_sv_sources(vec![(source, Path::new("invalid_matrix_slice.sv"))], "Top")
            .build_interpreter()
            .err()
            .unwrap()
            .to_string();
    assert!(
        error.contains("interface array slices in multiple dimensions"),
        "{error}"
    );
}

// The unindexed value-parameter shorthand is an explicit compatibility mode.
sv_backends! {
    fn unindexed_interface_array_parameters_preserve_bound_values_and_types(sim) {
        @setup {
            let source = "interface Bus #(parameter int WIDTH=8, parameter logic signed [7:0] NEG=-3);
                    logic [WIDTH-1:0] data; modport sink(input data); endinterface
                module Child(Bus.sink bus[5:4], output wire [31:0] q, n);
                    function automatic int width_value(); return bus.WIDTH; endfunction
                    localparam W=width_value();
                    assign q=W;
                    assign n=bus.NEG;
                endmodule
                module Top(output wire [31:0] q,n);
                    Bus #(13,-5) buses[5:4](); Child dut(buses,q,n);
                endmodule";
            let mut policy=celox::DiagnosticPolicy::default();
            policy.set("interface_array_parameter",celox::DiagnosticLevel::Allow);
        }
        @build Simulator::from_sv_sources(vec![(source,Path::new("array_parameter_types.sv"))],"Top").diagnostic_policy(policy);
        assert_eq!(sim.get(sim.signal("q")),BigUint::from(13u8));
        assert_eq!(sim.get(sim.signal("n")),BigUint::from(0xfffffffbu32));
    }
}

#[test]
fn interface_array_parameter_shorthand_does_not_bind_runtime_members() {
    let source = "interface Bus #(parameter WIDTH=8); logic [WIDTH-1:0] data; endinterface
        module Top(output wire [7:0] q); Bus bus[2](); assign q=bus.data; endmodule";
    let mut policy = celox::DiagnosticPolicy::default();
    policy.set("interface_array_parameter", celox::DiagnosticLevel::Allow);
    let error = Simulator::from_sv_sources(vec![(source, Path::new("array_signal.sv"))], "Top")
        .diagnostic_policy(policy)
        .build_cranelift()
        .err()
        .unwrap();
    assert!(error.to_string().contains("bus.data"), "{error}");
}

// A module with a non-ANSI header beside an interface declaration, as
// OpenPiton's `monitor`: the declarations of its header ports are in its body
// (IEEE 1800-2023 23.2.2.1), and the interface does not change them.
#[test]
fn non_ansi_modules_beside_interfaces_keep_their_ports() {
    let source = r#"
        `timescale 1ns/1ns
        interface bus_if;
            logic v;
        endinterface
        module counter(/*AUTOARG*/
            // Inputs
            clk, n
            );
            input clk;
            output integer n;
            initial n = 0;
            always @(posedge clk) n = n + 1;
        endmodule
        module Top;
            logic clk = 0;
            integer n;
            bus_if b();
            always #5 clk = ~clk;
            counter c(.clk(clk), .n(n));
            initial #32 $display("%0d", n);
        endmodule
    "#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("non_ansi.sv"))], "Top")
                .four_state(four_state)
                .build_cranelift()
                .unwrap();
        sim.run_until(40).unwrap();
        let displayed: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8(message).unwrap())
                }
                _ => None,
            })
            .collect();
        assert_eq!(displayed, ["3"], "four_state={four_state}");
    }
}

// A select of a member of an element of an interface instance array has the
// type of that member (IEEE 1800-2023 25.3), here a structure member of a
// width that differs between the two buses.
#[test]
fn selects_of_interface_array_element_members_are_typed() {
    let source = r#"
        interface Bus #(parameter DATA_SIZE = 1, parameter TAG_WIDTH = 1) ();
            typedef struct packed {
                logic [DATA_SIZE*8-1:0] data;
                logic [TAG_WIDTH-1:0] tag;
            } rsp_data_t;
            rsp_data_t rsp_data;
        endinterface
        module Top(
            input logic [21:0] first, second,
            output logic [19:0] narrowed, output logic [1:0] low, output logic top
        );
            localparam TD = 6, TS = 4;
            Bus #(.DATA_SIZE(2), .TAG_WIDTH(TD)) wide[2]();
            Bus #(.DATA_SIZE(2), .TAG_WIDTH(TS)) narrow();
            assign wide[0].rsp_data = first;
            assign wide[1].rsp_data = second;
            assign narrow.rsp_data.data = wide[0].rsp_data.data;
            assign narrow.rsp_data.tag = wide[0].rsp_data.tag[TD-1 -: TS];
            assign narrowed = narrow.rsp_data;
            assign low = wide[1].rsp_data.tag[1:0];
            assign top = wide[1].rsp_data.tag[5];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("array_member.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let (first, second) = (sim.signal("first"), sim.signal("second"));
    sim.modify(|io| {
        io.set(first, 0x2a_bcd5u32);
        io.set(second, 0x3f_ffe2u32);
    })
    .unwrap();
    assert_eq!(sim.get(sim.signal("narrowed")), BigUint::from(0xaaf35u32));
    assert_eq!(sim.get(sim.signal("low")), BigUint::from(2u8));
    assert_eq!(sim.get(sim.signal("top")), BigUint::from(1u8));
}

// A variable of a task, a generate block or a procedure hides an array of
// interface instances of its name (IEEE 1800-2023 23.9): a member select of
// its element has the type of the variable's member, here of an ascending
// range, not that of the interface signal.
#[test]
fn variables_hide_interface_arrays_of_their_name() {
    let source = r#"
        `timescale 1ns/1ns
        interface Bus ();
            logic [7:0] data;
        endinterface
        module Top;
            typedef struct packed { logic [0:3] data; } item_t;
            Bus bus[2]();
            logic [3:0] a = 4'b1000;
            logic [1:0] t, g_out, p_out;
            assign bus[0].data = 8'h81;
            assign bus[1].data = 8'h7e;
            task automatic run(input logic [3:0] v, output logic [1:0] r);
                item_t bus [2];
                bus[0].data = v;
                bus[1].data = ~v;
                r = {bus[0].data[0], bus[1].data[0 +: 1]};
            endtask
            if (1) begin : g
                item_t bus [2];
                assign bus[0].data = a;
                assign bus[1].data = ~a;
                assign g_out = {bus[0].data[0], bus[1].data[0]};
            end
            always_comb begin : p
                item_t bus [2];
                bus[0].data = a;
                bus[1].data = ~a;
                p_out = {bus[0].data[0], bus[1].data[0]};
            end
            initial begin
                #1 run(a, t);
                $display("%b %b %b %b%b", t, g_out, p_out, bus[0].data[0], bus[1].data[0]);
            end
        endmodule
    "#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("hidden_array.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    sim.run_until(10).unwrap();
    let displayed: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => Some(String::from_utf8(message).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(displayed, ["10 10 10 10"]);
}

sv_backends! {
    // A task declared in an interface is called through the instance or a
    // port, also through an imported modport task (IEEE 1800-2023 25.7),
    // and its timing controls wait on the interface's signals.
    fn interface_tasks_wait_on_interface_signals(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            interface bus_if(input logic clk);
              logic [7:0] data;
              logic valid;
              task automatic send(input logic [7:0] value);
                @(posedge clk);
                data <= value;
                valid <= 1'b1;
                @(posedge clk);
                valid <= 1'b0;
              endtask
              task get(output logic [7:0] value);
                do @(posedge clk); while (!valid);
                value = data;
              endtask
              modport drv(output data, output valid, import send);
            endinterface
            module Driver(bus_if.drv b);
              initial begin
                b.send(8'h11);
                b.send(8'h22);
              end
            endmodule
            module Top(output logic [7:0] got, seen, output int got_at);
              logic clk = 0;
              always #5 clk = ~clk;
              bus_if bus(.clk(clk));
              bus_if loop(.clk(clk));
              Driver d(.b(bus));
              always @(posedge clk) if (bus.valid) seen <= bus.data;
              initial begin
                loop.valid = 0;
                fork
                  begin loop.send(8'h5a); loop.send(8'h3c); end
                  begin loop.get(got); got_at = $time; end
                join
              end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("interface_tasks.sv"))], "Top");
        sim.run_until(60).unwrap();
        assert_eq!(sim.get(sim.signal("got")), BigUint::from(0x5au8));
        assert_eq!(sim.get(sim.signal("got_at")), BigUint::from(15u8));
        assert_eq!(sim.get(sim.signal("seen")), BigUint::from(0x22u8));
    }
}
