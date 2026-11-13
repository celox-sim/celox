//! A connection of an interface port may name a modport of the interface
//! instance, `.p(bus.mp)` (IEEE 1800-2023 25.5). A modport `output` lets
//! the module assign the member; a module that does not is no driver of
//! it, so several modports of one instance may list the same output.
use celox::Simulation;
use std::path::Path;

/// Runs `source`, whose `initial` blocks print lines, and returns them.
fn display(source: &str, four_state: bool) -> Vec<String> {
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("modports.sv"))], "Top")
        .four_state(four_state)
        .build()
        .unwrap();
    sim.run_until(100).unwrap();
    sim.drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => {
                Some(String::from_utf8_lossy(&message).into_owned())
            }
            _ => None,
        })
        .collect()
}

const BUS: &str = r#"
    `timescale 1ns/1ns
    interface Bus;
      logic       clk;
      logic [7:0] wdata;
      logic [7:0] rdata;
      modport src  (output clk, output wdata, input rdata);
      modport tap  (output clk, input wdata, input rdata);
      modport sink (input clk, input wdata, output rdata);
    endinterface
"#;

#[test]
fn connections_name_modports_of_instances() {
    let source = format!(
        r#"{BUS}
    module Source(input logic clk, input logic [7:0] din, output logic [7:0] dout, Bus.src port);
      assign port.clk   = clk;
      assign port.wdata = din;
      assign dout       = port.rdata;
    endmodule
    module Sink(Bus.sink port);
      assign port.rdata = port.wdata + 8'd1;
    endmodule
    module Top;
      logic       clk = 1;
      logic [7:0] din = 8'h21;
      logic [7:0] dout [2];
      logic [7:0] generated;
      Bus bus();
      Bus buses [2] ();
      Source source(.port(bus.src), .dout(dout[0]), .clk(clk), .din(din));
      Sink   sink(.port(bus.sink));
      // An element of an array of instances, and an instance of a
      // generate block.
      Source element(.port(buses[1].src), .dout(dout[1]), .clk(clk), .din(din));
      Sink   element_sink(.port(buses[1].sink));
      if (1) begin : g
        Bus local_bus();
        Source inner(.port(local_bus.src), .dout(generated), .clk(clk), .din(din));
        Sink   inner_sink(.port(local_bus.sink));
      end
      initial begin
        #1;
        $display("dout=%h %h %h wdata=%h clk=%b", dout[0], dout[1], generated, bus.wdata, bus.clk);
      end
    endmodule
    "#
    );
    for four_state in [false, true] {
        assert_eq!(
            display(&source, four_state),
            ["dout=22 22 22 wdata=21 clk=1"],
            "four-state {four_state}"
        );
    }
}

/// The modport of the connection is that of the port (25.5).
#[test]
fn rejects_a_modport_other_than_that_of_the_port() {
    let source = format!(
        r#"{BUS}
    module Sink(Bus.sink port);
      assign port.rdata = port.wdata;
    endmodule
    module Top;
      Bus bus();
      Sink sink(.port(bus.src));
    endmodule
    "#
    );
    let error =
        Simulation::from_sv_sources(vec![(source.as_str(), Path::new("modports.sv"))], "Top")
            .build()
            .expect_err("the connection is rejected")
            .to_string();
    assert!(error.contains("modport mismatch"), "{error}");
}
