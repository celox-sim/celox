//! `$readmemh` and `$readmemb` load an associative array with integral
//! indices when they execute (IEEE 1800-2023 21.4, 21.4.1): each address of
//! the file creates the element of that index, or replaces it, and the
//! elements the file does not give keep their values. A file address is the
//! index's bit pattern. Errors are run-time reports that end the load and
//! keep the words loaded before them.
use celox::Simulation;
use std::path::Path;

/// The displayed lines and the `$readmem` reports, as severity and message,
/// of running `source` with `plusargs`.
fn run(
    source: &str,
    four_state: bool,
    plusargs: &[String],
) -> (Vec<String>, Vec<(celox::Severity, String)>) {
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("readmem.sv"))], "Top")
        .four_state(four_state)
        .plusargs(plusargs.iter().cloned())
        .build()
        .unwrap();
    sim.run_until(100).unwrap();
    let mut lines = Vec::new();
    let mut reports = Vec::new();
    for event in sim.drain_runtime_events() {
        match event {
            celox::RuntimeEvent::Display { message } => {
                lines.push(String::from_utf8_lossy(&message).into_owned())
            }
            celox::RuntimeEvent::Report(report) => reports.push((
                report.severity,
                String::from_utf8_lossy(&report.message).into_owned(),
            )),
            _ => {}
        }
    }
    (lines, reports)
}

/// A file named at run time, as BlackParrot's `bp_nonsynth_dram` names it
/// with `$value$plusargs`, loads `int`, `longint` and unsigned `longint`
/// indices, following sparse `@` addresses, and a range of the call loads
/// its addresses. Verilator 5.054 prints the same lines; Icarus Verilog 13
/// does not elaborate associative arrays of a typed index.
#[test]
fn run_time_loads_create_the_elements_of_their_addresses() {
    let directory = tempfile::tempdir().unwrap();
    let file = |name: &str, content: &str| {
        let path = directory.path().join(name);
        std::fs::write(&path, content).unwrap();
        path.display().to_string()
    };
    let words = file("a.hex", "11 22\n@10\n33 44\n@fffffffe\n55\n");
    let long = file("l.hex", "@ffffffffffffff00\naa bb\n@7fffffff00000000\ncc\n");
    let ranged = file("r.hex", "1 2 3\n");
    let source = format!(
        r#"
    `timescale 1ns/1ns
    module Top;
      logic [7:0] m [int];
      logic [7:0] l [longint];
      bit [3:0] u [longint unsigned];
      logic [7:0] r [int];
      string file;
      initial begin
        m[5] = 8'h99;
        if (!$value$plusargs("mem=%s", file)) file = "missing.hex";
        $readmemh(file, m);
        $readmemh("{long}", l);
        $readmemh("{long}", u);
        $readmemh("{ranged}", r, 7, 9);
        #1;
        $display("m n=%0d %h %h %h %h %h %h", m.num(), m[0], m[1], m[5], m[16], m[17], m[-2]);
        $display("l n=%0d %h %h %h", l.num(), l[-256], l[-255], l[64'h7fffffff00000000]);
        $display("u n=%0d %h %h", u.num(), u[64'hffffffffffffff00], u[64'hffffffffffffff01]);
        $display("r n=%0d %h %h %h", r.num(), r[7], r[8], r[9]);
      end
    endmodule
    "#
    );
    for four_state in [false, true] {
        let (lines, reports) = run(&source, four_state, &[format!("+mem={words}")]);
        assert_eq!(
            lines,
            [
                "m n=6 11 22 99 33 44 55",
                "l n=3 aa bb cc",
                "u n=3 a b",
                "r n=3 01 02 03",
            ],
            "four-state {four_state}"
        );
        assert!(reports.is_empty(), "{reports:?}");
    }
}

/// An associative array of another instance, or one that is an element of
/// another associative array, is loaded in place, and the processes that
/// read it see the load. An x or z of the file is 0 in a 2-state element
/// (21.4.2), and the words of a `$readmemb` range keep their order; as a
/// fixed array's load, it ignores the words past its finish address.
/// Verilator 5.054 prints the same `hier` line, also loads the third word,
/// past the finish address, and rejects the loads of a wildcard-indexed
/// array and of a nested one.
#[test]
fn loads_reach_hierarchical_and_nested_arrays() {
    let directory = tempfile::tempdir().unwrap();
    let hex = directory.path().join("program.hex");
    std::fs::write(&hex, "00 11 22 33\n@00010000\nAA xB\n").unwrap();
    let bin = directory.path().join("words.mem");
    std::fs::write(&bin, "0001 0010 0100\n").unwrap();
    let source = format!(
        r#"
    `timescale 1ns/1ns
    module Memory;
      bit [7:0] mem [bit [31:0]];
    endmodule
    module Top;
      Memory lmem();
      logic [7:0] wild [*];
      logic [3:0] ranged [int];
      bit [7:0] nested [int][int];
      bit [7:0] seen;
      always_comb seen = nested[3][2];
      initial begin
        $readmemh("{hex}", lmem.mem);
        $readmemh("{hex}", wild);
        $readmemb("{bin}", ranged, 6, 5);
        $readmemh("{hex}", nested[3]);
        #1;
        $display("hier: %h%h%h%h %h n=%0d", lmem.mem[3], lmem.mem[2], lmem.mem[1], lmem.mem[0],
                 lmem.mem[32'h10001], lmem.mem.num());
        $display("wild: %h %b n=%0d", wild[32'h10000], wild[32'h10001], wild.num());
        $display("ranged: %b %b n=%0d", ranged[6], ranged[5], ranged.num());
        $display("nested: %h n=%0d %0d", seen, nested.num(), nested[3].num());
      end
    endmodule
    "#,
        hex = hex.display(),
        bin = bin.display(),
    );
    for four_state in [false, true] {
        let wild = if four_state { "xxxx1011" } else { "00001011" };
        let (lines, reports) = run(&source, four_state, &[]);
        assert_eq!(
            lines,
            [
                "hier: 33221100 0b n=6".to_string(),
                format!("wild: aa {wild} n=6"),
                "ranged: 0001 0010 n=2".to_string(),
                "nested: 22 n=1 6".to_string(),
            ],
            "four-state {four_state}"
        );
        // Three words for a range of two.
        assert!(
            matches!(&reports[..], [(celox::Severity::Warning, message)]
                if message.contains("file contains 3 words for requested range [6:5]")),
            "{reports:?}"
        );
    }
}

/// The run reports an error that ends a load, keeping the words loaded
/// before it, and the simulation goes on: an invalid word, an address past
/// the index type, an address or a word that is no member of an enumerated
/// index or element type, and an address with an unknown bit. A finish
/// address alone bounds the range from 0, whose word count is checked.
#[test]
fn errors_end_a_load_at_run_time() {
    let directory = tempfile::tempdir().unwrap();
    let file = |name: &str, content: &str| {
        let path = directory.path().join(name);
        std::fs::write(&path, content).unwrap();
        path.display().to_string()
    };
    let invalid = file("invalid.hex", "1 2 g 3\n");
    let past = file("past.hex", "@7ffe\n1 2 3\n");
    let members = file("members.hex", "@1\n5\n@2\n6\n");
    let values = file("values.hex", "1 2 7\n");
    let short = file("short.hex", "4 5\n");
    let source = format!(
        r#"
    `timescale 1ns/1ns
    module Top;
      typedef enum bit [1:0] {{ A = 0, B = 1, C = 3 }} key_t;
      typedef enum bit [2:0] {{ X = 1, Y = 2 }} word_t;
      bit [7:0] bad [int];
      bit [7:0] past [shortint];
      bit [7:0] keyed [key_t];
      word_t typed [int];
      bit [7:0] bounded [int];
      bit [7:0] unknown [int];
      logic [7:0] start;
      initial begin
        start = 'x;
        $readmemh("{invalid}", bad);
        $readmemh("{past}", past);
        $readmemh("{members}", keyed);
        $readmemh("{values}", typed);
        $readmemh("{short}", bounded, , 3);
        $readmemh("{short}", unknown, start);
        $display("%0d %0d %0d %0d %0d %0d", bad.num(), past.num(), keyed.num(), typed.num(),
                 bounded.num(), unknown.num());
      end
    endmodule
    "#
    );
    let (lines, reports) = run(&source, true, &[]);
    assert_eq!(lines, ["2 2 1 2 2 0"]);
    let expected = [
        (celox::Severity::Error, "invalid.hex"),
        (
            celox::Severity::Error,
            "outside the index range [-32768:32767]",
        ),
        (
            celox::Severity::Error,
            "address 2 is not a member of the enum index type",
        ),
        (
            celox::Severity::Error,
            "value 7 is not a member of the enum element type",
        ),
        (
            celox::Severity::Warning,
            "file contains 2 words for requested range [0:3]",
        ),
        (celox::Severity::Error, "address contains X or Z"),
    ];
    assert_eq!(reports.len(), expected.len(), "{reports:?}");
    for ((severity, message), (expected_severity, fragment)) in reports.iter().zip(expected) {
        assert_eq!(*severity, expected_severity, "{message}");
        assert!(message.contains(fragment), "{message} lacks {fragment}");
    }
}

/// The analyzer rejects what a load cannot store: an array whose index or
/// element is not an integral value of at most 64 bits, an array a call or
/// an operator gives, and an array selected with an effect, through which
/// each word would select another array.
#[test]
fn loads_reject_destinations_they_cannot_store() {
    for (expected, body) in [
        (
            "index type is not integral of at most 64 bits",
            "chandle h; bit [7:0] mem [chandle]; initial $readmemh(\"m.hex\", mem);",
        ),
        (
            "index type is not integral of at most 64 bits",
            "bit [7:0] mem [bit [64:0]]; initial $readmemh(\"m.hex\", mem);",
        ),
        (
            "elements are not integral values",
            "process mem [int]; initial $readmemh(\"m.hex\", mem);",
        ),
        (
            "not a variable",
            "bit [7:0] a [int], b [int]; bit s; initial $readmemh(\"m.hex\", s ? a : b);",
        ),
        (
            "selected with an effect",
            "bit [7:0] banks [int][int]; int i; initial $readmemh(\"m.hex\", banks[i++]);",
        ),
        (
            "selected with an effect",
            "bit [7:0] banks [int][int]; initial $readmemh(\"m.hex\", banks[$urandom]);",
        ),
    ] {
        let source = format!("module Top; {body} endmodule");
        let error = Simulation::from_sv_sources(vec![(source.as_str(), Path::new("t.sv"))], "Top")
            .build()
            .expect_err("the load is rejected")
            .to_string()
            .replace(['\n', '│'], " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(error.contains(expected), "{body}: {error}");
    }
}
