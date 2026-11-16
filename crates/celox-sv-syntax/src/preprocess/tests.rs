use super::*;

fn preprocess(code: &str) -> String {
    Preprocessor::default()
        .preprocess(code, "original.sv")
        .unwrap()
        .text()
}

fn words(code: &str) -> String {
    Preprocessor::default()
        .preprocess(code, "original.sv")
        .unwrap()
        .tokens
        .iter()
        .filter(|token| !token.is_trivia())
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn pragmas_require_a_name_on_the_directive_line() {
    for code in [
        "`pragma",
        "`pragma \t",
        "`pragma // comment\nmodule M; endmodule",
        "`pragma /* comment */\nmodule M; endmodule",
        "`pragma\nmodule M; endmodule",
    ] {
        let error = Preprocessor::default()
            .preprocess(code, "pragma.sv")
            .unwrap_err();
        assert!(error.to_string().contains("expected a name after `pragma"));
    }
    for code in [
        "`pragma unknown",
        "`pragma /* comment */ unknown value=1\nmodule M; endmodule",
        "`ifdef UNDEFINED\n`pragma\n`endif\nmodule M; endmodule",
    ] {
        Preprocessor::default()
            .preprocess(code, "pragma.sv")
            .unwrap();
    }
}

#[test]
fn macro_generated_includes_end_at_each_invocation() {
    let mut pp = Preprocessor::new(Options {
        includes: IncludePolicy::Ignore,
        ..Default::default()
    });
    let output = pp.preprocess(
            "`define INCLUDE(F) `include F\n`INCLUDE(\"one.svh\") `INCLUDE(\"two.svh\")\nmodule M; endmodule",
            "macro-include.sv",
        ).unwrap();
    assert!(output.text().contains("module M; endmodule"));
}

#[cfg(unix)]
#[test]
fn includes_accept_readable_special_files() {
    let mut pp = Preprocessor::default();
    let output = pp
        .preprocess("`include \"/dev/null\"\nmodule M; endmodule", "include.sv")
        .unwrap();
    assert!(output.text().contains("module M; endmodule"));
}

#[test]
fn definitions_persist_and_inactive_directives_do_not_execute() {
    let mut pp = Preprocessor::default();
    pp.define("TOOL");
    pp.preprocess("`define SHARED 17\n", "first.sv").unwrap();
    let output = pp.preprocess("`ifdef TOOL\n`ifndef SHARED\nbad\n`elsif SHARED\n`SHARED\n`else\nbad\n`endif\n`else\n`undef SHARED\n`define HIDDEN 1\n`include \"missing\"\n`endif\n", "second.sv").unwrap();
    assert_eq!(output.text().trim(), "17");
    assert!(pp.is_defined("SHARED"));
    assert!(!pp.is_defined("HIDDEN"));
    pp.preprocess("`undef SHARED\n`undefineall\n", "third.sv")
        .unwrap();
    assert!(!pp.is_defined("TOOL"));
    assert!(!pp.is_defined("SHARED"));
}

#[test]
fn arguments_defaults_and_nested_delimiters_follow_22_5_1() {
    let definition = "`define M(a=5,b,c=\"C\") a+b+c\n";
    assert_eq!(words(&format!("{definition}`M(1,,3)")), "1 + + 3");
    assert_eq!(words(&format!("{definition}`M(,2,)")), "5 + 2 + \"C\"");
    assert_eq!(words(&format!("{definition}`M(,2)")), "5 + 2 + \"C\"");
    assert_eq!(words("`define M(a,b=) a+b\n`M(1)"), "1 +");
    assert_eq!(
        words("`define M(a,b) a+b\n`M({1,2},f(3,[4,5]))"),
        "{ 1 , 2 } + f ( 3 , [ 4 , 5 ] )"
    );
    assert_eq!(
        words("`define M(a) a\n`M(\\name,with)paren )"),
        "\\name,with)paren"
    );
    for usage in ["`M(1)", "`M()", "`M(1,2,3)", "`M", "`M(1,2]"] {
        assert!(
            Preprocessor::default()
                .preprocess(&format!("`define M(a,b) a+b\n{usage}"), "bad.sv")
                .is_err(),
            "{usage}"
        );
    }
    // Whitespace before a definition's '(' makes an object-like macro;
    // whitespace before an invocation's '(' is allowed.
    assert_eq!(words("`define M (a)\n`M"), "( a )");
    assert_eq!(words("`define M(a) a\n`M (2)"), "2");
    assert_eq!(words("`define \\M@CRO (a=5) a\n`\\M@CRO ( )"), "5");
}

#[test]
fn nested_expansion_alias_rescan_and_recursion() {
    assert_eq!(
        words("`define TOP(a,b) a+b\n`TOP(`TOP(b,1),`TOP(42,a))"),
        "b + 1 + 42 + a"
    );
    assert_eq!(
        words("`define ADD(x) x+1\n`define ALIAS `ADD\n`ALIAS(2) `ALIAS(3)"),
        "2 + 1 3 + 1"
    );
    assert_eq!(
        words("`define A `B\n`define B 4\n`A\n`define B 7\n`A"),
        "4 7"
    );
    for code in [
        "`define A `A\n`A",
        "`define A `B\n`define B `A\n`A",
        "`MISSING",
        "`define M(x=`M()) x\n`M()",
        "`define M(x=`N()) x\n`define N(x=`M()) x\n`M()",
        "`define Q(x=`Q()) `\"x`\"\n`Q()",
    ] {
        assert!(
            Preprocessor::default().preprocess(code, "bad.sv").is_err(),
            "{code}"
        );
    }
    assert_eq!(words("`define M(x=4) x\n`M(`M())"), "4");
}

#[test]
fn comments_strings_continuations_and_macro_operators() {
    assert_eq!(
        words("`define A 1 /* ignored\n comment */ + \\\n2 // ignored\n`A"),
        "1 + 2"
    );
    assert_eq!(words("`define A 1 // ignored \\\n+2\n`A"), "1 + 2");
    assert_eq!(
        words("`define H(x) \"Hello x `X\"\n`H(world)"),
        "\"Hello x `X\""
    );
    assert_eq!(
        words("`define append(f) f``_primary\n`append(clock)"),
        "clock_primary"
    );
    assert_eq!(
        words("`define QUOTE(x) `\"x`\"\n`QUOTE(hello world)"),
        "\"hello world\""
    );
    assert_eq!(
        words("`define HI hello\n`define Q(x) `\"`HI x`\"\n`Q(world)"),
        "\"hello world\""
    );
    assert_eq!(
        words("`define msg(x,y) `\"x: `\\`\"y`\\`\"`\"\n`msg(left side,right side)"),
        "\"left side: \\\"right side\\\"\""
    );
    assert_eq!(words("`define M(x) x``+\n`M(foo) 1"), "foo + 1");
    // A defined macro that ends at `` is expanded before the join; a
    // name without a definition is still being built.
    assert_eq!(
        words("`define P generic\n`define J(p, n) p``_``n\n`define N(n) `J(`P, n)\n`N(and2)"),
        "generic_and2"
    );
    assert_eq!(
        words("`define P a b\n`define J(n) `P``_``n\n`J(c)"),
        "a b_c"
    );
    assert_eq!(words("`define P_x 7\n`define J(n) `P_``n\n`J(x)"), "7");
    assert_eq!(words("`define E\n`define J(n) `E``n\n`J(c) d"), "c d");
    // The operator can build the delimiters of a block comment.
    assert_eq!(
        words("`define C(x) /``* pragma `\"x`\" *``/ always @(x)\n`C(clk) begin end"),
        "always @ ( clk ) begin end"
    );
    assert!(
        Preprocessor::default()
            .preprocess("`define C /``* open\n`C", "bad.sv")
            .is_err()
    );
    // And the delimiter of a one-line comment, which runs to the end of its
    // line: the newline of a continued definition, or the line of the usage.
    assert_eq!(
        words("`define L(x) wire x; /``/ note x \\\n wire x``_n;\n`L(a) wire b;"),
        "wire a ; wire a_n ; wire b ;"
    );
    assert_eq!(
        words("`define L(x) wire x; /``/ note x\n`L(a) hidden;\nwire b;"),
        "wire a ; wire b ;"
    );
    assert_eq!(
        words("`define I(x) x /``/ inner\n`define O(x) `I(x) + 1\n`O(a)\nb"),
        "a b"
    );
    assert_eq!(
        words("`define S \"continued\\\nstring\"\n`S"),
        "\"continuedstring\""
    );
    assert_eq!(
        words("`define S \"continued\\\r\nstring\"\r\n`S"),
        "\"continuedstring\""
    );
    let code = "// retained\nmodule/**/Top; /* retained */ endmodule";
    assert_eq!(preprocess(code), code);
    let mut pp = Preprocessor::new(Options {
        strip_comments: true,
        ..Options::default()
    });
    let output = pp.preprocess(code, "comments.sv").unwrap().text();
    assert!(!output.contains("retained"));
    assert!(output.contains("module Top"));
}

#[test]
fn original_spelling_invocation_and_derived_contributors_survive() {
    let code = "`define INNER(x) x``_suffix\n`define OUTER(x) `INNER(x)\nmodule Top; wire `OUTER(clock); endmodule";
    let mut pp = Preprocessor::default();
    let output = pp.preprocess(code, "authored.sv").unwrap();
    let token = output
        .tokens
        .iter()
        .find(|t| t.text == "clock_suffix")
        .unwrap();
    assert_eq!(pp.sources.text(token.origin.spelling), "clock");
    assert!(token.contributors.len() >= 2);
    assert_eq!(pp.sources.text(pp.sources.invocation(token.origin)), "`");
    let inner = pp.sources.expansion(token.origin.expansion.unwrap());
    assert_eq!(inner.name, "INNER");
    let outer = pp.sources.expansion(inner.invocation.expansion.unwrap());
    assert_eq!(outer.name, "OUTER");
    assert_eq!(
        pp.sources.file(token.origin.spelling.file).path,
        Path::new("authored.sv")
    );
}

#[test]
fn line_directives_preserve_physical_spans_and_supply_logical_builtins() {
    let mut pp = Preprocessor::default();
    let output = pp
        .preprocess(
            "`line 90 \"generated.sv\" 0\n`__FILE__ `__LINE__\n",
            "physical.sv",
        )
        .unwrap();
    let tokens = significant(&output.tokens);
    assert_eq!(tokens[tokens.len() - 2].text, "\"generated.sv\"");
    let line = tokens[tokens.len() - 1];
    assert_eq!(line.text, "90");
    assert_eq!(
        pp.sources.file(line.origin.spelling.file).path,
        Path::new("physical.sv")
    );
    assert_eq!(
        pp.sources
            .file(line.origin.spelling.file)
            .line_column(line.origin.spelling.start)
            .0,
        2
    );
    let error = pp
        .preprocess("`line 40 \"logical.sv\" 0\n`MISSING", "physical.sv")
        .unwrap_err();
    assert_eq!((error.path, error.line), (PathBuf::from("logical.sv"), 40));
    let output = pp
        .preprocess("`line 5 \"a\\\\b\\\"c.sv\" 0\n`__FILE__", "physical.sv")
        .unwrap();
    assert_eq!(
        significant(&output.tokens).last().unwrap().text,
        "\"a\\\\b\\\"c.sv\""
    );
    let output = pp
        .preprocess(
            &format!("`line {} \"max.sv\" 0\n`__LINE__", usize::MAX),
            "physical.sv",
        )
        .unwrap();
    assert_eq!(
        significant(&output.tokens).last().unwrap().text,
        usize::MAX.to_string()
    );
    assert!(
        pp.preprocess(
            &format!("`line {} \"overflow.sv\" 0\n\n`__LINE__", usize::MAX),
            "physical.sv"
        )
        .unwrap_err()
        .message
        .contains("supported range")
    );
    for source in [
        "`line 1 \"x.sv\" 0 // comment\n",
        "/* comment */ `line 1 \"x.sv\" 0\n",
        "module Top; `line 1 \"x.sv\" 0\n",
    ] {
        assert!(
            pp.preprocess(source, "physical.sv")
                .unwrap_err()
                .message
                .contains("only whitespace")
        );
    }
}

#[test]
fn malformed_or_unsupported_preprocessing_fails_explicitly() {
    for source in [
        "`ifdef X\n",
        "`endif",
        "`else",
        "`ifdef X\n`else\n`else\n`endif",
        "`ifdef X\n`else\n`elsif Y\n`endif",
        "`define define 1",
        "`define M(x,x) x",
        "`ifdef (X && Y)\n`endif",
        "`define Q(x) `\"x\n`Q(a)",
        "`ifdef UNDEFINED\n/* unterminated",
    ] {
        assert!(
            Preprocessor::default()
                .preprocess(source, "bad.sv")
                .is_err(),
            "{source}"
        );
    }
}

#[test]
fn observations_retain_active_definitions_without_parser_tokens() {
    let mut pp = Preprocessor::default();
    let result = pp
        .preprocess(
            "`define UNUSED timeunit 1ns; // retained\nmodule Top; endmodule\n`undef UNUSED\n",
            "observed.sv",
        )
        .unwrap();
    assert!(!result.tokens.iter().any(|t| t.text == "timeunit"));
    assert!(result.observations.iter().any(|t| t.text == "timeunit"));
    assert!(result.inspection_text().contains("// retained"));
    assert!(result.inspection_text().contains("`undef UNUSED"));
    assert_eq!(
        words("`SV_COV_START `SV_COV_HIER `SV_COV_OVERFLOW"),
        "0 11 - 2"
    );
}

#[test]
fn macro_quotes_preserve_string_characters_and_keep_lexical_scope() {
    assert_eq!(words("`define Q(x) `\"λ x`\"\n`Q(world)"), "\"λ world\"");
    assert_eq!(words("`define Q(x) `\"λ``x`\"\n`Q(world)"), "\"λworld\"");
    assert_eq!(
        words("`define Q(x) `\"/*x*/ x`\"\n`Q(world)"),
        "\"/*world*/ world\""
    );
    assert_eq!(
        words("`define Q(x) `\"λ \\\nx`\"\n`Q(world)"),
        "\"λ world\""
    );
    assert_eq!(
        words("`define Q(x) `\"λ \\\r\nx`\"\r\n`Q(world)"),
        "\"λ world\""
    );
    let mut pp = Preprocessor::default();
    assert!(
        pp.preprocess("`define UNUSED `\"unclosed\nmodule λ; endmodule", "bad.sv")
            .is_err()
    );
    assert!(
        pp.preprocess(
            "`define UNUSED `\"unclosed\nmodule Top; endmodule",
            "unused.sv"
        )
        .is_ok()
    );
    assert!(
        pp.preprocess("`define Q(x) `\"unclosed\n`Q(world)", "bad.sv")
            .is_err()
    );
}

#[test]
fn includes_resolve_relative_and_search_paths_with_persistent_macros() {
    let directory = std::env::temp_dir().join(format!(
        "celox-sv-includes-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(directory.join("search")).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(directory.clone());
    std::fs::write(
        directory.join("first.svh"),
        "`define VALUE 19\n`include <second.svh>\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("search/second.svh"),
        "wire included = `VALUE;\n",
    )
    .unwrap();
    let code = "`define HEADER `\"first.svh`\"\n`include `HEADER\n`VALUE\n";
    let mut pp = Preprocessor::new(Options {
        include_paths: vec![directory.join("search")],
        ..Options::default()
    });
    let output = pp.preprocess(code, directory.join("top.sv")).unwrap();
    assert!(
        output
            .text()
            .split_whitespace()
            .collect::<String>()
            .contains("wireincluded=19;")
    );
    assert_eq!(output.tokens.iter().filter(|t| t.text == "19").count(), 2);
    let wire = output.tokens.iter().find(|t| t.text == "wire").unwrap();
    assert_eq!(
        pp.sources.file(wire.origin.spelling.file).path,
        directory.join("search/second.svh")
    );
    assert!(
        pp.preprocess("`include \"missing.svh\"\n", directory.join("top.sv"))
            .is_err()
    );
    std::fs::write(
        directory.join("recursive.svh"),
        "`include \"recursive.svh\"\n",
    )
    .unwrap();
    assert!(
        pp.preprocess("`include \"recursive.svh\"\n", directory.join("top.sv"))
            .unwrap_err()
            .message
            .contains("nesting limit")
    );
    // Explicit Ignore policy performs no I/O and cannot leak active time
    // directives/macros from a header into either existing consumer.
    std::fs::write(
        directory.join("time.svh"),
        "`timescale 10ns/1ps\n`define HIDDEN 1\n",
    )
    .unwrap();
    let mut ignored = Preprocessor::new(Options {
        includes: IncludePolicy::Ignore,
        ..Options::default()
    });
    let output = ignored
        .preprocess(
            "`include \"time.svh\"\n`include \"missing.svh\"\nmodule Top; endmodule",
            directory.join("top.sv"),
        )
        .unwrap();
    assert!(!output.text().contains("timescale"));
    assert!(!ignored.is_defined("HIDDEN"));
    for source in [
        "`include \"time.svh\" module Hidden; endmodule",
        "`include",
        "`include 1",
        "`include `HEADER extra",
    ] {
        assert!(
            ignored
                .preprocess(source, directory.join("top.sv"))
                .is_err(),
            "{source}"
        );
    }
    // Header guards permit including the same physical file recursively
    // when the nested conditional suppresses the repeated include.
    std::fs::write(
        directory.join("guarded.svh"),
        "`ifndef GUARD\n`define GUARD\n`include \"guarded.svh\"\nwire guarded;\n`endif\n",
    )
    .unwrap();
    assert!(
        pp.preprocess("`include \"guarded.svh\"\n", directory.join("top.sv"))
            .unwrap()
            .text()
            .contains("wire guarded;")
    );
}
