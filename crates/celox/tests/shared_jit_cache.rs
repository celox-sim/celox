#![cfg(any(
    target_arch = "x86_64",
    all(target_arch = "aarch64", feature = "experimental-arm64-backend")
))]

use std::sync::Arc;

use celox::native_backend::NativeImageContainerError;
use celox::{
    NativeBackend, NativeProgramInstance, NativeProgramLoadError, SharedNativeCode,
    SignalDirection, SimBackend, Simulator,
};

const ADDER: &str = r#"
    module Top (
        a: input logic<8>,
        b: input logic<8>,
        sum: output logic<8>,
    ) {
        assign sum = a + b;
    }
"#;

const FF: &str = r#"
    module Top (
        i_clk: input  clock,
        i_rst: input  reset,
        d:     input  logic<8>,
        q:     output logic<8>,
    ) {
        always_ff (i_clk, i_rst) {
            if_reset {
                q = 0;
            } else {
                q = d;
            }
        }
    }
"#;

const HIERARCHICAL: &str = r#"
    module Child (
        i: input signed logic<8>,
        o: output logic<8>,
    ) {
        var state: logic<8>;
        always_comb {
            state = i;
            o = state;
        }
    }

    module Top (
        a: input signed logic<8>,
        y: output logic<8>,
    ) {
        inst u_child: Child (
            i: a,
            o: y,
        );
    }
"#;

#[cfg(target_arch = "x86_64")]
type CopiedNativeFunc = unsafe extern "sysv64" fn(*mut u8) -> i64;
#[cfg(target_arch = "aarch64")]
type CopiedNativeFunc = unsafe extern "C" fn(*mut u8) -> i64;

#[test]
fn shared_code_packs_every_function_into_one_image() {
    let sim = Simulator::builder(FF, "Top").build().unwrap();
    let shared = sim.shared_code();
    let entries = shared.code_entries();
    let image = shared.code_image();

    assert!(entries.len() > 1, "FF design should emit multiple entries");
    assert_eq!(entries[0].name, "eval_comb");
    for (index, entry) in entries.iter().enumerate() {
        assert_eq!(entry.offset % 16, 0, "unaligned entry {entry:?}");
        assert!(entry.size > 0, "empty entry {entry:?}");
        assert!(entry.offset + entry.size <= image.len());
        if let Some(next) = entries.get(index + 1) {
            assert!(entry.offset + entry.size <= next.offset);
        }
    }
}

#[test]
fn copied_native_image_executes_from_recorded_entry_offset() {
    let sim = Simulator::builder(ADDER, "Top").build().unwrap();
    let shared = sim.shared_code();
    let eval_comb = shared
        .code_entries()
        .iter()
        .find(|entry| entry.name == "eval_comb")
        .unwrap();
    let copied = celox::native_backend::jit_mem::JitCode::new(shared.code_image()).unwrap();
    let entry_ptr = copied.entry_ptr(eval_comb.offset).unwrap();
    // Safety: the entry metadata and image were produced together, and this
    // test preserves the complete image while resolving the copied base.
    let eval_comb = unsafe { std::mem::transmute::<*const u8, CopiedNativeFunc>(entry_ptr) };

    let a = sim.signal("a");
    let b = sim.signal("b");
    let sum = sim.signal("sum");
    let mut backend = NativeBackend::from_shared(shared);
    backend.set(a, 19u8);
    backend.set(b, 23u8);
    let (state, _) = backend.memory_as_mut_ptr();
    assert_eq!(unsafe { eval_comb(state) }, 0);
    assert_eq!(backend.get_as::<u8>(sum), 42);
}

#[test]
fn precompiled_runtime_reattaches_pointer_free_program_image() {
    let sim = Simulator::builder(FF, "Top").build().unwrap();
    let image = sim.shared_code().program_image().clone();
    let reloaded = Arc::new(SharedNativeCode::from_image(image).unwrap());
    let mut backend = NativeBackend::from_shared(reloaded);
    let clock = backend.id_to_event_slice()[0];
    let reset = sim.signal("i_rst");
    let data = sim.signal("d");
    let output = sim.signal("q");

    backend.set(reset, 1u8);
    backend.set(data, 77u8);
    backend.eval_comb().unwrap();
    backend.eval_apply_ff_at(clock).unwrap();
    backend.eval_comb().unwrap();
    assert_eq!(backend.get_as::<u8>(output), 77);
}

#[test]
fn native_program_image_round_trips_through_appended_runtime() {
    const RUNTIME_PREFIX: &[u8] = b"\x7fELFprecompiled-celox-runtime";

    let sim = Simulator::builder(FF, "Top").build().unwrap();
    let encoded = sim
        .shared_code()
        .program_image()
        .append_to_runtime(RUNTIME_PREFIX)
        .unwrap();
    assert_eq!(&encoded[..RUNTIME_PREFIX.len()], RUNTIME_PREFIX);

    let appended = celox::NativeProgramImage::discover_appended(&encoded)
        .unwrap()
        .unwrap();
    assert_eq!(appended.runtime_len, RUNTIME_PREFIX.len());
    assert_eq!(appended.image.code_image(), sim.shared_code().code_image());
    assert_eq!(
        appended.image.code_entries(),
        sim.shared_code().code_entries()
    );
    assert_eq!(
        appended.image.reflection(),
        sim.shared_code().program_image().reflection()
    );

    let reloaded = Arc::new(SharedNativeCode::from_image(appended.image).unwrap());
    let mut backend = NativeBackend::from_shared(reloaded);
    let clock = backend.id_to_event_slice()[0];
    let reset = sim.signal("i_rst");
    let data = sim.signal("d");
    let output = sim.signal("q");
    backend.set(reset, 1u8);
    backend.set(data, 91u8);
    backend.eval_comb().unwrap();
    backend.eval_apply_ff_at(clock).unwrap();
    backend.eval_comb().unwrap();
    assert_eq!(backend.get_as::<u8>(output), 91);
}

#[test]
fn native_program_image_carries_source_independent_design_reflection() {
    let sim = Simulator::builder(HIERARCHICAL, "Top").build().unwrap();
    let compiled = sim.shared_code();
    let image = compiled.program_image();
    let reflection = image.reflection();

    let (_, root) = reflection.scope_by_name("Top").unwrap();
    assert_eq!(root.name, "Top");
    assert_eq!(root.module_name, "Top");
    assert!(root.parent.is_none());
    let (_, child) = reflection.scope_by_name("Top.u_child[0]").unwrap();
    assert_eq!(child.name, "u_child[0]");
    assert_eq!(child.module_name, "Child");
    assert_eq!(
        child.parent.unwrap(),
        reflection.scope_by_name("Top").unwrap().0
    );

    let (_, input) = reflection.signal_by_name("Top.a").unwrap();
    assert_eq!(input.direction, SignalDirection::Input);
    assert!(input.signed);
    assert_eq!(input.signal.width, 8);
    let (_, child_input) = reflection.signal_by_name("Top.u_child[0].i").unwrap();
    assert_eq!(child_input.direction, SignalDirection::Input);
    assert!(child_input.signed);
    let (_, child_state) = reflection.signal_by_name("Top.u_child[0].state").unwrap();
    assert_eq!(child_state.direction, SignalDirection::Internal);

    let input = input.signal;
    let output = reflection.signal_by_name("Top.y").unwrap().1.signal;
    let shared = Arc::new(SharedNativeCode::from_image(image.clone()).unwrap());
    let mut backend = NativeBackend::from_shared(shared);
    backend.set(input, 0xa5u8);
    backend.eval_comb().unwrap();
    assert_eq!(backend.get_as::<u8>(output), 0xa5);
}

#[test]
fn precompiled_runtime_instance_loads_and_runs_attached_bytes_without_source() {
    let sim = Simulator::builder(ADDER, "Top").build().unwrap();
    let attached = sim
        .shared_code()
        .program_image()
        .append_to_runtime(b"precompiled runtime")
        .unwrap();
    drop(sim);

    let mut runtime = NativeProgramInstance::from_attached_bytes(&attached).unwrap();
    let a = runtime.signal_ref("Top.a").unwrap();
    let b = runtime.signal_ref("Top.b").unwrap();
    let sum = runtime.signal_ref("Top.sum").unwrap();
    runtime.backend_mut().set(a, 100u8);
    runtime.backend_mut().set(b, 27u8);
    runtime.eval_comb().unwrap();
    assert_eq!(runtime.backend().get_as::<u8>(sum), 127);

    assert!(matches!(
        NativeProgramInstance::from_attached_bytes(b"plain runtime"),
        Err(NativeProgramLoadError::MissingImage)
    ));
}

#[test]
fn precompiled_runtime_restores_initial_state_and_settles_outputs() {
    let directory = tempfile::tempdir().unwrap();
    let memory_path = directory.path().join("initial.hex");
    std::fs::write(&memory_path, "2a\n00\n").unwrap();
    let source = format!(
        r#"
            module Top (out: output logic<8>) {{
                var mem: logic<8>[2];
                initial {{
                    $readmemh("{}", mem);
                }}
                assign out = mem[0] + 8'd1;
            }}
        "#,
        memory_path.display()
    );
    let sim = Simulator::builder(&source, "Top").build().unwrap();
    let image = sim.shared_code().program_image().clone();
    drop(sim);

    let runtime = NativeProgramInstance::from_image(image).unwrap();
    let output = runtime.signal_ref("Top.out").unwrap();
    assert_eq!(runtime.backend().get_as::<u8>(output), 0x2b);
}

#[test]
fn precompiled_runtime_preserves_comb_runtime_events() {
    let sim = Simulator::builder(
        r#"
            module Top (a: input logic<8>, y: output logic<8>) {
                always_comb {
                    y = a + 8'd1;
                    $display("y=%0d", y);
                }
            }
        "#,
        "Top",
    )
    .build()
    .unwrap();
    let image = sim.shared_code().program_image().clone();
    drop(sim);

    let mut runtime = NativeProgramInstance::from_image(image).unwrap();
    assert_eq!(
        runtime.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "y=1".to_string(),
        }]
    );

    let input = runtime.signal_ref("Top.a").unwrap();
    runtime.backend_mut().set(input, 6u8);
    runtime.settle_active_edges(&[]).unwrap();
    assert_eq!(
        runtime.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "y=7".to_string(),
        }]
    );
}

#[test]
fn precompiled_runtime_reports_fatal_assertions() {
    let sim = Simulator::builder(
        r#"
            module Top (a: input logic<8>) {
                always_comb {
                    $assert(a != 8'd1, "fatal a=%0d", a);
                }
            }
        "#,
        "Top",
    )
    .build()
    .unwrap();
    let image = sim.shared_code().program_image().clone();
    drop(sim);

    let mut runtime = NativeProgramInstance::from_image(image).unwrap();
    runtime.drain_runtime_events();
    let input = runtime.signal_ref("Top.a").unwrap();
    runtime.backend_mut().set(input, 1u8);
    let error = runtime.settle_active_edges(&[]).unwrap_err();
    assert!(error.to_string().contains("fatal a=1"));
    assert_eq!(
        runtime.drain_runtime_events(),
        vec![celox::RuntimeEvent::AssertFatal {
            message: "fatal a=1".to_string(),
        }]
    );
}

#[test]
fn native_program_image_rejects_corrupt_appended_payload() {
    let sim = Simulator::builder(ADDER, "Top").build().unwrap();
    let mut encoded = sim
        .shared_code()
        .program_image()
        .append_to_runtime(b"runtime")
        .unwrap();
    encoded["runtime".len()] ^= 0x80;

    assert!(matches!(
        celox::NativeProgramImage::discover_appended(&encoded),
        Err(NativeImageContainerError::ChecksumMismatch)
    ));
    assert!(
        celox::NativeProgramImage::discover_appended(b"ordinary runtime")
            .unwrap()
            .is_none()
    );
}

#[test]
fn native_program_image_writes_an_executable_runtime_file() {
    let sim = Simulator::builder(ADDER, "Top").build().unwrap();
    let shared = sim.shared_code();
    let image = shared.program_image();
    let directory = tempfile::tempdir().unwrap();
    let runtime_path = directory.path().join("celox-runtime");
    let output_path = directory.path().join("compiled-design");
    std::fs::write(&runtime_path, b"precompiled runtime").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&runtime_path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&runtime_path, permissions).unwrap();
    }

    image
        .write_attached_runtime(&runtime_path, &output_path)
        .unwrap();
    // Replacing an existing attached image must strip the old trailer rather
    // than nesting containers indefinitely.
    image
        .write_attached_runtime(&output_path, &output_path)
        .unwrap();

    let output = std::fs::read(&output_path).unwrap();
    let appended = celox::NativeProgramImage::discover_appended(&output)
        .unwrap()
        .unwrap();
    assert_eq!(appended.runtime_len, b"precompiled runtime".len());
    assert_eq!(&output[..appended.runtime_len], b"precompiled runtime");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            std::fs::metadata(&output_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
    }
}

/// Two backends from the same SharedNativeCode produce correct, independent results.
#[test]
fn shared_code_produces_independent_instances() {
    let sim = Simulator::builder(ADDER, "Top").build().unwrap();
    let shared = sim.shared_code();

    let a = sim.signal("a");
    let b = sim.signal("b");
    let sum = sim.signal("sum");

    let mut b1 = NativeBackend::from_shared(Arc::clone(&shared));
    let mut b2 = NativeBackend::from_shared(shared);

    b1.set(a, 10u8);
    b1.set(b, 20u8);
    b1.eval_comb().unwrap();
    assert_eq!(b1.get_as::<u8>(sum), 30);

    b2.set(a, 100u8);
    b2.set(b, 55u8);
    b2.eval_comb().unwrap();
    assert_eq!(b2.get_as::<u8>(sum), 155);

    // b1 is unaffected by b2.
    assert_eq!(b1.get_as::<u8>(sum), 30);
}

/// Sequential (FF) logic: EventRef from one Simulator works on a
/// from_shared backend (both share the same compiled function pointers).
#[test]
fn shared_code_sequential_logic() {
    let mut sim1 = Simulator::builder(FF, "Top").build().unwrap();
    let shared = sim1.shared_code();

    let d = sim1.signal("d");
    let q = sim1.signal("q");
    let clk_event = sim1.event("i_clk");

    // Drive sim1: reset → d=42 → tick
    sim1.set(sim1.signal("i_rst"), 0u8);
    sim1.tick(clk_event).unwrap();
    assert_eq!(sim1.get(q), 0u32.into());
    sim1.set(sim1.signal("i_rst"), 1u8);
    sim1.set(d, 42u8);
    sim1.tick(clk_event).unwrap();
    assert_eq!(sim1.get(q), 42u32.into());

    // Build a second Simulator from the SAME source
    let mut sim2 = Simulator::builder(FF, "Top").build().unwrap();
    sim2.set(sim2.signal("i_rst"), 0u8);
    sim2.tick(sim2.event("i_clk")).unwrap();
    sim2.set(sim2.signal("i_rst"), 1u8);
    sim2.set(d, 99u8);
    sim2.tick(sim2.event("i_clk")).unwrap();
    assert_eq!(sim2.get(q), 99u32.into());

    // sim1 is still 42.
    assert_eq!(sim1.get(q), 42u32.into());

    // Verify layouts are identical between shared codes.
    let shared2 = sim2.shared_code();
    assert_eq!(shared.layout().total_size, shared2.layout().total_size);
    assert_eq!(
        shared.layout().merged_total_size,
        shared2.layout().merged_total_size
    );
}

/// Memory isolation: writing to one backend does not affect another.
#[test]
fn shared_code_memory_isolation() {
    let sim = Simulator::builder(ADDER, "Top").build().unwrap();
    let shared = sim.shared_code();
    let a = sim.signal("a");

    let mut b1 = NativeBackend::from_shared(Arc::clone(&shared));
    let mut b2 = NativeBackend::from_shared(shared);

    b1.set(a, 0xAAu8);
    b2.set(a, 0x55u8);

    assert_eq!(b1.get_as::<u8>(a), 0xAA);
    assert_eq!(b2.get_as::<u8>(a), 0x55);
}

/// from_shared produces a backend with the same layout sizes as the original.
#[test]
fn shared_code_layout_consistency() {
    let sim = Simulator::builder(ADDER, "Top").build().unwrap();
    let shared = sim.shared_code();

    let original_stable = sim.stable_region_size();
    let (_, original_total) = sim.memory_as_ptr();

    let backend = NativeBackend::from_shared(shared);
    assert_eq!(backend.stable_region_size(), original_stable);
    let (_, new_total) = backend.memory_as_ptr();
    assert_eq!(new_total, original_total);
}

/// Concurrent threads sharing one SharedNativeCode produce correct, independent results.
#[test]
fn shared_code_concurrent_comb() {
    let sim = Simulator::builder(ADDER, "Top").build().unwrap();
    let shared = sim.shared_code();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let sum = sim.signal("sum");

    let threads: Vec<_> = (0..8)
        .map(|i| {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                let mut backend = NativeBackend::from_shared(shared);
                let va = (i * 10) as u8;
                let vb = (i * 3) as u8;
                backend.set(a, va);
                backend.set(b, vb);
                backend.eval_comb().unwrap();
                let result: u8 = backend.get_as(sum);
                assert_eq!(result, va.wrapping_add(vb), "thread {i}");
            })
        })
        .collect();

    for t in threads {
        t.join().unwrap();
    }
}

/// Concurrent threads running sequential (FF) logic on shared code.
#[test]
fn shared_code_concurrent_ff() {
    let sim = Simulator::builder(FF, "Top").build().unwrap();
    let shared = sim.shared_code();
    let d = sim.signal("d");
    let q = sim.signal("q");
    let rst = sim.signal("i_rst");
    let clk_event = sim.event("i_clk");

    let threads: Vec<_> = (0..8)
        .map(|i| {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                let mut backend = NativeBackend::from_shared(shared);
                // Reset (AsyncLow: active at 0)
                backend.set(rst, 0u8);
                backend.eval_comb().unwrap();
                backend.eval_apply_ff_at(clk_event).unwrap();
                backend.eval_comb().unwrap();
                assert_eq!(backend.get_as::<u8>(q), 0, "thread {i} after reset");
                // Deactivate reset, drive data
                backend.set(rst, 1u8);
                let val = (i * 17 + 3) as u8;
                backend.set(d, val);
                backend.eval_comb().unwrap();
                backend.eval_apply_ff_at(clk_event).unwrap();
                backend.eval_comb().unwrap();
                assert_eq!(backend.get_as::<u8>(q), val, "thread {i} after tick");
            })
        })
        .collect();

    for t in threads {
        t.join().unwrap();
    }
}

/// Stress test: many threads repeatedly ticking shared FF code.
#[test]
fn shared_code_concurrent_stress() {
    let sim = Simulator::builder(FF, "Top").build().unwrap();
    let shared = sim.shared_code();
    let d = sim.signal("d");
    let q = sim.signal("q");
    let rst = sim.signal("i_rst");
    let clk_event = sim.event("i_clk");

    let threads: Vec<_> = (0..4)
        .map(|i| {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                let mut backend = NativeBackend::from_shared(shared);
                // Reset
                backend.set(rst, 0u8);
                backend.eval_comb().unwrap();
                backend.eval_apply_ff_at(clk_event).unwrap();
                backend.eval_comb().unwrap();
                backend.set(rst, 1u8);
                // Tick 100 times
                for cycle in 0u8..100 {
                    let val = cycle.wrapping_add(i as u8 * 50);
                    backend.set(d, val);
                    backend.eval_comb().unwrap();
                    backend.eval_apply_ff_at(clk_event).unwrap();
                    backend.eval_comb().unwrap();
                    assert_eq!(backend.get_as::<u8>(q), val, "thread {i} cycle {cycle}");
                }
            })
        })
        .collect();

    for t in threads {
        t.join().unwrap();
    }
}
