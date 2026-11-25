//! Which units the plan takes for opaque dependencies, on hand-built SIR.
use super::*;
use celox_sir::extern_abi::{COVERAGE_HELPER_ID, HEAP_HELPER_ID};
use celox_sir::native_memory::NativeMemoryOp;
use celox_sir::{BasicBlock, BlockId, FrameLayout, FunctionId, FunctionUnit, RegisterType};

/// A unit of `instructions` over 64-bit registers 0 and 1.
fn unit(instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>) -> Unit {
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: (0..2)
            .map(|register| {
                (
                    RegisterId(register),
                    RegisterType::Bit {
                        width: 64,
                        signed: false,
                    },
                )
            })
            .collect(),
    }
}

/// An extern call of `func` with `args` zero words.
fn extern_call(func: u32, args: usize) -> Vec<SIRInstruction<RegionedAbsoluteAddr>> {
    vec![
        SIRInstruction::Imm(RegisterId(0), SIRValue::new(0u8)),
        SIRInstruction::ExternCall {
            dst: None,
            func,
            args: vec![RegisterId(0); args],
        },
    ]
}

/// A heap helper call through `func`: an opcode word and three operands.
fn helper_call(func: u32) -> Vec<SIRInstruction<RegionedAbsoluteAddr>> {
    extern_call(func, celox_sir::extern_abi::HEAP_HELPER_ARGUMENTS)
}

/// A call of `function` at frame offset zero, whose effects
/// `SirProgram::summarize_calls` gives.
fn call(function: u32) -> Vec<SIRInstruction<RegionedAbsoluteAddr>> {
    vec![
        SIRInstruction::Imm(RegisterId(0), SIRValue::new(0u8)),
        SIRInstruction::Call {
            function: FunctionId(function),
            frame: RegisterId(0),
            args: Vec::new(),
            outputs: Vec::new(),
            dst: None,
            effects: Default::default(),
        },
    ]
}

/// A store of register 0 into the first byte of a state object.
fn store() -> SIRInstruction<RegionedAbsoluteAddr> {
    let address = AbsoluteAddr {
        instance_id: celox_design::InstanceId(0),
        var_id: celox_design::StateObjectId(0),
    };
    SIRInstruction::Store(
        stable(address),
        SIROffset::Static(0),
        8,
        RegisterId(0),
        Vec::new(),
        Vec::new(),
    )
}

/// A unit that only calls the coverage helper is no opaque dependency of the
/// plan, whether the call is its own or a function's; any other call into
/// the host still makes it one, a heap call or a user function's among them,
/// of its own or in a function it calls. In combinational logic every call
/// of a host function counts, the coverage helper's too. A call of an
/// isolated function is none in either, and a coverage helper call beside
/// it changes nothing of that outside combinational logic.
#[test]
fn coverage_helper_calls_are_no_opaque_dependency() {
    let sampling = [helper_call(COVERAGE_HELPER_ID), vec![store()]].concat();
    let function = |instructions| FunctionUnit {
        unit: unit(instructions),
        frame: FrameLayout {
            size: 0,
            slots: Vec::new(),
        },
        effects: Default::default(),
    };
    // The design's functions: 0 may reach any state, 1 is isolated.
    let extern_function = |isolated| celox_design::ExternFunction {
        name: String::new(),
        signature: celox_design::ExternSignature {
            pure: false,
            result: None,
            arguments: Vec::new(),
            directions: Vec::new(),
        },
        isolated,
    };
    let functions = [extern_function(false), extern_function(true)];
    // fn0 samples and stores; fn1 calls fn0; fn2 makes a heap call; fn3
    // samples and calls fn2; fn4 calls a user function; fn5 calls fn4; fn6
    // calls an isolated function.
    let mut program = crate::ir::SirProgram {
        updates: Default::default(),
        parallel: None,
        processes: vec![
            unit(sampling.clone()),
            unit(call(0)),
            unit(call(1)),
            unit(helper_call(HEAP_HELPER_ID)),
            unit(call(2)),
            unit(call(3)),
            unit([helper_call(COVERAGE_HELPER_ID), helper_call(HEAP_HELPER_ID)].concat()),
            unit(extern_call(0, 1)),
            unit(call(4)),
            unit(call(5)),
            unit([helper_call(COVERAGE_HELPER_ID), call(4)].concat()),
            unit(extern_call(NativeMemoryOp::ReadIndirect.function(), 2)),
            unit(vec![store()]),
            unit(extern_call(1, 1)),
            unit([helper_call(COVERAGE_HELPER_ID), extern_call(1, 1)].concat()),
            unit(call(6)),
        ],
        functions: vec![
            function(sampling),
            function(call(0)),
            function(helper_call(HEAP_HELPER_ID)),
            function([helper_call(COVERAGE_HELPER_ID), call(2)].concat()),
            function(extern_call(0, 1)),
            function(call(4)),
            function(extern_call(1, 1)),
        ],
    };
    program.summarize_calls();
    let opaque_units: Vec<bool> = program
        .processes
        .iter()
        .map(|unit| opaque(unit, &functions))
        .collect();
    // A summary does not say which function a callee calls, so a call of a
    // function that calls an isolated one stays opaque (the last unit).
    assert_eq!(
        opaque_units,
        [
            false, false, false, true, true, true, true, true, true, true, true, false, false,
            false, false, true
        ]
    );
    // In a settle the sampling units count like the others: only the native
    // memory operation, the plain store and the call of an isolated function
    // are no opaque calls there.
    let opaque_in_settles: Vec<bool> = program
        .processes
        .iter()
        .map(|unit| opaque_in_settle(unit, &functions))
        .collect();
    assert_eq!(
        opaque_in_settles,
        [
            true, true, true, true, true, true, true, true, true, true, true, false, false, false,
            true, true
        ]
    );
    // Without the design's table no function is isolated, and the coverage
    // helper's `func` is in no table.
    assert!(opaque(&program.processes[13], &[]));
    assert!(!opaque(&program.processes[0], &[]));
    // The scheduler still sees an external call in each sampling unit, and
    // the state a sampling function stores as the caller's write.
    for sampler in &program.processes[..3] {
        assert!(instructions(sampler).any(makes_extern_calls));
        assert_eq!(instructions(sampler).flat_map(written_storage).count(), 1);
    }
    assert!(!instructions(&program.processes[12]).any(makes_extern_calls));
}
