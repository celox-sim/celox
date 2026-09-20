use celox::SimulatorBuilder;

struct Case {
    name: &'static str,
    parameters: &'static str,
    expression: &'static str,
    overrides: &'static [(&'static str, u64)],
    expected: u64,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "bit default",
            parameters: "param P: bit = 1",
            expression: "{P repeat 64}",
            overrides: &[],
            expected: u64::MAX,
        },
        Case {
            name: "bit zero",
            parameters: "param P: bit = 1",
            expression: "{P repeat 64}",
            overrides: &[("P", 0)],
            expected: 0,
        },
        Case {
            name: "bit one",
            parameters: "param P: bit = 0",
            expression: "{P repeat 64}",
            overrides: &[("P", 1)],
            expected: u64::MAX,
        },
        Case {
            name: "bit truncation",
            parameters: "param P: bit = 0",
            expression: "{P repeat 64}",
            overrides: &[("P", 3)],
            expected: u64::MAX,
        },
        Case {
            name: "nibble",
            parameters: "param P: bit<4> = 0",
            expression: "{P repeat 16}",
            overrides: &[("P", 0xa)],
            expected: 0xaaaa_aaaa_aaaa_aaaa,
        },
        Case {
            name: "nibble truncation",
            parameters: "param P: logic<4> = 0",
            expression: "{P repeat 16}",
            overrides: &[("P", 0x1f)],
            expected: u64::MAX,
        },
        Case {
            name: "u32",
            parameters: "param P: u32 = 0",
            expression: "{P repeat 2}",
            overrides: &[("P", 0xa123_4567)],
            expected: 0xa123_4567_a123_4567,
        },
        Case {
            name: "u64 high bit",
            parameters: "param P: u64 = 0",
            expression: "P",
            overrides: &[("P", 0x8000_0000_0000_0001)],
            expected: 0x8000_0000_0000_0001,
        },
        Case {
            name: "signed extension",
            parameters: "param P: signed logic<8> = 0",
            expression: "P",
            overrides: &[("P", 0xff)],
            expected: u64::MAX,
        },
        Case {
            name: "signed comparison",
            parameters: "param P: i32 = 0",
            expression: "if P <: 0 ? 64'd7 : 64'd9",
            overrides: &[("P", 0xffff_fff8)],
            expected: 7,
        },
        Case {
            name: "signed division",
            parameters: "param P: i32 = 0",
            expression: "P / 2",
            overrides: &[("P", 0xffff_fff8)],
            expected: (-4_i64) as u64,
        },
        Case {
            name: "dependent width",
            parameters: "param W: u32 = 4, param P: bit<W> = 0",
            expression: "{P repeat 8}",
            overrides: &[("W", 8), ("P", 0xa5)],
            expected: 0xa5a5_a5a5_a5a5_a5a5,
        },
        Case {
            name: "reverse builder order",
            parameters: "param W: u32 = 4, param P: bit<W> = 0",
            expression: "{P repeat 8}",
            overrides: &[("P", 0xa5), ("W", 8)],
            expected: 0xa5a5_a5a5_a5a5_a5a5,
        },
        Case {
            name: "dependent default",
            parameters: "param W: u32 = 4, param P: bit<W> = '1",
            expression: "{P repeat 8}",
            overrides: &[("W", 8)],
            expected: u64::MAX,
        },
        Case {
            name: "truncated dependency",
            parameters: "param F: bit = 0, param W: u32 = F + 3, param P: bit<W> = 0",
            expression: "{P repeat 16}",
            overrides: &[("P", 0xa), ("F", 3)],
            expected: 0xaaaa_aaaa_aaaa_aaaa,
        },
        Case {
            name: "declared signed const",
            parameters: "param P: i32 = 0",
            expression: "P / 2",
            overrides: &[],
            expected: 0,
        },
    ]
}

macro_rules! check_backend {
    ($test:ident, $build:ident) => {
        #[test]
        fn $test() {
            for case in cases() {
                let source = format!(
                    "module Top #({}) (a: input logic<64>, o: output logic<64>) {{ let value: logic<64> = {}; assign o = a ^ value; }}",
                    case.parameters, case.expression,
                );
                let mut builder = SimulatorBuilder::new(&source, "Top");
                for &(name, value) in case.overrides {
                    builder = builder.param(name, value);
                }
                let mut sim = builder.$build().unwrap();
                let a = sim.signal("a");
                let o = sim.signal("o");
                for input in [0_u64, 1, u64::MAX, 0x0123_4567_89ab_cdef, 1 << 63] {
                    sim.modify(|io| io.set(a, input)).unwrap();
                    assert_eq!(sim.get(o), (input ^ case.expected).into(), "{}", case.name);
                }
            }

            // A u64 API value must zero-extend to a wider declared parameter.
            let mut sim = SimulatorBuilder::new(
                "module Top #(param P: bit<96> = 0) (a: input logic<64>, o: output logic<192>) { assign o = {P repeat 2} ^ {128'd0, a}; }",
                "Top",
            ).param("P", u64::MAX).$build().unwrap();
            let a = sim.signal("a");
            let o = sim.signal("o");
            for input in [0_u64, 1, u64::MAX, 0x0123_4567_89ab_cdef, 1 << 63] {
                sim.modify(|io| io.set(a, input)).unwrap();
                assert_eq!(sim.get(o).to_u64_digits(), vec![u64::MAX ^ input, 0xffff_ffff_0000_0000, 0xffff_ffff]);
            }
        }
    };
}

check_backend!(native_parameter_width, build_native);
check_backend!(cranelift_parameter_width, build_cranelift);
check_backend!(wasm_parameter_width, build_wasm);
check_backend!(interpreter_parameter_width, build_interpreter);
