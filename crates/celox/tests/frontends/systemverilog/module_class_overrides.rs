//! Overrides of virtual methods by classes declared in a module, called
//! through handles of package classes from any scope (IEEE 1800-2023
//! 8.20), as UVM's abstract-class bus functional models are.

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

/// The displays of `source` with top `top`, run until `until`, in each
/// state mode.
fn run(source: &str, top: &str, until: u64) -> Vec<Vec<String>> {
    [false, true]
        .into_iter()
        .map(|four_state| {
            let mut sim =
                celox::Simulation::from_sv_sources(vec![(source, Path::new("overrides.sv"))], top)
                    .four_state(four_state)
                    .build()
                    .unwrap_or_else(|error| panic!("{error}"));
            sim.run_until(until).unwrap();
            displays(&mut sim)
        })
        .collect()
}

#[test]
fn package_code_calls_the_override_of_a_class_of_a_module() {
    // Package code calls `monitor` through a handle of the package class;
    // the object is of a class of module `bfm`, whose override waits on the
    // module's clock and counts its edges (IEEE 1800-2023 8.20).
    let source = r#"`timescale 1ns/1ns
package agent;
  class item; int data; endclass
  class monitor_abstract;
    virtual task monitor(item it); endtask
  endclass
  monitor_abstract shared;
  class monitor;
    int n;
    task run();
      item it;
      forever begin
        it = new;
        shared.monitor(it);
        n++;
        if (n == 3) begin $display("n %0d at %0t data %0d", n, $time, it.data); $finish; end
        if (n == 1000) begin $display("spun at %0t", $time); $finish; end
      end
    endtask
  endclass
endpackage
module bfm(input logic clk);
  import agent::*;
  int count;
  class monitor_concrete extends monitor_abstract;
    task monitor(item it);
      @(posedge clk);
      count++;
      it.data = count;
    endtask
  endclass
  monitor_concrete c;
  initial begin c = new; shared = c; end
endmodule
module top; logic clk = 0; bfm i(clk); initial forever #5 clk = ~clk;
  initial begin agent::monitor m = new; #1 m.run(); end
endmodule
"#;
    for displays in run(source, "top", 30) {
        assert_eq!(displays, ["n 3 at 25 data 3"]);
    }
}

#[test]
fn virtual_functions_of_classes_of_a_module_read_its_variables() {
    // `read` returns a variable of the module instance; `plain` reads no
    // item of the module.
    let source = r#"`timescale 1ns/1ns
package agent;
  virtual class sensor; pure virtual function int read(); virtual function int plain(); return -1; endfunction endclass
  sensor s;
endpackage
module dev #(int W = 8) (input logic clk);
  import agent::*;
  logic [W-1:0] value = 0;
  always @(posedge clk) value <= value + 1;
  class concrete extends sensor;
    function int read(); return value; endfunction
    function int plain(); return 42; endfunction
  endclass
  concrete c;
  initial begin c = new; s = c; end
endmodule
module top; logic clk = 0; dev #(4) d(clk); initial forever #5 clk = ~clk;
  initial begin #22 $display("read %0d plain %0d", agent::s.read(), agent::s.plain()); $finish; end
endmodule
"#;
    for displays in run(source, "top", 30) {
        assert_eq!(displays, ["read 2 plain 42"]);
    }
}

#[test]
fn overrides_of_overrides_in_a_module_are_called_from_package_code() {
    // `leaf` overrides `mid`'s override; `plain_leaf` runs `mid`'s.
    let source = r#"`timescale 1ns/1ns
package agent;
  class base; virtual function int who(); return 1; endfunction endclass
  base list[3];
endpackage
module m;
  import agent::*;
  int tag = 100;
  class mid extends base; function int who(); return tag + 2; endfunction endclass
  class leaf extends mid; function int who(); return tag + 3; endfunction endclass
  class plain_leaf extends mid; endclass
  initial begin mid a = new; leaf b = new; plain_leaf c = new; list[0] = a; list[1] = b; list[2] = c; end
endmodule
module top; m u();
  initial begin agent::base x = new; #1 $display("%0d %0d %0d %0d", agent::list[0].who(), agent::list[1].who(), agent::list[2].who(), x.who()); end
endmodule
"#;
    for displays in run(source, "top", 2) {
        assert_eq!(displays, ["102 103 102 1"]);
    }
}

#[test]
fn each_specialization_of_a_module_runs_its_own_overrides() {
    // Two specializations of the module, each with a bus of its own width.
    let source = r#"`timescale 1ns/1ns
package agent;
  virtual class drv; pure virtual task drive(int v); pure virtual function int seen(); endclass
  drv d[2];
endpackage
module bfm #(int W = 8, int INDEX = 0) (input logic clk);
  import agent::*;
  logic [W-1:0] bus = 0;
  class concrete extends drv;
    task drive(int v); @(posedge clk); bus = v; endtask
    function int seen(); return bus; endfunction
  endclass
  initial begin concrete c = new; d[INDEX] = c; end
endmodule
module top; logic clk = 0; bfm #(8, 0) a(clk); bfm #(4, 1) b(clk); initial forever #5 clk = ~clk;
  initial begin
    #1 fork agent::d[0].drive(300); agent::d[1].drive(19); join
    $display("a %0d b %0d seen %0d %0d at %0t", a.bus, b.bus, agent::d[0].seen(), agent::d[1].seen(), $time);
    $finish;
  end
endmodule
"#;
    for displays in run(source, "top", 10) {
        assert_eq!(displays, ["a 44 b 3 seen 44 3 at 5"]);
    }
}

#[test]
fn each_instance_of_a_module_runs_the_override_of_its_objects() {
    // Two instances of one specialization: the object of each drives the
    // instance it was created in. `b` registers first.
    let source = r#"`timescale 1ns/1ns
package agent;
  virtual class drv; pure virtual task drive(int v); pure virtual function int seen(); endclass
  drv d[2];
  int n;
  function void register(drv x); d[n] = x; n++; endfunction
endpackage
module bfm (input logic clk);
  import agent::*;
  logic [7:0] bus = 0;
  class concrete extends drv;
    task drive(int v); @(posedge clk); bus = v; endtask
    function int seen(); return bus; endfunction
  endclass
  initial begin concrete c = new; register(c); end
endmodule
module top; logic clk = 0; bfm a(clk); bfm b(clk); initial forever #5 clk = ~clk;
  initial begin
    #1 fork agent::d[0].drive(7); agent::d[1].drive(9); join
    $display("a %0d b %0d seen %0d %0d at %0t", a.bus, b.bus, agent::d[0].seen(), agent::d[1].seen(), $time);
    $finish;
  end
endmodule
"#;
    for displays in run(source, "top", 10) {
        assert_eq!(displays, ["a 9 b 7 seen 7 9 at 5"]);
    }
}

#[test]
fn classes_of_a_module_still_override_each_other_within_it() {
    // A class of the module overrides another class of the module.
    let source = r#"`timescale 1ns/1ns
module m;
  int base_value = 5;
  class e; virtual function int g(); return base_value; endfunction endclass
  class f extends e; function int g(); return base_value * 10; endfunction endclass
  initial begin e x; f y = new; x = y; #1 $display("g %0d", x.g()); end
endmodule
"#;
    for displays in run(source, "m", 2) {
        assert_eq!(displays, ["g 50"]);
    }
}

#[test]
fn overrides_of_modules_that_hierarchical_names_reach_are_exported() {
    // `m` reads `top.tick`, so it is analyzed once the hierarchy is known,
    // after the other modules; its override is exported all the same.
    let source = r#"`timescale 1ns/1ns
package agent; class base; virtual function int get(); return -1; endfunction endclass base h; endpackage
module m;
  import agent::*;
  int seen;
  always @(top.tick) seen = top.tick;
  class c extends base; function int get(); return seen + 100; endfunction endclass
  initial begin c x = new; h = x; end
endmodule
module top; int tick = 0; m u(); initial begin #1 tick = 5; #1 $display("get %0d", agent::h.get()); end endmodule
"#;
    for displays in run(source, "top", 3) {
        assert_eq!(displays, ["get 105"]);
    }
}

#[test]
fn each_specialization_of_a_module_keeps_its_own_locals() {
    // The local `tmp` of the override is as wide as each specialization's
    // `value`.
    let source = r#"`timescale 1ns/1ns
package agent; class base; virtual function int get(); return -1; endfunction endclass base h[2]; endpackage
module m #(int W = 4, int I = 0);
  import agent::*;
  logic [W-1:0] value = '1;
  class c extends base; function int get(); logic [W-1:0] tmp; tmp = value; return tmp; endfunction endclass
  initial begin c x = new; h[I] = x; end
endmodule
module top; m #(4, 0) a(); m #(8, 1) b(); initial begin #1 $display("get %0d %0d", agent::h[0].get(), agent::h[1].get()); end endmodule
"#;
    for displays in run(source, "top", 2) {
        assert_eq!(displays, ["get 15 255"]);
    }
}

#[test]
fn formals_that_share_a_name_with_module_variables_stay_formals() {
    // The formal `count` is not the module variable `count`.
    let source = r#"`timescale 1ns/1ns
package agent; class base; virtual function int get(int count); return -1; endfunction endclass base h; endpackage
module m;
  import agent::*;
  int count = 7;
  class c extends base; function int get(int count); int tmp; tmp = count * 2; return tmp; endfunction endclass
  initial begin c x = new; h = x; end
endmodule
module top; m u(); initial begin #1 $display("get %0d", agent::h.get(20)); end endmodule
"#;
    for displays in run(source, "top", 2) {
        assert_eq!(displays, ["get 40"]);
    }
}

#[test]
fn module_variables_sized_by_package_parameters_are_read() {
    // The width of `value` is a parameter of an imported package.
    let source = r#"`timescale 1ns/1ns
package widths; parameter int W = 6; endpackage
package agent; class base; virtual function int get(); return -1; endfunction endclass base h; endpackage
module m;
  import agent::*;

  logic [widths::W-1:0] value = '1;
  class c extends base; function int get(); return value; endfunction endclass
  initial begin c x = new; h = x; end
endmodule
module top; m u(); initial begin #1 $display("get %0d", agent::h.get()); end endmodule
"#;
    for displays in run(source, "top", 2) {
        assert_eq!(displays, ["get 63"]);
    }
}

#[test]
fn overrides_whose_helpers_default_to_module_variables_are_rejected() {
    // `helper`'s default reads the module's `value`, which the caller's
    // scope would otherwise bind to its own `value`.
    let source = r#"`timescale 1ns/1ns
package agent; class base; virtual function int get(); return -1; endfunction endclass base h; endpackage
module m;
  import agent::*;
  int value = 9;
  function int helper(int v = value); return v; endfunction
  class c extends base; function int get(); return helper(); endfunction endclass
  initial begin c x = new; h = x; end
endmodule
module top; int value = 3; m u(); initial begin #1 $display("get %0d", agent::h.get()); end endmodule
"#;
    let Err(error) =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("overrides.sv"))], "top")
            .build()
    else {
        panic!("the override is rejected");
    };
    let error = error.to_string();
    assert!(error.contains("which reads the module's items"), "{error}");
}

#[test]
fn generated_override_subroutines_take_no_declared_name() {
    // The class declares a method `get$instances` as well: the subroutine
    // that selects the instance of `get` is another.
    let source = r#"`timescale 1ns/1ns
package agent; class base; virtual function int get(); return -1; endfunction endclass base h; endpackage
module m;
  import agent::*;
  int value = 5;
  class c extends base;
    function int get(); return value; endfunction
    function int get$instances(); return 999; endfunction
  endclass
  initial begin c x = new; h = x; #2 $display("in m %0d", h.get()); end
endmodule
module top; m u(); initial begin #1 $display("get %0d", agent::h.get()); end endmodule
"#;
    for displays in run(source, "top", 3) {
        assert_eq!(displays, ["get 5", "in m 5"]);
    }
}

#[test]
fn package_variables_of_exported_overrides_are_the_packages_objects() {
    // `count` is the variable of package `counters` that `m` imports; `top`
    // declares its own `count`, which the override does not read.
    let source = r#"`timescale 1ns/1ns
package counters; int count = 42; endpackage
package agent; class base; virtual function int get(); return -1; endfunction endclass base h; endpackage
module m;
  import agent::*;
  import counters::*;
  class c extends base; function int get(); return count; endfunction endclass
  initial begin c x = new; h = x; end
endmodule
module top; int count = 7; m u(); initial begin #1 $display("get %0d", agent::h.get()); end endmodule
"#;
    for displays in run(source, "top", 2) {
        assert_eq!(displays, ["get 42"]);
    }
}

#[test]
fn the_instance_property_is_not_a_property_of_that_name() {
    // The package base class declares a property `\\$instance`, which the
    // objects keep apart from the instance their module class records.
    let source = r#"`timescale 1ns/1ns
package agent; class base; int \$instance ; virtual function int get(); return -1; endfunction endclass base h[2]; endpackage
module m #(int I = 0);
  import agent::*;
  int value = I * 10 + 5;
  class c extends base; function int get(); return value; endfunction endclass
  initial begin c x = new; x.\$instance = 77; h[I] = x; end
endmodule
module top; m #(0) a(); m #(1) b(); initial begin #1 $display("get %0d %0d user %0d", agent::h[0].get(), agent::h[1].get(), agent::h[0].\$instance ); end endmodule
"#;
    for displays in run(source, "top", 2) {
        assert_eq!(displays, ["get 5 15 user 77"]);
    }
}

#[test]
fn helper_defaults_name_package_variables_by_their_packages() {
    // `helper`'s default reads `count` of package `counters`, which `m`
    // imports; `top` declares its own `count`.
    let source = r#"`timescale 1ns/1ns
package counters; int count = 42; endpackage
package agent; class base; virtual function int get(); return -1; endfunction endclass base h; endpackage
module m;
  import agent::*;
  import counters::*;
  function int helper(int v = count); return v; endfunction
  class c extends base; function int get(); return helper(); endfunction endclass
  initial begin c x = new; h = x; end
endmodule
module top; int count = 7; m u(); initial begin #1 $display("get %0d", agent::h.get()); end endmodule
"#;
    for displays in run(source, "top", 2) {
        assert_eq!(displays, ["get 42"]);
    }
}

#[test]
fn calls_of_overrides_are_sensitive_to_the_module_variables_they_name() {
    // `get` names `m.value` in a branch a parameter removes; an always_comb
    // calling it runs again when that variable changes, not when the
    // caller's own `value` does (IEEE 1800-2023 9.2.2.2.1).
    let source = r#"`timescale 1ns/1ns
package agent; class base; virtual function int get(); return -1; endfunction endclass base h; endpackage
module m #(bit P = 0);
  import agent::*;
  int value = 5;
  class c extends base; function int get(); if (P) return value; return 7; endfunction endclass
  initial begin c x = new; h = x; #2 value = 6; end
endmodule
module top; int y; int value = 1; m u();
  always_comb begin
    if (agent::h != null) y = agent::h.get(); else y = 0;
    $display("eval %0d at %0t", y, $time);
  end
  initial begin #3 value = 2; end
endmodule
"#;
    for displays in run(source, "top", 5) {
        assert_eq!(displays, ["eval 7 at 0", "eval 7 at 2"]);
    }
}

#[test]
fn modules_with_dollar_signs_in_their_names_export_their_overrides() {
    // The definition of `m$x` keys the instances its override selects among.
    let source = r#"`timescale 1ns/1ns
package agent; class base; virtual function int get(); return -1; endfunction endclass base h; endpackage
module m$x;
  import agent::*;
  int value = 5;
  class c extends base; function int get(); return value; endfunction endclass
  initial begin c x = new; h = x; end
endmodule
module top; m$x u(); initial begin #1 $display("get %0d", agent::h.get()); end endmodule
"#;
    for displays in run(source, "top", 2) {
        assert_eq!(displays, ["get 5"]);
    }
}

#[test]
fn overrides_with_static_variables_are_rejected() {
    // A static variable of the override is the class's, which the copies of
    // the override in the scopes that call it would each hold apart.
    let source = r#"`timescale 1ns/1ns
package agent; class base; virtual function int get(); return -1; endfunction endclass base h; endpackage
module m;
  import agent::*;
  class c extends base; function int get(); static int calls = 0; calls++; return calls; endfunction endclass
  initial begin c x = new; h = x; end
endmodule
module top; m u(); initial begin #1 $display("get %0d %0d", agent::h.get(), agent::h.get()); end endmodule
"#;
    let Err(error) =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("overrides.sv"))], "top")
            .build()
    else {
        panic!("the override is rejected");
    };
    let error = error.to_string();
    assert!(error.contains("static variables of override"), "{error}");
}

#[test]
fn helpers_of_overrides_of_different_modules_stay_apart() {
    // `m1`, `m2` and `top` each declare a `helper`: each override calls its
    // own module's.
    let source = r#"`timescale 1ns/1ns
package agent; class base; virtual function int get(); return -1; endfunction endclass base h[2]; endpackage
module m1; import agent::*; function int helper(); return 1; endfunction class c1 extends base; function int get(); return helper(); endfunction endclass initial begin c1 x = new; h[0] = x; end endmodule
module m2; import agent::*; function int helper(); return 2; endfunction class c2 extends base; function int get(); return helper(); endfunction endclass initial begin c2 x = new; h[1] = x; end endmodule
module top; function int helper(); return 9; endfunction m1 a(); m2 b(); initial begin #1 $display("get %0d %0d %0d", agent::h[0].get(), agent::h[1].get(), helper()); end endmodule
"#;
    for displays in run(source, "top", 2) {
        assert_eq!(displays, ["get 1 2 9"]);
    }
}

#[test]
fn helpers_with_static_variables_are_rejected() {
    // `helper`'s static variable is the module's, which every copy of
    // the override would hold apart.
    let source = r#"`timescale 1ns/1ns
package agent; class base; virtual function int get(); return -1; endfunction endclass base h; endpackage
module m; import agent::*; function int helper(); static int calls = 0; calls++; return calls; endfunction class c extends base; function int get(); return helper(); endfunction endclass initial begin c x = new; h = x; end endmodule
module top; m u(); initial begin #1 $display("get %0d %0d", agent::h.get(), agent::h.get()); end endmodule
"#;
    let Err(error) =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("overrides.sv"))], "top")
            .build()
    else {
        panic!("the override is rejected");
    };
    let error = error.to_string();
    assert!(error.contains("which has static variables"), "{error}");
}

#[test]
fn overrides_reading_hierarchical_names_are_rejected() {
    // `top.tick` is bound where `m`'s hierarchy is, not in the scopes that
    // call the override.
    let source = r#"`timescale 1ns/1ns
package agent; class base; virtual function int get(); return -1; endfunction endclass base h; endpackage
module m; import agent::*; class c extends base; function int get(); return top.tick; endfunction endclass initial begin c x = new; h = x; end endmodule
module top; int tick = 5; m u(); initial begin #1 $display("get %0d", agent::h.get()); end endmodule
"#;
    let Err(error) =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("overrides.sv"))], "top")
            .build()
    else {
        panic!("the override is rejected");
    };
    let error = error.to_string();
    assert!(error.contains("reads the hierarchical name"), "{error}");
}
