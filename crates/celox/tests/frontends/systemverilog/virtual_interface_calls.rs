//! Tasks and functions of an interface called through a virtual interface
//! (IEEE 1800-2023 25.9), as UVM drivers call their bus functional models.

use std::path::Path;

fn displays(sim: &mut celox::Simulation) -> Vec<String> {
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

/// A class drives two instances of a bus functional model through virtual
/// interfaces: each call runs the task of the instance its handle denotes,
/// waiting on that instance's clock and writing its variable.
#[test]
fn classes_call_tasks_and_functions_of_the_instances_virtual_interfaces_denote() {
    let source = r#"`timescale 1ns / 1ns
interface bfm(input logic clk);
  logic [7:0] data = 0;
  task drive(input logic [7:0] v);
    @(posedge clk);
    data <= v;
  endtask
  function automatic logic [7:0] peek();
    return data;
  endfunction
endinterface
package p;
  class drv;
    virtual bfm b;
    task run(logic [7:0] first);
      b.drive(first);
      b.drive(first + 1);
      $display("peek %0d at %0t", b.peek(), $time);
    endtask
  endclass
endpackage
module top;
  logic fast = 0, slow = 0;
  bfm a(fast), b(slow);
  initial forever #5 fast = ~fast;
  initial forever #20 slow = ~slow;
  initial begin
    p::drv d = new;
    d.b = a;
    d.run(8'd10);
  end
  initial begin
    p::drv d = new;
    d.b = b;
    d.run(8'd20);
    #1 $display("a %0d b %0d", a.data, b.data);
    $finish;
  end
endmodule
"#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("bfm.sv"))], "top")
                .four_state(four_state)
                .build()
                .unwrap_or_else(|error| panic!("{error}"));
        sim.run_until(1_000).unwrap();
        assert_eq!(
            displays(&mut sim),
            ["peek 10 at 15", "peek 20 at 60", "a 11 b 21"],
            "four_state={four_state}"
        );
    }
}

/// A task of the interface calls another for the same instance, takes a
/// class handle whose object it reads and writes, and returns a value
/// through an output; a call through `null` is an error (25.9).
#[test]
fn interface_subroutines_called_through_virtual_interfaces_call_each_other() {
    let source = r#"`timescale 1ns / 1ns
package items;
  class item;
    logic [7:0] data;
    int delay;
  endclass
endpackage
interface bfm(input logic clk);
  import items::*;
  logic [7:0] bus = 0;
  int sent = 0;
  task automatic wait_cycles(int n);
    repeat (n) @(posedge clk);
  endtask
  task drive(item req, output int count);
    wait_cycles(req.delay);
    bus <= req.data;
    req.data = req.data + 1;
    sent++;
    count = sent;
  endtask
endinterface
package drivers;
  import items::*;
  class driver;
    virtual bfm vif;
    task send(logic [7:0] data, int delay);
      item req = new;
      int count;
      req.data = data;
      req.delay = delay;
      vif.drive(req, count);
      $display("sent %0d count %0d next %0d at %0t", data, count, req.data, $time);
    endtask
  endclass
endpackage
module top;
  logic clk = 0;
  bfm i(clk);
  initial forever #5 clk = ~clk;
  initial begin
    drivers::driver d = new;
    d.vif = i;
    d.send(8'd3, 2);
    d.send(8'd7, 1);
    #1 $display("bus %0d sent %0d", i.bus, i.sent);
    d.vif = null;
    d.send(8'd9, 1);
    $display("after null");
  end
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("bfm.sv"))], "top")
        .build()
        .unwrap_or_else(|error| panic!("{error}"));
    let result = sim.run_until(1_000);
    let lines = displays(&mut sim);
    assert_eq!(
        lines[..3],
        [
            "sent 3 count 1 next 4 at 15",
            "sent 7 count 2 next 8 at 25",
            "bus 7 sent 2",
        ],
        "{lines:?}"
    );
    assert!(
        result.is_err() || !lines.contains(&"after null".to_string()),
        "{result:?} {lines:?}"
    );
}

/// Module code calls an interface function through a virtual interface
/// variable, also one of a modport type that imports it (25.5.4).
#[test]
fn modules_call_interface_functions_through_virtual_interfaces() {
    let source = r#"`timescale 1ns / 1ns
interface regs;
  logic [7:0] r = 8'd4;
  function automatic logic [7:0] scaled(input logic [7:0] by);
    return r * by;
  endfunction
  modport reader(import scaled);
endinterface
module top;
  regs a(), b();
  virtual regs v;
  virtual regs.reader m;
  initial begin
    b.r = 8'd5;
    v = a;
    m = b;
    $display("%0d %0d", v.scaled(2), m.scaled(3));
  end
endmodule
"#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("regs.sv"))], "top")
                .four_state(four_state)
                .build()
                .unwrap_or_else(|error| panic!("{error}"));
        sim.run_until(10).unwrap();
        assert_eq!(displays(&mut sim), ["8 15"], "four_state={four_state}");
    }
}

/// The usual UVM layout: the bus functional model imports the package of
/// its driver, whose class holds the model through a virtual interface and
/// passes it the package's items.
#[test]
fn bus_functional_models_import_the_package_of_their_driver() {
    let source = r#"`timescale 1ns / 1ns
package agent;
  class item;
    logic [7:0] data;
  endclass
  class driver;
    virtual bfm vif;
    task send(logic [7:0] data);
      item req = new;
      req.data = data;
      vif.drive(req);
    endtask
  endclass
endpackage
interface bfm(input logic clk, output logic [7:0] bus);
  import agent::*;
  task drive(item req);
    @(posedge clk);
    bus <= req.data;
  endtask
endinterface
module top;
  logic clk = 0;
  logic [7:0] bus;
  bfm i(clk, bus);
  initial forever #5 clk = ~clk;
  initial begin
    agent::driver d = new;
    d.vif = i;
    d.send(8'd9);
    #1 $display("bus %0d at %0t", bus, $time);
  end
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("agent.sv"))], "top")
        .build()
        .unwrap_or_else(|error| panic!("{error}"));
    sim.run_until(100).unwrap();
    assert_eq!(displays(&mut sim), ["bus 9 at 6"]);
}

/// A default that reads a member of the interface reads it of the instance
/// the handle denotes, where the subroutine is declared (13.5.3), and a
/// function's result type is published: `$bits` of a call is its width.
#[test]
fn defaults_and_result_types_of_calls_through_virtual_interfaces() {
    let source = r#"`timescale 1ns / 1ns
interface bfm;
  logic [11:0] data = 12'd7;
  function automatic logic [11:0] shifted(input logic [11:0] by = data);
    return data + by;
  endfunction
endinterface
package p;
  class c;
    virtual bfm b;
    function void show();
      $display("class %0d %0d", b.shifted(), b.shifted(1));
    endfunction
  endclass
endpackage
module top;
  bfm x(), y();
  virtual bfm v;
  initial begin
    p::c o = new;
    y.data = 12'd100;
    o.b = y;
    v = x;
    o.show();
    $display("module %0d %0d bits %0d", v.shifted(), v.shifted(1), $bits(v.shifted()));
  end
endmodule
"#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("defaults.sv"))], "top")
                .four_state(four_state)
                .build()
                .unwrap_or_else(|error| panic!("{error}"));
        sim.run_until(10).unwrap();
        assert_eq!(
            displays(&mut sim),
            ["class 200 101", "module 14 8 bits 12"],
            "four_state={four_state}"
        );
    }
}

/// Through a modport, only the subroutines it imports may be called
/// (25.7); a DPI import of an interface is not called through a virtual
/// interface (an interface that declares one is rejected).
#[test]
fn calls_through_virtual_interfaces_that_are_rejected() {
    for (dpi, body, expected) in [
        (
            "",
            "virtual regs.reader m; initial begin m = i; $display(\"%0d\", m.hidden()); end",
            &["whose modport does not import it"][..],
        ),
        (
            "import \"DPI-C\" function int external();",
            "virtual regs v; initial begin v = i; $display(\"%0d\", v.external()); end",
            &[
                "DPI import `external` of interface `regs` called through a virtual interface",
                "nested declaration in interface `regs`",
            ],
        ),
    ] {
        let source = format!(
            r#"interface regs;
  logic [7:0] r = 8'd4;
  function automatic logic [7:0] scaled(); return r * 2; endfunction
  function automatic logic [7:0] hidden(); return r; endfunction
  {dpi}
  modport reader(import scaled);
endinterface
module top;
  regs i();
  {body}
endmodule
"#
        );
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("rejected.sv"))],
            "top",
        )
        .build()
        .err()
        .unwrap()
        .to_string()
        .split_whitespace()
        .filter(|word| *word != "│")
        .collect::<Vec<_>>()
        .join(" ");
        assert!(
            expected.iter().any(|expected| error.contains(expected)),
            "{body}: {error}"
        );
    }
}

/// A static variable of an interface subroutine belongs to the instance
/// (13.3.2, 6.21): calls through a virtual interface keep it for the
/// instance the handle denotes, across calls, from its initializer on.
#[test]
fn static_variables_of_interface_subroutines_belong_to_each_instance() {
    let source = r#"`timescale 1ns / 1ns
interface bfm;
  task automatic count(output int n);
    static int calls = 10;
    calls++;
    n = calls;
  endtask
  function int next();
    int k = 0;
    k++;
    return k;
  endfunction
endinterface
module top;
  bfm a(), b();
  virtual bfm v, w;
  int n, m, o;
  initial begin
    v = a;
    w = b;
    v.count(n);
    v.count(m);
    w.count(o);
    $display("count %0d %0d %0d", n, m, o);
    n = v.next();
    m = v.next();
    o = w.next();
    $display("next %0d %0d %0d", n, m, o);
  end
endmodule
"#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("statics.sv"))], "top")
                .four_state(four_state)
                .build()
                .unwrap_or_else(|error| panic!("{error}"));
        sim.run_until(10).unwrap();
        assert_eq!(
            displays(&mut sim),
            ["count 11 12 11", "next 1 2 1"],
            "four_state={four_state}"
        );
    }
}

/// A call of another subroutine of the interface, in a default or with
/// named actuals in another order, runs it for the same instance.
#[test]
fn interface_subroutines_call_each_other_in_defaults_and_by_name() {
    let source = r#"`timescale 1ns / 1ns
interface regs;
  logic [7:0] r = 8'd3;
  function automatic logic [7:0] base();
    return r;
  endfunction
  function automatic logic [7:0] add(input logic [7:0] x = base(), input logic [7:0] y = 1);
    return x + y;
  endfunction
  function automatic logic [7:0] sub(input logic [7:0] left, input logic [7:0] right);
    return left - right;
  endfunction
  function automatic logic [7:0] both();
    return sub(.right(r), .left(8'd10));
  endfunction
endinterface
module top;
  regs a(), b();
  virtual regs v;
  initial begin
    b.r = 8'd5;
    v = b;
    $display("%0d %0d %0d", v.add(), v.add(.y(2)), v.both());
  end
endmodule
"#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("calls.sv"))], "top")
                .four_state(four_state)
                .build()
                .unwrap_or_else(|error| panic!("{error}"));
        sim.run_until(10).unwrap();
        assert_eq!(displays(&mut sim), ["6 7 5"], "four_state={four_state}");
    }
}
