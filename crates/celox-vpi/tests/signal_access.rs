//! Explicit signal access is a native-image API/optimization contract.
#![cfg(feature = "systemverilog")]
use celox::BigUint;
use celox::{NativeProgramInstance, OptLevel, SignalAccessOptions, SimBackend, Simulator};

fn instance(source: &str, level: OptLevel, access: SignalAccessOptions) -> NativeProgramInstance {
    let simulator =
        Simulator::from_sv_sources(vec![(source, std::path::Path::new("force.sv"))], "Top")
            .four_state(true)
            .opt_level(level)
            .signal_access(access)
            .build()
            .unwrap();
    let bytes = simulator
        .shared_code()
        .program_image()
        .to_container_bytes()
        .unwrap();
    // Safety: execute only the compiler artifact created by this test.
    unsafe { NativeProgramInstance::from_attached_bytes(&bytes) }.unwrap()
}
fn id(sim: &NativeProgramInstance, name: &str) -> celox::ReflectionSignalId {
    sim.reflection()
        .signals()
        .iter()
        .position(|signal| signal.full_name == name)
        .map(|index| celox::ReflectionSignalId(index as u32))
        .unwrap()
}
fn read(sim: &NativeProgramInstance, name: &str) -> (BigUint, BigUint) {
    sim.backend()
        .get_four_state(sim.reflection().signal(id(sim, name)).unwrap().signal)
}
fn deposit(sim: &mut NativeProgramInstance, name: &str, value: u8) {
    let signal = sim.reflection().signal(id(sim, name)).unwrap().signal;
    sim.backend_mut().set(signal, value);
}
#[test]
fn explicit_force_preserves_blocking_loop_semantics_at_every_opt_level() {
    let source = r#"module Top(input logic [7:0] a, output logic [7:0] x, total);
      always_comb begin total=a; for (int i=0; i<4; i++) begin x=1; total+=x; end end
    endmodule"#;
    for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
        let mut sim = instance(
            source,
            level,
            SignalAccessOptions {
                force_signals: vec!["Top.x".into()],
                ..Default::default()
            },
        );
        let x = id(&sim, "Top.x");
        assert!(sim.force_signal(x, 3u8.into(), 0u8.into()));
        assert!(!sim.force_signal(id(&sim, "Top.a"), 8u8.into(), 0u8.into()));
        deposit(&mut sim, "Top.a", 1);
        sim.eval_comb().unwrap();
        assert_eq!(
            read(&sim, "Top.total"),
            (13u8.into(), 0u8.into()),
            "{level:?}"
        );
        assert_eq!(read(&sim, "Top.x").0, 3u8.into());
        sim.release_signal(x);
        sim.eval_comb().unwrap();
        assert_eq!(read(&sim, "Top.total").0, 5u8.into(), "release {level:?}");
        assert_eq!(read(&sim, "Top.x").0, 1u8.into());
    }
}
#[test]
fn explicit_live_root_keeps_internal_computation_at_o2() {
    let source = "module Top(input logic [7:0] a, output logic [7:0] y); logic [7:0] hidden; assign hidden=a+7; assign y=a+1; endmodule";
    let mut sim = instance(
        source,
        OptLevel::O2,
        SignalAccessOptions {
            live_signals: vec!["Top.hidden".into()],
            ..Default::default()
        },
    );
    deposit(&mut sim, "Top.a", 11);
    sim.eval_comb().unwrap();
    assert_eq!(read(&sim, "Top.hidden"), (18u8.into(), 0u8.into()));
}
#[test]
fn misspelled_access_paths_fail_even_without_dse() {
    for force in [false, true] {
        let mut access = SignalAccessOptions::default();
        if force {
            access.force_signals.push("Top.missing".into());
        } else {
            access.live_signals.push("Top.missing".into());
        }
        let result = Simulator::from_sv_sources(
            vec![(
                "module Top; logic a; endmodule",
                std::path::Path::new("missing.sv"),
            )],
            "Top",
        )
        .opt_level(OptLevel::O1)
        .signal_access(access)
        .build();
        assert!(result.err().unwrap().to_string().contains("Top.missing"));
    }
}

#[test]
fn force_contract_is_per_instance_and_keeps_internal_o2_targets_live() {
    let source = r#"
    module Child(input logic [7:0] a, output logic [7:0] y);
      logic [7:0] hidden;
      always_comb begin hidden=a+7; y=hidden+1; end
    endmodule
    module Top(input logic [7:0] a, output logic [7:0] y, z);
      Child left(a,y), right(a,z);
    endmodule"#;
    let mut sim = instance(
        source,
        OptLevel::O2,
        SignalAccessOptions {
            force_signals: vec!["Top.left.hidden".into()],
            ..Default::default()
        },
    );
    assert!(
        !sim.reflection()
            .signals()
            .iter()
            .any(|signal| signal.name.contains("@force"))
    );
    assert!(sim.force_signal(id(&sim, "Top.left.hidden"), 30u8.into(), 0u8.into()));
    assert!(!sim.force_signal(id(&sim, "Top.right.hidden"), 50u8.into(), 0u8.into()));
    deposit(&mut sim, "Top.a", 11);
    sim.eval_comb().unwrap();
    assert_eq!(read(&sim, "Top.y").0, 31u8.into());
    assert_eq!(read(&sim, "Top.z").0, 19u8.into());
    assert_eq!(read(&sim, "Top.left.hidden").0, 30u8.into());
}

#[test]
fn force_unknown_value_and_replacement_survive_optimized_partial_writes() {
    let source = "module Top(input logic [7:0] a, output logic [7:0] x, y); always_comb begin x[3:0]=a[3:0]; x[7:4]=a[7:4]; y=x; end endmodule";
    let mut sim = instance(
        source,
        OptLevel::O2,
        SignalAccessOptions {
            force_signals: vec!["Top.x".into()],
            ..Default::default()
        },
    );
    let x = id(&sim, "Top.x");
    assert!(sim.force_signal(x, 0xa5u8.into(), 0x18u8.into()));
    deposit(&mut sim, "Top.a", 11);
    sim.eval_comb().unwrap();
    assert_eq!(read(&sim, "Top.y"), (0xa5u8.into(), 0x18u8.into()));
    assert!(sim.force_signal(x, 7u8.into(), 0u8.into()));
    sim.eval_comb().unwrap();
    assert_eq!(read(&sim, "Top.y"), (7u8.into(), 0u8.into()));
}

#[test]
fn force_on_owned_interface_member_propagates_through_port_glue() {
    let source = r#"
    interface Bus; logic [7:0] data; modport sink(input data); endinterface
    module Child(Bus.sink s_axis, output logic [7:0] y); assign y=s_axis.data+1; endmodule
    module Top(output logic [7:0] y); Bus s_axis(); assign s_axis.data=11; Child uut(s_axis,y); endmodule
    "#;
    let mut sim = instance(
        source,
        OptLevel::O2,
        SignalAccessOptions {
            force_signals: vec!["Top.s_axis.data".into()],
            ..Default::default()
        },
    );
    assert!(sim.force_signal(id(&sim, "Top.s_axis.data"), 30u8.into(), 0u8.into()));
    sim.eval_comb().unwrap();
    assert_eq!(read(&sim, "Top.y").0, 31u8.into());
}

#[test]
fn force_profile_does_not_disable_o2_dead_store_elimination() {
    let source = "module Top(input logic [7:0] a, output logic [7:0] y); logic [7:0] preserved, discarded; assign preserved=a+7; assign discarded=a+8; assign y=a+1; endmodule";
    let mut sim = instance(
        source,
        OptLevel::O2,
        SignalAccessOptions {
            force_signals: vec!["Top.preserved".into()],
            ..Default::default()
        },
    );
    deposit(&mut sim, "Top.a", 11);
    sim.eval_comb().unwrap();
    assert_eq!(read(&sim, "Top.preserved"), (18u8.into(), 0u8.into()));
    assert_eq!(read(&sim, "Top.discarded").1, 255u8.into());
}

#[test]
fn optimized_sequential_blocking_assignments_observe_force_and_release() {
    let source = "module Top(input logic clk, output logic [7:0] q, y); always @(posedge clk) begin q=1; y=q+1; end endmodule";
    let mut sim = instance(
        source,
        OptLevel::O2,
        SignalAccessOptions {
            force_signals: vec!["Top.q".into()],
            ..Default::default()
        },
    );
    let q = id(&sim, "Top.q");
    let clock = sim
        .reflection()
        .signal(id(&sim, "Top.clk"))
        .unwrap()
        .state_address;
    assert!(sim.force_signal(q, 3u8.into(), 0u8.into()));
    deposit(&mut sim, "Top.clk", 1);
    sim.settle_active_edges(&[clock]).unwrap();
    assert_eq!(read(&sim, "Top.q").0, 3u8.into());
    assert_eq!(read(&sim, "Top.y").0, 4u8.into());
    sim.release_signal(q);
    sim.eval_comb().unwrap();
    assert_eq!(read(&sim, "Top.q").0, 3u8.into());
    deposit(&mut sim, "Top.clk", 0);
    sim.eval_comb().unwrap();
    deposit(&mut sim, "Top.clk", 1);
    sim.settle_active_edges(&[clock]).unwrap();
    assert_eq!(read(&sim, "Top.q").0, 1u8.into());
    assert_eq!(read(&sim, "Top.y").0, 2u8.into());
}

#[test]
fn wide_force_controls_keep_value_and_unknown_planes() {
    let source = "module Top(output logic [128:0] x, y); assign x=129'd1; assign y=x; endmodule";
    let mut sim = instance(
        source,
        OptLevel::O2,
        SignalAccessOptions {
            force_signals: vec!["Top.x".into()],
            ..Default::default()
        },
    );
    let value = (BigUint::from(1u8) << 128usize) + BigUint::from(5u8);
    assert!(sim.force_signal(id(&sim, "Top.x"), value.clone(), 2u8.into()));
    sim.eval_comb().unwrap();
    assert_eq!(read(&sim, "Top.y"), (value, 2u8.into()));
}

#[test]
fn unsupported_force_targets_fail_explicitly() {
    for (source, target, expected) in [
        (
            "module Top; logic [7:0] mem[4]; endmodule",
            "Top.mem",
            "packed non-clock",
        ),
        (
            "module Top(input logic clk, output logic q); always @(posedge clk) q<=1; endmodule",
            "Top.clk",
            "packed non-clock",
        ),
        (
            "interface Bus; logic data; modport sink(input data); endinterface module Child(Bus.sink bus, output logic y); assign y=bus.data; endmodule module Top(output logic y); Bus bus(); Child uut(bus,y); endmodule",
            "Top.uut.bus.data",
            "owning interface variable",
        ),
    ] {
        let error = Simulator::from_sv_sources(
            vec![(source, std::path::Path::new("unsupported.sv"))],
            "Top",
        )
        .signal_access(SignalAccessOptions {
            force_signals: vec![target.into()],
            ..Default::default()
        })
        .build()
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn explicit_force_runs_with_parallel_optimized_kernels() {
    let source = "module Top(input logic [7:0] a, b, output logic [7:0] x, y, z); always_comb begin x=a+1; y=x+1; end assign z=b+2; endmodule";
    let simulator =
        Simulator::from_sv_sources(vec![(source, std::path::Path::new("parallel.sv"))], "Top")
            .four_state(true)
            .opt_level(OptLevel::O2)
            .threads(4)
            .parallel_partition(celox::ParallelPartition::Always)
            .signal_access(SignalAccessOptions {
                force_signals: vec!["Top.x".into()],
                ..Default::default()
            })
            .build()
            .unwrap();
    // Safety: this test just compiled the image being executed.
    let mut sim = unsafe {
        NativeProgramInstance::from_image(simulator.shared_code().program_image().clone())
    }
    .unwrap();
    assert!(sim.force_signal(id(&sim, "Top.x"), 99u8.into(), 0u8.into()));
    deposit(&mut sim, "Top.a", 11);
    deposit(&mut sim, "Top.b", 20);
    sim.eval_comb().unwrap();
    assert_eq!(read(&sim, "Top.y").0, 100u8.into());
    assert_eq!(read(&sim, "Top.z").0, 22u8.into());
}

#[test]
fn forced_signed_values_keep_sign_extension_division_and_comparison() {
    let source = "module Top(output logic signed [7:0] x, quotient, output logic signed [15:0] wide, output logic negative); always_comb begin x=1; wide=x; quotient=x/8'sd2; negative=x<8'sd0; end endmodule";
    let mut sim = instance(
        source,
        OptLevel::O2,
        SignalAccessOptions {
            force_signals: vec!["Top.x".into()],
            ..Default::default()
        },
    );
    assert!(sim.force_signal(id(&sim, "Top.x"), 250u8.into(), 0u8.into()));
    sim.eval_comb().unwrap();
    assert_eq!(read(&sim, "Top.wide").0, 65530u32.into());
    assert_eq!(read(&sim, "Top.quotient").0, 253u8.into());
    assert_eq!(read(&sim, "Top.negative").0, 1u8.into());
}

#[test]
fn veryl_observation_roots_work_and_unsupported_force_is_rejected() {
    let source = "module Top(a: input logic<8>, y: output logic<8>) { var hidden: logic<8>; assign hidden=a+7; assign y=a+1; }";
    let simulator = Simulator::builder(source, "Top")
        .four_state(true)
        .opt_level(OptLevel::O2)
        .signal_access(SignalAccessOptions {
            live_signals: vec!["Top.hidden".into()],
            ..Default::default()
        })
        .build()
        .unwrap();
    // Safety: this test just compiled the image being executed.
    let mut sim = unsafe {
        NativeProgramInstance::from_image(simulator.shared_code().program_image().clone())
    }
    .unwrap();
    deposit(&mut sim, "Top.a", 11);
    sim.eval_comb().unwrap();
    assert_eq!(read(&sim, "Top.hidden"), (18u8.into(), 0u8.into()));
    let error = Simulator::builder(source, "Top")
        .signal_access(SignalAccessOptions {
            force_signals: vec!["Top.hidden".into()],
            ..Default::default()
        })
        .build()
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("pure SystemVerilog"), "{error}");
}

#[test]
fn force_permissions_preserve_declaration_initial_values() {
    let source = "module Top(output logic [7:0] y); logic [7:0] x=4; assign y=x+1; endmodule";
    let mut sim = instance(
        source,
        OptLevel::O2,
        SignalAccessOptions {
            force_signals: vec!["Top.x".into()],
            ..Default::default()
        },
    );
    assert_eq!(read(&sim, "Top.x"), (4u8.into(), 0u8.into()));
    assert_eq!(read(&sim, "Top.y"), (5u8.into(), 0u8.into()));
    assert!(sim.force_signal(id(&sim, "Top.x"), 8u8.into(), 0u8.into()));
    sim.eval_comb().unwrap();
    assert_eq!(read(&sim, "Top.y").0, 9u8.into());
}
