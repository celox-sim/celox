#![allow(
    clippy::disallowed_types,
    reason = "test-only: gates and counters shared with the background compile thread"
)]
use super::*;
use crate::backend::native::NativeBackend;
use crate::{CodegenError, RuntimeEvent, Simulator, SimulatorBuilder, SimulatorErrorKind};
use std::sync::atomic::{AtomicBool, Ordering};

/// Two-stage pipeline: `q` trails `d` by two clock edges.
const PIPELINE: &str = r#"
module Top (
    clk: input clock,
    rst: input reset,
    d: input logic<8>,
    q: output logic<8>,
) {
    var stage1: logic<8>;
    var stage2: logic<8>;
    always_ff (clk, rst) {
        if_reset {
            stage1 = 0;
            stage2 = 0;
        } else {
            stage1 = d;
            stage2 = stage1;
        }
    }
    assign q = stage2;
}
"#;

struct Gate(Arc<AtomicBool>);

impl Gate {
    fn closed() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    fn open(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn wait_for_gate_or_cancel(
    gate: &AtomicBool,
    cancel: &CompileCancel,
) -> Result<(), SimulatorError> {
    while !gate.load(Ordering::Acquire) {
        if cancel.is_cancelled() {
            return Err(super::super::compile_cancel::cancelled_error());
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    Ok(())
}

/// Compile through the Cranelift tier behind a gate. The native tier is
/// covered end-to-end by the public `build_tiered` integration tests on
/// native hosts; gating lets these unit tests pin promotion timing.
fn build_gated(gate: &Gate) -> Simulator<TieredBackend> {
    let worker_gate = gate.0.clone();
    SimulatorBuilder::<Simulator>::new(PIPELINE, "Top")
        .build_tiered_with_compiler(move |laid_out, options, cancel| {
            wait_for_gate_or_cancel(&worker_gate, cancel)?;
            {
                let image = NativeBackend::compile_image(laid_out, options)?;
                let shared = unsafe { SharedNativeCode::from_image(image)? };
                Ok(CompiledCode::Native(Arc::new(shared)))
            }
        })
        .unwrap()
}

/// Drive the pipeline through a reset and an input sweep, recording `q`
/// after every tick. Identical inputs must produce identical outputs no
/// matter when (or whether) promotion lands, because both tiers are
/// bit-exact.
fn drive(sim: &mut Simulator<TieredBackend>, inputs: &[u8]) -> Vec<u8> {
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d = sim.signal("d");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(d, inputs[0]);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    let mut observed = Vec::new();
    for &value in inputs {
        sim.modify(|io| io.set(d, value)).unwrap();
        sim.tick(clk).unwrap();
        sim.tick(clk).unwrap();
        observed.push(sim.get_as::<u8>(sim.signal("q")));
    }
    observed
}

fn reference_outputs(inputs: &[u8]) -> Vec<u8> {
    // Each iteration ticks twice before sampling, which fully absorbs
    // the pipeline's two-cycle latency: the sampled output equals the
    // current input.
    inputs.to_vec()
}

#[test_case::test_case(false; "optimizing")]
#[test_case::test_case(true; "baseline")]
fn unpacked_array_works_across_promotion_on_strided_layout(baseline: bool) {
    // On native-default hosts this runs the interpreter against an
    // element-strided layout, exercising strided element addressing and
    // plane-sized state before and after promotion.
    let code = r#"
module Top (
    clk: input clock,
    we: input logic,
    waddr: input logic<3>,
    wdata: input logic<8>,
    raddr: input logic<3>,
    q: output logic<8>,
) {
    var mem: logic<8>[8];
    always_ff (clk) {
        if we {
            mem[waddr] = wdata;
        }
    }
    assign q = mem[raddr];
}
"#;
    let gate = Gate::closed();
    let worker_gate = gate.0.clone();
    let mut sim: Simulator<TieredBackend> = SimulatorBuilder::<Simulator>::new(code, "Top")
        .build_tiered_with_compiler(move |laid_out, options, cancel| {
            wait_for_gate_or_cancel(&worker_gate, cancel)?;
            {
                let mut options = options.clone();
                options.x86_options.baseline = baseline;
                options.x86_options.diagnostics.verify_mir_passes = true;
                options.x86_options.diagnostics.verify_regalloc = true;
                let image = NativeBackend::compile_image(laid_out, &options)?;
                let shared = unsafe { SharedNativeCode::from_image(image)? };
                Ok(CompiledCode::Native(Arc::new(shared)))
            }
        })
        .unwrap();
    let clk = sim.event("clk");
    let we = sim.signal("we");
    let waddr = sim.signal("waddr");
    let wdata = sim.signal("wdata");
    let raddr = sim.signal("raddr");
    let q = sim.signal("q");

    // Fill every element while still interpreted.
    for i in 0..8u8 {
        sim.modify(|io| {
            io.set(we, 1u8);
            io.set(waddr, i);
            io.set(wdata, i * 7 + 1);
        })
        .unwrap();
        sim.tick(clk).unwrap();
    }
    sim.modify(|io| io.set(we, 0u8)).unwrap();

    let read_back = |sim: &mut Simulator<TieredBackend>, index: u8| -> u8 {
        sim.modify(|io| io.set(raddr, index)).unwrap();
        sim.get_as::<u8>(q)
    };

    // Verify interpreted results...
    for i in 0..8u8 {
        assert_eq!(read_back(&mut sim, i), i * 7 + 1);
    }

    // ...release the worker and promote...
    gate.open();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !sim.is_compiled() && std::time::Instant::now() < deadline {
        sim.tick(clk).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(sim.is_compiled());

    // ...and confirm the whole array survived the memory handoff.
    for i in 0..8u8 {
        assert_eq!(read_back(&mut sim, i), i * 7 + 1, "element {i}");
    }

    // Writes keep working on the compiled tier too.
    sim.modify(|io| {
        io.set(we, 1u8);
        io.set(waddr, 3u8);
        io.set(wdata, 250u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(we, 0u8)).unwrap();
    assert_eq!(read_back(&mut sim, 3), 250u8);
}

/// Four-state dynamically-indexed array through native promotion.
///
/// Exercises element-strided addressing, sparse commit, and mask plane
/// handling across the tier boundary.
#[test_case::test_case(false; "optimizing")]
#[test_case::test_case(true; "baseline")]
fn four_state_unpacked_array_planes_initialize_and_survive_promotion(baseline: bool) {
    let code = r#"
module Top (
    clk: input clock,
    we: input logic,
    waddr: input logic<2>,
    wdata: input logic<8>,
    raddr: input logic<2>,
    q: output logic<8>,
) {
    var mem: logic<8>[4];
    always_ff (clk) {
        if we {
            mem[waddr] = wdata;
        }
    }
    assign q = mem[raddr];
}
"#;
    let gate = Gate::closed();
    let worker_gate = gate.0.clone();
    let mut sim: Simulator<TieredBackend> = SimulatorBuilder::<Simulator>::new(code, "Top")
        .four_state(true)
        .build_tiered_with_compiler(move |laid_out, options, cancel| {
            wait_for_gate_or_cancel(&worker_gate, cancel)?;
            let mut options = options.clone();
            options.x86_options.baseline = baseline;
            options.x86_options.diagnostics.verify_mir_passes = true;
            options.x86_options.diagnostics.verify_regalloc = true;
            let image = NativeBackend::compile_image(laid_out, &options)?;
            let shared = unsafe { SharedNativeCode::from_image(image)? };
            Ok(CompiledCode::Native(Arc::new(shared)))
        })
        .unwrap();
    let clk = sim.event("clk");
    let we = sim.signal("we");
    let waddr = sim.signal("waddr");
    let wdata = sim.signal("wdata");
    let raddr = sim.signal("raddr");
    let q = sim.signal("q");

    // Write all elements on the interpreted tier.
    for i in 0..4u8 {
        sim.modify(|io| {
            io.set(we, 1u8);
            io.set(waddr, i);
            io.set(wdata, i * 60 + 5);
        })
        .unwrap();
        sim.tick(clk).unwrap();
    }
    sim.modify(|io| io.set(we, 0u8)).unwrap();

    // Verify interpreted-tier self-consistency.
    for i in 0..4u8 {
        sim.modify(|io| io.set(raddr, i)).unwrap();
        assert_eq!(sim.get_as::<u8>(q), i * 60 + 5);
    }

    // Promote to native tier.
    gate.open();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !sim.is_compiled() && std::time::Instant::now() < deadline {
        sim.tick(clk).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(sim.is_compiled());

    // Verify all elements survived promotion and are readable on native.
    for i in 0..4u8 {
        sim.modify(|io| io.set(raddr, i)).unwrap();
        assert_eq!(
            sim.get_as::<u8>(q),
            i * 60 + 5,
            "element {i} after native promotion"
        );
    }

    // Writes keep working on the native tier.
    sim.modify(|io| {
        io.set(we, 1u8);
        io.set(waddr, 2u8);
        io.set(wdata, 200u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(we, 0u8)).unwrap();
    sim.modify(|io| io.set(raddr, 2u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(q), 200u8, "native-tier write");

    // Exercise the mask plane too: baseline lowering must preserve X/Z
    // bits when a dynamic store is committed and read through the array.
    sim.modify(|io| {
        io.set(we, 1u8);
        io.set_four_state(wdata, 0xa5u8.into(), 0x3cu8.into());
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(we, 0u8)).unwrap();
    assert_eq!(sim.get_four_state(q), (0xa5u8.into(), 0x3cu8.into()));
}

/// Compare Packed-mode interpreter vs ElementStrided-mode interpreter
/// (no promotion). If these differ, the strided addressing formula is
/// wrong independent of any tier transition.
#[test]
fn strided_interp_matches_packed_interp_for_four_state_array() {
    let code = r#"
module Top (
    clk: input clock,
    we: input logic,
    waddr: input logic<2>,
    wdata: input logic<8>,
    raddr: input logic<2>,
    q: output logic<8>,
) {
    var mem: logic<8>[4];
    always_ff (clk) {
        if we {
            mem[waddr] = wdata;
        }
    }
    assign q = mem[raddr];
}
"#;
    // VCD explicitly requests the packed compatibility layout.
    let packed_trace = tempfile::tempdir().unwrap();
    let mut packed_sim = SimulatorBuilder::<Simulator>::new(code, "Top")
        .vcd(packed_trace.path().join("packed.vcd"))
        .four_state(true)
        .build_interpreter()
        .unwrap();

    // Strided: ElementStrided interpreter via gated tiered (never promotes).
    let gate = Gate::closed();
    let worker_gate = gate.0.clone();
    let mut strided_sim: Simulator<TieredBackend> = SimulatorBuilder::<Simulator>::new(code, "Top")
        .four_state(true)
        .build_tiered_with_compiler(move |laid_out, options, cancel| {
            wait_for_gate_or_cancel(&worker_gate, cancel)?;
            {
                let image = NativeBackend::compile_image(laid_out, options)?;
                let shared = unsafe { SharedNativeCode::from_image(image)? };
                Ok(CompiledCode::Native(Arc::new(shared)))
            }
        })
        .unwrap();

    let packed_clk = packed_sim.event("clk");
    let strided_clk = strided_sim.event("clk");
    let packed_we = packed_sim.signal("we");
    let strided_we = strided_sim.signal("we");
    let packed_waddr = packed_sim.signal("waddr");
    let strided_waddr = strided_sim.signal("waddr");
    let packed_wdata = packed_sim.signal("wdata");
    let strided_wdata = strided_sim.signal("wdata");
    let packed_raddr = packed_sim.signal("raddr");
    let strided_raddr = strided_sim.signal("raddr");

    // Write identical data to both simulators.
    for i in 0..4u8 {
        // Drive packed
        packed_sim
            .modify(|io| {
                io.set(packed_we, 1u8);
                io.set(packed_waddr, i);
                io.set(packed_wdata, i * 60 + 5);
            })
            .unwrap();
        packed_sim.tick(packed_clk).unwrap();
        // Drive strided
        strided_sim
            .modify(|io| {
                io.set(strided_we, 1u8);
                io.set(strided_waddr, i);
                io.set(strided_wdata, i * 60 + 5);
            })
            .unwrap();
        strided_sim.tick(strided_clk).unwrap();
    }
    packed_sim.modify(|io| io.set(packed_we, 0u8)).unwrap();
    strided_sim.modify(|io| io.set(strided_we, 0u8)).unwrap();

    // Read back and compare.
    for i in 0..4u8 {
        packed_sim.modify(|io| io.set(packed_raddr, i)).unwrap();
        strided_sim.modify(|io| io.set(strided_raddr, i)).unwrap();
        let pv = packed_sim.get_as::<u8>(packed_sim.signal("q"));
        let sv = strided_sim.get_as::<u8>(strided_sim.signal("q"));
        assert_eq!(
            pv, sv,
            "element {i} diverges between Packed and ElementStrided interp"
        );
    }
}

/// SV unpacked arrays start unknown through packed initial values, take
/// element writes and whole-array NBA commits, and read back whole: the
/// element-strided interpreter, the packed interpreter and native agree.
#[test]
fn strided_sv_arrays_initialize_commit_and_read_like_packed_ones() {
    const SOURCE: &str = r#"
            module Top (
                input logic clk, input logic we,
                input logic [2:0] wi, input logic [12:0] wd, input logic [2:0] ri,
                output logic [12:0] q
            );
                logic [12:0] mem [8];
                always_ff @(posedge clk) if (we) mem[wi] <= wd;
                assign q = mem[ri];
            endmodule
        "#;
    fn run<B: crate::backend::SimBackend>(
        mut sim: Simulator<B>,
    ) -> (bool, Vec<(num_bigint::BigUint, num_bigint::BigUint)>) {
        let strided = sim.signal("mem").array_layout.is_some();
        let (clk, we, wi, wd, ri, q, mem) = (
            sim.event("clk"),
            sim.signal("we"),
            sim.signal("wi"),
            sim.signal("wd"),
            sim.signal("ri"),
            sim.signal("q"),
            sim.signal("mem"),
        );
        let mut seen = vec![sim.get_four_state(mem)];
        for step in 0..12u16 {
            sim.modify(|io| {
                io.set(we, u8::from(step % 4 != 3));
                io.set(wi, (step * 5 % 8) as u8);
                io.set(wd, step.wrapping_mul(0x9e5) & 0x1fff);
            })
            .unwrap();
            sim.tick(clk).unwrap();
            for index in 0..8u8 {
                sim.modify(|io| io.set(ri, index)).unwrap();
                seen.push(sim.get_four_state(q));
            }
            seen.push(sim.get_four_state(mem));
        }
        (strided, seen)
    }
    let builder = || {
        Simulator::from_sv_sources(
            vec![(SOURCE, std::path::Path::new("strided_array.sv"))],
            "Top",
        )
        .four_state(true)
    };
    let packed_trace = tempfile::tempdir().unwrap();
    let (strided, reference) = run(builder()
        .vcd(packed_trace.path().join("packed.vcd"))
        .build_interpreter()
        .unwrap());
    assert!(!strided, "VCD requests packed storage");
    let (strided, interpreted) = run(builder().build_interpreter().unwrap());
    assert!(strided, "the standalone interpreter stores arrays strided");
    assert_eq!(interpreted, reference);
    // Every element starts unknown.
    assert_eq!(
        reference[0].1,
        (num_bigint::BigUint::from(1u8) << (13 * 8)) - 1u8
    );
    let gate = Gate::closed();
    let worker_gate = gate.0.clone();
    let (strided, interpreted) = run(builder()
        .build_tiered_with_compiler(move |laid_out, options, cancel| {
            wait_for_gate_or_cancel(&worker_gate, cancel)?;
            let image = NativeBackend::compile_image(laid_out, options)?;
            let shared = unsafe { SharedNativeCode::from_image(image)? };
            Ok(CompiledCode::Native(Arc::new(shared)))
        })
        .unwrap());
    assert!(strided, "the tiered interpreter stores the array strided");
    assert_eq!(interpreted, reference);
    let (strided, native) = run(builder().build_native().unwrap());
    assert!(strided, "native stores the array strided");
    assert_eq!(native, reference);
}

#[test]
fn outputs_match_reference_when_promotion_never_happens() {
    let inputs: Vec<u8> = (10..40).collect();
    let gate = Gate::closed();
    let mut sim = build_gated(&gate);
    let observed = drive(&mut sim, &inputs);
    assert_eq!(observed, reference_outputs(&inputs));
    assert!(!sim.is_compiled());
    assert!(sim.promotion_error().is_none(), "gate still closed");
}

#[test]
fn outputs_match_reference_when_promotion_lands_mid_run() {
    let inputs: Vec<u8> = (10..40).collect();
    let gate = Gate::closed();
    let mut sim = build_gated(&gate);

    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d = sim.signal("d");
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(d, inputs[0]);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Run the first half strictly interpreted.
    let mut observed = Vec::new();
    for &value in &inputs[..inputs.len() / 2] {
        sim.modify(|io| io.set(d, value)).unwrap();
        sim.tick(clk).unwrap();
        sim.tick(clk).unwrap();
        observed.push(sim.get_as::<u8>(sim.signal("q")));
    }

    // Release the worker; the next safe points adopt the compiled tier.
    gate.open();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !sim.is_compiled() && std::time::Instant::now() < deadline {
        // Hold the input steady so any extra ticks are unobservable.
        sim.tick(clk).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(sim.is_compiled());
    assert!(sim.promotion_error().is_none());

    for &value in &inputs[inputs.len() / 2..] {
        sim.modify(|io| io.set(d, value)).unwrap();
        sim.tick(clk).unwrap();
        sim.tick(clk).unwrap();
        observed.push(sim.get_as::<u8>(sim.signal("q")));
    }

    assert_eq!(observed, reference_outputs(&inputs));
}

#[test]
fn waveform_activity_survives_promotion_before_dump() {
    for expose_raw in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("promotion.vcd");
        let gate = Gate::closed();
        let worker_gate = gate.0.clone();
        let mut sim: Simulator<TieredBackend> = SimulatorBuilder::<Simulator>::new(PIPELINE, "Top")
            .vcd(&path)
            .build_tiered_with_compiler(move |laid_out, options, cancel| {
                wait_for_gate_or_cancel(&worker_gate, cancel)?;
                let image = NativeBackend::compile_image(laid_out, options)?;
                let shared = unsafe { SharedNativeCode::from_image(image)? };
                Ok(CompiledCode::Native(Arc::new(shared)))
            })
            .unwrap();
        let descs = sim.build_vcd_descs(false);
        let mut reference = crate::VcdWriter::from_writer(Vec::new(), &descs);
        let dump = |sim: &mut Simulator<TieredBackend>,
                    reference: &mut crate::VcdWriter<Vec<u8>>,
                    time| {
            sim.dump(time);
            let (ptr, size) = sim.memory_as_ptr();
            reference
                .dump(time, unsafe { std::slice::from_raw_parts(ptr, size) })
                .unwrap();
        };
        let clk = sim.event("clk");
        let rst = sim.signal("rst");
        let d = sim.signal("d");
        sim.set(rst, 0u8);
        sim.tick(clk).unwrap();
        dump(&mut sim, &mut reference, 0);
        let raw = expose_raw.then(|| {
            let (ptr, size) = sim.memory_as_ptr();
            // Snapshot before exposing a pointer, then restore the full image.
            let snapshot = unsafe { std::slice::from_raw_parts(ptr, size) }.to_vec();
            let (ptr, _) = sim.memory_as_mut_ptr();
            unsafe {
                std::ptr::copy_nonoverlapping(snapshot.as_ptr(), ptr, size);
            }
            ptr
        });
        sim.set(rst, 1u8);
        sim.set(d, 0xa5u8);
        sim.tick(clk).unwrap();
        sim.tick(clk).unwrap();
        assert!(!sim.is_compiled());
        // Leave interpreted writes pending while the compiled tier is adopted.
        gate.open();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !sim.is_compiled() && std::time::Instant::now() < deadline {
            sim.tick(clk).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(sim.is_compiled(), "{:?}", sim.promotion_error());
        dump(&mut sim, &mut reference, 1);
        let before = sim.vcd_statistics().unwrap().comparisons;
        if let Some(raw) = raw {
            // Reuse the pointer obtained while interpreting, without exposing
            // a new pointer from the compiled backend or notifying a setter.
            unsafe {
                *raw.add(d.offset) = 0x3c;
            }
        }
        dump(&mut sim, &mut reference, 2);
        assert_eq!(
            sim.vcd_statistics().unwrap().comparisons,
            before + if expose_raw { descs.len() as u64 } else { 0 }
        );
        sim.flush_vcd().unwrap();
        let parse = |bytes: Vec<u8>| {
            let mut parser = vcd::Parser::new(bytes.as_slice());
            parser.parse_header().unwrap();
            parser.map(Result::unwrap).collect::<Vec<_>>()
        };
        assert_eq!(
            parse(std::fs::read(path).unwrap()),
            parse(reference.into_inner().unwrap())
        );
    }
}

#[test]
fn compilation_failure_keeps_the_interpreter() {
    let mut sim: Simulator<TieredBackend> = SimulatorBuilder::<Simulator>::new(PIPELINE, "Top")
        .build_tiered_with_compiler(|_, _, _| {
            Err(SimulatorError::from(crate::RuntimeErrorCode::InternalError))
        })
        .unwrap();

    let inputs: Vec<u8> = (10..20).collect();
    let observed = drive(&mut sim, &inputs);
    assert_eq!(observed, reference_outputs(&inputs));
    assert!(!sim.is_compiled());

    // The worker reports its failure through the channel; keep polling
    // safe points until it lands (thread start may lag under load).
    let clk = sim.event("clk");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while sim.promotion_error().is_none() && std::time::Instant::now() < deadline {
        sim.tick(clk).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(sim.promotion_error().is_some());
}

#[test]
fn runtime_events_stay_continuous_across_promotion() {
    let code = r#"
module Top (
    clk: input clock,
    cnt: output logic<8>,
) {
    var c: logic<8>;
    always_ff (clk) {
        c = c + 1;
        $display("tick %0d", c);
    }
    assign cnt = c;
}
"#;
    let gate = Gate::closed();
    let worker_gate = gate.0.clone();
    let mut sim: Simulator<TieredBackend> = SimulatorBuilder::<Simulator>::new(code, "Top")
        .build_tiered_with_compiler(move |laid_out, options, cancel| {
            wait_for_gate_or_cancel(&worker_gate, cancel)?;
            {
                let image = NativeBackend::compile_image(laid_out, options)?;
                let shared = unsafe { SharedNativeCode::from_image(image)? };
                Ok(CompiledCode::Native(Arc::new(shared)))
            }
        })
        .unwrap();
    let clk = sim.event("clk");

    // Five interpreted ticks emit tick 0..4 (display observes the
    // pre-increment value).
    for _ in 0..5 {
        sim.tick(clk).unwrap();
    }
    let pre = sim.drain_runtime_events();
    assert_eq!(pre.len(), 5);
    for (index, event) in pre.iter().enumerate() {
        let RuntimeEvent::Display { message } = event else {
            panic!("unexpected event {event:?}");
        };
        assert_eq!(
            std::str::from_utf8(message).unwrap(),
            format!("tick {index}")
        );
    }

    gate.open();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !sim.is_compiled() && std::time::Instant::now() < deadline {
        sim.tick(clk).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(sim.is_compiled());
    // Discard the unbounded wait-loop events.
    let _ = sim.drain_runtime_events();

    // Eight compiled-tier ticks continue the counter seamlessly.
    const POST_TICKS: u32 = 8;
    for _ in 0..POST_TICKS {
        sim.tick(clk).unwrap();
    }
    let base = sim
        .get_as::<u8>(sim.signal("cnt"))
        .wrapping_sub(POST_TICKS as u8);
    let post = sim.drain_runtime_events();
    assert_eq!(post.len(), POST_TICKS as usize);
    for (index, event) in post.iter().enumerate() {
        let RuntimeEvent::Display { message } = event else {
            panic!("unexpected event {event:?}");
        };
        assert_eq!(
            std::str::from_utf8(message).unwrap(),
            format!("tick {}", base.wrapping_add(index as u8))
        );
    }
}

/// Explicit cancellation unwinds background compilation and leaves the
/// interpreter as the permanent tier, with the cancellation retrievable
/// through `promotion_error`.
#[test]
fn cancel_background_compilation_stays_on_the_interpreter() {
    let mut sim: Simulator<TieredBackend> = SimulatorBuilder::<Simulator>::new(PIPELINE, "Top")
        .build_tiered_with_compiler(|laid_out, options, cancel| {
            while !cancel.is_cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let image = NativeBackend::compile_image_with_cancel(laid_out, options, cancel)?;
            let shared = unsafe { SharedNativeCode::from_image(image)? };
            Ok(CompiledCode::Native(Arc::new(shared)))
        })
        .unwrap();

    assert!(sim.cancel_background_compilation());

    let clk = sim.event("clk");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while sim.promotion_error().is_none() && std::time::Instant::now() < deadline {
        sim.tick(clk).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(!sim.is_compiled());
    match sim.promotion_error() {
        Some(error) => assert!(matches!(
            error.kind(),
            SimulatorErrorKind::Codegen(CodegenError::Cancelled)
        )),
        None => panic!("cancellation was not reported through promotion_error"),
    }

    // The interpreted tier keeps producing correct results afterwards.
    let inputs: Vec<u8> = (0..8).collect();
    assert_eq!(drive(&mut sim, &inputs), reference_outputs(&inputs));
}

/// Dropping the simulator cancels and joins the worker before returning.
#[test]
fn drop_cancels_pending_background_compilation() {
    let saw_cancel = Arc::new(AtomicBool::new(false));
    let worker_saw_cancel = Arc::clone(&saw_cancel);
    let sim: Simulator<TieredBackend> = SimulatorBuilder::<Simulator>::new(PIPELINE, "Top")
        .build_tiered_with_compiler(move |_laid_out, _options, cancel| {
            while !cancel.is_cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            worker_saw_cancel.store(true, Ordering::SeqCst);
            Err(SimulatorError::new(SimulatorErrorKind::Codegen(
                CodegenError::Cancelled,
            )))
        })
        .unwrap();
    drop(sim);
    assert!(saw_cancel.load(Ordering::Acquire));
}

/// `TierPromotion::Never` never spawns the background worker and keeps
/// the simulation interpreted permanently without recording an error.
#[test]
fn tier_promotion_never_skips_background_compilation() {
    let invoked = Arc::new(AtomicBool::new(false));
    let worker_invoked = Arc::clone(&invoked);
    let mut sim: Simulator<TieredBackend> = SimulatorBuilder::<Simulator>::new(PIPELINE, "Top")
        .tier_promotion(crate::TierPromotion::Never)
        .build_tiered_with_compiler(move |_laid_out, _options, _cancel| {
            worker_invoked.store(true, Ordering::SeqCst);
            Err(SimulatorError::from(crate::RuntimeErrorCode::InternalError))
        })
        .unwrap();

    // Give a hypothetical worker ample time to start; the policy must
    // have prevented the spawn entirely.
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(!invoked.load(Ordering::SeqCst));
    assert!(!sim.is_compiled());
    assert!(sim.promotion_error().is_none());
    let before = sim.tiered_execution_stats();
    assert_eq!(before.tier, TieredExecutionTier::Interpreter);
    assert_eq!(before.promotion, TieredPromotionStatus::Disabled);
    assert_eq!(before.compiled_evaluations, 0);

    // The interpreted tier still produces correct results.
    let inputs: Vec<u8> = (0..8).collect();
    assert_eq!(drive(&mut sim, &inputs), reference_outputs(&inputs));
    let after = sim.tiered_execution_stats();
    assert!(after.interpreted_evaluations > before.interpreted_evaluations);
    assert_eq!(after.compiled_evaluations, 0);
}

/// `TierPromotion::AfterSteps` lets background compilation finish but
/// defers adoption until the interpreter crosses the step threshold.
#[test]
fn tier_promotion_after_steps_defers_adoption() {
    const THRESHOLD: u64 = 1000;
    let sim_build: Simulator<TieredBackend> = SimulatorBuilder::<Simulator>::new(PIPELINE, "Top")
        .tier_promotion(crate::TierPromotion::AfterSteps(THRESHOLD))
        .build_tiered_with_compiler(|laid_out, options, _cancel| {
            let image = NativeBackend::compile_image(laid_out, options)?;
            let shared = unsafe { SharedNativeCode::from_image(image)? };
            Ok(CompiledCode::Native(Arc::new(shared)))
        })
        .unwrap();
    let mut sim = sim_build;
    let clk = sim.event("clk");
    let initial = sim.tiered_execution_stats();
    assert!(initial.interpreted_evaluations < THRESHOLD);

    // A handful of ticks stay far below the threshold; even once the
    // compiled tier is ready, adoption must not land.
    for _ in 0..4 {
        sim.tick(clk).unwrap();
    }
    assert!(
        !sim.is_compiled(),
        "adopted before the interpreted-step threshold"
    );

    // Crossing the threshold lets subsequent safe points adopt.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !sim.is_compiled() && std::time::Instant::now() < deadline {
        sim.tick(clk).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(sim.is_compiled());
    assert!(sim.promotion_error().is_none());
    let promoted = sim.tiered_execution_stats();
    assert_eq!(promoted.tier, TieredExecutionTier::Compiled);
    assert_eq!(promoted.promotion, TieredPromotionStatus::Promoted);
    assert!(promoted.interpreted_evaluations >= THRESHOLD);
    assert_eq!(
        promoted.promoted_after_interpreted_evaluations,
        Some(promoted.interpreted_evaluations)
    );
    assert!(promoted.threshold_deferrals > 0);

    sim.tick(clk).unwrap();
    assert!(sim.tiered_execution_stats().compiled_evaluations > promoted.compiled_evaluations);
}

/// Promoting to the Cranelift tier must report an image requirement that
/// covers adoption's resize target (a MemorySpilled plan grows the layout
/// beyond the interpreter's semantic state), and the growth must stay
/// within the reserved arena so the live image never moves.
#[test]
#[cfg(feature = "cranelift")]
fn cranelift_promotion_covers_its_image_requirement_without_moving_the_image() {
    use std::sync::Mutex;

    let gate = Gate::closed();
    let worker_gate = gate.0.clone();
    let observed: Arc<Mutex<Option<(usize, usize)>>> = Arc::new(Mutex::new(None));
    let worker_observed = observed.clone();

    let mut sim: Simulator<TieredBackend> = SimulatorBuilder::<Simulator>::new(PIPELINE, "Top")
        .build_tiered_with_compiler(move |laid_out, options, cancel| {
            wait_for_gate_or_cancel(&worker_gate, cancel)?;
            let shared = Arc::new(JitBackend::compile(laid_out, options, None)?);
            let code = CompiledCode::Cranelift(Arc::clone(&shared));
            // (adoption resize target in words, reported requirement)
            *worker_observed.lock().unwrap() = Some((
                shared.layout.merged_total_size.div_ceil(8),
                code.required_image_words(),
            ));
            Ok(code)
        })
        .unwrap();

    let clk = sim.event("clk");
    for _ in 0..2 {
        sim.tick(clk).unwrap();
    }
    let (base_before, _) = sim.memory_as_ptr();

    gate.open();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !sim.is_compiled() && std::time::Instant::now() < deadline {
        sim.tick(clk).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(sim.is_compiled());
    assert!(sim.promotion_error().is_none());

    let (adopt_target, required) = observed.lock().unwrap().expect("compiler ran");
    assert!(
        required >= adopt_target,
        "required_image_words ({required}) must cover the Cranelift \
             adoption resize target ({adopt_target})"
    );

    let (base_after, _) = sim.memory_as_ptr();
    assert_eq!(
        base_before, base_after,
        "promotion must not move the live memory image"
    );
}

#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
fn build_gated_two_stage(
    baseline_gate: &Gate,
    upgrade_gate: &Gate,
    fail_upgrade: bool,
) -> (Simulator<TieredBackend>, mpsc::Receiver<()>) {
    // Reuse the existing test builder to obtain the finalized layout;
    // the temporary backend never compiles or executes generated code.
    let (layout_sender, layout_receiver) = mpsc::channel();
    let code = PIPELINE.replace(
        "assign q = stage2;",
        "always_comb { q = stage2; $display(\"q %0d\", q); }",
    );
    let mut sim = SimulatorBuilder::<Simulator>::new(&code, "Top")
        .build_tiered_with_compiler(move |laid_out, options, _| {
            layout_sender
                .send((laid_out.clone(), options.clone()))
                .unwrap();
            Err(SimulatorError::from(crate::RuntimeErrorCode::InternalError))
        })
        .unwrap();
    let (laid_out, mut baseline_options) = layout_receiver
        .recv_timeout(Duration::from_secs(60))
        .unwrap();
    let mut optimizing_options = baseline_options.clone();
    baseline_options.x86_options.baseline = true;
    optimizing_options.x86_options.baseline = false;
    let worker_baseline_gate = baseline_gate.0.clone();
    let worker_upgrade_gate = upgrade_gate.0.clone();
    let (baseline_ready_sender, baseline_ready_receiver) = mpsc::channel();
    sim.backend = TieredBackend::with_two_stage_compiler(
        &Arc::new(laid_out),
        &baseline_options,
        optimizing_options,
        move |laid_out, options, cancel| {
            if options.x86_options.baseline {
                wait_for_gate_or_cancel(&worker_baseline_gate, cancel)?;
            } else {
                // Entering stage two proves the baseline is already queued.
                baseline_ready_sender.send(()).unwrap();
                wait_for_gate_or_cancel(&worker_upgrade_gate, cancel)?;
                if fail_upgrade {
                    return Err(SimulatorError::from(crate::RuntimeErrorCode::InternalError));
                }
            }
            let image = NativeBackend::compile_image_with_cancel(laid_out, options, cancel)?;
            let shared = unsafe { SharedNativeCode::from_image(image)? };
            if !options.x86_options.baseline {
                baseline_ready_sender.send(()).unwrap();
            }
            Ok(CompiledCode::Native(Arc::new(shared)))
        },
    );
    (sim, baseline_ready_receiver)
}

#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
fn build_lazy_derived_test(
    fail_stage: Option<usize>,
) -> (Simulator<TieredBackend>, [Gate; 3], mpsc::Receiver<usize>) {
    let source = r#"
            module Top(clk: input '_ clock, rst: input '_ reset,
                       q: output logic<8>, b: output logic<8>) {
                always_ff (clk, rst) {
                    if_reset { q = 0; } else { q += 1; }
                }
                let divided: '_ clock = q[1];
                always_ff (divided, rst) {
                    if_reset { b = 0; } else { b += 1; }
                }
            }
        "#;
    let (layout_sender, layout_receiver) = mpsc::channel();
    let mut sim = SimulatorBuilder::<Simulator>::new(source, "Top")
        .four_state(false)
        .build_tiered_with_compiler(move |laid_out, options, _| {
            layout_sender
                .send((laid_out.clone(), options.clone()))
                .unwrap();
            Err(SimulatorError::from(crate::RuntimeErrorCode::InternalError))
        })
        .unwrap();
    let (laid_out, mut baseline_options) = layout_receiver
        .recv_timeout(Duration::from_secs(60))
        .unwrap();
    assert!(!laid_out.runtime.runtime_schema.derived_edges.is_empty());
    // The derived-edge image expands its units from deferred recipes.
    assert_eq!(
        laid_out.deferred_derived_edges.len(),
        2 * laid_out.runtime.runtime_schema.derived_edges.len()
    );
    let mut optimizing_options = baseline_options.clone();
    baseline_options.x86_options.baseline = true;
    optimizing_options.x86_options.baseline = false;
    let gates = [Gate::closed(), Gate::closed(), Gate::closed()];
    let worker_gates = gates.each_ref().map(|gate| gate.0.clone());
    let (stages, receiver) = mpsc::channel();
    sim.backend = TieredBackend::with_two_stage_compiler(
        &Arc::new(laid_out),
        &baseline_options,
        optimizing_options,
        move |program, options, cancel| {
            let stage = if options.x86_options.baseline {
                0
            } else if program.runtime.runtime_schema.derived_edges.is_empty() {
                1
            } else {
                2
            };
            stages.send(stage).unwrap();
            wait_for_gate_or_cancel(&worker_gates[stage], cancel)?;
            if fail_stage == Some(stage) {
                return Err(SimulatorError::from(crate::RuntimeErrorCode::InternalError));
            }
            let image = NativeBackend::compile_image_with_cancel(program, options, cancel)?;
            let shared = unsafe { SharedNativeCode::from_image(image)? };
            Ok(CompiledCode::Native(Arc::new(shared)))
        },
    );
    let rst = sim.signal("rst");
    let q = sim.signal("q");
    let b = sim.signal("b");
    sim.set(rst, 1u8);
    sim.set(q, 1u8);
    sim.set(b, 9u8);
    sim.eval_comb().unwrap();
    assert_eq!(receiver.recv_timeout(Duration::from_secs(60)).unwrap(), 0);
    (sim, gates, receiver)
}

#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
fn adopt_lazy_core(
    sim: &mut Simulator<TieredBackend>,
    gates: &[Gate; 3],
    stages: &mpsc::Receiver<usize>,
) {
    gates[0].open();
    assert_eq!(stages.recv_timeout(Duration::from_secs(60)).unwrap(), 1);
    sim.backend.maybe_promote();
    let baseline = adopted_native_code(&sim.backend);
    let base = sim.backend.memory_as_ptr().0;
    gates[1].open();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        sim.backend.maybe_upgrade();
        if !Arc::ptr_eq(&baseline, &adopted_native_code(&sim.backend)) {
            break;
        }
        assert!(Instant::now() < deadline, "optimized core was not adopted");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!sim.backend.derived_edges_live);
    assert_eq!(sim.backend.memory_as_ptr().0, base);
    assert_eq!(stages.try_recv(), Err(mpsc::TryRecvError::Empty));
    assert_eq!(sim.get_as::<u8>(sim.signal("q")), 1);
    assert_eq!(sim.get_as::<u8>(sim.signal("b")), 9);
}

#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[test]
fn native_lazy_derived_adopts_core_then_requested_edge_units() {
    let (mut sim, gates, stages) = build_lazy_derived_test(None);
    adopt_lazy_core(&mut sim, &gates, &stages);
    let core = adopted_native_code(&sim.backend);
    let base = sim.backend.memory_as_ptr().0;
    sim.backend.request_derived_edge_units();
    assert!(!sim.backend.runs_derived_edge_units());
    assert_eq!(stages.recv_timeout(Duration::from_secs(60)).unwrap(), 2);
    gates[2].open();
    sim.backend.compiler_worker.take().unwrap().join().unwrap();
    sim.backend.maybe_upgrade();
    assert!(sim.backend.derived_edges_live);
    assert!(!Arc::ptr_eq(&core, &adopted_native_code(&sim.backend)));
    assert_eq!(sim.backend.memory_as_ptr().0, base);
    let clk = sim.event("clk");
    sim.edge(clk, celox_runtime::Edge::Rising).unwrap();
    assert_eq!(sim.get_as::<u8>(sim.signal("q")), 2);
    assert_eq!(sim.get_as::<u8>(sim.signal("b")), 10);
}

#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[test]
fn native_lazy_derived_early_request_keeps_two_stage_path() {
    let (mut sim, gates, stages) = build_lazy_derived_test(None);
    sim.backend.request_derived_edge_units();
    assert!(!sim.backend.runs_derived_edge_units());
    gates[0].open();
    assert_eq!(stages.recv_timeout(Duration::from_secs(60)).unwrap(), 2);
    sim.backend.maybe_promote();
    gates[2].open();
    sim.backend.compiler_worker.take().unwrap().join().unwrap();
    sim.backend.maybe_upgrade();
    assert!(sim.backend.derived_edges_live);
    let clk = sim.event("clk");
    sim.edge(clk, celox_runtime::Edge::Rising).unwrap();
    assert_eq!(sim.get_as::<u8>(sim.signal("q")), 2);
    assert_eq!(sim.get_as::<u8>(sim.signal("b")), 10);
}

#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[test]
fn native_lazy_derived_cancel_wakes_idle_compiler_and_keeps_core() {
    let (mut sim, gates, stages) = build_lazy_derived_test(None);
    adopt_lazy_core(&mut sim, &gates, &stages);
    let core = adopted_native_code(&sim.backend);
    assert!(sim.backend.cancel_background_compilation());
    sim.backend.compiler_worker.take().unwrap().join().unwrap();
    let clk = sim.event("clk");
    sim.edge(clk, celox_runtime::Edge::Rising).unwrap();
    assert!(Arc::ptr_eq(&core, &adopted_native_code(&sim.backend)));
    assert_eq!(sim.get_as::<u8>(sim.signal("q")), 2);
    assert_eq!(sim.get_as::<u8>(sim.signal("b")), 10);
}

#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[test]
fn native_lazy_derived_failed_edge_compilation_keeps_core() {
    let (mut sim, gates, stages) = build_lazy_derived_test(Some(2));
    adopt_lazy_core(&mut sim, &gates, &stages);
    let core = adopted_native_code(&sim.backend);
    sim.backend.request_derived_edge_units();
    assert!(!sim.backend.runs_derived_edge_units());
    assert_eq!(stages.recv_timeout(Duration::from_secs(60)).unwrap(), 2);
    gates[2].open();
    sim.backend.compiler_worker.take().unwrap().join().unwrap();
    sim.backend.maybe_upgrade();
    assert!(!sim.backend.derived_edges_live);
    assert!(Arc::ptr_eq(&core, &adopted_native_code(&sim.backend)));
    let clk = sim.event("clk");
    sim.edge(clk, celox_runtime::Edge::Rising).unwrap();
    assert_eq!(sim.get_as::<u8>(sim.signal("q")), 2);
    assert_eq!(sim.get_as::<u8>(sim.signal("b")), 10);
}

#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[test]
fn native_lazy_derived_drop_wakes_idle_compiler() {
    let (mut sim, gates, stages) = build_lazy_derived_test(None);
    adopt_lazy_core(&mut sim, &gates, &stages);
    let worker = sim.backend.compiler_worker.take().unwrap();
    drop(sim);
    worker.join().unwrap();
}

#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
fn adopted_native_code(backend: &TieredBackend) -> Arc<SharedNativeCode> {
    let Phase::Compiled(CompiledTier::Native(native)) = &backend.phase else {
        panic!("native code was not adopted");
    };
    native.shared_code()
}

#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[test]
fn native_two_stage_preserves_live_pipeline_across_split_apply() {
    let baseline_gate = Gate::closed();
    let upgrade_gate = Gate::closed();
    let (mut sim, baseline_ready) = build_gated_two_stage(&baseline_gate, &upgrade_gate, false);
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d = sim.signal("d");
    let q = sim.signal("q");
    let backend = &mut sim.backend;
    let base = backend.memory_as_ptr().0;
    let events: Vec<_> = backend
        .id_to_event_slice()
        .iter()
        .map(|e| (e.addr(), e.id()))
        .collect();
    backend.set(rst, 0u8);
    backend.eval_apply_ff_at(clk).unwrap();
    backend.set(rst, 1u8);
    backend.set(d, 17u8);
    backend.eval_apply_ff_at(clk).unwrap();
    backend.eval_comb().unwrap();
    assert_eq!(backend.get_as::<u8>(q), 0);

    baseline_gate.open();
    baseline_ready
        .recv_timeout(Duration::from_secs(60))
        .unwrap();
    backend.maybe_promote();
    let baseline = adopted_native_code(backend);
    assert_eq!(backend.memory_as_ptr().0, base);

    // The second edge must retain stage1=17 from interpreted execution,
    // rather than producing d=29 or resetting the pipeline on adoption.
    backend.set(d, 29u8);
    backend.eval_apply_ff_at(clk).unwrap();
    backend.eval_comb().unwrap();
    assert_eq!(backend.get_as::<u8>(q), 17, "after2");

    let eval = backend.resolve_eval_only_event(&clk.addr()).unwrap();
    let apply = backend.resolve_apply_event(&clk.addr()).unwrap();
    backend.set(d, 43u8);
    backend.eval_only_ff_at(eval).unwrap();
    upgrade_gate.open();
    // Joining proves the optimizing result is queued, without advancing
    // the simulation by an unbounded number of clock edges.
    backend.compiler_worker.take().unwrap().join().unwrap();
    backend.maybe_upgrade();
    assert!(Arc::ptr_eq(&baseline, &adopted_native_code(backend)));
    assert_eq!(backend.memory_as_ptr().0, base);
    backend.apply_ff_at(apply).unwrap();
    assert!(Arc::ptr_eq(&baseline, &adopted_native_code(backend)));

    // Keep a capture queued and its flag enabled across code replacement.
    backend.set_comb_capture_event_enabled(&[true]);
    let capture_start = crate::simulator::runtime_event_write_seq_for_backend(backend);
    if let Phase::Compiled(compiled) = &mut backend.phase {
        compiled.eval_comb().unwrap();
    }
    assert_eq!(backend.get_as::<u8>(q), 29);
    let event_buffer = backend.runtime_event_buffer().unwrap();
    let (buffer_ptr, buffer_size) = backend.runtime_event_buffer_as_ptr();
    let queued = unsafe { std::slice::from_raw_parts(buffer_ptr, buffer_size) }.to_vec();
    let before = crate::simulator::runtime_event_write_seq_for_backend(backend);
    assert_eq!(before, capture_start + 1);
    // Capture emission may consume its enable flag. Re-arm it before
    // replacement to prove the next image uses the same live flags.
    backend.set_comb_capture_event_enabled(&[true]);
    backend.clear_triggered_bits();
    backend.mark_triggered_bit(clk.id());
    let triggered = backend.get_triggered_bits();
    backend.maybe_upgrade();
    assert!(!Arc::ptr_eq(&baseline, &adopted_native_code(backend)));
    assert_eq!(backend.memory_as_ptr().0, base);
    assert_eq!(backend.get_as::<u8>(q), 29);
    assert_eq!(backend.get_triggered_bits(), triggered);
    assert_eq!(
        backend.runtime_event_buffer_as_ptr(),
        (buffer_ptr, buffer_size)
    );
    assert!(Arc::ptr_eq(
        &event_buffer,
        &backend.runtime_event_buffer().unwrap()
    ));
    assert_eq!(
        unsafe { std::slice::from_raw_parts(buffer_ptr, buffer_size) },
        queued
    );
    assert_eq!(
        backend
            .id_to_event_slice()
            .iter()
            .map(|e| (e.addr(), e.id()))
            .collect::<Vec<_>>(),
        events
    );
    for (addr, id) in events {
        assert_eq!(backend.resolve_event(&addr).id(), id);
    }

    backend.eval_comb().unwrap();
    assert_eq!(
        crate::simulator::runtime_event_write_seq_for_backend(backend),
        before + 1
    );
    backend.set_comb_capture_event_enabled(&[false]);
    backend.eval_comb().unwrap();
    assert_eq!(
        crate::simulator::runtime_event_write_seq_for_backend(backend),
        before + 1
    );
    // Handles resolved before either replacement still address the right
    // event, and the next edge observes stage1=43 from baseline execution.
    backend.set(d, 61u8);
    backend.eval_apply_ff_at(clk).unwrap();
    backend.eval_comb().unwrap();
    assert_eq!(backend.get_as::<u8>(q), 43);
    backend.eval_apply_ff_at(clk).unwrap();
    backend.eval_comb().unwrap();
    assert_eq!(backend.get_as::<u8>(q), 61);
    assert_eq!(backend.memory_as_ptr().0, base);
}

/// A design without derived edges compiles a baseline and an optimizing
/// image, and the timing names them so.
#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[test]
fn timing_names_the_stages_of_an_edge_free_two_stage_build() {
    let baseline_gate = Gate::closed();
    let upgrade_gate = Gate::closed();
    let (mut sim, _baseline_ready) = build_gated_two_stage(&baseline_gate, &upgrade_gate, false);
    let backend = &mut sim.backend;
    backend.start_execution_timing();
    baseline_gate.open();
    upgrade_gate.open();
    backend.compiler_worker.take().unwrap().join().unwrap();
    backend.maybe_promote();
    backend.maybe_upgrade();
    let timing = backend.finish_execution_timing().unwrap();
    let kinds: Vec<_> = timing.compiles().map(|stage| stage.kind).collect();
    assert_eq!(
        kinds,
        [TieredCompileKind::Baseline, TieredCompileKind::Optimizing]
    );
    assert!(timing.compiles().all(|stage| stage.completed));
    assert!(timing.promotion().is_some());
    assert!(timing.upgrade().is_some());
}

#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[test_case::test_case(false; "cancelled")]
#[test_case::test_case(true; "failed")]
fn unsuccessful_optimization_keeps_baseline_code(fail_upgrade: bool) {
    let baseline_gate = Gate::closed();
    let upgrade_gate = Gate::closed();
    let (mut sim, baseline_ready) =
        build_gated_two_stage(&baseline_gate, &upgrade_gate, fail_upgrade);
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d = sim.signal("d");
    let q = sim.signal("q");
    let backend = &mut sim.backend;
    backend.set(rst, 0u8);
    backend.eval_apply_ff_at(clk).unwrap();
    backend.set(rst, 1u8);
    backend.set(d, 37u8);
    backend.eval_apply_ff_at(clk).unwrap();
    baseline_gate.open();
    baseline_ready
        .recv_timeout(Duration::from_secs(60))
        .unwrap();
    backend.maybe_promote();
    let baseline = adopted_native_code(backend);
    let base = backend.memory_as_ptr().0;
    upgrade_gate.open();
    backend.compiler_worker.take().unwrap().join().unwrap();
    if fail_upgrade {
        backend.maybe_upgrade();
    } else {
        assert!(backend.cancel_background_compilation());
    }
    assert!(!backend.cancel_background_compilation());
    backend.set(d, 53u8);
    backend.eval_apply_ff_at(clk).unwrap();
    backend.eval_comb().unwrap();
    assert!(Arc::ptr_eq(&baseline, &adopted_native_code(backend)));
    assert_eq!(backend.get_as::<u8>(q), 37);
    backend.eval_apply_ff_at(clk).unwrap();
    backend.eval_comb().unwrap();
    assert_eq!(backend.get_as::<u8>(q), 53);
    assert_eq!(backend.memory_as_ptr().0, base);
    assert_eq!(
        backend.execution_stats().promotion,
        TieredPromotionStatus::Promoted
    );
    assert!(backend.promotion_error().is_none());
}

/// `y = a` with a unit appended to the settle by [`call_factorial`].
const FACTORIAL: &str = r#"
module Top (
    a: input logic<32>,
    y: output logic<32>,
) {
    assign y = a;
}
"#;

/// Append the function unit `fact(n, fp)` and a combinational unit that
/// stores `fact(a)` to `y`. `fact` gets its own frame's offset `fp` as an
/// argument and calls itself in the frame after it.
fn call_factorial(sir: &mut crate::ir::SirProgram) {
    use crate::ir::{BasicBlock, SIRValue};
    use celox_sir::{
        BinaryOp, BlockId, ExecutionUnit, FrameLayout, FrameSlot, FunctionId, FunctionUnit,
        RegisterId, RegisterType, SIRInstruction, SIROffset, SIRTerminator,
    };
    let comb = sir.eval_comb.iter().flat_map(|unit| unit.blocks.values());
    let accesses = comb
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    let input = accesses
        .iter()
        .find_map(|instruction| match instruction {
            SIRInstruction::Load(_, addr, ..) => Some(*addr),
            _ => None,
        })
        .expect("the settle reads `a`");
    let output = accesses
        .iter()
        .find_map(|instruction| match instruction {
            SIRInstruction::Store(addr, ..) => Some(*addr),
            _ => None,
        })
        .expect("the settle writes `y`");
    let slot = |index: u32| crate::ir::RegionedAbsoluteAddr {
        region: celox_design::FRAME_REGION,
        instance_id: celox_design::InstanceId(0),
        var_id: celox_design::StateObjectId(index),
    };
    let reg = RegisterId;
    let bits = |width| RegisterType::Bit {
        width,
        signed: false,
    };
    let load = |dst, addr| SIRInstruction::Load(reg(dst), addr, SIROffset::Static(0), 32);
    let store =
        |addr, src| SIRInstruction::Store(addr, SIROffset::Static(0), 32, reg(src), vec![], vec![]);
    let call = |frame, args: Vec<(usize, usize)>, dst| SIRInstruction::Call {
        function: FunctionId(0),
        frame: reg(frame),
        args: args
            .into_iter()
            .map(|(slot, arg)| (slot, reg(arg)))
            .collect(),
        outputs: vec![2],
        dst: Some(reg(dst)),
        effects: Default::default(),
    };
    let block = |id, instructions, terminator| {
        (
            BlockId(id),
            BasicBlock {
                id: BlockId(id),
                params: vec![],
                instructions,
                terminator,
            },
        )
    };
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [
            block(
                0,
                vec![
                    load(0, slot(0)),
                    SIRInstruction::Imm(reg(1), SIRValue::new(2u8)),
                    SIRInstruction::Binary(reg(2), reg(0), BinaryOp::LtU, reg(1)),
                ],
                SIRTerminator::Branch {
                    cond: reg(2),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            ),
            block(
                1,
                vec![
                    SIRInstruction::Imm(reg(3), SIRValue::new(1u8)),
                    store(slot(2), 3),
                ],
                SIRTerminator::Return,
            ),
            block(
                2,
                vec![
                    load(4, slot(1)),
                    SIRInstruction::Imm(reg(5), SIRValue::new(12u8)),
                    SIRInstruction::Binary(reg(6), reg(4), BinaryOp::Add, reg(5)),
                    SIRInstruction::Imm(reg(7), SIRValue::new(1u8)),
                    SIRInstruction::Binary(reg(8), reg(0), BinaryOp::Sub, reg(7)),
                    call(6, vec![(0, 8), (1, 6)], 9),
                    SIRInstruction::Binary(reg(10), reg(0), BinaryOp::Mul, reg(9)),
                    store(slot(2), 10),
                ],
                SIRTerminator::Return,
            ),
        ]
        .into_iter()
        .collect(),
        register_map: (0..=10)
            .map(|id| (reg(id), bits(if id == 2 { 1 } else { 32 })))
            .collect(),
    };
    sir.functions.push(FunctionUnit {
        unit,
        frame: FrameLayout {
            size: 12,
            slots: (0..3)
                .map(|index| FrameSlot {
                    addr: slot(index),
                    offset: index as usize * 4,
                    width: 32,
                    four_state: false,
                    handle: false,
                    element_width: None,
                })
                .collect(),
        },
        effects: Default::default(),
    });
    sir.eval_comb.push(ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [block(
            0,
            vec![
                load(0, input),
                SIRInstruction::Imm(reg(1), SIRValue::new(0u8)),
                call(1, vec![(0, 0), (1, 1)], 2),
                store(output, 2),
            ],
            SIRTerminator::Return,
        )]
        .into_iter()
        .collect(),
        register_map: (0..=2).map(|id| (reg(id), bits(32))).collect(),
    });
    sir.summarize_calls();
}

#[test]
fn native_runs_function_units() {
    let mut sim = SimulatorBuilder::<Simulator>::new(FACTORIAL, "Top")
        .transform_sir(call_factorial)
        .build_native()
        .unwrap();
    let a = sim.signal("a");
    let y = sim.signal("y");
    for (n, factorial) in [(1u32, 1u32), (5, 120), (10, 3_628_800)] {
        sim.modify(|io| io.set(a, n)).unwrap();
        assert_eq!(sim.get_as::<u32>(y), factorial, "fact({n})");
    }
}

#[test_case::test_case(false; "optimizing")]
#[test_case::test_case(true; "baseline")]
fn promotes_a_program_with_function_units(baseline: bool) {
    let gate = Gate::closed();
    let worker_gate = gate.0.clone();
    let mut sim: Simulator<TieredBackend> = SimulatorBuilder::<Simulator>::new(FACTORIAL, "Top")
        .transform_sir(call_factorial)
        .build_tiered_with_compiler(move |laid_out, options, cancel| {
            wait_for_gate_or_cancel(&worker_gate, cancel)?;
            let mut options = options.clone();
            options.x86_options.baseline = baseline;
            options.x86_options.diagnostics.verify_mir_passes = true;
            options.x86_options.diagnostics.verify_regalloc = true;
            let image = NativeBackend::compile_image(laid_out, &options)?;
            let shared = unsafe { SharedNativeCode::from_image(image)? };
            Ok(CompiledCode::Native(Arc::new(shared)))
        })
        .unwrap();
    let a = sim.signal("a");
    let y = sim.signal("y");
    // Interpreted...
    sim.modify(|io| io.set(a, 5u32)).unwrap();
    assert_eq!(sim.get_as::<u32>(y), 120);
    assert!(!sim.is_compiled());
    // ...promoted at a settle, where no call runs...
    gate.open();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut n = 0u32;
    while !sim.is_compiled() && std::time::Instant::now() < deadline {
        n = (n + 1) % 8;
        sim.modify(|io| io.set(a, n)).unwrap();
        sim.get_as::<u32>(y);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(sim.is_compiled(), "{:?}", sim.backend.promotion_error());
    // ...and compiled, on the state image the interpreter left.
    for (n, factorial) in [(6u32, 720u32), (1, 1), (12, 479_001_600)] {
        sim.modify(|io| io.set(a, n)).unwrap();
        assert_eq!(sim.get_as::<u32>(y), factorial, "fact({n})");
    }
}
