use super::*;

sv_backends! {
    fn soft_signed_ports(sim) {
        @case "unions::soft_signed_ports";
    }
    fn packed_overlay(sim) {
        @case "unions::packed_overlay";
    }
    fn soft_overlay(sim) {
        @case "unions::soft_overlay";
    }
    fn packed_mixed_state(sim) {
        @case "unions::packed_mixed_state";
    }
    fn soft_wide_four_state(sim) {
        @case "unions::soft_wide_four_state";
    }
    fn unpacked_overlay(sim) {
        @case "unions::unpacked_overlay";
    }
    fn unpacked_four_state(sim) {
        @case "unions::unpacked_four_state";
    }
    fn unpacked_whole_values(sim) {
        @case "unions::unpacked_whole_values";
    }
    fn unpacked_four_state_equality(sim) {
        @case "unions::unpacked_four_state_equality";
    }
    // An unpacked union is no bit-stream type (IEEE 1800-2023 6.24.3), so
    // `$cast` assigns nothing between one and a vector of its width, and its
    // type name is not that of a packed union (20.6.1). Verilator and
    // Icarus do not take these calls on an unpacked union, so the case is
    // not a shared one.
    fn unpacked_union_is_no_bit_stream(sim) {
        @setup { let source = r#"
module Top(output int to_vector, to_union, output bit [7:0] raw, wide, output bit named);
    typedef union { bit [7:0] v1; bit [3:0] v2; } u_t;
    u_t u;
    initial begin
        u.v1 = 8'hab;
        raw = 8'h11;
        to_vector = $cast(raw, u);
        to_union = $cast(u, raw);
        wide = u.v1;
        named = $typename(u_t) == "union{bit[7:0] v1;bit[3:0] v2;}Top.u_t";
    end
endmodule
"#; }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("unpacked_union_is_no_bit_stream.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("to_vector")), 0u32.into());
        assert_eq!(sim.get(sim.signal("to_union")), 0u32.into());
        assert_eq!(sim.get(sim.signal("raw")), 0x11u32.into());
        assert_eq!(sim.get(sim.signal("wide")), 0xabu32.into());
        assert_eq!(sim.get(sim.signal("named")), 1u32.into());
    }
}

/// An unpacked union is not an integral value (IEEE 1800-2023 7.3, unlike
/// a packed union): only assignment from its own type, the conditional
/// operator and the equality operators take a whole value of it.
#[test]
fn unpacked_union_whole_value_uses_are_rejected() {
    let types = "typedef union { bit [7:0] v1; bit [3:0] v2; } u_t; \
                 typedef union { bit [7:0] w1; bit [3:0] w2; } w_t;";
    for (body, want) in [
        ("u_t u; initial u = 8'd5;", "not of its type"),
        ("u_t u; int x; initial x = u;", "not of its type"),
        ("u_t u; w_t w; initial u = w;", "not of its type"),
        (
            "u_t u; int x; initial x = u + 1;",
            "applied to an unpacked union",
        ),
        (
            "u_t u; initial $display(\"%0d\", u[3]);",
            "applied to an unpacked union",
        ),
        (
            "u_t u; bit [8:0] x; initial x = {u, 1'b0};",
            "applied to an unpacked union",
        ),
        (
            "u_t u; w_t w; bit x; initial x = u == w;",
            "comparing an unpacked union",
        ),
        (
            "u_t u; initial u = '{v1: 8'd5};",
            "assignment pattern for an unpacked union",
        ),
        (
            "u_t u; initial u++;",
            "increment or decrement of an unpacked union",
        ),
        (
            "u_t u; initial u += u;",
            "operator `+=` on an unpacked union",
        ),
        (
            "u_t a[2]; int x; initial x = a[0] + 1;",
            "applied to an unpacked union",
        ),
        ("u_t [3:0] x;", "packed array of an unpacked union"),
        (
            "typedef union { real a; bit [31:0] b; } r_t; r_t r;",
            "unpacked struct, union",
        ),
        (
            "typedef union tagged { bit a; } t_t; t_t t;",
            "unpacked struct, union",
        ),
        (
            "typedef union { string s; bit [7:0] b; } s_t; s_t x;",
            "unpacked struct, union",
        ),
        (
            "class C; endclass typedef union { C c; bit b; } c_t; c_t x;",
            "unpacked struct, union",
        ),
        (
            "typedef union { event e; bit b; } e_t; e_t x;",
            "unpacked struct, union",
        ),
        (
            "u_t u; int x; initial if (u) x = 1;",
            "used as a value of another type",
        ),
        (
            "u_t u; int x; initial x = u ? 1 : 2;",
            "used as a value of another type",
        ),
        (
            "u_t u; initial while (u) u.v1 = 0;",
            "used as a value of another type",
        ),
        (
            "u_t u; int a; initial for (int i = 0; u; i++) a = 1;",
            "used as a value of another type",
        ),
        (
            "u_t u; int a; initial case (u) default: a = 1; endcase",
            "used as a value of another type",
        ),
        (
            "u_t u; int a; initial repeat (u) a++;",
            "used as a value of another type",
        ),
        (
            "u_t u; logic clk; int a, b; initial a = repeat (u) @(posedge clk) b;",
            "used as a value of another type",
        ),
        (
            "u_t u; int a; initial #(u) a = 1;",
            "used as a value of another type",
        ),
        (
            "u_t u; bit [7:0] m[256]; int a; initial a = m[u];",
            "used as a value of another type",
        ),
        (
            "u_t u; int x; initial x = $countones(u);",
            "used as a value of another type",
        ),
        (
            "u_t u; int x; initial x = $signed(u);",
            "used as a value of another type",
        ),
        (
            "u_t u; initial $display(\"%0d\", u);",
            "used as a value of another type",
        ),
        (
            "typedef struct { bit [7:0] payload; } s_t; s_t s; u_t u; initial s = '{payload: u};",
            "used as a value of another type",
        ),
        (
            "bit [7:0] arr[2]; u_t u; initial arr = '{u, u};",
            "used as a value of another type",
        ),
        (
            "u_t u; bit x; initial {u, x} = 9'd0;",
            "target of a concatenation assignment",
        ),
        (
            "u_t u; bit [7:0] b; initial {>>{u}} = b;",
            "target of a streaming concatenation",
        ),
        (
            "u_t u; bit [7:0] b; initial b = {>>{u}};",
            "used as a value of another type",
        ),
        (
            "typedef union { bit v; } o_t; o_t u; initial u = '{v: 1'b1};",
            "assignment pattern for an unpacked union",
        ),
        (
            "u_t u; initial u[0] = 1'b1;",
            "applied to an unpacked union",
        ),
        (
            "u_t u; initial u[3:0] = 4'd1;",
            "applied to an unpacked union",
        ),
        (
            "u_t arr[2]; u_t u; initial arr = u;",
            "used as a value of another type",
        ),
        (
            "u_t a[2], c; initial a = '{c, c};",
            "used as a value of another type",
        ),
        (
            "u_t c; initial wait (c) ;",
            "used as a value of another type",
        ),
        (
            "u_t c; int a; always @(c) a = 1;",
            "used as a value of another type",
        ),
        (
            "u_t u; initial u = {>>{8'hA5}};",
            "streaming concatenation into an unpacked union",
        ),
        ("u_t q[$];", "dynamic container of unpacked unions"),
        ("u_t d[];", "dynamic container of unpacked unions"),
        ("u_t m[int];", "dynamic container of unpacked unions"),
        (
            "typedef struct { u_t m; int k; } s_t; s_t s;",
            "unpacked struct, union",
        ),
        (
            "typedef union { u_t inner; bit b; } n_t; n_t n;",
            "unpacked struct, union",
        ),
        ("u_t arr[2]; initial arr[0] = 8'd5;", "not of its type"),
        ("u_t arr[2]; assign arr[0] = 8'd5;", "not of its type"),
        ("u_t arr[2]; w_t w; initial arr[0] = w;", "not of its type"),
        (
            "function void f(u_t x); endfunction w_t w; initial f(w);",
            "not of its type",
        ),
        (
            "class C; u_t p; endclass",
            "class property of unpacked union type",
        ),
        (
            "class C; function u_t get(); u_t t; return t; endfunction endclass",
            "interface of a class method",
        ),
    ] {
        let source = format!("module Top; {types} {body} endmodule");
        let Err(error) = Simulator::from_sv_sources(vec![(&source, Path::new("union.sv"))], "Top")
            .build_cranelift()
        else {
            panic!("`{body}` must be rejected");
        };
        assert!(
            error.to_string().contains(want),
            "`{body}`: got {error}, want ..{want}.."
        );
    }
}

/// A member of an unpacked union is written as a member of a packed one
/// is, also where the union has a single member: an enumerated member
/// takes a value of its base type.
#[test]
fn single_member_union_is_written_as_a_union_member() {
    for members in ["e_t e;", "e_t e; bit [31:0] b;"] {
        let source = format!(
            "module Top(output logic [31:0] y);
                typedef enum {{ A, B }} e_t;
                typedef union {{ {members} }} eu_t;
                eu_t u;
                always_comb begin u.e = 1; y = u.e; end
            endmodule"
        );
        let mut sim = Simulator::from_sv_sources(vec![(&source, Path::new("union.sv"))], "Top")
            .build_cranelift()
            .unwrap_or_else(|error| panic!("`{members}`: {error}"));
        sim.eval_comb().unwrap();
        assert_eq!(sim.get(sim.signal("y")), 1u32.into(), "{members}");
    }
}

/// A connection is not checked against the union type of the port it
/// binds, so a port of one is rejected where it is declared, and a whole
/// union where it is connected.
#[test]
fn unpacked_union_ports_are_rejected() {
    for (connection, want) in [
        ("8'd5", "unpacked union port"),
        ("u", "used as a value of another type"),
    ] {
        let source = format!(
            "package p; typedef union {{ bit [7:0] v1; bit [3:0] v2; }} u_t; endpackage \
             module Sub(input p::u_t x); endmodule \
             module Top; p::u_t u; Sub s(.x({connection})); endmodule"
        );
        let Err(error) = Simulator::from_sv_sources(vec![(&source, Path::new("union.sv"))], "Top")
            .build_cranelift()
        else {
            panic!("a connection of `{connection}` to an unpacked union port must be rejected");
        };
        assert!(
            error.to_string().contains(want),
            "`{connection}`: got {error}, want ..{want}.."
        );
    }
}

/// A signal of an interface is a variable of the scope that names it, so
/// one of a union type is a whole union there: it is assigned to a union of
/// its type and no operand. A variable of another module is read as a
/// vector by the scope that names it, so a hierarchical reference to a
/// union is rejected, and a virtual interface is no member of a union.
#[test]
fn unpacked_unions_of_other_scopes() {
    let design = |top: &str| {
        format!(
            "package p; typedef union {{ bit [7:0] v1; bit [3:0] v2; }} u_t; endpackage
             interface I; import p::*; u_t u; always_comb u.v1 = 8'd9; endinterface
             module Sub; import p::*; u_t u; always_comb u.v1 = 8'd5; endmodule
             module Top(output logic [7:0] y); import p::*; {top} endmodule"
        )
    };
    let build = |top: &str| {
        Simulator::from_sv_sources(vec![(&design(top), Path::new("union.sv"))], "Top")
            .build_cranelift()
    };
    let mut sim = build("I i(); u_t c; always_comb begin c = i.u; y = c.v1; end")
        .unwrap_or_else(|error| panic!("{error}"));
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(sim.signal("y")), 9u8.into());
    for (top, want) in [
        (
            "I i(); always_comb y = i.u + 8'd1;",
            "applied to an unpacked union",
        ),
        (
            "Sub s(); always_comb y = s.u + 8'd1;",
            "hierarchical reference `s.u` to an unpacked union",
        ),
        // A member such a path goes on to select is integral, and is the
        // member select of a hierarchical name, which is not supported.
        (
            "Sub s(); always_comb y = s.u.v1;",
            "structure member `s.u.v1`",
        ),
        (
            "typedef union { virtual I v; bit b; } v_t; v_t x; assign y = 8'd0;",
            "unpacked struct, union",
        ),
    ] {
        let Err(error) = build(top) else {
            panic!("`{top}` must be rejected");
        };
        assert!(
            error.to_string().contains(want),
            "`{top}`: got {error}, want ..{want}.."
        );
    }
}

/// The type of a union is its declaration with the types its members have
/// there (IEEE 1800-2023 6.22.1): elaborations of one declaration whose
/// member is another enumeration are other types, and one whose member is
/// the same enumeration is the same type through a function and an
/// interface.
#[test]
fn union_types_tell_apart_enumerated_members() {
    let design = |top: &str| {
        format!(
            "package p;
                typedef enum logic [1:0] {{ A, B }} ab_t;
                typedef enum logic [1:0] {{ C, D }} cd_t;
             endpackage
             interface I #(type T = p::ab_t); typedef union {{ T e; logic [1:0] raw; }} u_t; u_t u;
                always_comb u.raw = 2'd1;
             endinterface
             module Top(output logic [1:0] y); import p::*; {top} endmodule"
        )
    };
    let build = |top: &str| {
        Simulator::from_sv_sources(vec![(&design(top), Path::new("union.sv"))], "Top")
            .build_cranelift()
    };
    let mut sim = build(
        "typedef union { ab_t e; logic [1:0] raw; } l_t;
         l_t a, b;
         function automatic l_t pass(l_t x); return x; endfunction
         always_comb begin a.raw = 2'd1; b = pass(a); y = b.raw; end",
    )
    .unwrap_or_else(|error| panic!("{error}"));
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
    let mut sim = build("I #(ab_t) i(); I #(ab_t) j(); bit same; always_comb begin same = i.u == j.u; y = {1'b0, same}; end")
        .unwrap_or_else(|error| panic!("{error}"));
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
    let Err(error) = build(
        "I #(ab_t) i(); I #(cd_t) j(); bit same; always_comb begin same = i.u == j.u; y = {1'b0, same}; end",
    ) else {
        panic!("unions of other enumerations must not compare");
    };
    assert!(
        error.to_string().contains("comparing an unpacked union"),
        "{error}"
    );
}
