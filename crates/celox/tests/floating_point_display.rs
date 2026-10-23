use celox::{RuntimeEvent, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn rtl_comb_float_display_and_write(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
            module Top(a: input i32, result: output i32) {
                var wide: f64;
                var narrow: f32;
                always_comb {
                    wide = (a as f64) / 2.0;
                    narrow = a as f32;
                    result = wide as i32;
                    $display("wide=%f narrow=%0.2f neg=%0.2f", wide, narrow, -wide);
                    $write("fraction=%0.2f", wide);
                }
            }
        "#, "Top");
        sim.drain_runtime_events();
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 3u32)).unwrap();
        assert_eq!(sim.drain_runtime_events(), vec![
            RuntimeEvent::Display { message: b"wide=1.500000 narrow=3.00 neg=-1.50".to_vec() },
            RuntimeEvent::Write { message: b"fraction=1.50".to_vec() },
        ]);
    }

    fn rtl_implicit_float_messages(sim) {
        @omit_veryl;
        // Veryl separates arguments with spaces and uses %g. Emitted SV's
        // implicit real formatting concatenates %#g fields instead.
        @omit_sv;
        @build Simulator::builder(r#"
            module Top(a: input i32) {
                always_comb { $display((a as f64) / 2.0, a as f32); }
            }
        "#, "Top");
        sim.drain_runtime_events();
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 3u32)).unwrap();
        assert_eq!(sim.drain_runtime_events(), vec![RuntimeEvent::Display { message: b"1.5 3".to_vec() }]);
    }

    fn rtl_negative_float_constants(sim) {
        @omit_veryl;
        // Upstream Veryl constant folding corrupts negative f32 payloads (#1657).
        @ignore_on(native, native_parallel, cranelift, cranelift_parallel, wasm, interp);
        @build Simulator::builder(r#"
            module Top(clk: input clock, q: output i32) {
                always_ff (clk) {
                    q = (-2.25 as f32) as i32;
                    $display("constant=%0.2f wide=%0.2f", -2.25 as f32, -1.5);
                }
            }
        "#, "Top");
        let clk = sim.event("clk");
        sim.drain_runtime_events();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 0xfffffffeu32.into());
        assert_eq!(sim.drain_runtime_events(), vec![RuntimeEvent::Display { message: b"constant=-2.25 wide=-1.50".to_vec() }]);
    }

    fn rtl_ff_float_messages(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
            module Top(clk: input clock, a: input i32) {
                always_ff (clk) {
                    $display("ff=%0.2f f32=%0.2f", (a as f64) / 2.0, a as f32);
                    $assert_continue(1'b0, "value=%0.2f", (a as f64) / 2.0);
                }
            }
        "#, "Top");
        let a = sim.signal("a");
        let clk = sim.event("clk");
        sim.modify(|io| io.set(a, 3u32)).unwrap();
        sim.drain_runtime_events();
        sim.tick(clk).unwrap();
        assert_eq!(sim.drain_runtime_events(), vec![
            RuntimeEvent::Display { message: b"ff=1.50 f32=3.00".to_vec() },
            RuntimeEvent::AssertContinue { message: b"value=1.50".to_vec() },
        ]);
    }
}

#[test]
#[ignore = "upstream Veryl negative f32 constant folding: https://github.com/celox-sim/celox/issues/1657"]
fn testbench_float_output_and_assertion_messages() {
    const CHILD: &str = "CELOX_TEST_FLOAT_DISPLAY_CHILD";
    #[allow(clippy::disallowed_methods)]
    let child = std::env::var_os(CHILD).is_some();
    if child {
        let result = Simulator::builder(
            r#"
            #[test(Top)]
            module Top {
                var wide: f64;
                var narrow: f32;
                initial {
                    wide = 1.5;
                    narrow = -2.25 as f32;
                    $display("tb=%f narrow=%0.2f", wide, narrow);
                    $write("joined=%0.2f", -wide);
                    $display(" done=%0.2f", narrow);
                    $display(wide, narrow);
                    $display("exp=%0.2e general=%g signed=%+8.2f", wide, wide, wide);
                    $assert_continue(1'b0, "message=%0.2f/%0.2f", wide, narrow);
                    $finish();
                }
            }
        "#,
            "Top",
        )
        .run_test_detailed()
        .unwrap();
        assert!(!result.passed);
        assert_eq!(
            result.assertions[0].message.as_deref(),
            Some(b"message=1.50/-2.25".as_slice())
        );
        return;
    }
    let current = std::thread::current();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            current.name().unwrap(),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("tb=1.500000 narrow=-2.25\n"), "{stderr}");
    assert!(stderr.contains("joined=-1.50 done=-2.25\n"), "{stderr}");
    assert!(stderr.contains("1.5 -2.25\n"), "{stderr}");
    assert!(
        stderr.contains("exp=1.50e+00 general=1.5 signed=   +1.50\n"),
        "{stderr}"
    );
}

#[test]
fn testbench_runtime_float_output_and_assertion_messages() {
    const CHILD: &str = "CELOX_TEST_FLOAT_DISPLAY_CHILD";
    #[allow(clippy::disallowed_methods)]
    let child = std::env::var_os(CHILD).is_some();
    if child {
        let result = Simulator::builder(
            r#"
            #[test(Top)]
            module Top {
                var wide: f64;
                var narrow: f32;
                initial {
                    wide = 1.5;
                    narrow = -(wide + 0.75) as f32;
                    $display("tb=%f narrow=%0.2f", wide, narrow);
                    $write("joined=%0.2f", -wide);
                    $display(" done=%0.2f", narrow);
                    $display(wide, narrow);
                    $display("exp=%0.2e general=%g signed=%+8.2f", wide, wide, wide);
                    $assert_continue(1'b0, "message=%0.2f/%0.2f", wide, narrow);
                    $finish();
                }
            }
        "#,
            "Top",
        )
        .run_test_detailed()
        .unwrap();
        assert!(!result.passed);
        assert_eq!(
            result.assertions[0].message.as_deref(),
            Some(b"message=1.50/-2.25".as_slice())
        );
        return;
    }
    let current = std::thread::current();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            current.name().unwrap(),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("tb=1.500000 narrow=-2.25\n"), "{stderr}");
    assert!(stderr.contains("joined=-1.50 done=-2.25\n"), "{stderr}");
    assert!(stderr.contains("1.5 -2.25\n"), "{stderr}");
    assert!(
        stderr.contains("exp=1.50e+00 general=1.5 signed=   +1.50\n"),
        "{stderr}"
    );
}

#[test]
#[ignore = "upstream Veryl FP constant folding: https://github.com/celox-sim/celox/issues/1657"]
fn named_float_constants_and_parameters_display_numeric_values() {
    let source = r#"
        module Top #(param P: f32 = -2.25 as f32, param Q: f32 = P + (0.5 as f32))
                   (clk: input clock, mirror: output f32) {
            const C: f32 = -2.25 as f32;
            const POS: f32 = 1.25 as f32;
            const W: f64 = -1.5;
            const DEP: f32 = C + (0.5 as f32);
            assign mirror = P;
            always_ff (clk) {
                $display("C=%0.2f P=%0.2f Q=%0.2f W=%0.2f POS=%0.2f DEP=%0.2f", C, P, Q, W, POS, DEP);
            }
        }
    "#;
    for (override_bits, expected) in [
        (None, "C=-2.25 P=-2.25 Q=-1.75 W=-1.50 POS=1.25 DEP=-1.75"),
        (
            Some(1.25f32.to_bits()),
            "C=-2.25 P=1.25 Q=1.75 W=-1.50 POS=1.25 DEP=-1.75",
        ),
    ] {
        let builder = Simulator::builder(source, "Top");
        let builder = if let Some(bits) = override_bits {
            builder.param("P", bits as u64)
        } else {
            builder
        };
        let mut sim = builder.build().unwrap();
        let clk = sim.event("clk");
        sim.tick(clk).unwrap();
        assert_eq!(
            sim.drain_runtime_events(),
            vec![RuntimeEvent::Display {
                message: expected.as_bytes().to_vec()
            }]
        );
    }
    // Preserve an intentional override even when its bits equal Veryl's buggy
    // default cache (unsigned f64(-2.25) bits converted numerically to f32).
    let same_bits = ((-2.25f64).to_bits() as f32).to_bits();
    let mut sim = Simulator::builder(source, "Top")
        .param("P", same_bits as u64)
        .build()
        .unwrap();
    assert_eq!(sim.get(sim.signal("mirror")), same_bits.into());
}

#[test]
#[ignore = "upstream Veryl FP constant folding: https://github.com/celox-sim/celox/issues/1657"]
fn hierarchical_float_parameter_overrides_keep_their_own_scope() {
    let source = r#"
        module Child #(param P: f32 = -2.25 as f32) (clk: input clock) {
            const C: f32 = P + (0.5 as f32);
            always_ff (clk) { $display("P=%0.2f C=%0.2f", P, C); }
        }
        module Top(clk: input clock) {
            inst child: Child #(P: 1.25 as f32) (clk);
        }
    "#;
    let mut sim = Simulator::builder(source, "Top").build().unwrap();
    let clk = sim.event("clk");
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![RuntimeEvent::Display {
            message: b"P=1.25 C=1.75".to_vec()
        }]
    );
}

#[test]
#[ignore = "upstream Veryl FP constant folding: https://github.com/celox-sim/celox/issues/1657"]
fn testbench_named_float_constants_display_numeric_values() {
    let result = Simulator::builder(
        r#"
        #[test(Top)] module Top #(param P: f32 = -2.25 as f32) {
            const C: f32 = -2.25 as f32;
            initial { $assert_continue(1'b0, "C=%0.2f P=%0.2f", C, P); $finish(); }
        }
    "#,
        "Top",
    )
    .run_test_detailed()
    .unwrap();
    assert_eq!(
        result.assertions[0].message.as_deref(),
        Some(b"C=-2.25 P=-2.25".as_slice())
    );
}

#[test]
#[ignore = "upstream Veryl FP constant folding: https://github.com/celox-sim/celox/issues/1657"]
fn package_float_constants_do_not_alias_same_named_parameter_overrides() {
    let source = r#"
        package Constants { const P: f32 = -2.25 as f32; }
        module Top #(param P: f32 = 0.5 as f32) (clk: input clock) {
            always_ff (clk) { $display("local=%0.2f package=%0.2f", P, Constants::P); }
        }
    "#;
    let mut sim = Simulator::builder(source, "Top")
        .param("P", 1.25f32.to_bits() as u64)
        .build()
        .unwrap();
    let clk = sim.event("clk");
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![RuntimeEvent::Display {
            message: b"local=1.25 package=-2.25".to_vec()
        }]
    );
}

#[test]
fn integral_parameter_override_is_used_by_float_constant() {
    let source = r#"
        module Child #(param N: u32 = 1) (clk: input clock) {
            const K: u32 = N + 1;
            const F: f32 = N as f32;
            const TRANSITIVE: f32 = K as f32;
            const DIRECT: f32 = (N + 1) as f32;
            always_ff (clk) { $display("F=%0.2f transitive=%0.2f direct=%0.2f", F, TRANSITIVE, DIRECT); }
        }
        module Top(clk: input clock) { inst child: Child #(N: 3) (clk); }
    "#;
    let mut sim = Simulator::builder(source, "Top").build().unwrap();
    let clk = sim.event("clk");
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![RuntimeEvent::Display {
            message: b"F=3.00 transitive=4.00 direct=4.00".to_vec()
        }]
    );
}
