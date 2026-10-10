//! Processes authored through the frontend SDK: compiled resumable kernels
//! driven by the timed simulation scheduler.

use celox::Simulation;
use celox::frontend_sdk::{
    BinaryOp, BuildError, Constant, Edge, FrontendArtifact, ModuleBuilder, Statement, UnaryOp,
    ValueType,
};

fn constant(module: &mut ModuleBuilder, value: u64, width: usize) -> celox::frontend_sdk::ExprId {
    module.constant(Constant::two_state(value, width).unwrap())
}

/// `forever #5 clk = ~clk;` drives a counter that increments on every
/// rising edge.
fn clocked_counter() -> FrontendArtifact {
    let bit = ValueType::bits(1).unwrap();
    let byte = ValueType::bits(8).unwrap();
    let mut module = ModuleBuilder::new("ClockedCounter").unwrap();
    let clk = module.internal("clk", bit).unwrap();
    let count = module.output("count", byte).unwrap();
    module
        .set_initial(clk, Constant::two_state(0u8, 1).unwrap())
        .unwrap();
    module
        .set_initial(count, Constant::two_state(0u8, 8).unwrap())
        .unwrap();
    let count_expr = module.read(count).unwrap();
    let one = constant(&mut module, 1, 8);
    let next = module.binary(BinaryOp::Add, count_expr, one, byte).unwrap();
    let count_target = module.whole(count).unwrap();
    module
        .register(count_target, next, clk, Edge::Posedge, None, None)
        .unwrap();

    let half_period = constant(&mut module, 5, 8);
    let clk_expr = module.read(clk).unwrap();
    let toggled = module.unary(UnaryOp::BitNot, clk_expr, bit).unwrap();
    let clk_target = module.whole(clk).unwrap();
    module
        .process(vec![Statement::Forever {
            body: vec![
                Statement::Delay {
                    amount: half_period,
                },
                Statement::Assign {
                    target: clk_target,
                    value: toggled,
                },
            ],
        }])
        .unwrap();
    module.finish()
}

#[test]
fn process_generated_clock_drives_registers() {
    let mut sim = Simulation::from_frontend(clocked_counter())
        .build()
        .unwrap();
    let count = sim.signal("count");
    let clk = sim.signal("clk");

    sim.run_until(4).unwrap();
    assert_eq!(sim.get(clk), 0u8.into());
    assert_eq!(sim.get(count), 0u8.into());

    // Rising edges at 5, 15, ..., 95.
    sim.run_until(100).unwrap();
    assert_eq!(sim.get(count), 10u8.into());
    assert_eq!(sim.time(), 100);
}

#[test]
fn process_suspends_and_finishes() {
    let byte = ValueType::bits(8).unwrap();
    let mut module = ModuleBuilder::new("Sequence").unwrap();
    let a = module.output("a", byte).unwrap();
    let target = module.whole(a).unwrap();
    let one = constant(&mut module, 1, 8);
    let two = constant(&mut module, 2, 8);
    let ten = constant(&mut module, 10, 8);
    module
        .process(vec![
            Statement::Assign { target, value: one },
            Statement::Delay { amount: ten },
            Statement::Assign { target, value: two },
            Statement::Delay { amount: ten },
            Statement::Finish,
            // Never reached.
            Statement::Assign { target, value: one },
        ])
        .unwrap();
    let mut sim = Simulation::from_frontend(module.finish()).build().unwrap();
    let a = sim.signal("a");

    assert_eq!(sim.step().unwrap(), Some(0));
    assert_eq!(sim.get(a), 1u8.into());
    assert_eq!(sim.next_event_time(), Some(10));
    assert_eq!(sim.step().unwrap(), Some(10));
    assert_eq!(sim.get(a), 2u8.into());
    assert!(!sim.is_finished());
    assert_eq!(sim.step().unwrap(), Some(20));
    assert!(sim.is_finished());
    assert_eq!(sim.get(a), 2u8.into());
    assert_eq!(sim.next_event_time(), None);
    assert_eq!(sim.step().unwrap(), None);

    // A finished simulation does not advance.
    sim.run_until(100).unwrap();
    assert_eq!(sim.time(), 20);
}

#[test]
fn process_loops_until_its_condition_fails() {
    let byte = ValueType::bits(8).unwrap();
    let bit = ValueType::bits(1).unwrap();
    let mut module = ModuleBuilder::new("Loop").unwrap();
    let i = module.output("i", byte).unwrap();
    let target = module.whole(i).unwrap();
    let zero = constant(&mut module, 0, 8);
    let one = constant(&mut module, 1, 8);
    let two = constant(&mut module, 2, 8);
    let three = constant(&mut module, 3, 8);
    let current = module.read(i).unwrap();
    let below = module
        .binary(BinaryOp::LessUnsigned, current, three, bit)
        .unwrap();
    let incremented = module.binary(BinaryOp::Add, current, one, byte).unwrap();
    module
        .process(vec![
            Statement::Assign {
                target,
                value: zero,
            },
            Statement::While {
                condition: below,
                body: vec![
                    Statement::Delay { amount: two },
                    Statement::Assign {
                        target,
                        value: incremented,
                    },
                ],
            },
            Statement::Finish,
        ])
        .unwrap();
    let mut sim = Simulation::from_frontend(module.finish()).build().unwrap();
    let i = sim.signal("i");

    sim.run_until(100).unwrap();
    assert!(sim.is_finished());
    assert_eq!(sim.time(), 6);
    assert_eq!(sim.get(i), 3u8.into());
}

#[test]
fn process_branches_on_state() {
    let byte = ValueType::bits(8).unwrap();
    let bit = ValueType::bits(1).unwrap();
    let mut module = ModuleBuilder::new("Branch").unwrap();
    let flag = module.internal("flag", bit).unwrap();
    let out = module.output("out", byte).unwrap();
    let flag_target = module.whole(flag).unwrap();
    let out_target = module.whole(out).unwrap();
    let set = constant(&mut module, 1, 1);
    let flag_expr = module.read(flag).unwrap();
    let seven = constant(&mut module, 7, 8);
    let nine = constant(&mut module, 9, 8);
    let one = constant(&mut module, 1, 8);
    let branch = Statement::If {
        condition: flag_expr,
        then_body: vec![Statement::Assign {
            target: out_target,
            value: seven,
        }],
        else_body: vec![Statement::Assign {
            target: out_target,
            value: nine,
        }],
    };
    module
        .process(vec![
            branch.clone(),
            Statement::Delay { amount: one },
            Statement::Assign {
                target: flag_target,
                value: set,
            },
            branch,
        ])
        .unwrap();
    let mut sim = Simulation::from_frontend(module.finish()).build().unwrap();
    let out = sim.signal("out");

    sim.step().unwrap();
    assert_eq!(sim.get(out), 9u8.into());
    sim.step().unwrap();
    assert_eq!(sim.get(out), 7u8.into());
    // The process ended without finishing the simulation.
    assert!(!sim.is_finished());
    assert_eq!(sim.next_event_time(), None);
}

/// Processes resuming at one time run in declaration order, and a zero
/// delay runs after the other processes of that time.
#[test]
fn zero_delay_runs_after_other_processes_of_the_same_time() {
    let byte = ValueType::bits(8).unwrap();
    let mut module = ModuleBuilder::new("Ordering").unwrap();
    let source = module.internal("source", byte).unwrap();
    let eager = module.output("eager", byte).unwrap();
    let deferred = module.output("deferred", byte).unwrap();
    let source_target = module.whole(source).unwrap();
    let eager_target = module.whole(eager).unwrap();
    let deferred_target = module.whole(deferred).unwrap();
    let source_expr = module.read(source).unwrap();
    let zero = constant(&mut module, 0, 8);
    let seven = constant(&mut module, 7, 8);
    module
        .process(vec![
            Statement::Assign {
                target: eager_target,
                value: source_expr,
            },
            Statement::Delay { amount: zero },
            Statement::Assign {
                target: deferred_target,
                value: source_expr,
            },
        ])
        .unwrap();
    module
        .process(vec![Statement::Assign {
            target: source_target,
            value: seven,
        }])
        .unwrap();
    let mut sim = Simulation::from_frontend(module.finish()).build().unwrap();
    let eager = sim.signal("eager");
    let deferred = sim.signal("deferred");

    assert_eq!(sim.step().unwrap(), Some(0));
    assert_eq!(sim.get(eager), 0u8.into());
    assert_eq!(sim.get(deferred), 7u8.into());
}

#[test]
fn checkpoint_restores_a_suspended_process() {
    let mut sim = Simulation::from_frontend(clocked_counter())
        .build()
        .unwrap();
    let count = sim.signal("count");
    sim.run_until(50).unwrap();
    let checkpoint = sim.checkpoint().unwrap();
    sim.run_until(100).unwrap();
    assert_eq!(sim.get(count), 10u8.into());

    sim.switch_vcd(std::env::temp_dir().join("celox-process-checkpoint.vcd"))
        .ok();
    sim.restore(&checkpoint).unwrap();
    assert_eq!(sim.time(), 50);
    assert_eq!(sim.get(count), 5u8.into());
    sim.run_until(100).unwrap();
    assert_eq!(sim.get(count), 10u8.into());
}

#[test]
fn state_files_reject_processes() {
    let mut sim = Simulation::from_frontend(clocked_counter())
        .build()
        .unwrap();
    assert!(matches!(
        sim.save_state(),
        Err(celox::StateError::Processes)
    ));
}

#[test]
fn processes_survive_json_interchange() {
    let artifact = clocked_counter();
    assert_eq!(artifact.format_version(), 2);
    let json = artifact.to_json().unwrap();
    let artifact = FrontendArtifact::from_json(&json).unwrap();
    assert_eq!(artifact.processes().len(), 1);

    // Version 1 cannot carry processes.
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    value["format_version"] = 1.into();
    assert!(FrontendArtifact::from_json(&value.to_string()).is_err());

    let mut sim = Simulation::from_frontend(artifact).build().unwrap();
    let count = sim.signal("count");
    sim.run_until(20).unwrap();
    assert_eq!(sim.get(count), 2u8.into());
}

#[test]
fn processes_cannot_drive_inputs_or_continuously_driven_signals() {
    let byte = ValueType::bits(8).unwrap();
    let mut module = ModuleBuilder::new("Drivers").unwrap();
    let input = module.input("input", byte).unwrap();
    let wire = module.output("wire", byte).unwrap();
    let input_expr = module.read(input).unwrap();
    let wire_target = module.whole(wire).unwrap();
    module.assign(wire_target, input_expr).unwrap();
    let zero = constant(&mut module, 0, 8);

    let input_target = module.whole(input).unwrap();
    assert!(matches!(
        module.process(vec![Statement::Assign {
            target: input_target,
            value: zero,
        }]),
        Err(BuildError::InvalidDriverTarget { .. })
    ));
    assert!(matches!(
        module.process(vec![Statement::Assign {
            target: wire_target,
            value: zero,
        }]),
        Err(BuildError::ProcessDriverConflict { .. })
    ));

    // A continuous driver added after the process conflicts as well.
    let other = module.output("other", byte).unwrap();
    let other_target = module.whole(other).unwrap();
    module
        .process(vec![Statement::Assign {
            target: other_target,
            value: zero,
        }])
        .unwrap();
    assert!(matches!(
        module.assign(other_target, input_expr),
        Err(BuildError::ProcessDriverConflict { .. })
    ));
}

#[test]
fn delay_amounts_are_at_most_64_bits() {
    let mut module = ModuleBuilder::new("WideDelay").unwrap();
    let wide = module.constant(Constant::two_state(1u8, 65).unwrap());
    assert!(matches!(
        module.process(vec![Statement::Delay { amount: wide }]),
        Err(BuildError::DelayTooWide { width: 65 })
    ));
}

#[test]
fn process_writes_reach_the_waveform() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("processes.vcd");
    let mut sim = Simulation::from_frontend(clocked_counter())
        .vcd(&path)
        .build()
        .unwrap();
    sim.run_until(30).unwrap();
    sim.flush_vcd().unwrap();
    let vcd = std::fs::read_to_string(&path).unwrap();
    let clk_id = vcd
        .lines()
        .find_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            (fields.first() == Some(&"$var") && fields.get(4) == Some(&"clk"))
                .then(|| fields[3].to_string())
        })
        .expect("clk is declared in the waveform");
    let mut time = 0u64;
    let mut changes = Vec::new();
    for line in vcd.lines() {
        if let Some(stamp) = line.strip_prefix('#') {
            time = stamp.parse().unwrap();
        } else if line.len() == 1 + clk_id.len() && line.ends_with(&clk_id) {
            changes.push((time, line[..1].to_string()));
        }
    }
    assert_eq!(
        changes
            .iter()
            .filter(|(time, _)| *time > 0)
            .cloned()
            .collect::<Vec<_>>(),
        [
            (5, "1".to_string()),
            (10, "0".to_string()),
            (15, "1".to_string()),
            (20, "0".to_string()),
            (25, "1".to_string()),
            (30, "0".to_string()),
        ]
    );
}

#[test]
fn veryl_testbenches_reject_artifacts_with_processes() {
    let source = r#"
        #[test(t)]
        module ProcessTb {
            var count: logic<8>;
            inst dut: $sv::ClockedCounter (count);
            initial {
                $finish();
            }
        }
    "#;
    let error = celox::Simulator::from_frontend_with_testbench(
        clocked_counter(),
        vec![(source, std::path::Path::new("process_tb.veryl"))],
        "ProcessTb",
    )
    .run_test_cranelift()
    .unwrap_err();
    assert!(error.to_string().contains("processes"), "{error}");
}

/// Dead-store elimination keeps combinational values only a process reads.
#[test]
fn dead_store_elimination_keeps_values_read_by_processes() {
    let byte = ValueType::bits(8).unwrap();
    let mut module = ModuleBuilder::new("Observer").unwrap();
    let source = module.internal("source", byte).unwrap();
    let doubled = module.internal("doubled", byte).unwrap();
    let observed = module.output("observed", byte).unwrap();
    let source_expr = module.read(source).unwrap();
    let double = module
        .binary(BinaryOp::Add, source_expr, source_expr, byte)
        .unwrap();
    let doubled_target = module.whole(doubled).unwrap();
    module.assign(doubled_target, double).unwrap();
    let source_target = module.whole(source).unwrap();
    let observed_target = module.whole(observed).unwrap();
    let doubled_expr = module.read(doubled).unwrap();
    let twenty_one = constant(&mut module, 21, 8);
    let one = constant(&mut module, 1, 8);
    module
        .process(vec![
            Statement::Assign {
                target: source_target,
                value: twenty_one,
            },
            Statement::Delay { amount: one },
            Statement::Assign {
                target: observed_target,
                value: doubled_expr,
            },
        ])
        .unwrap();
    let mut sim = Simulation::from_frontend(module.finish())
        .opt_level(celox::OptLevel::O2)
        .build()
        .unwrap();
    let observed = sim.signal("observed");
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(observed), 42u8.into());
}

/// Statements after a `Finish` or `Forever` never run, including delays.
#[test]
fn unreachable_statements_are_not_lowered() {
    let byte = ValueType::bits(8).unwrap();
    let mut module = ModuleBuilder::new("Unreachable").unwrap();
    let a = module.output("a", byte).unwrap();
    let target = module.whole(a).unwrap();
    let one = constant(&mut module, 1, 8);
    let two = constant(&mut module, 2, 8);
    let a_expr = module.read(a).unwrap();
    module
        .process(vec![
            Statement::Forever {
                body: vec![
                    Statement::Delay { amount: one },
                    Statement::If {
                        condition: a_expr,
                        then_body: vec![Statement::Finish, Statement::Delay { amount: one }],
                        else_body: vec![Statement::Assign { target, value: one }],
                    },
                ],
            },
            Statement::Delay { amount: two },
            Statement::Assign { target, value: two },
        ])
        .unwrap();
    let mut sim = Simulation::from_frontend(module.finish()).build().unwrap();
    let a = sim.signal("a");
    sim.run_until(10).unwrap();
    assert!(sim.is_finished());
    assert_eq!(sim.time(), 2);
    assert_eq!(sim.get(a), 1u8.into());
}

/// A condition is true when some bit is a known one, even if other bits are
/// unknown, and false when its truth is unknown.
#[test]
fn multi_bit_and_four_state_conditions() {
    let byte = ValueType::bits(8).unwrap();
    let mut module = ModuleBuilder::new("Conditions").unwrap();
    let sel = module
        .internal("sel", ValueType::logic(8).unwrap())
        .unwrap();
    let n = module.internal("n", byte).unwrap();
    let out = module.output("out", byte).unwrap();
    let total = module.output("total", byte).unwrap();
    module
        .set_initial(sel, Constant::four_state(0b0100u8, 0b0010u8, 8).unwrap())
        .unwrap();
    module
        .set_initial(n, Constant::two_state(3u8, 8).unwrap())
        .unwrap();
    module
        .set_initial(total, Constant::two_state(0u8, 8).unwrap())
        .unwrap();
    let sel_target = module.whole(sel).unwrap();
    let n_target = module.whole(n).unwrap();
    let out_target = module.whole(out).unwrap();
    let total_target = module.whole(total).unwrap();
    let sel_expr = module.read(sel).unwrap();
    let n_expr = module.read(n).unwrap();
    let total_expr = module.read(total).unwrap();
    let unknown = module.constant(Constant::four_state(0u8, 1u8, 8).unwrap());
    let one = constant(&mut module, 1, 8);
    let two = constant(&mut module, 2, 8);
    let next_n = module.binary(BinaryOp::Sub, n_expr, one, byte).unwrap();
    let next_total = module.binary(BinaryOp::Add, total_expr, one, byte).unwrap();
    let branch = Statement::If {
        condition: sel_expr,
        then_body: vec![Statement::Assign {
            target: out_target,
            value: one,
        }],
        else_body: vec![Statement::Assign {
            target: out_target,
            value: two,
        }],
    };
    module
        .process(vec![
            branch.clone(),
            Statement::While {
                condition: n_expr,
                body: vec![
                    Statement::Assign {
                        target: n_target,
                        value: next_n,
                    },
                    Statement::Assign {
                        target: total_target,
                        value: next_total,
                    },
                ],
            },
            Statement::Delay { amount: one },
            Statement::Assign {
                target: sel_target,
                value: unknown,
            },
            branch,
        ])
        .unwrap();
    let mut sim = Simulation::from_frontend(module.finish()).build().unwrap();
    let out = sim.signal("out");
    let total = sim.signal("total");

    assert_eq!(sim.step().unwrap(), Some(0));
    assert_eq!(sim.get(out), 1u8.into());
    assert_eq!(sim.get(total), 3u8.into());
    assert_eq!(sim.step().unwrap(), Some(1));
    assert_eq!(sim.get(out), 2u8.into());
}

/// A pulse separated by a zero delay is an edge: registers see the rising
/// edge before the process resumes and lowers the clock again.
#[test]
fn zero_delay_pulse_triggers_registers() {
    let bit = ValueType::bits(1).unwrap();
    let byte = ValueType::bits(8).unwrap();
    let mut module = ModuleBuilder::new("Pulse").unwrap();
    let clk = module.internal("clk", bit).unwrap();
    let count = module.output("count", byte).unwrap();
    module
        .set_initial(clk, Constant::two_state(0u8, 1).unwrap())
        .unwrap();
    module
        .set_initial(count, Constant::two_state(0u8, 8).unwrap())
        .unwrap();
    let count_expr = module.read(count).unwrap();
    let one = constant(&mut module, 1, 8);
    let next = module.binary(BinaryOp::Add, count_expr, one, byte).unwrap();
    let count_target = module.whole(count).unwrap();
    module
        .register(count_target, next, clk, Edge::Posedge, None, None)
        .unwrap();
    let clk_target = module.whole(clk).unwrap();
    let high = constant(&mut module, 1, 1);
    let low = constant(&mut module, 0, 1);
    let zero = constant(&mut module, 0, 64);
    module
        .process(vec![
            Statement::Assign {
                target: clk_target,
                value: high,
            },
            Statement::Delay { amount: zero },
            Statement::Assign {
                target: clk_target,
                value: low,
            },
            Statement::Delay { amount: zero },
            Statement::Assign {
                target: clk_target,
                value: high,
            },
        ])
        .unwrap();
    let mut sim = Simulation::from_frontend(module.finish()).build().unwrap();
    let count = sim.signal("count");
    let clk = sim.signal("clk");

    assert_eq!(sim.step().unwrap(), Some(0));
    assert_eq!(sim.get(count), 2u8.into());
    assert_eq!(sim.get(clk), 1u8.into());
    assert_eq!(sim.next_event_time(), None);
}
