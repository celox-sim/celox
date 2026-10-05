use std::path::Path;

use crate::{analyze_source, ir};

#[test]
fn preserves_layout_signedness_and_state_kind() {
    let ir = analyze_source(
        r#"
            module Top;
                typedef struct packed { logic signed [3:0] x; bit [3:0] y; } mixed_t;
                typedef struct packed signed { bit [3:0] x, y; } signed_t;
                mixed_t mixed;
                signed_t signed_value;
                struct packed { byte a; shortint b; int c; longint d; } atoms;
            endmodule
        "#,
        Path::new("struct_layout.sv"),
    )
    .unwrap();
    let signals = ir.modules()[0].signals();
    for (name, width, signed, kind) in [
        ("mixed", 8, false, ir::TypeKind::Logic),
        ("signed_value", 8, true, ir::TypeKind::Bit),
        ("atoms", 120, false, ir::TypeKind::Bit),
    ] {
        let ty = signals
            .iter()
            .find(|signal| signal.name() == name)
            .unwrap()
            .r#type();
        assert_eq!(ty.resolved_width(), Some(width), "{name}");
        assert_eq!(ty.is_signed(), signed, "{name}");
        assert_eq!(ty.kind(), kind, "{name}");
    }
}

#[test]
fn rejects_invalid_or_unsupported_struct_shapes() {
    for body in [
        "struct { logic a; } value;",
        "union packed { logic a; logic b; } value;",
        "struct packed { real a; } value;",
        "struct packed { logic a[2]; } value;",
        "struct packed { logic a; logic a; } value;",
        "struct packed { logic a = 1; } value;",
        "struct packed { missing_t a; } value;",
        "struct packed { logic a; } value; assign y = value.missing;",
        "struct packed { logic [3:0] a; } value; assign y = value.a[index];",
        "struct packed { logic a; } value[2]; assign y = value[0].a;",
        "typedef struct packed { logic a; } t; t value; function automatic logic f(input t arg); return arg.a; endfunction assign y = f(value);",
        "typedef struct packed { logic a; logic b; } t; localparam t P = 2'b10; if (P.b) assign y = 1; else assign y = 0;",
    ] {
        let source = format!("module Top(input bit index, output logic y); {body} endmodule");
        assert!(
            analyze_source(&source, Path::new("unsupported_struct.sv")).is_err(),
            "{body}"
        );
    }
}

#[test]
fn rejects_member_selects_that_would_overlap_a_neighbor() {
    for access in ["value.low[4]", "value.low[-1]", "value.low[5:2]"] {
        for statement in [
            format!("assign y = {access};"),
            format!("assign {access} = 1'b1;"),
        ] {
            let source = format!(
                "module Top(output logic y); struct packed {{ logic [3:0] high, low; }} value; {statement} endmodule"
            );
            assert!(
                analyze_source(&source, Path::new("invalid_struct_select.sv")).is_err(),
                "{statement}"
            );
        }
    }
}

#[test]
fn rejects_invalid_multidimensional_member_selects() {
    for access in [
        "value.m[2]",
        "value.m[-1]",
        "value.m[2][0]",
        "value.m[0][4]",
        "value.m[0][-1]",
        "value.m[0][0][0]",
        "value.m[index][0][0]",
        "value.m[2][1:0]",
        "value.m[0][4:2]",
        "value.offset[1]",
        "value.offset[2][3]",
        "value.scalar[0][0]",
        "value.shifted[0]",
        "value.shifted[3]",
        "value.shifted[8]",
    ] {
        for statement in [
            format!("assign y = {access};"),
            format!("assign {access} = 1'b1;"),
            format!("always_comb begin value = '0; {access} = 1'b1; end"),
            format!("always_ff @(posedge clk) {access} <= 1'b1;"),
            format!("Pass p(.y({access}));"),
        ] {
            let source = format!(
                "module Top(input bit clk, index, output logic [7:0] y); struct packed {{ logic [3:0] high; logic [1:0][3:0] m; logic [3:2][7:4] offset; logic scalar; logic [7:4] shifted; }} value; {statement} endmodule module Pass(output logic y); assign y = 1'b1; endmodule"
            );
            assert!(
                analyze_source(&source, Path::new("invalid_multidimensional_member.sv")).is_err(),
                "{statement}"
            );
        }
    }
}

#[test]
fn preserves_packed_dimensions_on_struct_aliases() {
    let ir = analyze_source(
        r#"
            module Top;
                typedef struct packed signed { bit [3:0] a, b; } t;
                typedef t [1:0] pair_t;
                typedef pair_t [2:0] triple_t;
                pair_t pair;
                triple_t triple;
                struct packed { pair_t pairs; bit flag; } nested;
            endmodule
        "#,
        Path::new("struct_alias_dimensions.sv"),
    )
    .unwrap();
    for (name, width, signed) in [
        // A packed array of signed structs is unsigned (IEEE 1800-2023 7.4.1).
        ("pair", 16, false),
        ("triple", 48, false),
        ("nested", 17, false),
    ] {
        let ty = ir.modules()[0]
            .signals()
            .iter()
            .find(|signal| signal.name() == name)
            .unwrap()
            .r#type();
        assert_eq!(ty.resolved_width(), Some(width), "{name}");
        assert_eq!(ty.is_signed(), signed, "{name}");
        assert_eq!(ty.kind(), ir::TypeKind::Bit, "{name}");
    }
}

#[test]
fn rejects_member_access_on_packed_struct_array_aliases() {
    for access in ["value.a", "value[0].a", "outer.pairs.a", "outer.pairs[0].a"] {
        let source = format!(
            "module Top(output logic [3:0] y); typedef struct packed {{ logic [3:0] a, b; }} t; typedef t [1:0] pair_t; pair_t value; struct packed {{ pair_t pairs; logic flag; }} outer; assign y = {access}; endmodule"
        );
        assert!(
            analyze_source(&source, Path::new("struct_array_alias_member.sv")).is_err(),
            "{access}"
        );
    }
}
