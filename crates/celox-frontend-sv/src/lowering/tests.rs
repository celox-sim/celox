use super::*;

#[test]
fn locator_locals_take_their_dimensions_from_the_type_table() {
    // The iterator of `find` is a local the analyzer makes for the call
    // (a locator local), of the element type `logic [W-1:0]`, whose range
    // stays as written: it is typed through the type table as every
    // declared local is.
    let source = "module Top #(parameter int W = 8)(output int y);
                      logic [W-1:0] q[$];
                      initial begin
                          int k = 2;
                          q.push_back(8'h04);
                          y = q.find(x) with (x[k] == 1'b1).size();
                      end
                      endmodule";
    let ir = sv::analyze_source(source, Path::new("locator.sv")).unwrap();
    let local = ir.modules()[0]
        .locals()
        .iter()
        .find(|local| local.source_name == "x")
        .expect("the iterator `x`");
    let info =
        signal_type_from_sv(&local.r#type, &HashMap::default(), &HashMap::default()).unwrap();
    assert_eq!(info.packed_ranges, vec![(7, 0)]);
}

#[test]
fn packed_dimensions_come_from_the_type_table() {
    // `W` is a parameter, not a local parameter, so the analyzer keeps
    // the local's range `[W-1:0]` as written: the frontend takes its
    // bounds from the analysis's type table, whatever its own constants
    // hold. UVM's `uvm_hdl_data_t` (`logic [UVM_HDL_MAX_WIDTH-1:0]`)
    // reached the frontend so, whose constants held the parameter only
    // by its qualified name: the dimension was left out.
    let source = "module Top #(parameter int W = 8)(output int y); \
                      initial begin logic [W-1:0] v; v = '1; y = v; end endmodule";
    let ir = sv::analyze_source(source, Path::new("bounds.sv")).unwrap();
    let module = &ir.modules()[0];
    let local = module
        .locals()
        .iter()
        .find(|local| local.source_name == "v")
        .expect("the local `v`");
    let range = &local.r#type.packed_ranges()[0];
    assert!(
        !matches!(range.left(), sv::ir::ConstExpr::Literal(_)),
        "the range stays as written: {:?}",
        range.left()
    );
    let info =
        signal_type_from_sv(&local.r#type, &HashMap::default(), &HashMap::default()).unwrap();
    assert_eq!(info.packed_ranges, vec![(7, 0)]);
    assert_eq!(info.width, 8);
}

#[test]
fn shared_always_temporaries_exclude_queued_writes() {
    // The syntax frontend currently rejects untimed wildcard NBAs before
    // lowering. Exercise the IR transformation directly so it remains safe
    // when such a process reaches this pass.
    for (left, right) in [
        ("j=a; x=j;", "j=b; y=j;"),
        ("{j}=a; x=j;", "{j}=b; y=j;"),
        (
            "j=0; case(a) default:j=a; endcase x=j;",
            "j=0; case(b) default:{j}=b; endcase y=j;",
        ),
    ] {
        let source = format!(
            "module Top(input bit a,b, output bit x,y); bit j; \
                 always @* begin {left} end always @* begin {right} end endmodule"
        );
        let ir = sv::analyze_source(&source, Path::new("shared_nba.sv")).unwrap();
        let module = ir.modules()[0]
            .clone()
            .map_comb_process_bodies(|_, mut body, sensitivity| {
                if !sensitivity {
                    for stmt in &mut body {
                        stmt.walk_mut(&mut |stmt| match stmt {
                            sv::ir::Stmt::Assign {
                                lhs,
                                rhs: sv::ir::Expr::Ident(_),
                                nonblocking,
                            } if lhs.name() == "j" => *nonblocking = true,
                            sv::ir::Stmt::AssignConcat {
                                parts, nonblocking, ..
                            } if parts.iter().any(|part| part.name() == "j") => *nonblocking = true,
                            _ => {}
                        });
                    }
                }
                body
            });
        let mut warnings = Vec::new();
        let (converted, shared) = share_always_variables(
            &module,
            &module_constants_with_overrides(&module, &[]),
            &module_parameter_types(&module),
            SharedAlways {
                temporaries: true,
                foreign_references: &HashSet::default(),
            },
            &mut warnings,
        )
        .unwrap();
        assert!(shared.contains("j"), "{source}");
        assert!(warnings.is_empty(), "{source}");
        assert!(converted.comb_processes().is_empty(), "{source}");
        assert_eq!(converted.initial_processes().len(), 2, "{source}");
    }
}

std::thread_local! {
    // Observe the real compilation path without sharing counters between
    // concurrently running tests or adding instrumentation to release builds.
    #[allow(clippy::disallowed_types, reason = "test-local specialization observations")]
    static SPECIALIZATIONS: std::cell::RefCell<Option<Vec<(LoweredSvModuleKey, bool)>>> = const {
        std::cell::RefCell::new(None)
    };
    #[allow(clippy::disallowed_types, reason = "test-local analysis observations")]
    static ANALYSES: std::cell::RefCell<Option<Vec<LoweredSvModuleKey>>> = const {
        std::cell::RefCell::new(None)
    };
}

pub(super) fn record_analysis(key: &LoweredSvModuleKey) {
    ANALYSES.with_borrow_mut(|records| {
        if let Some(records) = records {
            records.push(key.clone());
        }
    });
}

pub(super) fn record_specialization(key: &LoweredSvModuleKey, lists: LevelLists<'_>) {
    SPECIALIZATIONS.with_borrow_mut(|records| {
        if let Some(records) = records {
            records.push((key.clone(), matches!(lists, LevelLists::Lowered { .. })));
        }
    });
}

#[test]
fn stateless_function_scratch_loops_do_not_become_runtime_loops() {
    // The second result keeps bit 15 of every row between calls; nothing
    // reads it, so each call still returns the same constant.
    let sources = [16, 15].map(|columns| {
        format!(
            r#"module Top(input wire [255:0] d, output wire [255:0] q);
            function [15:0][15:0] pattern();
                logic [15:0] words [15:0];
                for (int i=0; i<16; i++) words[i] = 16'b1 << i;
                for (int i=0; i<16; i++) for (int j=0; j<{columns}; j++) pattern[i][j] = words[i][j];
            endfunction
            assign q = d ^ pattern();
        endmodule"#
        )
    });
    for (source, four_state) in sources
        .iter()
        .flat_map(|source| [(source, false), (source, true)])
    {
        let output = schedule_sources(
            &[(source.as_str(), Path::new("top.sv"))],
            "Top",
            &[],
            &[],
            &[],
            four_state,
            &celox_frontend_core::ParallelScheduleOptions::default(),
            &FrontendTraceOptions::default(),
            None,
            &mut Vec::new(),
        )
        .unwrap();
        let instructions: usize = output
            .scheduled
            .sir
            .eval_comb
            .iter()
            .flat_map(|unit| unit.blocks.values())
            .map(|block| block.instructions.len())
            .sum();
        assert!(
            instructions < 32,
            "constant scratch loops produced {instructions} runtime instructions"
        );
    }
}

#[test]
fn bit_select_width_does_not_evaluate_a_runtime_zero_sample() {
    let index = sv::ir::ConstExpr::Ident("index".into());
    let bound = sv::ir::ConstExpr::Binary {
        left: Box::new(sv::ir::ConstExpr::Literal("255".into())),
        op: sv::ir::BinaryOp::Div,
        right: Box::new(index.clone()),
    };
    let names = HashMap::from_iter([("index".into(), SourceVarId(0))]);
    let constants = HashMap::default();
    let types = HashMap::default();
    assert_eq!(
        runtime_select_width(&bound, &bound, &names, &constants, &types),
        Some(1)
    );
    let high = sv::ir::ConstExpr::Binary {
        left: Box::new(index.clone()),
        op: sv::ir::BinaryOp::Add,
        right: Box::new(sv::ir::ConstExpr::Literal("7".into())),
    };
    assert_eq!(
        runtime_select_width(&high, &index, &names, &constants, &types),
        Some(8)
    );
    let constant = sv::ir::ConstExpr::Literal("3".into());
    assert_eq!(
        runtime_select_width(&constant, &constant, &names, &constants, &types),
        None
    );
}

/// The per-name driver scan [`ModuleDrivers`] replaced, which walks every
/// process for each name.
mod per_name_scan;

/// [`ModuleDrivers`] finds the drivers the per-name scan finds, in the
/// same order, for every variable and port.
#[test]
fn module_drivers_match_per_name_scan() {
    let source = r#"
module Child(input logic [7:0] i, output logic [7:0] o);
  assign o = i;
endmodule

module Top(input logic clk, input logic [3:0] idx, input logic [7:0] d,
           output logic [7:0] q, output logic [15:0] w, inout wire [3:0] io);
  logic [7:0] mem [0:15];
  logic [7:0] a, b, c;
  logic [3:0] wide [0:3][0:1];
  logic [7:0] f_out, t_out;
  integer plus;
  wire [7:0] n0, n1;
  wire [15:0] cat;
  localparam bit EN = 1'b1;

  function automatic void set_f(output logic [7:0] r, input logic [7:0] v);
    r = v;
  endfunction

  task automatic set_t(output logic [7:0] r);
    r = 8'h5;
  endtask

  assign w[7:0] = a;
  assign w[15:8] = b;
  assign {n0, n1} = cat;
  assign cat = {a, b};
  assign io[1:0] = 2'b01;

  always_comb begin
    c = a;
    c[3:0] = b[3:0];
    set_f(f_out, d);
  end

  always_ff @(posedge clk) begin
    mem[idx] <= d;
    mem[idx][3:0] <= d[3:0];
    mem[3][idx[2:0]] <= d[0];
    wide[idx[1:0]][0] <= d[3:0];
    q <= mem[idx];
  end

  always @(posedge clk) a = d;
  always @(posedge clk) b = a;

  always begin
    #1 set_t(t_out);
  end

  initial begin
    a = 8'h0;
    if ($value$plusargs("p=%d", plus)) b = 8'h1;
  end

  generate
    if (EN) begin : g
      always_comb t_out = d;
    end
  endgenerate

  Child u (.i(d), .o(n0));
endmodule
"#;
    let ir =
        sv::analyze_source(source, Path::new("top.sv")).unwrap_or_else(|error| panic!("{error}"));
    let constants = HashMap::default();
    let parameter_types = HashMap::default();
    let mut checked = 0;
    for module in ir.modules() {
        let drivers = ModuleDrivers::new(module, &constants, &parameter_types);
        let names = module
            .signals()
            .iter()
            .map(|signal| signal.name())
            .chain(module.ports().iter().map(|port| port.name()));
        for name in names {
            assert_eq!(
                drivers.local(name),
                per_name_scan::local_driver_ranges(module, name, &constants, &parameter_types),
                "local drivers of `{name}` in `{}`",
                module.name()
            );
            assert_eq!(
                drivers.continuous(name),
                per_name_scan::continuous_driver_ranges(module, name, &constants, &parameter_types),
                "continuous drivers of `{name}` in `{}`",
                module.name()
            );
            assert_eq!(
                drivers.initial(name),
                per_name_scan::initial_driver_ranges(module, name, &constants, &parameter_types),
                "initial drivers of `{name}` in `{}`",
                module.name()
            );
            checked += usize::from(!drivers.local(name).is_empty())
                + usize::from(!drivers.initial(name).is_empty());
        }
    }
    assert!(
        checked > 10,
        "the source has drivers to compare ({checked})"
    );
}

#[test]
fn parameter_override_validation_rejects_unresolved_values() {
    let overrides = [LoweredSvParameterOverride {
        name: "P".into(),
        value: Some(sv::ir::ConstExpr::Ident("unresolved".into())),
        bound_type: None,
        bound_pattern: None,
    }];
    let error = evaluated_parameter_overrides(&overrides).unwrap_err();
    assert_eq!(error.tracking_issue(), 461);
    assert!(
        error
            .to_string()
            .contains("nonconstant module parameter override `P`"),
        "{error}"
    );
}

#[test]
fn derived_long_string_parameters_give_format_overrides_their_value() {
    let source = "\
        module Leaf #(parameter string ID = \"\") (); endmodule \
        module Mid #(parameter string ID = \"a-very-long-instance-name\", \
                     parameter string TAG = ID) (); \
            Leaf #(.ID($sformatf(\"%s-alu\", TAG))) alu(); \
        endmodule";
    let ir = sv::analyze_source(source, Path::new("derived.sv")).unwrap();
    let module = ir
        .modules()
        .iter()
        .find(|module| module.name() == "Mid")
        .unwrap();
    let instances = lower_instances(
        module,
        &module_constants_with_overrides(module, &[]),
        &module_parameter_types(module),
    )
    .unwrap();
    assert_eq!(instances.len(), 1);
    let evaluated = evaluated_parameter_overrides(&instances[0].parameter_overrides)
        .expect("the derived string parameter is a constant override value");
    assert!(matches!(evaluated["ID"], sv::ir::ConstExpr::Literal(_)));
}

/// The process kernels of `source`, other than declaration initializers.
fn procedure_kernels(source: &str) -> usize {
    let scheduled = schedule_sources(
        &[(source, Path::new("top.sv"))],
        "Top",
        &[],
        &[],
        &[],
        false,
        &celox_frontend_core::ParallelScheduleOptions::default(),
        &FrontendTraceOptions::default(),
        None,
        &mut Vec::new(),
    )
    .unwrap_or_else(|error| panic!("{error}"));
    scheduled
        .scheduled
        .runtime_schema
        .processes
        .iter()
        .filter(|process| process.kind != celox_design::ProcessKind::Initializer)
        .count()
}

/// The targets of the `always_comb` alternatives the modules of `source`
/// lowered, by name.
fn comb_alternatives(source: &str) -> BTreeSet<String> {
    try_comb_alternatives(source, false).unwrap_or_else(|error| panic!("{error}"))
}

fn try_comb_alternatives(
    source: &str,
    four_state: bool,
) -> Result<BTreeSet<String>, FrontendError> {
    let mut trace = FrontendTrace::default();
    schedule_sources(
        &[(source, Path::new("top.sv"))],
        "Top",
        &[],
        &[],
        &[],
        four_state,
        &celox_frontend_core::ParallelScheduleOptions::default(),
        &FrontendTraceOptions {
            sim_modules: true,
            ..Default::default()
        },
        Some(&mut trace),
        &mut Vec::new(),
    )?;
    Ok(trace
        .sim_modules
        .expect("traced modules")
        .values()
        .flat_map(|module| {
            module.comb_alternatives.iter().flat_map(|alternative| {
                alternative
                    .targets
                    .iter()
                    .map(|target| module.variables[&target.id].path.join("."))
            })
        })
        .collect())
}

/// In two-state mode an `always @*` whose variables it alone drives is
/// lowered symbolically like an `always_comb`, with the imperative
/// alternative for a watched target; in four-state mode, where it waits
/// for its first source event, and where it refuses the symbolic form,
/// it stays the guarded unit, which has no alternative.
#[test]
fn wildcard_always_is_combinational_in_two_state() {
    let watched = |process: &str| {
        format!(
            "module Top(input bit [7:0] s);
                   bit [7:0] y;
                   {process}
                   initial forever @(y) ;
                 endmodule"
        )
    };
    let wildcard = watched("always @* begin y = 0; if (s[0]) y = s; end");
    assert!(
        try_comb_alternatives(&wildcard, false)
            .unwrap()
            .contains("y")
    );
    assert!(
        !try_comb_alternatives(&wildcard, true)
            .unwrap()
            .contains("y")
    );
    let list = watched("always @(s) begin y = 0; if (s[0]) y = s; end");
    assert!(try_comb_alternatives(&list, false).unwrap().contains("y"));
    let latching = watched("always @* if (s[0]) y = s;");
    assert!(
        !try_comb_alternatives(&latching, false)
            .unwrap()
            .contains("y")
    );
    let wildcard_labels = watched(
        "always @* begin y = 0; casez (s) 8'b1???????: y = 1; 8'b01??????: y = 2; endcase end",
    );
    assert!(
        try_comb_alternatives(&wildcard_labels, false)
            .unwrap()
            .contains("y")
    );
    let parts = watched(
        "always @* begin y[3:0] = 0; if (s[0]) y[3:0] = s[3:0]; end
             always @(s) y[7:4] = s[7:4];",
    );
    assert!(try_comb_alternatives(&parts, false).unwrap().contains("y"));
}

/// An `always_comb` that stores a variable twice gets its imperative
/// alternative only where a wait or a flip-flop domain may watch it,
/// directly or through any expression of combinational logic or a port.
#[test]
fn comb_alternatives_only_for_watchable_targets() {
    let multi_store = "always_comb begin y = 0; if (s[0]) y = s; end";
    for (case, body, keeps) in [
        ("unwatched", "bit u; initial forever @(u) ;", false),
        ("source waited", "initial forever @(s) ;", true),
        ("waited", "initial forever @(y) ;", true),
        (
            "part-select",
            "bit [3:0] w; assign w = y[3:0]; initial forever @(w) ;",
            true,
        ),
        (
            "concatenation",
            "bit [15:0] w; assign w = {y, s}; initial forever @(w) ;",
            true,
        ),
        (
            "arithmetic",
            "bit [8:0] w; assign w = y + 9'd1; initial forever @(w) ;",
            true,
        ),
        (
            "aliased clock",
            "bit c; assign c = y[0]; bit q; always_ff @(posedge c) q <= ~q;
                 bit u; initial forever @(u) ;",
            true,
        ),
        (
            "output argument of an expression call",
            "function automatic bit copy(input bit v, output bit o);
                   o = v; return 1;
                 endfunction
                 bit w; always_comb if (copy(y[0], w)) begin end
                 bit q; always_ff @(posedge w) q <= ~q;
                 bit t; initial forever @(t) ;",
            true,
        ),
        (
            "output argument of a nested call",
            "function automatic bit draw(output int o);
                   return $random(o) == 0;
                 endfunction
                 int w; always @* if (draw(w) && y[0]) begin end
                 initial forever @(w) ;",
            true,
        ),
        (
            "argument a system function writes",
            "int w; always @* if ($random(w) == 0 && y[0]) begin end
                 initial forever @(w) ;",
            true,
        ),
        (
            "nonblocking switch",
            "bit [7:0] t; bit u; initial forever @(u) t <= u;",
            true,
        ),
    ] {
        let source = format!(
            "module Top(input bit [7:0] s);
                   bit [7:0] y;
                   {multi_store}
                   {body}
                 endmodule"
        );
        assert_eq!(comb_alternatives(&source).contains("y"), keeps, "{case}");
    }
    let child = "module Child(input bit [7:0] s, output bit [7:0] y);
               always_comb begin y = 0; if (s[0]) y = s; end
             endmodule";
    let port = "module Top(input bit [7:0] s);
               bit [7:0] y;
               Child child(.s(s), .y(y));
               initial forever @(y) ;
             endmodule";
    assert!(
        comb_alternatives(&format!("{child}\n{port}")).contains("y"),
        "child output port"
    );
    // A hierarchical name reaches the variables of another module too.
    let hierarchical = "module Top(input bit [7:0] s);
               Child child(.s(s), .y());
               initial forever @(child.y) ;
             endmodule";
    assert!(
        comb_alternatives(&format!("{child}\n{hierarchical}")).contains("y"),
        "hierarchical name"
    );
}

/// A level list naming the bits of the implicit event expression in
/// other pieces is that expression, whatever the body does with its
/// targets; a list of more bits than the body reads is not, once the
/// body reads a variable it writes.
#[test]
fn level_lists_equal_to_the_implicit_reads_bitwise_are_combinational() {
    let same_bits = "module Top(input bit [31:0] w, input bit [1:0] a, output bit [7:0] y);
               always @(w[7:0] or w[31:8] or a) begin
                 y = 8'd0;
                 case (a)
                   2'd0: y = w[7:0];
                   2'd1: y = w[15:8];
                   2'd2: y = w[23:16];
                   2'd3: y = w[31:24];
                 endcase
               end
               initial begin #1; end
             endmodule";
    assert_eq!(procedure_kernels(same_bits), 1);
    let more_bits = "module Top(input bit [31:0] w, input bit [1:0] a, output bit [7:0] y);
               always @(w[7:0] or w[31:8] or a) y = y ^ w[15:8] ^ {6'd0, a};
               initial begin #1; end
             endmodule";
    assert_eq!(procedure_kernels(more_bits), 2);
}

/// Only the `always @(list)` processes of a combinational loop stay
/// processes; one `initial` block drives each design.
#[test]
fn level_lists_in_a_combinational_loop_stay_processes() {
    for (source, kernels) in [
        (
            "module Top(output bit [7:0] a, b, c);
                   bit [7:0] s;
                   initial begin #1; s = 1; end
                   always @(s, b) a = b + s;
                   always @(a) c = a;
                 endmodule",
            1,
        ),
        (
            "module Top(output bit [7:0] a, b);
                   initial begin #1; a = 1; end
                   always @(a) if (a < 4) b = a;
                   always @(b) if (b < 4) a = b + 1;
                 endmodule",
            3,
        ),
        (
            "module Top(output bit [7:0] a, b);
                   bit [7:0] s;
                   initial begin #1; s = 1; end
                   always @(s or b) a = (b < 4) ? b + s : b;
                   always_comb b = (a < 4) ? a : 8'd3;
                 endmodule",
            2,
        ),
        (
            "module Top(output bit [7:0] a, b, c);
                   bit [7:0] s;
                   initial begin #1; s = 1; end
                   always @(s or b) a = (b < 4) ? b + s : b;
                   assign b = (a < 4) ? a : 8'd3;
                   always @(b) c = b + 8'd10;
                 endmodule",
            2,
        ),
    ] {
        assert_eq!(procedure_kernels(source), kernels, "{source}");
    }
}

#[test]
fn level_list_retry_reuses_unaffected_parameter_specializations() {
    let source = "module Leaf #(parameter N = 8)
                        (input bit [N-1:0] d, output wire [N-1:0] q);
                        for (genvar i = 0; i < N; i++) assign q[i] = ~d[i];
                      endmodule
                      module Top(output bit [7:0] a, b, output wire [7:0] y,
                                 output wire [15:0] z);
                        initial begin #1; a = 1; end
                        always @(a) if (a < 4) b = a;
                        always @(b) if (b < 4) a = b + 1;
                        Leaf #(8) byte_leaf(.d(a), .q(y));
                        Leaf #(16) word_leaf(.d({a, b}), .q(z));
                      endmodule";
    SPECIALIZATIONS.set(Some(Vec::new()));
    let kernels = procedure_kernels(source);
    let observations = SPECIALIZATIONS.take().unwrap();
    assert_eq!(kernels, 3, "the affected level lists remain processes");
    let mut lowered = HashMap::<LoweredSvModuleKey, usize>::default();
    for (key, final_lowering) in observations {
        if final_lowering {
            *lowered.entry(key).or_default() += 1;
        }
    }
    assert_eq!(lowered.len(), 3, "Top and two distinct Leaf parameters");
    assert_eq!(lowered[&LoweredSvModuleKey::base("Top".into())], 2);
    let leaves: Vec<_> = lowered
        .iter()
        .filter(|(key, _)| key.name == "Leaf")
        .collect();
    assert_eq!(leaves.len(), 2);
    assert!(leaves.iter().all(|(_, count)| **count == 1), "{leaves:?}");
}

#[test]
fn hierarchy_discovery_analyses_are_reused_by_parameter_identity() {
    let source = "module Leaf #(parameter N = 8)
                        (input bit [N-1:0] d, output wire [N-1:0] q);
                        for (genvar i = 0; i < N; i++) assign q[i] = ~d[i];
                      endmodule
                      module Top(input bit [15:0] d, output wire [7:0] y,
                                 output wire [15:0] z);
                        Leaf #(8) byte_leaf(.d(d[7:0]), .q(y));
                        Leaf #(16) word_leaf(.d(d), .q(z));
                      endmodule";
    for four_state in [false, true] {
        ANALYSES.set(Some(Vec::new()));
        SPECIALIZATIONS.set(Some(Vec::new()));
        let result = schedule_sources(
            &[(source, Path::new("discovery.sv"))],
            "Top",
            &[],
            &[],
            &[],
            four_state,
            &celox_frontend_core::ParallelScheduleOptions::default(),
            &FrontendTraceOptions::default(),
            None,
            &mut Vec::new(),
        );
        let observations = ANALYSES.take().unwrap();
        let lowerings = SPECIALIZATIONS.take().unwrap();
        result.unwrap_or_else(|error| panic!("{error}"));
        let mut counts = HashMap::<LoweredSvModuleKey, usize>::default();
        for key in observations {
            *counts.entry(key).or_default() += 1;
        }
        assert_eq!(
            lowerings.len(),
            3,
            "lower each specialization only for execution"
        );
        assert!(
            lowerings.iter().all(|(_, final_lowering)| *final_lowering),
            "hierarchy discovery must not lower executable bodies: {lowerings:?}"
        );
        assert_eq!(counts.len(), 3, "Top and two Leaf parameterizations");
        assert!(counts.values().all(|&count| count == 1), "{counts:?}");
    }
}

#[test]
fn discovery_reuse_keeps_hierarchical_resolution_separate() {
    let source = "module Leaf(input bit [7:0] d, output wire [7:0] q);
                        assign q = d + 1;
                      endmodule
                      module Top(input bit [7:0] d, output wire [7:0] y);
                        Leaf leaf(.d(d), .q());
                        assign y = leaf.q;
                      endmodule";
    for four_state in [false, true] {
        ANALYSES.set(Some(Vec::new()));
        let result = schedule_sources(
            &[(source, Path::new("hierarchical-discovery.sv"))],
            "Top",
            &[],
            &[],
            &[],
            four_state,
            &celox_frontend_core::ParallelScheduleOptions::default(),
            &FrontendTraceOptions::default(),
            None,
            &mut Vec::new(),
        );
        let observations = ANALYSES.take().unwrap();
        result.unwrap_or_else(|error| panic!("{error}"));
        let count = |name: &str| observations.iter().filter(|key| key.name == name).count();
        assert_eq!(count("Leaf"), 1, "reuse the independent child analysis");
        assert!(count("Top") >= 2, "resolve the parent's hierarchical names");
    }
}

#[test]
fn power_signedness_follows_base_with_self_determined_exponent() {
    let variables = HashMap::default();
    let names = HashMap::default();
    let parameters = HashMap::default();
    for (base, exponent, signed) in [
        ("2", "4'd2", true),
        ("8'shfd", "4'd3", true),
        ("8'd2", "2", false),
    ] {
        let expression = sv::ir::Expr::Binary {
            left: Box::new(sv::ir::Expr::Literal(base.into())),
            op: sv::ir::BinaryOp::Pow,
            right: Box::new(sv::ir::Expr::Literal(exponent.into())),
        };
        assert_eq!(
            sv_glue_expr_is_signed(&expression, &variables, &names, &parameters),
            signed,
            "port connection: {base} ** {exponent}"
        );
        assert_eq!(
            sv_expr_is_signed_with_parameters(&expression, &variables, &names, &parameters),
            signed,
            "expression: {base} ** {exponent}"
        );
    }
}

#[test]
fn select_sources_adds_nested_offsets_and_keeps_computed_dependencies() {
    let nested = sv::ir::Expr::Select {
        expr: Box::new(sv::ir::Expr::Ident("a".to_string())),
        msb: sv::ir::ConstExpr::Literal("15".to_string()),
        lsb: sv::ir::ConstExpr::Literal("8".to_string()),
        signed: false,
    };
    let nested_sources = HashSet::from_iter([VarAtomBase::new(1u8, 8, 15)]);
    let narrowed = select_sources(&nested, nested_sources, BitAccess::new(0, 0)).unwrap();
    assert_eq!(narrowed, HashSet::from_iter([VarAtomBase::new(1u8, 8, 8)]));

    let computed = sv::ir::Expr::Binary {
        left: Box::new(sv::ir::Expr::Ident("a".to_string())),
        op: sv::ir::BinaryOp::Add,
        right: Box::new(sv::ir::Expr::Ident("b".to_string())),
    };
    let computed_sources =
        HashSet::from_iter([VarAtomBase::new(1u8, 0, 7), VarAtomBase::new(2u8, 0, 7)]);
    assert_eq!(
        select_sources(&computed, computed_sources.clone(), BitAccess::new(7, 7)).unwrap(),
        computed_sources
    );
}
