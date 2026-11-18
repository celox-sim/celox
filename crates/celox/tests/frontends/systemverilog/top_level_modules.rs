//! Several top-level modules simulated together (IEEE 1800-2023 23.3.1).

use std::path::Path;

/// A UVM-style testbench: `hdl_top` holds the clock and an interface,
/// hands the interface to `hvl_top` through a package, and `hvl_top` waits
/// on it. Each top is an instance of its own name under `$root`.
const TESTBENCH: &str = r#"
interface bus_if(input logic clk);
  int data = 0;
endinterface
package shared;
  virtual bus_if bus;
endpackage
module counter(input logic clk, output int count);
  initial count = 0;
  always @(posedge clk) count <= count + 1;
endmodule
module hdl_top;
  logic clk = 0;
  int count;
  bus_if bus(clk);
  counter c(clk, count);
  initial forever #5 clk = ~clk;
  always @(posedge clk) bus.data <= bus.data + 10;
  initial begin
    shared::bus = bus;
    $display("%m handed over");
  end
endmodule
module hvl_top;
  initial begin
    #1 $display("%m starts");
    repeat (3) @(posedge shared::bus.clk);
    #1 $display("%m saw %0d %0d %0d", shared::bus.data, hdl_top.count, $root.hdl_top.c.count);
    $finish;
  end
endmodule
"#;

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

#[test]
fn top_level_modules_hand_over_virtual_interfaces_through_packages() {
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(TESTBENCH, Path::new("tb.sv"))], "hdl_top")
                .tops(["hdl_top", "hvl_top"])
                .four_state(four_state)
                .build()
                .unwrap_or_else(|error| panic!("{error}"));
        sim.run_until(100_000).unwrap();
        assert_eq!(
            displays(&mut sim),
            [
                "hdl_top handed over",
                "hvl_top starts",
                "hvl_top saw 30 3 3"
            ],
            "four_state={four_state}"
        );
    }
}

/// Each top is a waveform scope of its own, and a `$dumpvars` argument that
/// names a top, written in another top or after `$root.`, selects it.
#[test]
fn top_level_modules_are_waveform_scopes_of_their_own() {
    for (selection, dumped, left_out) in [
        ("", &["hdl_top", "hdl_top.c", "hvl_top"][..], &[][..]),
        ("0, hdl_top", &["hdl_top", "hdl_top.c"], &["hvl_top"]),
        (
            "0, $root.hdl_top.c",
            &["hdl_top.c"],
            &["hdl_top", "hvl_top"],
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tops.vcd");
        let source = format!(
            r#"
module counter(input logic clk, output int count);
  initial count = 0;
  always @(posedge clk) count <= count + 1;
endmodule
module hdl_top;
  logic clk = 0;
  int count;
  counter c(clk, count);
  initial repeat (4) #5 clk = ~clk;
endmodule
module hvl_top;
  int ticks = 7;
  initial begin
    $dumpfile("{}");
    $dumpvars({selection});
  end
endmodule
"#,
            path.display()
        );
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("vcd.sv"))], "")
                .tops(["hdl_top", "hvl_top"])
                .build()
                .unwrap_or_else(|error| panic!("{error}"));
        sim.run_until(30_000).unwrap();
        sim.flush_vcd().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("$root"), "{selection}: {text}");
        let scopes: Vec<_> = text
            .lines()
            .filter_map(|line| line.strip_prefix("$scope module "))
            .filter_map(|line| line.strip_suffix(" $end"))
            .collect();
        for scope in dumped {
            assert!(scopes.contains(scope), "{selection}: {scope} in {text}");
        }
        for scope in left_out {
            assert!(!scopes.contains(scope), "{selection}: {scope} in {text}");
        }
    }
}

/// Each top keeps its own time unit; `$root` has the finest (3.14.2.3).
#[test]
fn top_level_modules_keep_their_time_units() {
    let source = r#"
module hdl_top;
  timeunit 1ns / 1ps;
  int at = 0;
  initial #3 at = 1;
endmodule
module hvl_top;
  timeunit 10ns / 1ps;
  int at = 0;
  initial #3 at = 1;
endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("units.sv"))], "")
        .tops(["hdl_top", "hvl_top"])
        .build()
        .unwrap_or_else(|error| panic!("{error}"));
    let hdl = sim.signal("hdl_top.at");
    let hvl = sim.child_signal(&[("hvl_top", 0)], "at");
    assert_eq!(hvl, sim.signal("hvl_top.at"));
    sim.run_until(3_000).unwrap();
    assert_eq!(sim.get(hdl), 1u32.into());
    assert_eq!(sim.get(hvl), 0u32.into());
    sim.run_until(30_000).unwrap();
    assert_eq!(sim.get(hvl), 1u32.into());
}

/// VPI and the reflection see each top as a root scope (37.10).
#[cfg(target_arch = "x86_64")]
#[test]
fn top_level_modules_are_reflection_roots() {
    let image = celox::Simulation::from_sv_sources(vec![(TESTBENCH, Path::new("tb.sv"))], "")
        .tops(["hdl_top", "hvl_top"])
        .compile_native()
        .unwrap_or_else(|error| panic!("{error}"))
        .into_program_image();
    let reflection = image.reflection();
    reflection.validate().unwrap();
    let roots: Vec<_> = reflection
        .scopes()
        .iter()
        .filter(|scope| scope.parent.is_none())
        .map(|scope| scope.full_name.as_str())
        .collect();
    // A package is a scope outside the hierarchy.
    assert_eq!(roots, ["hdl_top", "hvl_top", "shared::"]);
    // A package is no module, whatever its name ends with.
    assert!(
        reflection
            .scopes()
            .iter()
            .all(|scope| scope.package == (scope.full_name == "shared::"))
    );
    assert!(
        reflection
            .scope_by_name("hdl_top.c")
            .is_some_and(|(_, scope)| scope.module_name == "counter")
    );
    assert!(reflection.signal_by_name("hdl_top.c.count").is_some());
    assert!(
        reflection
            .scopes()
            .iter()
            .all(|scope| !scope.full_name.contains("$root"))
    );
}

/// A forced signal of a design of several tops is named from a top, as the
/// reflection names it.
#[cfg(target_arch = "x86_64")]
#[test]
fn top_level_modules_force_signals_named_from_a_top() {
    let source = "module a(output logic [7:0] y); logic [7:0] v; always_comb v = 8'd5; assign y = v + 1; endmodule module b; endmodule";
    let simulator = celox::Simulator::from_sv_sources(vec![(source, Path::new("force.sv"))], "")
        .tops(["a", "b"])
        .signal_access(celox::SignalAccessOptions {
            force_signals: vec!["a.v".into()],
            ..Default::default()
        })
        .build()
        .unwrap_or_else(|error| panic!("{error}"));
    let bytes = simulator
        .shared_code()
        .program_image()
        .to_container_bytes()
        .unwrap();
    // Safety: execute only the compiler artifact created by this test.
    let mut sim = unsafe { celox::NativeProgramInstance::from_attached_bytes(&bytes) }.unwrap();
    let id = |sim: &celox::NativeProgramInstance, name: &str| {
        celox::ReflectionSignalId(
            sim.reflection()
                .signals()
                .iter()
                .position(|signal| signal.full_name == name)
                .unwrap_or_else(|| panic!("no {name}")) as u32,
        )
    };
    let v = id(&sim, "a.v");
    assert!(sim.force_signal(v, 9u8.into(), 0u8.into()));
    sim.eval_comb().unwrap();
    let y = sim.reflection().signal(id(&sim, "a.y")).unwrap().signal;
    use celox::SimBackend;
    assert_eq!(sim.backend().get_four_state(y).0, 10u8.into());
}

/// Parameter overrides name the parameters of one top.
#[test]
fn top_level_modules_take_no_parameter_overrides() {
    let source = "module a #(parameter N = 1); endmodule module b; endmodule";
    let error = celox::Simulation::from_sv_sources(vec![(source, Path::new("params.sv"))], "")
        .tops(["a", "b"])
        .param("N", 2)
        .build()
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("parameter overrides"), "{error}");
}

/// The generated root is marked as generated, never told by its name: a
/// module legally named `\$root` is an ordinary top, alone or among several,
/// and the generated root takes a name no module has.
#[test]
fn modules_named_like_the_generated_root_are_ordinary_tops() {
    let source = r#"`timescale 1ns / 1ns
module \$root ; initial #1 $display("%m root"); endmodule
module b; initial #1 $display("%m b"); endmodule
"#;
    let run = |tops: &[&str]| {
        let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("root.sv"))], "")
            .tops(tops.iter().copied())
            .build()
            .unwrap_or_else(|error| panic!("{tops:?}: {error}"));
        sim.run_until(2).unwrap();
        let mut lines = displays(&mut sim);
        lines.sort();
        lines
    };
    let alone = run(&["\\$root"]);
    assert_eq!(alone.len(), 1);
    assert!(
        alone[0].contains("$root") && alone[0].ends_with(" root"),
        "{alone:?}"
    );
    let together = run(&["\\$root", "b"]);
    assert_eq!(together.len(), 2);
    assert!(together.contains(&"b b".to_string()), "{together:?}");
    assert!(
        together
            .iter()
            .any(|line| line.contains("$root") && line.ends_with(" root")),
        "{together:?}"
    );
}

/// The top-level modules are those no source instantiates, an escaped
/// declaration or instantiation naming a module as its simple name does
/// (IEEE 1800-2023 5.6.1, 23.3.1).
#[test]
fn top_level_modules_match_escaped_names() {
    let source = "module \\leaf ; endmodule module mid; leaf u(); endmodule module top; \\mid m(); endmodule";
    assert_eq!(
        celox_frontend_sv::top_level_modules(&[(source, Path::new("escaped.sv"))], false).unwrap(),
        ["top"]
    );
}

/// One top named with `tops` replaces the builder's top in a design of
/// Veryl and SystemVerilog too.
#[test]
fn one_top_named_with_tops_replaces_the_builders_in_mixed_designs() {
    let veryl = "module Top(a: input logic<8>, y: output logic<8>) { inst bus: $sv::Bus; assign bus.req = a; assign y = bus.rsp; }";
    let bus = "interface Bus; logic [7:0] req, rsp; assign rsp = req + 1; endinterface";
    let mut sim = celox::Simulator::from_mixed_sources(
        vec![(veryl, Path::new("top.veryl"))],
        vec![(bus, Path::new("bus.sv"))],
        "Missing",
    )
    .tops(["Top"])
    .build()
    .unwrap_or_else(|error| panic!("{error}"));
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 41u8)).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 42u8.into());
}

/// The design's tops are the selected top-level modules: `PreserveTopPorts`
/// keeps the ports of each, and the host's view of the hierarchy starts at
/// them, never at the root generated for them.
#[test]
fn top_level_modules_are_the_designs_tops_for_the_host() {
    let source = r#"
module a(output logic [7:0] y);
  logic [7:0] r = 8'd6;
  assign y = r + 8'd1;
endmodule
module b(output logic [7:0] z);
  assign z = 8'd9;
endmodule
"#;
    let mut sim = celox::Simulator::from_sv_sources(vec![(source, Path::new("ports.sv"))], "")
        .tops(["a", "b"])
        .opt_level(celox::OptLevel::O2)
        .build()
        .unwrap_or_else(|error| panic!("{error}"));
    let y = sim
        .try_child_signal(&[("a", 0)], "y")
        .unwrap_or_else(|error| panic!("{error}"));
    let z = sim
        .try_child_signal(&[("b", 0)], "z")
        .unwrap_or_else(|error| panic!("{error}"));
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(y), 7u8.into());
    assert_eq!(sim.get(z), 9u8.into());
    let names: Vec<_> = sim
        .named_signals()
        .into_iter()
        .map(|signal| signal.name)
        .collect();
    assert!(
        names.contains(&"a.y".to_string()) && names.contains(&"b.z".to_string()),
        "{names:?}"
    );
    assert!(names.iter().all(|name| name.contains('.')), "{names:?}");
    let hierarchy = sim.named_hierarchy();
    assert_eq!(hierarchy.module_name, "$root");
    assert!(hierarchy.signals.is_empty());
    let children: Vec<_> = hierarchy
        .children
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(children, ["a", "b"]);
    let tops: Vec<_> = sim
        .named_hierarchies()
        .into_iter()
        .map(|(name, hierarchy)| (name, hierarchy.module_name))
        .collect();
    assert_eq!(
        tops,
        [
            ("a".to_string(), "a".to_string()),
            ("b".to_string(), "b".to_string())
        ]
    );
}

/// A time scale query names an instance below another top (20.4.1).
#[test]
fn top_level_modules_answer_time_scale_queries_of_other_tops() {
    let source = r#"
`timescale 1ms / 1us
module top; initial $printtimescale(mod0.m); endmodule
`timescale 1us / 1ns
module mod0; mod1 m(); endmodule
`timescale 1ns / 1ps
module mod1; endmodule
"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("scale.sv"))], "")
        .tops(["mod0", "top"])
        .build()
        .unwrap_or_else(|error| panic!("{error}"));
    sim.run_until(1).unwrap();
    assert_eq!(displays(&mut sim), ["Time scale of (mod0.m) is 1ns / 1ps"]);
}

/// A top whose name is a keyword, declared by its escaped spelling
/// (`\module `, 5.6.1), is found and instantiated as any top.
#[test]
fn tops_named_by_escaped_keywords_are_found_and_run() {
    let source = "`timescale 1ns / 1ns
module \\module ; initial #1 $display(\"%m keyword\"); endmodule
module b; initial #1 $display(\"%m b\"); endmodule";
    let path = Path::new("keyword.sv");
    let tops = celox_frontend_sv::top_level_modules(&[(source, path)], false).unwrap();
    // `\\module` names `module` (5.6.1); the generated root escapes it.
    assert_eq!(tops, ["b", "module"]);
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, path)], "")
        .tops(tops)
        .build()
        .unwrap_or_else(|error| panic!("{error}"));
    sim.run_until(2).unwrap();
    let mut lines = displays(&mut sim);
    lines.sort();
    assert_eq!(lines, ["b b", "module keyword"]);
}

/// An escaped reference names the module a `begin_keywords` region declares
/// without escaping (5.6.1, 22.14): under 1364-2005 `checker` is not a
/// keyword, so `\checker` instantiates it and it is no top.
#[test]
fn escaped_references_name_modules_of_older_keyword_versions() {
    let source = "`timescale 1ns / 1ns
`begin_keywords \"1364-2005\"
module checker; initial #1 $display(\"%m checker\"); endmodule
`end_keywords
module a; \\checker c (); endmodule
module b; initial #1 $display(\"%m b\"); endmodule";
    let path = Path::new("keywords.sv");
    let tops = celox_frontend_sv::top_level_modules(&[(source, path)], false).unwrap();
    assert_eq!(tops, ["a", "b"]);
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, path)], "")
        .tops(tops)
        .build()
        .unwrap_or_else(|error| panic!("{error}"));
    sim.run_until(2).unwrap();
    let mut lines = displays(&mut sim);
    lines.sort();
    assert_eq!(lines, ["a.c checker", "b b"]);
}

/// A module declared by an escaped name is the module of its simple name
/// (5.6.1), as a top and as an instance.
#[test]
fn escaped_module_declarations_are_named_by_their_simple_spelling() {
    let source = "`timescale 1ns / 1ns
module \\foo ; initial #1 $display(\"%m foo\"); endmodule
module top; foo f (); endmodule";
    let path = Path::new("escaped.sv");
    let tops = celox_frontend_sv::top_level_modules(&[(source, path)], false).unwrap();
    assert_eq!(tops, ["top"]);
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, path)], "top")
        .build()
        .unwrap_or_else(|error| panic!("{error}"));
    sim.run_until(2).unwrap();
    assert_eq!(displays(&mut sim), ["top.f foo"]);
}

/// A path names a signal from a top only when it starts with a top's name; a
/// package variable whose escaped package name holds a dot is found whole.
#[test]
fn signal_paths_split_only_at_a_top() {
    let source = "package \\p.q ; logic [7:0] v = 8'd5; endpackage
module a; logic [7:0] x; initial x = \\p.q ::v; endmodule
module b; logic [7:0] y = 8'd7; endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("paths.sv"))], "")
        .tops(["a", "b"])
        .build()
        .unwrap_or_else(|error| panic!("{error}"));
    sim.run_until(1).unwrap();
    let v = sim.signal("\\p.q::v");
    assert_eq!(sim.get(v), 5u8.into());
    let y = sim.signal("b.y");
    assert_eq!(sim.get(y), 7u8.into());
}

/// The packages of the classes the interface instance of the next tests
/// handles, and the interface, whose file imports them where `hdl_top`'s
/// file does not (IEEE 1800-2023 26.3).
const CLASS_PACKAGES: &str = r#"
package base_pkg;
  virtual class obj;
    string m_name;
    function new(string name); m_name = name; endfunction
    virtual function string get_name(); return m_name; endfunction
    virtual function string get_type_name(); return "obj"; endfunction
  endclass
  class seq #(type T = int) extends obj;
    function new(string name); super.new(name); endfunction
  endclass
  class sequencer;
    obj seqs[$];
    function void report();
      $display("name '%s' type '%s'", seqs[0].get_name(), seqs[0].get_type_name());
    endfunction
  endclass
endpackage
package agent_pkg;
  import base_pkg::*;
  class item; endclass
  class my_seq extends seq #(item);
    function new(string name = "my_seq"); super.new(name); endfunction
    virtual function string get_type_name(); return "my_seq"; endfunction
  endclass
  class driver; sequencer s; function void end_transfer(); s.report(); endfunction endclass
endpackage
"#;
const CLASS_BFM: &str = r#"
import agent_pkg::*;
interface bfm(input logic clk);
  driver proxy;
  bit start;
  always @(posedge clk) if (start) begin start = 0; proxy.end_transfer(); end
endinterface
package config_pkg; virtual bfm h; endpackage
package env_pkg;
  import base_pkg::*;
  import agent_pkg::*;
  function automatic void make();
    my_seq q;
    driver d;
    sequencer s;
    q = new("q1"); d = new; s = new;
    s.seqs.push_back(q);
    d.s = s;
    config_pkg::h.proxy = d;
  endfunction
endpackage
"#;

/// The processes of an interface instance run in the scope that binds it:
/// a virtual method they call on an object of a class that only the
/// interface's imports name dispatches to that class's override, also
/// when the binding scope uses the package of the base class (8.20).
#[test]
fn interface_instances_dispatch_to_the_classes_they_see() {
    let tops = r#"
module hdl_top;
  logic clk = 0;
  base_pkg::obj keep;
  bfm b(clk);
  initial forever #10 clk = ~clk;
  initial config_pkg::h = b;
endmodule
module hvl_top;
  initial begin
    #1 env_pkg::make();
    config_pkg::h.start = 1;
    #50 $finish;
  end
endmodule
"#;
    for four_state in [false, true] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![
                (CLASS_PACKAGES, Path::new("packages.sv")),
                (CLASS_BFM, Path::new("bfm.sv")),
                (tops, Path::new("tops.sv")),
            ],
            "hdl_top",
        )
        .tops(["hdl_top", "hvl_top"])
        .four_state(four_state)
        .build()
        .unwrap_or_else(|error| panic!("{error}"));
        sim.run_until(100_000).unwrap();
        assert_eq!(
            displays(&mut sim),
            ["name 'q1' type 'my_seq'"],
            "four_state={four_state}"
        );
    }
}

/// The classes an interface instance sees are not names of the scope that
/// binds it (26.3).
#[test]
fn classes_an_interface_instance_sees_stay_hidden_from_its_binding_scope() {
    let tops = r#"
module hdl_top;
  logic clk = 0;
  my_seq hidden;
  bfm b(clk);
endmodule
"#;
    let built = celox::Simulation::from_sv_sources(
        vec![
            (CLASS_PACKAGES, Path::new("packages.sv")),
            (CLASS_BFM, Path::new("bfm.sv")),
            (tops, Path::new("tops.sv")),
        ],
        "hdl_top",
    )
    .build();
    let error = match built {
        Ok(_) => panic!("`my_seq` is not a name of `hdl_top`"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("my_seq"), "{error}");
}
