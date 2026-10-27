//! Ownership tests which need the host queue and raw backend drain boundary.
use super::*;

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn raw_thread_deferred_string_is_resolved_before_core_heap_restore() {
    let source = r#"module Top;
        string message;
        initial begin
            message="retained";
            assert #0 (1) $display("%s", message);
            #1;
        end
    endmodule"#;
    let simulation = crate::Simulation::from_sv_sources(
        vec![(source, std::path::Path::new("raw_deferred.sv"))],
        "Top",
    )
    .build_native()
    .unwrap();
    let mut sim = simulation.simulator;
    let checkpoint = sim.checkpoint().unwrap();
    let kernel = sim
        .schedule
        .processes
        .iter()
        .position(|process| process.slots.kind == celox_design::ProcessKind::Regular)
        .unwrap();
    let mut drain = sim.runtime_event_drain().unwrap();
    sim.enter_deferred_process(7);
    sim.run_process_checked(kernel).unwrap();
    // Deliberately consume the ring on another thread before the owning
    // thread can stash/resolve the deferred action at leave.
    let mut drain = std::thread::spawn(move || {
        assert!(drain.drain().is_empty());
        drain
    })
    .join()
    .unwrap();
    {
        let pending = sim.pending_host_events.lock().unwrap();
        assert_eq!(pending.deferred.waiting.len(), 1);
        let RawRuntimeEvent::Event { args, .. } = &pending.deferred.waiting[0].event else {
            panic!("deferred event");
        };
        assert!(args.iter().any(|arg| arg.handle));
    }
    sim.leave_deferred_process();
    {
        let pending = sim.pending_host_events.lock().unwrap();
        let RawRuntimeEvent::Event { args, .. } = &pending.deferred.waiting[0].event else {
            panic!("deferred event");
        };
        assert!(args.iter().all(|arg| !arg.handle));
    }
    sim.restore(&checkpoint).unwrap();
    // Reusing a restored slot cannot flush an earlier host-side report.
    sim.enter_deferred_process(7);
    {
        let mut pending = sim.pending_host_events.lock().unwrap();
        let queue = pending.deferred.waiting[0].queue;
        pending.flush(queue);
        assert_eq!(pending.deferred.waiting.len(), 1);
    }
    sim.leave_deferred_process();
    sim.mature_deferred_assertions().unwrap();
    assert!(
        matches!(drain.drain().as_slice(), [RuntimeEvent::Display { message }] if message == b"retained")
    );
}

#[test]
fn deferred_process_context_is_cleared_after_kernel_error() {
    let source = r#"module Top;
        initial fork begin
            assert #0 (1) $display("pending");
            $fatal(1, "kernel error");
        end join
    endmodule"#;
    let mut simulation = crate::Simulation::from_sv_sources(
        vec![(source, std::path::Path::new("deferred_error.sv"))],
        "Top",
    )
    .build_interpreter()
    .unwrap();
    let error = simulation.run_until(0).unwrap_err();
    assert!(error.to_string().contains("kernel error"), "{error}");
    assert!(
        simulation
            .simulator
            .pending_host_events
            .lock()
            .unwrap()
            .deferred_owners
            .current
            .is_none()
    );
}

#[test]
fn ordinary_deferred_queue_is_not_flushed_by_a_child_context() {
    let mut pending = PendingRuntimeEvents::default();
    pending.deferred.sites = vec![
        Some((celox_design::DeferredSite::FlushPoint, false)),
        Some((
            celox_design::DeferredSite::Action {
                queue: 0,
                region: celox_design::AssertionDeferral::Observed,
            },
            false,
        )),
    ];
    pending.push(
        RawRuntimeEvent::Event {
            site_id: 1,
            args: Vec::new(),
        },
        100,
    );
    pending.enter_deferred_process(3);
    pending.push(
        RawRuntimeEvent::Event {
            site_id: 1,
            args: Vec::new(),
        },
        100,
    );
    pending.flush(0);
    assert_eq!(pending.deferred.waiting.len(), 1);
    pending.deferred_owners.current = None;
    pending.flush(0);
    assert!(pending.deferred.waiting.is_empty());
}

#[test]
fn ordinary_flush_preserves_ambiguous_fatal_loss_until_its_region() {
    let mut pending = PendingRuntimeEvents::default();
    pending.deferred.sites = vec![Some((
        celox_design::DeferredSite::Action {
            queue: 7,
            region: celox_design::AssertionDeferral::Final,
        },
        true,
    ))];
    pending.push(RawRuntimeEvent::Missed { count: 3 }, 100);
    // The ordinary owner can cover several unrelated comb/FF queues. A
    // flush has no evidence that these lost records belonged to its queue.
    pending.flush(0);
    assert!(
        pending
            .mature(celox_design::AssertionDeferral::Observed, 100)
            .is_none()
    );
    assert!(matches!(
        pending.mature(celox_design::AssertionDeferral::Final, 100),
        Some(RawRuntimeEvent::Missed { count: 3 })
    ));
}

#[test]
fn mirrored_deferred_queues_keep_their_execution_owner_mapping() {
    let source = "module Top; initial begin assert #0 (1) $display(\"pending\"); #1; end endmodule";
    let build = || {
        crate::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("mirror_deferred.sv"))],
            "Top",
        )
        .build_interpreter()
        .unwrap()
        .simulator
    };
    let mut original = build();
    let mut mirrored = build();
    let kernel = original
        .schedule
        .processes
        .iter()
        .position(|process| process.slots.kind == celox_design::ProcessKind::Regular)
        .unwrap();
    original.enter_deferred_process(7);
    original.run_process_checked(kernel).unwrap();
    original.leave_deferred_process();
    for process in [3, 7] {
        mirrored.enter_deferred_process(process);
        mirrored.leave_deferred_process();
    }
    mirrored.mirror(&mut original);
    mirrored.enter_deferred_process(7);
    {
        let mut pending = mirrored.pending_host_events.lock().unwrap();
        assert_eq!(pending.deferred.waiting.len(), 1);
        let queue = pending.deferred.waiting[0].queue;
        pending.flush(queue);
        assert!(pending.deferred.waiting.is_empty());
    }
    mirrored.leave_deferred_process();
    assert_eq!(
        original
            .pending_host_events
            .lock()
            .unwrap()
            .deferred
            .waiting
            .len(),
        1
    );
}

#[test]
fn failed_portable_load_restores_deferred_owner_context() {
    let source = r#"module Top(input bit a, output bit y);
        logic [1:0] v;
        assign v[0] = ~v[1] & a;
        assign v[1] = v[0];
        assign y = v[0];
        always_comb assert #0 (1) $display("ordinary");
    endmodule"#;
    let loop_net = (vec![], vec!["v".to_owned()]);
    let mut sim = Simulator::from_sv_sources(
        vec![(source, std::path::Path::new("failed_load_deferred.sv"))],
        "Top",
    )
    .true_loop(loop_net.clone(), loop_net, 10)
    .build_interpreter()
    .unwrap();
    let mut file = sim.save_state().unwrap();
    let input = file
        .objects
        .iter_mut()
        .find(|object| object.path == "a")
        .unwrap();
    input.value = vec![1];
    sim.enter_deferred_process(7);
    sim.leave_deferred_process();
    let before = sim.deferred_owner_context();
    assert!(!before.processes.is_empty());
    assert!(matches!(
        sim.load_state(&file),
        Err(crate::StateError::Runtime(_))
    ));
    let after = sim.deferred_owner_context();
    assert_eq!(before.processes, after.processes);
    assert_eq!(before.ordinary, after.ordinary);
    assert_eq!(before.current, after.current);
}
