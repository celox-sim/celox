use celox_sv_analyzer::syntax::{parse_source, preprocess_sources};
use std::path::Path;

#[test]
fn ordered_files_share_macros_and_directives_but_separate_calls_do_not() {
    let sources = [
        (
            "`define WIDTH 8\n`define PLUS(x) ((x)+1)\n`default_nettype none\n",
            Path::new("defs.sv"),
        ),
        (
            "module M(output logic [`WIDTH-1:0] q); assign q=`PLUS(40); endmodule\n`undef WIDTH\n",
            Path::new("consumer.sv"),
        ),
        (
            "`ifdef WIDTH\ninvalid\n`endif\nmodule N; endmodule // no trailing newline",
            Path::new("last.sv"),
        ),
    ];
    let text = preprocess_sources(&sources, &[], &[]).unwrap();
    assert!(
        text.split_whitespace()
            .collect::<String>()
            .contains("[8-1:0]"),
        "{text}"
    );
    assert!(
        text.split_whitespace()
            .collect::<String>()
            .contains("((40)+1)"),
        "{text}"
    );
    assert!(!text.contains("invalid"), "{text}");
    let parsed = parse_source(&text, Path::new("unit.sv")).unwrap();
    assert_eq!(parsed.units.len(), 2);
    assert!(
        preprocess_sources(
            &[(
                "module M; localparam N=`PLUS(0); endmodule",
                Path::new("separate.sv")
            )],
            &[],
            &[]
        )
        .is_err()
    );
    assert!(preprocess_sources(&sources[1..], &[], &[]).is_err());
}

#[test]
fn later_files_retain_builtin_locations_and_preprocessing_diagnostics() {
    let sources = [
        ("`define VALUE 42\n", Path::new("defs.sv")),
        (
            "module M;\nlocalparam int N=`__LINE__;\ninitial $display(\"%s %d\", `__FILE__, `VALUE); endmodule",
            Path::new("later.sv"),
        ),
    ];
    let text = preprocess_sources(&sources, &[], &[]).unwrap();
    assert!(
        text.split_whitespace().collect::<String>().contains("N=2"),
        "{text}"
    );
    assert!(text.contains("\"later.sv\""), "{text}");
    parse_source(&text, Path::new("defs.sv")).unwrap();
    let error = preprocess_sources(&[("`MISSING", Path::new("bad.sv"))], &[], &[])
        .unwrap_err()
        .to_string();
    assert!(error.contains("bad.sv"), "{error}");
}

#[test]
fn serialized_macro_operators_remain_distinct_tokens() {
    for sources in [
        vec![(
            "`define NEG -\nmodule M(output int y); assign y=`NEG-1; endmodule",
            Path::new("one.sv"),
        )],
        vec![
            ("`define NEG -\n", Path::new("defs.h")),
            (
                "module M(output int y); assign y=`NEG-1; endmodule",
                Path::new("two.sv"),
            ),
        ],
    ] {
        let expanded = preprocess_sources(&sources, &[], &[]).unwrap();
        assert!(!expanded.contains("--1"), "{expanded}");
        parse_source(&expanded, Path::new("unit.sv")).unwrap();
    }
}
