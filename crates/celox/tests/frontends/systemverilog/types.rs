use super::*;

// Receivers, counts and contexts that the external simulators of the shared
// suite do not accept: Verilator rejects non-constant counts and methods of
// casts, constants and call results; Icarus rejects methods of call results
// and of class properties, and methods in continuous assignments; xezim calls a method
// of an array element as one of a class handle and returns 0 instead of the
// 4-state base's 'x for a value that is no member (IEEE 1800-2023 6.19.5.3,
// 6.19.5.4, Table 6-7).
sv_backends! {
    fn enum_methods_of_any_enum_expression(sim) {
        @setup {
            let source = r#"
package p;
    typedef enum {PA, PB, PC} P;
    typedef enum {SHARED, OTHER} Q;
    P states[2] = '{PB, PC};
    localparam P START = PB;
endpackage
package r;
    typedef enum {SHARED, MIDDLE, LAST} R;
endpackage
module Top(output bit ok, output logic [1:0] unknown_next, output logic [1:0] unknown_prev,
           output bit unnamed, output bit [1:0] continuous, output bit [1:0] gap_next,
           output bit [3:0] chained);
    typedef enum bit [3:0] {W0, W1, W2, W3, W4, W5, W6, W7, W8, W9, W10, W11, W12, W13, W14, W15} W;
    W wide;
    // Each nested lookup lowers its selector once, not once per member.
    assign chained = wide.next().next().next().next().next().next().next().next();
    typedef enum logic [1:0] {IDLE, RUN, DONE} S;
    typedef enum bit [1:0] {K0, K1, K2} K;
    typedef enum bit [2:0] {A = 3'd1, B = 3'd4, C = 3'd6} E;
    S unknown;
    S queue[$];
    S states[3];
    E e;
    K current, gap;
    bit [2:0] steps;
    logic signed [2:0] signed_count;
    typedef enum logic [1:0] {LA, LB, LC} L;
    L le;
    // Parameters of an enumerated type.
    localparam S INITIAL = RUN;
    localparam E LAST = C;
    logic [1:0] unknown_count;
    int unsigned n;
    int calls;
    class Machine;
        static S initial_state = DONE;
        K state;
        function K current();
            return state;
        endfunction
        function string describe();
            return state.name();
        endfunction
    endclass
    function automatic S advance();
        calls++;
        return RUN;
    endfunction
    assign continuous = current.next(steps);
    assign gap_next = gap.next(2);
    initial begin
        Machine m;
        process running;
        ok = 1;
        e = B;
        ok &= e.next(2).name() == "A" && e.next.next.name == "A" && e.prev.name == "A";
        ok &= S'(0).name() == "IDLE" && E'(3'd6).prev().name() == "B";
        ok &= DONE.prev().name() == "RUN" && RUN.num() == 3 && C.next() == A;
        states[1] = DONE;
        ok &= states[1].name() == "DONE" && states[1].next.name == "IDLE";
        ok &= Machine::initial_state.name() == "DONE";
        queue.push_back(RUN);
        ok &= queue[0].prev().name() == "IDLE";
        ok &= advance().next(2).name() == "IDLE" && calls == 1;
        // Results that do not depend on the receiver still evaluate it once.
        ok &= advance().first() == IDLE && advance().last() == DONE && advance().num() == 3;
        ok &= calls == 4;
        ok &= p::PA.next().name() == "PB" && p::PC.prev(2).name() == "PA" && p::PB.num() == 3;
        // Each package's enumeration, though both declare SHARED = 0.
        ok &= p::states[0].name() == "PB" && p::states[1].next() == p::PA;
        ok &= p::SHARED.num() == 2 && r::SHARED.num() == 3 && r::SHARED.next().name() == "MIDDLE";
        queue = '{RUN, DONE};
        ok &= queue.pop_front().name() == "RUN" && queue.pop_back().next() == IDLE;
        ok &= DONE.name().len() == 4 && DONE.name().tolower() == "done";
        ok &= RUN.name.toupper() == "RUN" && RUN.next.name.tolower() == "done";
        m = new;
        m.state = K1;
        ok &= m.current().next().name() == "K2" && m.state.next() == K2;
        ok &= m.state.name() == "K1" && m.describe() == "K1";
        n = 5;
        ok &= e.next(n) == A && e.prev(n) == C;
        n = 32'hffff_ffff;
        ok &= e.next(n) == B && e.prev(n) == B;
        // A signed count converts to `int unsigned` with sign extension, at
        // run time as for a constant: -1 is a multiple of 3 and -2 is 2 more.
        signed_count = -1;
        ok &= e.next(signed_count) == B && e.next(3'sb111) == B;
        signed_count = -2;
        ok &= e.prev(signed_count) == C && e.prev(3'sb110) == C;
        // `int unsigned` is 2-state: an unknown count converts to 0.
        le = LB;
        unknown_count = 'x;
        ok &= le.next(unknown_count) === LB && le.prev(unknown_count) === LB;
        unknown_count = 2'bz1;
        ok &= le.next(unknown_count) === LC;
        ok &= INITIAL.next().name() == "DONE" && INITIAL.name() == "RUN";
        ok &= LAST.next() == A && LAST.num() == 3 && p::START.next() == p::PC;
        // The status of a process and the constants of its `state` type (9.7).
        running = process::self();
        ok &= running.status().name() == "RUNNING" && running.status.next() == process::WAITING;
        ok &= process::KILLED.next().name() == "FINISHED" && std::process::RUNNING.num() == 5;
        e = E'(3'd5);
        ok &= e.prev(n) == 0 && e.next(n) == 0;
        unknown_next = unknown.next();
        unknown_prev = unknown.prev(2);
        unnamed = unknown.name() == "";
        current = K1;
        wide = W3;
        gap = K'(3);
        steps = 4;
    end
endmodule
"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("enum_methods.sv"))], "Top")
            .four_state(true);
        sim.run_until(0).unwrap();
        let ok = sim.signal("ok");
        assert_eq!(sim.get(ok), BigUint::from(1u8));
        for name in ["unknown_next", "unknown_prev"] {
            let signal = sim.signal(name);
            assert_eq!(
                sim.get_four_state(signal),
                (BigUint::from(3u8), BigUint::from(3u8)),
                "{name}"
            );
        }
        let unnamed = sim.signal("unnamed");
        assert_eq!(sim.get(unnamed), BigUint::from(1u8));
        let continuous = sim.signal("continuous");
        assert_eq!(sim.get(continuous), BigUint::from(2u8));
        let chained = sim.signal("chained");
        assert_eq!(sim.get(chained), BigUint::from(11u8));
        let gap_next = sim.signal("gap_next");
        assert_eq!(sim.get(gap_next), BigUint::from(0u8));
    }
}

/// Types whose widths name parameters are integral (IEEE 1800-2023 6.11.1):
/// an intra-assignment delay assigns a variable of such a type, and a
/// parameter of such a type takes an assignment pattern.
#[test]
fn parameter_sized_types_are_integral() {
    let source = "`timescale 1ns/1ns
    module Top #(parameter int W = 8);
        localparam logic [W-1:0] P = '{default: 1'b1};
        logic [W-1:0] a, b;
        logic [W-1:0] pattern;
        initial begin b = 3; pattern = P; a = #1 b; end
    endmodule";
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("integral.sv"))], "Top")
            .build_interpreter()
            .unwrap();
    sim.run_until(2).unwrap();
    let a = sim.signal("a");
    assert_eq!(sim.get(a), BigUint::from(3u8));
    let pattern = sim.signal("pattern");
    assert_eq!(sim.get(pattern), BigUint::from(0xffu8));
}

/// Methods of an element of a packed array of enums are not supported yet;
/// they are rejected, not given the members of another type.
#[test]
fn enum_methods_of_packed_array_elements_are_rejected() {
    for (declaration, call) in [
        (
            "typedef enum bit [1:0] {A, B, C} E; E [1:0] states;",
            "states[0].next()",
        ),
        (
            "typedef enum bit [1:0] {A, B, C} E; E [1:0] states;",
            "states[1].name() == \"B\"",
        ),
        ("enum bit [1:0] {X, Y} [1:0] states;", "states[0].first()"),
    ] {
        let source =
            format!("module Top(output bit [1:0] y); {declaration} initial y = {call}; endmodule");
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("packed_enum_methods.sv"))],
            "Top",
        )
        .build()
        .err()
        .unwrap_or_else(|| panic!("{source} must be rejected"))
        .to_string();
        assert!(
            error.contains("method of an element of a packed array of enums"),
            "{error}"
        );
    }
}

/// Local typedefs of the same name in different scopes declare different
/// types, so unpacked arrays of their enumerations are not assignment
/// compatible (IEEE 1800-2023 6.18, 7.6).
#[test]
fn local_enumerations_of_one_name_are_distinct_types() {
    let source = "module Top; initial begin \
        typedef enum {A, B} t; t a[2]; \
        begin typedef enum {C, D} t; t b[2]; b = a; end end endmodule";
    let Err(error) =
        Simulator::from_sv_sources(vec![(source, Path::new("types.sv"))], "Top").build_cranelift()
    else {
        panic!("arrays of distinct local enumerations are not assignment compatible");
    };
    let error = error.to_string();
    assert!(error.contains("assignment"), "{error}");
}

/// A member of a local enumeration is a member of that enumeration, also
/// where a typedef of the module has a member of its name and value, so its
/// methods are those of the local enumeration (IEEE 1800-2023 6.19.5).
/// Verilator 5.052 does not call methods of enum constants, so this is not
/// a shared case.
#[test]
fn local_enumeration_constants_shadow_module_members() {
    let source = r#"
    module Top(output int nums);
        typedef enum {SA = 0, SB = 1} se_t;
        initial begin
            typedef enum {SA = 0, SC = 2, SD = 3} sf_t;
            nums = SA.num();
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("shadow.sv"))], "Top")
        .build()
        .unwrap();
    let nums = sim.signal("nums");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(nums), BigUint::from(3u8));
}

/// A local typedef that hides an enumeration typedef leaves the members of
/// that enumeration visible, with their type (IEEE 1800-2023 6.19).
#[test]
fn local_typedef_hiding_an_enumeration_keeps_its_members() {
    let source = r#"
    module Top(output int nums, output int next);
        typedef enum {A, B} e_t;
        initial begin
            typedef bit e_t;
            e_t flag;
            flag = 1;
            nums = A.num();
            next = A.next() + flag;
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("hide.sv"))], "Top")
        .build()
        .unwrap();
    let nums = sim.signal("nums");
    let next = sim.signal("next");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(nums), BigUint::from(2u8));
    assert_eq!(sim.get(next), BigUint::from(2u8));
}

/// The bounds of the base type of a local enumeration are those of the
/// declaring scope, also where a member of the enumeration shadows a
/// constant they read (IEEE 1800-2023 6.18, 6.19). Verilator 5.052 stops
/// with an internal error on this, so it is not a shared case.
#[test]
fn local_enumeration_bounds_precede_its_members() {
    let source = r#"
    module Top(output int nbits);
        localparam int NB = 4;
        initial begin
            typedef enum bit [NB-1:0] {NB = 1} nb_t;
            nb_t v;
            nbits = $bits(v);
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("bounds.sv"))], "Top")
        .build()
        .unwrap();
    let nbits = sim.signal("nbits");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(nbits), BigUint::from(4u8));
}

/// An explicitly sized initializer of a local enumeration must have the
/// width of its base type, as one of a typedef of the module (IEEE
/// 1800-2023 6.19).
#[test]
fn local_enumeration_initializers_are_checked() {
    let source = "module Top; int y; initial begin \
        typedef enum logic [1:0] {A = 3'd1} e_t; y = A; end endmodule";
    let Err(error) =
        Simulator::from_sv_sources(vec![(source, Path::new("types.sv"))], "Top").build_cranelift()
    else {
        panic!("an enum initializer of another width is rejected");
    };
    let error = error.to_string();
    assert!(error.contains("enum"), "{error}");
}

/// `$typename` of a variable of a local typedef of a queue of an
/// enumeration names the enumeration (IEEE 1800-2023 20.6.1).
#[test]
fn local_typedef_of_a_queue_of_an_enumeration_has_a_type_name() {
    let source = r#"
    module Top(output bit ok);
        initial begin
            typedef enum {LA, LB} e_t[$];
            e_t q;
            ok = $typename(q) != "";
        end
    endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("typename.sv"))], "Top")
            .build()
            .unwrap();
    let ok = sim.signal("ok");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(ok), BigUint::from(1u8));
}

/// The members of a local enumeration declared with nested container
/// dimensions are members of that enumeration (IEEE 1800-2023 6.19.5).
#[test]
fn local_enumeration_of_nested_containers_keeps_its_members() {
    let source = r#"
    module Top(output int n);
        initial begin
            typedef enum {NA, NB, NC} ne_t[$][$];
            n = NA.num();
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("nested.sv"))], "Top")
        .build()
        .unwrap();
    let n = sim.signal("n");
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(n), BigUint::from(3u8));
}

#[test]
fn enumerated_variables_take_only_values_of_their_type() {
    // IEEE 1800-2023 6.19.3, 6.19.4: a value assigned to an enumerated
    // variable, passed to an enumerated formal, returned as one or given as
    // an item of an array of them is of the enumeration's type; any other
    // value, such as the integral result of a compound assignment or an
    // increment, needs a cast.
    for body in [
        "e_t e; initial e = 1;",
        "e_t e; f_t f; initial e = f;",
        "e_t e; int i; initial e = i;",
        "e_t e; initial e = A + 1;",
        "e_t e; assign e = 1;",
        "e_t e = 1;",
        "function void take(e_t x); endfunction initial take(1);",
        "function e_t make(); return 1; endfunction",
        "e_t a [2]; initial a = '{0, 1};",
        "class C; e_t p; endclass C c; initial begin c = new; c.p = 1; end",
        "e_t e; bit s; initial e = s ? A : 1;",
        "e_t e; initial e += 1;",
        "e_t e; initial e++;",
        "e_t e; int i; initial i = e--;",
        "e_t a [2]; initial a[0] = 1;",
        "initial begin e_t v; v = 1; end",
        "e_t e; initial e = #1 1;",
        "e_t e; initial e <= 1;",
        "typedef struct packed { e_t e; logic [31:0] i; } s_t; s_t s; initial s.e = 1;",
        // An untyped parameter of an integral value is a logic vector
        // (6.20.2), and an explicitly integral one stays integral.
        "localparam P = B; e_t e; initial e = P;",
        "localparam int P = B; e_t e; initial e = P;",
        // An input or inout argument is copied in to its formal (13.5),
        // through any binder.
        "task t(inout e_t x); endtask int i; initial t(i);",
        "class C; task m(inout e_t x); endtask endclass C c; int i; initial begin c = new; c.m(i); end",
        "class C; task n(input e_t x); endtask endclass C c; initial begin c = new; c.n(1); end",
        // An element of a container, assigned or updated.
        "e_t q [$]; initial begin q.push_back(A); q[0] = 1; end",
        "e_t q [$]; initial begin q.push_back(A); q[0] += 1; end",
    ] {
        let source = format!(
            "`timescale 1ns/1ns\nmodule Top; typedef enum {{A, B}} e_t; typedef enum {{C0, C1}} f_t; {body} endmodule"
        );
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("enum.sv"))],
            "Top",
        )
        .build() else {
            panic!("an enumerated variable took a value of another type: {body}");
        };
        let error = error
            .to_string()
            .replace('│', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            error.contains("is not assignment compatible with `enum Top::e_t`"),
            "{body}: {error}"
        );
    }
    // An output argument is copied out of its enumerated formal, so its
    // actual takes an enum value only as its own type does (13.5).
    let source = "module Top; typedef enum {A, B} e_t; typedef enum {C0, C1} f_t;
        task t(output e_t x); x = A; endtask f_t f; initial t(f); endmodule";
    let Err(error) =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("enum.sv"))], "Top").build()
    else {
        panic!("an enum formal was copied out to another enumeration");
    };
    let error = error
        .to_string()
        .replace('│', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        error.contains("`enum Top::e_t` is not assignment compatible with `enum Top::f_t`"),
        "{error}"
    );
    // A member of a union takes any value of its bits (6.19.3), also by an
    // update; a select of an enumerated variable is integral; an output
    // copies an enum value out to an integral actual; and a container
    // element takes a value of its type.
    for body in [
        "typedef union packed { e_t e; logic [31:0] i; } u_t; u_t u; initial u.e = 1;",
        "typedef union packed { e_t e; logic [31:0] i; } u_t; u_t u; initial u.e += 1;",
        "typedef union packed { e_t e; logic [31:0] i; } u_t; u_t u; initial u.e++;",
        "typedef enum logic [1:0] {P, Q} two_t; two_t t; initial t[0] = 1'b1;",
        "task t(output e_t x); x = A; endtask int i; initial t(i);",
        "function int g(output e_t x); x = A; return 0; endfunction int i, r; initial r = g(i);",
        "class C; task o(output e_t x); x = A; endtask endclass C c; int i; initial begin c = new; c.o(i); end",
        "e_t q [$]; initial begin q.push_back(A); q[0] = B; end",
    ] {
        let source = format!("module Top; typedef enum {{A, B}} e_t; {body} endmodule");
        celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("enum.sv"))], "Top")
            .build()
            .unwrap_or_else(|error| panic!("{body}: {error}"));
    }
    // A member, a variable or method result of the type, a cast and a
    // conditional of members are of the type.
    let source = "module Top(output int first, output int second, output int third);
        typedef enum {A, B, C} e_t;
        e_t e, g, a [2];
        bit s = 1;
        function e_t pick(e_t x); return x; endfunction
        initial begin
            e = B;
            g = e;
            e = e_t'(2);
            first = e;
            e = s ? A : g;
            a = '{B, C};
            second = e + a[1];
            third = pick(e.next());
        end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("enum.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("first")), 2u32.into());
    assert_eq!(sim.get(sim.signal("second")), 2u32.into());
    assert_eq!(sim.get(sim.signal("third")), 1u32.into());
}

/// A compilation-unit typedef binds its target as a use does, so one that
/// names what nothing declares makes the design illegal even when no module
/// uses it (IEEE 1800-2023 3.12.1, 6.18); an unused unsupported declaration
/// there does not stop the design.
#[test]
fn unused_unit_and_package_subroutines_reading_undeclared_names_are_errors() {
    // Data are declared before they are used, and a read declares no
    // implicit net (IEEE 1800-2023 6.6, 6.10): a subroutine of a package or
    // of the compilation unit that reads an undeclared name is an error,
    // whether or not anything calls it.
    for subroutine in [
        "function automatic int f(); return missing; endfunction",
        "function void f(); int x; x = missing + 1; endfunction",
        "task t(); int x; x = missing; endtask",
        "package q; function int f(); return missing; endfunction endpackage",
        "class C; static int s; endclass function automatic int f(); return C::missing; endfunction",
    ] {
        let source = format!("{subroutine}\nmodule Top(output int y); assign y = 1; endmodule\n");
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), std::path::Path::new("unit.sv"))],
            "Top",
        )
        .build()
        .err()
        .unwrap_or_else(|| panic!("{subroutine} is accepted"))
        .to_string();
        assert!(
            error.contains("missing` is not declared"),
            "{subroutine}: {error}"
        );
    }
    // A name a package qualifies is one the package declares or exports
    // (26.3, 26.6).
    for subroutine in [
        "package q; int x; endpackage function automatic int f(); return q::missing; endfunction",
        "package q; int x; function automatic int f(); return q::missing; endfunction endpackage",
    ] {
        let source = format!("{subroutine}\nmodule Top(output int y); assign y = 1; endmodule\n");
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), std::path::Path::new("unit.sv"))],
            "Top",
        )
        .build()
        .err()
        .unwrap_or_else(|| panic!("{subroutine} is accepted"))
        .to_string();
        assert!(
            error.contains("package `q` has no item `missing`"),
            "{subroutine}: {error}"
        );
    }
    // Every name such a subroutine reads that its body, its scope or an
    // import declares (23.9).
    let source = "package p;
        typedef enum {A, B} e_t;
        localparam int P = 2;
        int pv = 3;
        function automatic int g(int a); return a + P; endfunction
    endpackage
    import p::*;
    localparam int U = 5;
    int uv;
    typedef enum {C0, C1} u_t;
    class K; static int ks = 4; endclass
    function automatic int f(int a);
        int l = a;
        static int s = 1;
        for (int i = 0; i < 2; i++) l += i;
        begin : blk int b = 1; l += b; end
        f = l + U + uv + pv + P + C1 + B + g(a) + s + p::pv + p::P + K::ks;
    endfunction
    function int h(); return f(1); endfunction
    task automatic t(output int o); o = U; endtask
    module Top(output int y); initial y = h(); endmodule
    ";
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, std::path::Path::new("unit.sv"))], "Top")
            .build()
            .unwrap();
    sim.run_until(1).unwrap();
    let y = sim.signal("y");
    assert_eq!(sim.get(y), num_bigint::BigUint::from(28u8));
}

#[test]
fn unused_compilation_unit_typedefs_of_undeclared_names_are_errors() {
    for typedef in [
        "typedef some_other_type myalias;",
        "typedef missing_forward_typedef;",
        "class C; typedef int T; endclass typedef C::Missing Alias;",
    ] {
        let source = format!("{typedef}\nmodule Top(output int y); assign y = 1; endmodule\n");
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), std::path::Path::new("unit.sv"))],
            "Top",
        )
        .build()
        .err()
        .unwrap_or_else(|| panic!("{typedef} is accepted"))
        .to_string();
        assert!(error.contains("not declared"), "{typedef}: {error}");
    }
    // A virtual interface names an interface of any source (IEEE 1800-2023
    // 25.9).
    let source = "module Top(output int y); typedef virtual interface Missing vif_t; assign y = 1; endmodule\n";
    let Err(error) =
        celox::Simulation::from_sv_sources(vec![(source, std::path::Path::new("top.sv"))], "Top")
            .build()
    else {
        panic!("a virtual interface of no interface is accepted");
    };
    let error = error.to_string();
    assert!(
        error.contains("Missing") && error.contains("not declared"),
        "{error}"
    );
    let interface = "interface Bus; logic v; endinterface\n";
    let top = "module Top(output int y); typedef virtual interface Bus vif_t; vif_t vif; assign y = 1; endmodule\n";
    celox::Simulation::from_sv_sources(
        vec![
            (interface, std::path::Path::new("bus.sv")),
            (top, std::path::Path::new("top.sv")),
        ],
        "Top",
    )
    .build()
    .unwrap();
    let source =
        "typedef union { int i; bit b; } u_t;\nmodule Top(output int y); assign y = 1; endmodule\n";
    celox::Simulation::from_sv_sources(vec![(source, std::path::Path::new("unit.sv"))], "Top")
        .build()
        .unwrap();
}

// An enum constant, or a parameter of an enumerated type, that a constant
// function passes to a formal of its enumerated type is assignment compatible
// with it (IEEE 1800-2023 6.19.3, 13.4.3), in a package and in a module, where
// the result sizes a declaration and bounds a loop generate construct, as
// fpnew's lane counts pass formats; an integral value is not, without a cast.
#[test]
fn constant_functions_pass_enum_constants_to_enum_formals() {
    let source = r#"
        `timescale 1ns/1ns
        package fp;
            typedef enum logic [2:0] { FP32, FP64, FP16 } fmt_e;
            localparam fmt_e HALF = FP16;
            function automatic int unsigned width(fmt_e fmt);
                return fmt == FP16 ? 16 : fmt == FP64 ? 64 : 32;
            endfunction
            function automatic int unsigned lanes();
                return width(FP16) + width(fp::FP64) + width(HALF);
            endfunction
        endpackage
        module Top;
            typedef enum logic [1:0] { A, B, C } kind_e;
            localparam kind_e K = C;
            function automatic int unsigned size(kind_e kind);
                return kind == C ? 3 : 1;
            endfunction
            function automatic int unsigned sizes();
                return size(C) + size(K) + size(A);
            endfunction
            localparam int unsigned N = fp::lanes();
            localparam int unsigned M = sizes();
            logic [N-1:0] lanes;
            logic [M-1:0] kinds;
            for (genvar g = 0; g < M; g++) begin : gen_kinds
                initial #1 $display("kind %0d", g);
            end
            // The functions of the scope also evaluate in a generate block.
            for (genvar g = 0; g < 2; g++) begin : gen_blocks
                localparam int unsigned X = size(K) + g;
                logic [X-1:0] v;
                if (sizes() == 7) begin : sized
                    initial #2 $display("block %0d %0d", g, $bits(v));
                end
            end
            initial #3 $display("%0d %0d", $bits(lanes), $bits(kinds));
        endmodule
    "#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("enums.sv"))], "Top")
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
    let mut expected: Vec<_> = (0..7).map(|g| format!("kind {g}")).collect();
    expected.extend(["block 0 3", "block 1 4", "96 7"].map(String::from));
    assert_eq!(displayed, expected);
    let source = r#"
        module Top;
            typedef enum logic [1:0] { A, B, C } kind_e;
            function automatic int unsigned size(kind_e kind);
                return kind == C ? 3 : 1;
            endfunction
            function automatic int unsigned sizes();
                return size(2'd2);
            endfunction
            localparam int unsigned M = sizes();
            logic [M-1:0] kinds;
        endmodule
    "#;
    let Err(error) =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("integral.sv"))], "Top").build()
    else {
        panic!("an integral argument of an enum formal is accepted");
    };
    let error = error.to_string();
    assert!(
        error.contains("argument 1 of `size`") && error.contains("Top::kind_e"),
        "{error}"
    );
}

// An enumeration declared in a parameterized module is a type of each
// instance's specialization (IEEE 1800-2023 6.19, 23.10): its methods take
// the members of the instance's own type, whose base width and values the
// parameter sets.
sv_backends! {
    fn enum_methods_of_module_specializations(sim) {
        @setup {
            let source = r#"
    module Leaf #(parameter int W = 2)(output logic [7:0] last);
      typedef enum logic [W-1:0] {A, B = W - 1} e_t;
      e_t v;
      always_comb begin
        v = A;
        last = 8'(v.last());
      end
    endmodule
    module Top(output logic [7:0] narrow, output logic [7:0] wide);
      Leaf #(2) n(.last(narrow));
      Leaf #(5) w(.last(wide));
    endmodule
  "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("enum_specializations.sv"))], "Top");
        let narrow = sim.signal("narrow");
        let wide = sim.signal("wide");
        sim.eval_comb().unwrap();
        assert_eq!(u64::try_from(sim.get(narrow)).unwrap(), 1);
        assert_eq!(u64::try_from(sim.get(wide)).unwrap(), 4);
    }
}

// Instances whose parameters change only the values of an enumeration's
// members, not its base type, still declare types of their own (IEEE
// 1800-2023 6.19, 6.22): the methods of each take its own members.
sv_backends! {
    fn enum_methods_of_specializations_with_one_base_type(sim) {
        @setup {
            let source = r#"
    module Leaf #(parameter int W = 1)(output logic [7:0] last);
      typedef enum logic [3:0] {A, B = W} e_t;
      e_t v;
      always_comb begin
        v = A;
        last = 8'(v.last());
      end
    endmodule
    module Top(output logic [7:0] one, output logic [7:0] two);
      Leaf #(1) a(.last(one));
      Leaf #(2) b(.last(two));
    endmodule
  "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("enum_values.sv"))], "Top");
        let one = sim.signal("one");
        let two = sim.signal("two");
        sim.eval_comb().unwrap();
        assert_eq!(u64::try_from(sim.get(one)).unwrap(), 1);
        assert_eq!(u64::try_from(sim.get(two)).unwrap(), 2);
    }
}
