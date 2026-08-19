use std::path::Path;

use celox::Simulator;
use celox::frontend_sdk::{
    ActiveLevel, BinaryOp, Constant, Direction, Edge, ModuleBuilder, ValueType,
};

fn adder_artifact() -> celox::FrontendArtifact {
    let byte = ValueType::bits(8).unwrap();
    let mut module = ModuleBuilder::new("NetAdder").unwrap();
    let a = module.input("a", byte).unwrap();
    let b = module.input("b", byte).unwrap();
    let y = module.output("y", byte).unwrap();
    let a_expr = module.read(a).unwrap();
    let b_expr = module.read(b).unwrap();
    let sum = module.binary(BinaryOp::Add, a_expr, b_expr, byte).unwrap();
    let y = module.whole(y).unwrap();
    module.assign(y, sum).unwrap();
    module.finish()
}

#[test]
fn frontend_artifact_preserves_signal_reflection_for_host_testbenches() {
    let json = adder_artifact().to_json().unwrap();
    let artifact = celox::FrontendArtifact::from_json(&json).unwrap();
    let mut sim = Simulator::from_frontend(artifact)
        .build_cranelift()
        .unwrap();

    let hierarchy = sim.named_hierarchy();
    assert_eq!(hierarchy.module_name, "NetAdder");
    assert_eq!(hierarchy.signals.len(), 3);
    assert!(hierarchy.signals.iter().any(|signal| {
        signal.name == "a"
            && signal.info.var_kind == celox::VariableKind::Input
            && signal.info.width == 8
    }));

    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(a, 10u8);
        io.set(b, 23u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 33u8.into());
}

#[test]
fn frontend_artifact_runs_edge_triggered_storage() {
    let bit = ValueType::bits(1).unwrap();
    let byte = ValueType::bits(8).unwrap();
    let mut module = ModuleBuilder::new("NetRegister").unwrap();
    let clock = module.input("clock", bit).unwrap();
    let reset_n = module.input("reset_n", bit).unwrap();
    let d = module.input("d", byte).unwrap();
    let q = module.output("q", byte).unwrap();
    module
        .set_initial(q, Constant::two_state(0u8, 8).unwrap())
        .unwrap();
    let d_expr = module.read(d).unwrap();
    let zero = module.constant(Constant::two_state(0u8, 8).unwrap());
    let reset = module.async_reset(reset_n, ActiveLevel::Low, zero).unwrap();
    let q_target = module.whole(q).unwrap();
    module
        .register(q_target, d_expr, clock, Edge::Posedge, Some(reset), None)
        .unwrap();

    let mut sim = Simulator::from_frontend(module.finish())
        .build_cranelift()
        .unwrap();
    let clock = sim.event("clock");
    let reset_n = sim.signal("reset_n");
    let d = sim.signal("d");
    let q = sim.signal("q");
    sim.modify(|io| {
        io.set(reset_n, 0u8);
        io.set(d, 42u8);
    })
    .unwrap();
    sim.tick(clock).unwrap();
    assert_eq!(sim.get(q), 0u8.into());
    sim.modify(|io| io.set(reset_n, 1u8)).unwrap();
    sim.tick(clock).unwrap();
    assert_eq!(sim.get(q), 42u8.into());
}

#[test]
fn veryl_native_testbench_can_instantiate_frontend_artifact() {
    let source = r#"
        #[test(t)]
        module NetlistTb {
            var a: logic<8>;
            var b: logic<8>;
            var y: logic<8>;
            inst dut: $sv::NetAdder (a, b, y);

            initial {
                a = 8'd10;
                b = 8'd23;
                $assert(y == 8'd33, "external frontend artifact");
                $finish();
            }
        }
    "#;
    let result = Simulator::from_frontend_with_testbench(
        adder_artifact(),
        vec![(source, Path::new("netlist_tb.veryl"))],
        "NetlistTb",
    )
    .run_test_cranelift()
    .unwrap();
    assert_eq!(result, celox::TestResult::Pass);
}

#[test]
fn frontend_builder_accepts_internal_signals_without_exposing_them_as_ports() {
    let mut module = ModuleBuilder::new("InternalSignal").unwrap();
    let signal = module
        .signal("tmp", Direction::Internal, ValueType::bits(1).unwrap())
        .unwrap();
    assert_eq!(signal.index(), 0);
    assert!(module.finish().port_order().is_empty());
}

#[test]
fn frontend_expression_result_width_is_preserved() {
    let byte = ValueType::bits(8).unwrap();
    let word = ValueType::bits(16).unwrap();
    let signed_byte = ValueType::new(8, true, false).unwrap();
    let signed_word = ValueType::new(16, true, false).unwrap();
    let mut module = ModuleBuilder::new("WideMultiply").unwrap();
    let a = module.input("a", byte).unwrap();
    let b = module.input("b", byte).unwrap();
    let signed_a = module.input("signed_a", signed_byte).unwrap();
    let signed_b = module.input("signed_b", signed_byte).unwrap();
    let y = module.output("y", word).unwrap();
    let signed_y = module.output("signed_y", signed_word).unwrap();

    let a_expr = module.read(a).unwrap();
    let b_expr = module.read(b).unwrap();
    let product = module.binary(BinaryOp::Mul, a_expr, b_expr, word).unwrap();
    let y = module.whole(y).unwrap();
    module.assign(y, product).unwrap();

    let signed_a_expr = module.read(signed_a).unwrap();
    let signed_b_expr = module.read(signed_b).unwrap();
    let signed_product = module
        .binary(BinaryOp::Mul, signed_a_expr, signed_b_expr, signed_word)
        .unwrap();
    let signed_y = module.whole(signed_y).unwrap();
    module.assign(signed_y, signed_product).unwrap();

    let mut sim = Simulator::from_frontend(module.finish())
        .build_cranelift()
        .unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let signed_a = sim.signal("signed_a");
    let signed_b = sim.signal("signed_b");
    let y = sim.signal("y");
    let signed_y = sim.signal("signed_y");
    sim.modify(|io| {
        io.set(a, 0xffu8);
        io.set(b, 0xffu8);
        io.set(signed_a, 0xffu8);
        io.set(signed_b, 2u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 65_025u16.into());
    assert_eq!(sim.get(signed_y), 0xfffeu16.into());
}

#[test]
fn frontend_shared_expression_dag_is_lowered_once_per_assignment() {
    let byte = ValueType::bits(8).unwrap();
    let mut module = ModuleBuilder::new("SharedDag").unwrap();
    let input = module.input("input", byte).unwrap();
    let output = module.output("output", byte).unwrap();
    let mut expression = module.read(input).unwrap();
    for _ in 0..64 {
        expression = module
            .binary(BinaryOp::Xor, expression, expression, byte)
            .unwrap();
    }
    let output = module.whole(output).unwrap();
    module.assign(output, expression).unwrap();

    Simulator::from_frontend(module.finish())
        .build_cranelift()
        .unwrap();
}

#[test]
fn frontend_rejects_one_async_reset_shared_across_clock_domains() {
    let bit = ValueType::bits(1).unwrap();
    let mut module = ModuleBuilder::new("SharedReset").unwrap();
    let clock_a = module.input("clock_a", bit).unwrap();
    let clock_b = module.input("clock_b", bit).unwrap();
    let reset = module.input("reset", bit).unwrap();
    let d_a = module.input("d_a", bit).unwrap();
    let d_b = module.input("d_b", bit).unwrap();
    let q_a = module.output("q_a", bit).unwrap();
    let q_b = module.output("q_b", bit).unwrap();
    let reset_value = module.constant(Constant::two_state(0u8, 1).unwrap());
    let reset = module
        .async_reset(reset, ActiveLevel::High, reset_value)
        .unwrap();
    let d_a = module.read(d_a).unwrap();
    let d_b = module.read(d_b).unwrap();
    let q_a = module.whole(q_a).unwrap();
    let q_b = module.whole(q_b).unwrap();
    module
        .register(q_a, d_a, clock_a, Edge::Posedge, Some(reset), None)
        .unwrap();
    module
        .register(q_b, d_b, clock_b, Edge::Posedge, Some(reset), None)
        .unwrap();

    let error = match Simulator::from_frontend(module.finish()).build_cranelift() {
        Ok(_) => panic!("shared async reset across clock domains was accepted"),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(message.contains("reset"));
    assert!(message.contains("clock_a"));
    assert!(message.contains("clock_b"));
}
