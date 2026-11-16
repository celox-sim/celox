//! The name after the end of a block or declaration (IEEE 1800-2023 9.3.4).
use super::*;

fn parse_error(code: &str) -> String {
    Declarations::parse(code, Path::new("end_labels.sv"))
        .unwrap_err()
        .to_string()
}

#[test]
fn rejects_end_labels_that_differ_from_the_block_name() {
    for (code, message) in [
        (
            "module top; initial begin : named_begin end : wrong_name endmodule",
            "end label `wrong_name` differs from the name `named_begin` of its block",
        ),
        (
            "module top; initial fork : named_begin join : wrong_name endmodule",
            "end label `wrong_name` differs from the name `named_begin` of its block",
        ),
        (
            "module top; initial fork : f join_none : g endmodule",
            "end label `g` differs from the name `f` of its block",
        ),
        (
            "module top; initial l : begin end : m endmodule",
            "end label `m` differs from the name `l` of its block",
        ),
        (
            "module test(); endmodule : test_bad",
            "end label `test_bad` differs from the name `test` of its block",
        ),
        (
            "module top; if (1) begin : g end : h endmodule",
            "end label `h` differs from the name `g` of its block",
        ),
        (
            "module top; function void f(); endfunction : g endmodule",
            "end label `g` differs from the name `f` of its subroutine",
        ),
        (
            "module top; task t; endtask : u endmodule",
            "end label `u` differs from the name `t` of its subroutine",
        ),
    ] {
        let error = parse_error(code);
        assert!(error.contains(message), "{code}: {error}");
    }
}

#[test]
fn rejects_end_labels_of_unnamed_blocks() {
    for code in [
        "module top; generate if (1) begin end : label endgenerate endmodule",
        "module top; initial begin end : label endmodule",
        "module top; initial fork join : label endmodule",
    ] {
        let error = parse_error(code);
        assert!(
            error.contains("end label `label` of an unnamed block"),
            "{code}: {error}"
        );
    }
}

#[test]
fn accepts_end_labels_that_repeat_the_block_name() {
    for code in [
        "module top; initial begin : b end : b endmodule",
        "module top; initial fork : f join_any : f endmodule",
        "module top; initial l : begin end : l endmodule",
        "module top; if (1) begin : g end : g else begin : h end : h endmodule",
        "module top; for (genvar i = 0; i < 2; i++) g : begin end : g endmodule",
        "module top; function void f(); endfunction : f task t; endtask : t endmodule : top",
        "package p; endpackage : p interface i; endinterface : i program q; endprogram : q",
        "class C; function new(); endfunction : new endclass : C module m; endmodule",
    ] {
        Declarations::parse(code, Path::new("end_labels.sv"))
            .unwrap_or_else(|error| panic!("{code}: {error}"));
    }
}
