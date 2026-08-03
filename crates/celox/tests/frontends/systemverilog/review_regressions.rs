use super::*;

fn cranelift_build_error(source: &str) -> String {
    match Simulator::from_sv_sources(vec![(source, Path::new("review.sv"))], "Top")
        .build_cranelift()
    {
        Ok(_) => panic!("unsupported SystemVerilog unexpectedly compiled"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn merges_always_ff_processes_with_the_same_trigger() {
    let source = r#"
        module Top(input logic clk, input logic a, input logic b,
                   output logic qa, output logic qb);
            always_ff @(posedge clk) qa <= a;
            always_ff @(posedge clk) qb <= b;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("shared_ff.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let clk = sim.event("clk");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let qa = sim.signal("qa");
    let qb = sim.signal("qb");
    sim.modify(|io| {
        io.set(a, 1u8);
        io.set(b, 1u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(qa), 1u8.into());
    assert_eq!(sim.get(qb), 1u8.into());
}

#[test]
fn uses_the_first_always_ff_event_as_a_negedge_clock() {
    let source = r#"
        module Top(input logic clk, input logic rst, input logic d, output logic q);
            always_ff @(negedge clk or posedge rst) begin
                if (rst) q <= 1'b0;
                else q <= d;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("negedge_ff.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d = sim.signal("d");
    let q = sim.signal("q");
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(d, 1u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u8.into());
}

#[test]
fn coerces_hierarchy_widths_and_leaves_omitted_ports_unconnected() {
    let source = r#"
        module Child(input logic [7:0] i, input logic omitted, output logic o);
            assign o = i[0] | omitted;
        endmodule
        module Top(input logic a, input logic omitted, output logic [7:0] y);
            Child child(.i(a), .o(y));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("widths.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let a = sim.signal("a");
    let omitted = sim.signal("omitted");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(a, 1u8);
        io.set(omitted, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 1u8.into());
}

#[test]
fn forwards_top_parameter_overrides_for_pure_sv() {
    let source = r#"
        module Top #(parameter VALUE = 1) (output logic [3:0] y);
            assign y = VALUE;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("param.sv"))], "Top")
        .param("VALUE", 9)
        .build_cranelift()
        .unwrap();
    let y = sim.signal("y");
    assert_eq!(sim.get(y), 9u8.into());
}

#[test]
fn preserves_signed_operations_and_assignment_extension() {
    let source = r#"
        module Top(input logic signed [7:0] a,
                   output logic signed [15:0] extended,
                   output logic signed [7:0] divided,
                   output logic less);
            assign extended = a;
            assign divided = a / 8'sd2;
            assign less = a < 8'sd1;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("signed.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let a = sim.signal("a");
    let extended = sim.signal("extended");
    let divided = sim.signal("divided");
    let less = sim.signal("less");
    sim.modify(|io| io.set(a, 0xfcu8)).unwrap();
    assert_eq!(sim.get(extended), 0xfffcu16.into());
    assert_eq!(sim.get(divided), 0xfeu8.into());
    assert_eq!(sim.get(less), 1u8.into());
}

#[test]
fn handles_fill_literals_ascending_ranges_atom_types_and_unary_constants() {
    let source = r#"
        module Top(input logic [0:7] ascending, input int a, b,
                   output logic first, output logic last,
                   output logic [63:0] fill,
                   output logic [31:0] sum,
                   output logic folded);
            assign first = ascending[0];
            assign last = ascending[7];
            assign fill = '1;
            assign sum = a + b;
            if ((~8'h00) == 8'hff && (&8'hff)) begin
                assign folded = 1'b1;
            end else begin
                assign folded = 1'b0;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("types.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let ascending = sim.signal("ascending");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let first = sim.signal("first");
    let last = sim.signal("last");
    let fill = sim.signal("fill");
    let sum = sim.signal("sum");
    let folded = sim.signal("folded");
    sim.modify(|io| {
        io.set(ascending, 0x80u8);
        io.set(a, 40u32);
        io.set(b, 2u32);
    })
    .unwrap();
    assert_eq!(sim.get(first), 1u8.into());
    assert_eq!(sim.get(last), 0u8.into());
    assert_eq!(sim.get(fill), u64::MAX.into());
    assert_eq!(sim.get(sum), 42u32.into());
    assert_eq!(sim.get(folded), 1u8.into());
}

#[test]
fn treats_unknown_procedural_conditions_as_false() {
    let source = r#"
        module Top(input logic clk, input logic clear, input logic en, output logic q);
            always_ff @(posedge clk) begin
                if (clear) q <= 1'b0;
                else if (en) q <= 1'b1;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("condition.sv"))], "Top")
        .four_state(true)
        .build_cranelift()
        .unwrap();
    let clk = sim.event("clk");
    let clear = sim.signal("clear");
    let en = sim.signal("en");
    let q = sim.signal("q");
    sim.modify(|io| io.set(clear, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| {
        io.set(clear, 0u8);
        io.set_four_state(en, BigUint::from(1u8), BigUint::from(1u8));
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get_four_state(q),
        (BigUint::from(0u8), BigUint::from(0u8))
    );
}

#[test]
fn rejects_constructs_that_are_not_yet_lowered() {
    let cases = [
        (
            "ordered port connection",
            r#"
            module Child(input logic a); endmodule
            module Top(input logic a); Child child(a); endmodule
        "#,
        ),
        (
            "ordered parameter assignment",
            r#"
            module Child #(parameter W = 1) (); endmodule
            module Top(); Child #(8) child(); endmodule
        "#,
        ),
        (
            "module instantiation inside loop-generate",
            r#"
            module Child(); endmodule
            module Top(); for (genvar i = 0; i < 2; i++) Child child(); endmodule
        "#,
        ),
        (
            "control flow inside always_comb",
            r#"
            module Top(input logic s, a, b, output logic y);
                always_comb if (s) y = a; else y = b;
            endmodule
        "#,
        ),
        (
            "net declaration assignment",
            r#"
            module Top(input logic a, output logic y); wire n = a; assign y = n; endmodule
        "#,
        ),
        (
            "blocking assignment inside always_ff",
            r#"
            module Top(input logic clk, d, output logic q);
                always_ff @(posedge clk) q = d;
            endmodule
        "#,
        ),
        (
            "output port lvalue connection",
            r#"
            module Child(output logic [7:0] y); assign y = 8'hff; endmodule
            module Top(output logic [15:0] y); Child child(.y(y[7:0])); endmodule
        "#,
        ),
        (
            "combinational expression",
            r#"
            module Top(input logic a, output logic y); assign y = unknown(a); endmodule
        "#,
        ),
    ];

    for (expected, source) in cases {
        let error = cranelift_build_error(source);
        assert!(error.contains(expected), "{expected}: {error}");
    }
}

#[test]
fn rejects_duplicate_modules_across_source_files() {
    let first = "module Top(output logic y); assign y = 1'b0; endmodule";
    let second = "module Top(output logic y); assign y = 1'b1; endmodule";
    let error = match Simulator::from_sv_sources(
        vec![
            (first, Path::new("first.sv")),
            (second, Path::new("second.sv")),
        ],
        "Top",
    )
    .build_cranelift()
    {
        Ok(_) => panic!("duplicate module unexpectedly compiled"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("Duplicate module declaration: Top"),
        "{error}"
    );
}
