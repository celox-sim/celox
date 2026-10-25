#![cfg(feature = "systemverilog")]
use std::path::Path;

const SOURCE: &str = r#"
module Top(output logic ok);
  logic [127:0] query, text;
  logic [7:0] value, missing, bad;
  int found;
  initial begin
    query = "number=%d";
    value = 0; missing = 91; bad = 0;
    found = $value$plusargs(query, value);
    ok = found == 1 && value == 244;
    ok &= $test$plusargs("HEL") && !$test$plusargs("LO");
    ok &= !$value$plusargs("absent=%d", missing) && missing == 91;
    ok &= $value$plusargs("text=%s", text) && text == "hello";
    ok &= $value$plusargs("bad=%d", bad) && $isunknown(bad);
  end
endmodule
"#;

macro_rules! run_plusargs {
    ($name:ident, $backend:ident) => {
        #[test]
        fn $name() {
            let mut sim =
                celox::Simulation::from_sv_sources(vec![(SOURCE, Path::new("plusargs.sv"))], "Top")
                    .four_state(true)
                    .plusargs([
                        "+HELLO",
                        "+number=-12",
                        "+number=8",
                        "+text=hello",
                        "+bad=oops",
                    ])
                    .$backend()
                    .unwrap();
            sim.run_until(0).unwrap();
            let ok = sim.signal("ok");
            assert_eq!(sim.get(ok), 1u8.into());
        }
    };
}
run_plusargs!(plusargs_interpreter, build_interpreter);
run_plusargs!(plusargs_cranelift, build);
#[cfg(target_arch = "x86_64")]
run_plusargs!(plusargs_native, build_native);
run_plusargs!(plusargs_wasm, build_wasm);

#[test]
fn plusargs_two_state_coerces_unknown_and_high_impedance_digits_to_zero() {
    let source = r#"
      module Top(output logic ok);
        logic [7:0] value;
        initial begin
          ok = $value$plusargs("value=%h", value) && value == 8'h10;
          ok &= $value$plusargs("invalid=%d", value) && value == 0;
        end
      endmodule
    "#;
    let mut sim = celox::Simulation::from_sv_sources(
        vec![(source, Path::new("plusargs_two_state.sv"))],
        "Top",
    )
    .plusargs(["+value=1z", "+invalid=nope"])
    .build_interpreter()
    .unwrap();
    sim.run_until(0).unwrap();
    let ok = sim.signal("ok");
    assert_eq!(sim.get(ok), 1u8.into());
}

#[test]
fn plusargs_rejects_an_unknown_packed_query() {
    let source = r#"
      module Top(output logic ok);
        logic [7:0] query;
        initial begin query = 8'hxx; ok = $test$plusargs(query); end
      endmodule
    "#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("plusargs_unknown.sv"))], "Top")
            .four_state(true)
            .build_interpreter()
            .unwrap();
    assert!(
        sim.run_until(0)
            .unwrap_err()
            .to_string()
            .contains("plusargs query contains X or Z")
    );
}

macro_rules! negative_unknown_plusargs_backend {
    ($name:ident, $backend:ident) => {
        #[test]
        fn $name() {
            let source = r#"
                module Top(output logic ok,
                    output logic [31:0] hex_value,
                    output logic [16:0] octal_value,
                    output logic [7:0] binary_value, known_value, positive_value);
                  initial begin
                    ok = $value$plusargs("negative_hex=%h", hex_value);
                    ok &= $value$plusargs("negative_octal=%o", octal_value);
                    ok &= $value$plusargs("negative_binary=%b", binary_value);
                    ok &= $value$plusargs("known=%h", known_value);
                    ok &= $value$plusargs("positive=%h", positive_value);
                  end
                endmodule
            "#;
            for four_state in [true, false] {
                let mut simulation = celox::Simulation::from_sv_sources(
                    vec![(source, Path::new("negative_unknown_plusargs.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .plusargs([
                    "+negative_hex=-1x",
                    "+negative_octal=-7z",
                    "+negative_binary=-1?",
                    "+known=-1f",
                    "+positive=1x",
                ])
                .$backend()
                .unwrap();
                simulation.run_until(0).unwrap();
                let ok = simulation.signal("ok");
                assert_eq!(simulation.get(ok), 1u8.into());
                for (name, width) in [("hex_value", 32), ("octal_value", 17), ("binary_value", 8)] {
                    let signal = simulation.signal(name);
                    let expected_mask = if four_state {
                        (celox::BigUint::from(1u8) << width) - celox::BigUint::from(1u8)
                    } else {
                        celox::BigUint::default()
                    };
                    assert_eq!(
                        simulation.get_four_state(signal),
                        (celox::BigUint::default(), expected_mask),
                        "{name}, four_state={four_state}"
                    );
                }
                let known = simulation.signal("known_value");
                assert_eq!(simulation.get(known), 225u32.into());
                let positive = simulation.signal("positive_value");
                assert_eq!(
                    simulation.get_four_state(positive),
                    (
                        16u32.into(),
                        if four_state {
                            15u32.into()
                        } else {
                            0u32.into()
                        }
                    )
                );
            }
        }
    };
}
negative_unknown_plusargs_backend!(negative_unknown_plusargs_interpreter, build_interpreter);
negative_unknown_plusargs_backend!(negative_unknown_plusargs_cranelift, build);
#[cfg(target_arch = "x86_64")]
negative_unknown_plusargs_backend!(negative_unknown_plusargs_native, build_native);
negative_unknown_plusargs_backend!(negative_unknown_plusargs_wasm, build_wasm);

macro_rules! decimal_and_separator_plusargs_backend {
    ($name:ident, $backend:ident) => {
        #[test]
        fn $name() {
            let source = r#"
                module Top(output logic ok,
                    output logic [15:0] z_lower, z_upper, question, x_lower, x_upper,
                    signed_z, signed_x, leading, signed_leading, separators, interior);
                  initial begin
                    ok = $value$plusargs("zl=%d", z_lower);
                    ok &= $value$plusargs("zu=%D", z_upper);
                    ok &= $value$plusargs("question=%d", question);
                    ok &= $value$plusargs("xl=%d", x_lower);
                    ok &= $value$plusargs("xu=%d", x_upper);
                    ok &= $value$plusargs("sz=%d", signed_z);
                    ok &= $value$plusargs("sx=%d", signed_x);
                    ok &= $value$plusargs("leading=%d", leading);
                    ok &= $value$plusargs("signed_leading=%h", signed_leading);
                    ok &= $value$plusargs("separators=%d", separators);
                    ok &= $value$plusargs("interior=%d", interior);
                  end
                endmodule
            "#;
            for four_state in [true, false] {
                let mut simulation = celox::Simulation::from_sv_sources(
                    vec![(source, Path::new("decimal_and_separator_plusargs.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .plusargs([
                    "+zl=z",
                    "+zu=Z",
                    "+question=?",
                    "+xl=x",
                    "+xu=X",
                    "+sz=-z",
                    "+sx=+x",
                    "+leading=_12",
                    "+signed_leading=-_12",
                    "+separators=___",
                    "+interior=1_2_3_4_",
                ])
                .$backend()
                .unwrap();
                simulation.run_until(0).unwrap();
                let ok = simulation.signal("ok");
                assert_eq!(simulation.get(ok), 1u8.into());
                for name in [
                    "z_lower",
                    "z_upper",
                    "question",
                    "x_lower",
                    "x_upper",
                    "signed_z",
                    "signed_x",
                    "leading",
                    "signed_leading",
                    "separators",
                ] {
                    let signal = simulation.signal(name);
                    let z = matches!(name, "z_lower" | "z_upper" | "question");
                    let value = if four_state && z { 65535u32 } else { 0 };
                    let mask = if four_state { 65535u32 } else { 0 };
                    assert_eq!(
                        simulation.get_four_state(signal),
                        (value.into(), mask.into()),
                        "{name}, four_state={four_state}"
                    );
                }
                let interior = simulation.signal("interior");
                assert_eq!(simulation.get(interior), 1234u32.into());
            }
        }
    };
}
decimal_and_separator_plusargs_backend!(
    decimal_and_separator_plusargs_interpreter,
    build_interpreter
);
decimal_and_separator_plusargs_backend!(decimal_and_separator_plusargs_cranelift, build);
#[cfg(target_arch = "x86_64")]
decimal_and_separator_plusargs_backend!(decimal_and_separator_plusargs_native, build_native);
decimal_and_separator_plusargs_backend!(decimal_and_separator_plusargs_wasm, build_wasm);

// These sources exercise literal decoding through both packed assignments and
// the runtime plusargs query transport, including physical CRLF continuations.
macro_rules! packed_string_literals_backend {
    ($name:ident, $backend:ident) => {
        #[test]
        fn $name() {
            let source = concat!(
                "module Top(output logic ok);\n",
                "  logic [39:0] text; int value;\n",
                "  initial begin\n",
                "    text = \"\"\"A\"B\nC\"\"\";\n",
                "    ok = text == 40'h4122420a43;\n",
                "    ok &= $value$plusargs(\"\"\"number=%d\"\"\", value) && value == 17;\n",
                "    ok &= $test$plusargs(\"ab\\\r\ncd\");\n",
                "    ok &= $test$plusargs(\"\"\"ab\\\r\ncd\"\"\");\n",
                "    ok &= $test$plusargs(\"ab\\\ncd\");\n",
                "    ok &= $test$plusargs(\"\"\"ab\\\ncd\"\"\");\n",
                "    text = \"ab\\\r\ncd\";\n",
                "    ok &= text == 40'h0061626364;\n",
                "  end\nendmodule\n",
            );
            let mut sim = celox::Simulation::from_sv_sources(
                vec![(source, Path::new("packed_string_literals.sv"))],
                "Top",
            )
            .plusargs(["+number=17", "+abcd"])
            .$backend()
            .unwrap();
            sim.run_until(0).unwrap();
            let ok = sim.signal("ok");
            assert_eq!(sim.get(ok), 1u8.into());
        }
    };
}
packed_string_literals_backend!(packed_string_literals_interpreter, build_interpreter);
packed_string_literals_backend!(packed_string_literals_cranelift, build);
#[cfg(target_arch = "x86_64")]
packed_string_literals_backend!(packed_string_literals_native, build_native);
packed_string_literals_backend!(packed_string_literals_wasm, build_wasm);

/// A plusargs query among the arguments of a `$display` suspends the process
/// for the host; the arguments read before and after it keep their values.
const DISPLAYED: &str = r#"
module Top;
  int i;
  logic [7:0] v;
  initial begin
    i = 3;
    v = 0;
    $display("%0d %0d %0d %0d", i, $test$plusargs("verbose"), $value$plusargs("v=%d", v), v);
  end
endmodule
"#;

macro_rules! display_plusargs {
    ($name:ident, $backend:ident) => {
        #[test]
        fn $name() {
            let mut sim = celox::Simulation::from_sv_sources(
                vec![(DISPLAYED, Path::new("displayed.sv"))],
                "Top",
            )
            .plusargs(["+verbose", "+v=7"])
            .$backend()
            .unwrap();
            sim.run_until(0).unwrap();
            let lines: Vec<String> = sim
                .drain_runtime_events()
                .into_iter()
                .filter_map(|event| match event {
                    celox::RuntimeEvent::Display { message } => {
                        Some(String::from_utf8_lossy(&message).into_owned())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(lines, ["3 1 1 7"]);
        }
    };
}
display_plusargs!(display_of_plusargs_interpreter, build_interpreter);
display_plusargs!(display_of_plusargs_compiled, build);
#[cfg(target_arch = "x86_64")]
display_plusargs!(display_of_plusargs_native, build_native);
