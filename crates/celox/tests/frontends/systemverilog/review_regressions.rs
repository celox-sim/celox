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
fn infers_the_clock_when_reset_precedes_it_in_the_event_list() {
    let source = r#"
        module Top(input logic clk, input logic rst, input logic d, output logic q);
            always_ff @(posedge rst or posedge clk) begin
                if (rst) q <= 1'b0;
                else q <= d;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("reset_first.sv"))], "Top")
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
fn preserves_comb_function_return_width() {
    let source = r#"
        module Top(input logic [7:0] x, output logic [15:0] y);
            function automatic logic [7:0] increment(input logic [7:0] value);
                return value + 1'b1;
            endfunction
            assign y = increment(x);
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("function_width.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let x = sim.signal("x");
    let y = sim.signal("y");
    sim.modify(|io| io.set(x, 0xffu8)).unwrap();
    assert_eq!(sim.get(y), 0u16.into());
}

#[test]
fn preserves_declared_parameter_width_in_comb_expressions() {
    let source = r#"
        module Top #(
            parameter logic [63:0] VALUE = 64'h0000_0001_0000_0000
        ) (output logic [63:0] y);
            assign y = VALUE;
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("parameter_width.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0x1_0000_0000u64.into());
}

#[test]
fn converts_unknown_bits_when_assigning_to_bit() {
    let source = r#"
        module Top(input logic x, output bit y);
            assign y = x;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("two_state.sv"))], "Top")
        .four_state(true)
        .build_cranelift()
        .unwrap();
    let x = sim.signal("x");
    let y = sim.signal("y");
    sim.modify(|io| io.set_four_state(x, BigUint::from(1u8), BigUint::from(1u8)))
        .unwrap();
    assert_eq!(
        sim.get_four_state(y),
        (BigUint::from(0u8), BigUint::from(0u8))
    );
}

#[test]
fn preserves_nested_loop_generate_assignments() {
    let source = r#"
        module Top #(parameter ENABLE = 1) (
            input logic [1:0] a,
            output logic [1:0] y
        );
            if (ENABLE) begin
                for (genvar i = 0; i < 2; i++) begin
                    assign y[i] = a[i];
                end
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("nested_loop.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let a = sim.signal("a");
    let y = sim.signal("y");
    sim.modify(|io| io.set(a, 2u8)).unwrap();
    assert_eq!(sim.get(y), 2u8.into());
}

#[test]
fn preserves_decimal_unknown_literals() {
    let source = r#"
        module Top(output logic [3:0] x, output logic [7:0] z);
            assign x = 4'dx;
            assign z = 8'dz;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("decimal_xz.sv"))], "Top")
        .four_state(true)
        .build_cranelift()
        .unwrap();
    assert_eq!(
        sim.get_four_state(sim.signal("x")),
        (BigUint::from(0x0fu8), BigUint::from(0x0fu8))
    );
    assert_eq!(
        sim.get_four_state(sim.signal("z")),
        (BigUint::from(0u8), BigUint::from(0xffu8))
    );
}

#[test]
fn preserves_arithmetic_shift_operators() {
    let source = r#"
        module Top(
            input logic signed [7:0] a,
            input logic [7:0] u,
            output logic signed [7:0] left,
            output logic signed [7:0] right,
            output logic [7:0] unsigned_right
        );
            assign left = a <<< 1;
            assign right = a >>> 1;
            assign unsigned_right = u >>> 1;
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("arithmetic_shift.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let a = sim.signal("a");
    let u = sim.signal("u");
    sim.modify(|io| {
        io.set(a, 0xfcu8);
        io.set(u, 0xfcu8);
    })
    .unwrap();
    assert_eq!(sim.get(sim.signal("left")), 0xf8u8.into());
    assert_eq!(sim.get(sim.signal("right")), 0xfeu8.into());
    assert_eq!(sim.get(sim.signal("unsigned_right")), 0x7eu8.into());
}

#[test]
fn does_not_treat_typedef_variables_as_instances() {
    let source = r#"
        module Top(input logic [7:0] a, output logic [7:0] y);
            typedef logic [7:0] word_t;
            word_t value;
            assign value = a;
            assign y = value;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("typedef_signal.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let a = sim.signal("a");
    let y = sim.signal("y");
    sim.modify(|io| io.set(a, 0xa5u8)).unwrap();
    assert_eq!(sim.get(y), 0xa5u8.into());
}

#[test]
fn preserves_ternary_parameter_values() {
    let source = r#"
        module Top #(
            parameter ENABLE = 1,
            parameter W = ENABLE ? 8 : 4
        ) (
            input logic [W-1:0] a,
            output logic [W-1:0] y
        );
            assign y = a;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("ternary_param.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let a = sim.signal("a");
    let y = sim.signal("y");
    sim.modify(|io| io.set(a, 0xa5u8)).unwrap();
    assert_eq!(sim.get(y), 0xa5u8.into());
}

#[test]
fn sign_extends_signed_literals_in_constant_expressions() {
    let source = r#"
        module Top #(
            parameter FLAG = (8'shff < 0)
        ) (
            output logic y
        );
            assign y = FLAG;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("signed_param.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn produces_unknown_for_four_state_division_by_zero() {
    let source = r#"
        module Top(output logic [7:0] div, output logic [7:0] rem);
            assign div = 8'd5 / 8'd0;
            assign rem = 8'd5 % 8'd0;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("zero_div.sv"))], "Top")
        .four_state(true)
        .build_cranelift()
        .unwrap();
    let div = sim.signal("div");
    let rem = sim.signal("rem");
    assert_eq!(
        sim.get_four_state(div),
        (BigUint::from(0xffu8), BigUint::from(0xffu8))
    );
    assert_eq!(
        sim.get_four_state(rem),
        (BigUint::from(0xffu8), BigUint::from(0xffu8))
    );
}

#[test]
fn preserves_typedef_function_return_width_in_ff_case() {
    let source = r#"
        module Top(input logic clk, output logic [7:0] q);
            typedef logic [1:0] word_t;
            function automatic word_t decode(input logic ignored);
                return 4;
            endfunction
            always_ff @(posedge clk) begin
                case (decode(1'b0))
                    2'b00: q <= 8'hfa;
                    default: q <= 0;
                endcase
            end
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("typedef_function.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 0xfau8.into());
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
fn propagates_assignment_width_into_combinational_expressions() {
    let source = r#"
        module Top(input logic [7:0] a, output logic [15:0] y);
            assign y = a << 8;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("context_width.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let a = sim.signal("a");
    let y = sim.signal("y");
    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get(y), 0x100u16.into());
}

#[test]
fn preserves_last_write_order_inside_always_comb() {
    let source = r#"
        module Top(input logic a, b, output logic y);
            always_comb begin
                y = a;
                y = b;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("comb_order.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(a, 1u8);
        io.set(b, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 0u8.into());
}

#[test]
fn resolves_parent_parameters_in_input_port_connections() {
    let source = r#"
        module Child(input logic [15:0] value, output logic [15:0] y);
            assign y = value;
        endmodule
        module Top #(parameter WIDTH_VALUE = 9) (output logic [15:0] y);
            Child child(.value(WIDTH_VALUE), .y(y));
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("parent_constant.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 9u16.into());
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
        (
            "always_ff assignment lowering",
            r#"
            module Top(input logic clk, input logic i, d, output logic [1:0] q);
                always_ff @(posedge clk) q[i] <= d;
            endmodule
        "#,
        ),
        (
            "unpacked array dimension",
            r#"
            module Top(input logic [7:0] mem [0:1], output logic [7:0] y);
                assign y = mem[0];
            endmodule
        "#,
        ),
        (
            "local data declaration inside loop-generate",
            r#"
            module Top(input logic [1:0] a, output logic [1:0] y);
                for (genvar i = 0; i < 2; i++) begin
                    logic tmp;
                    assign tmp = a[i];
                    assign y[i] = tmp;
                end
            endmodule
        "#,
        ),
        (
            "initial construct",
            r#"
            module Top(output logic y); initial y = 1'b1; endmodule
        "#,
        ),
        (
            "non-ANSI module port declarations",
            r#"
            module Top(y); output y; assign y = 1'b1; endmodule
        "#,
        ),
        (
            "ref port direction",
            r#"
            module Top(ref logic value); endmodule
        "#,
        ),
        (
            "dependent repeated assignment inside always_comb",
            r#"
            module Top(input logic a, output logic y);
                always_comb begin y = a; y = y + 1'b1; end
            endmodule
        "#,
        ),
        (
            "always and always_latch processes",
            r#"
            module Top(input logic a, output logic y); always @* y = a; endmodule
        "#,
        ),
        (
            "always and always_latch processes",
            r#"
            module Top(input logic a, output logic y); always_latch y = a; endmodule
        "#,
        ),
        (
            "case-generate construct",
            r#"
            module Top #(parameter MODE = 0) (output logic y);
                case (MODE)
                    0: assign y = 1'b0;
                    default: assign y = 1'b1;
                endcase
            endmodule
        "#,
        ),
        (
            "always_ff event control",
            r#"
            module Top(input logic a, b, d, output logic q);
                always_ff @(posedge a or posedge b) q <= d;
            endmodule
        "#,
        ),
        (
            "always_ff inside loop-generate",
            r#"
            module Top(input logic clk, input logic [1:0] d, output logic [1:0] q);
                for (genvar i = 0; i < 2; i++) always_ff @(posedge clk) q[i] <= d[i];
            endmodule
        "#,
        ),
        (
            "variable declaration initializer",
            r#"
            module Top(output logic y); logic value = 1'b1; assign y = value; endmodule
        "#,
        ),
        (
            "indexed part-select",
            r#"
            module Top(input logic [15:0] a, input logic [3:0] index,
                       output logic [7:0] y);
                assign y = a[index +: 8];
            endmodule
        "#,
        ),
        (
            "non-zero-based or ascending multidimensional packed range",
            r#"
            module Top(input logic [2:1][7:0] a, output logic [7:0] y);
                assign y = a[1];
            endmodule
        "#,
        ),
        (
            "casez, casex, or pattern case inside always_ff",
            r#"
            module Top(input logic clk, input logic [1:0] a, output logic y);
                always_ff @(posedge clk) casez (a) 2'b1?: y <= 1'b1; default: y <= 0; endcase
            endmodule
        "#,
        ),
        (
            "local data declaration inside conditional-generate",
            r#"
            module Top #(parameter ENABLE = 1) (input logic a, output logic y);
                if (ENABLE) begin
                    logic tmp; assign tmp = a; assign y = tmp;
                end else begin
                    logic [1:0] tmp; assign tmp = {a, a}; assign y = tmp[0];
                end
            endmodule
        "#,
        ),
        (
            "packed struct or union type",
            r#"
            module Top(output logic [7:0] y);
                struct packed { logic [3:0] a; logic [3:0] b; } value;
                assign y = value;
            endmodule
        "#,
        ),
        (
            "cast expression",
            r#"
            module Top(input logic a, b, output logic [1:0] y);
                assign y = {logic'(a), b};
            endmodule
        "#,
        ),
        (
            "wildcard port connection",
            r#"
            module Child(input logic a, output logic y); assign y = a; endmodule
            module Top(input logic a, output logic y); Child child (.*); endmodule
        "#,
        ),
        (
            "procedural loop inside always_ff",
            r#"
            module Top(input logic clk, d, output logic q);
                always_ff @(posedge clk) repeat (1) q <= d;
            endmodule
        "#,
        ),
        (
            "delayed continuous assignment",
            r#"
            module Top(input logic a, output wire y); assign #5 y = a; endmodule
        "#,
        ),
        (
            "mixed clock-edge polarities for one signal",
            r#"
            module Top(input logic clk, a, b, output logic qa, qb);
                always_ff @(posedge clk) qa <= a;
                always_ff @(negedge clk) qb <= b;
            endmodule
        "#,
        ),
        (
            "procedural local data declaration",
            r#"
            module Top(input logic a, output logic y);
                always_comb begin logic tmp; tmp = a; y = tmp; end
            endmodule
        "#,
        ),
        (
            "continuous assignment expression",
            r#"
            module Top(input logic [7:0] a, output logic [7:0] y);
                assign y = {<<{a}};
            endmodule
        "#,
        ),
        (
            "conditional function return without else",
            r#"
            module Top(input logic a, output logic y);
                function automatic logic choose(input logic x);
                    if (x) return 1'b1;
                    return 1'b0;
                endfunction
                assign y = choose(a);
            endmodule
        "#,
        ),
        (
            "loop-generate unroll limit exceeded",
            r#"
            module Top(input logic [10000:0] a, output logic [10000:0] y);
                for (genvar i = 0; i < 10001; i++) assign y[i] = a[i];
            endmodule
        "#,
        ),
        (
            "open input port connection",
            r#"
            module Child(input wire a, output logic y); assign y = (a === 1'bz); endmodule
            module Top(output logic y); Child child(.a(), .y(y)); endmodule
        "#,
        ),
        (
            "nonblocking assignment inside always_comb",
            r#"
            module Top(input logic a, output logic y); always_comb y <= a; endmodule
        "#,
        ),
        (
            "iff-qualified always_ff event",
            r#"
            module Top(input logic clk, enable, d, output logic q);
                always_ff @(posedge clk iff enable) q <= d;
            endmodule
        "#,
        ),
        (
            "concatenated always_ff assignment target",
            r#"
            module Top(input logic clk, input logic [1:0] d, output logic q1, q0);
                always_ff @(posedge clk) {q1, q0} <= d;
            endmodule
        "#,
        ),
        (
            "output or inout function argument",
            r#"
            module Top(input logic a, output logic y, side);
                function automatic logic f(output logic out, input logic value);
                    out = value;
                    return value;
                endfunction
                assign y = f(side, a);
            endmodule
        "#,
        ),
        (
            "dependent repeated assignment inside always_comb",
            r#"
            module Top(input logic b, d, output logic a, c);
                always_comb begin a = b; c = a; a = d; end
            endmodule
        "#,
        ),
        (
            "reduction operator in parameter expression",
            r#"
            module Top #(parameter logic [3:0] P = 4'hf, parameter FLAG = &P)
                       (output logic y);
                assign y = FLAG;
            endmodule
        "#,
        ),
        (
            "casez or casex inside function",
            r#"
            module Top(input logic [1:0] a, output logic y);
                function automatic logic f(input logic [1:0] x);
                    casez (x) 2'b1?: return 1'b1; default: return 1'b0; endcase
                endfunction
                assign y = f(a);
            endmodule
        "#,
        ),
        (
            "genvar update operator",
            r#"
            module Top(input logic [7:0] a, output logic [7:0] y);
                for (genvar i = 7; i > 0; i &= i - 1) assign y[i] = a[i];
            endmodule
        "#,
        ),
        (
            "gate primitive instantiation",
            r#"
            module Top(input logic a, b, output logic y); and (y, a, b); endmodule
        "#,
        ),
    ];

    for (expected, source) in cases {
        let error = cranelift_build_error(source);
        assert!(error.contains(expected), "{expected}: {error}");
    }
}

#[test]
fn rejects_unknown_top_parameter_overrides() {
    let source = r#"
        module Top #(parameter WIDTH = 4) (output logic [WIDTH-1:0] y);
            assign y = '0;
        endmodule
    "#;
    let error = match Simulator::from_sv_sources(vec![(source, Path::new("bad_param.sv"))], "Top")
        .param("WIDHT", 8)
        .build_cranelift()
    {
        Ok(_) => panic!("unknown parameter override unexpectedly compiled"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("unknown top-level parameter override `WIDHT`"));
}

#[test]
fn rejects_cycles_across_the_module_graph() {
    let source = r#"
        module A(); B child(); endmodule
        module B(); A child(); endmodule
    "#;
    let error = match Simulator::from_sv_sources(vec![(source, Path::new("cycle.sv"))], "A")
        .build_cranelift()
    {
        Ok(_) => panic!("recursive hierarchy unexpectedly compiled"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("recursive systemverilog module instantiation"),
        "{error}"
    );
}

#[test]
fn ignores_unreachable_invalid_systemverilog_hierarchy_in_mixed_designs() {
    let veryl = r#"
        module Top (y: output logic) {
            inst good: $sv::Good (y);
        }
    "#;
    let sv = r#"
        module Good(output logic y); assign y = 1'b1; endmodule
        module Unused(output logic y); Missing child(.y(y)); endmodule
    "#;
    let mut sim = Simulator::from_mixed_sources(
        vec![(veryl, Path::new("top.veryl"))],
        vec![(sv, Path::new("external.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn rejects_invalid_systemverilog_hierarchy_when_mixed_design_reaches_it() {
    let veryl = r#"
        module Top (y: output logic) {
            inst broken: $sv::Broken (y);
        }
    "#;
    let sv = r#"
        module Broken(output logic y); Missing child(.y(y)); endmodule
    "#;
    let error = match Simulator::from_mixed_sources(
        vec![(veryl, Path::new("top.veryl"))],
        vec![(sv, Path::new("external.sv"))],
        "Top",
    )
    .build_cranelift()
    {
        Ok(_) => panic!("reachable invalid hierarchy unexpectedly compiled"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("module \"Missing\""), "{error}");
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
