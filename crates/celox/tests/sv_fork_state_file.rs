#![cfg(feature = "systemverilog")]
//! Portable state files are a Celox API, not a language conformance fixture.
use celox::{SimBackend, Simulation, StateFile};
use celox_runtime::ProcessRef;
use std::path::Path;

const SOURCE: &str = r#"`timescale 1ns/1ns
module Top(output int q);
 initial begin
   q=0;
   fork
     begin #2; q=q+1; end
     begin #4; q=q+2; end
   join
   q=q+4;
 end
endmodule"#;
fn bytes(file: &StateFile) -> StateFile {
    let mut bytes = Vec::new();
    file.write_to(&mut bytes).unwrap();
    StateFile::read_from(bytes.as_slice()).unwrap()
}
fn finish<B: SimBackend>(sim: &mut Simulation<B>) {
    sim.run_until(3).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 1u8.into());
    sim.run_until(5).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 7u8.into());
}
fn saved() -> StateFile {
    let mut sim = Simulation::from_sv_sources(vec![(SOURCE, Path::new("fork.sv"))], "Top")
        .build_interpreter()
        .unwrap();
    sim.run_until(1).unwrap();
    bytes(&sim.save_state().unwrap())
}
#[test]
fn live_children_cross_backend() {
    let file = saved();
    let mut native = Simulation::from_sv_sources(vec![(SOURCE, Path::new("fork.sv"))], "Top")
        .build_native()
        .unwrap();
    native.load_state(&file).unwrap();
    finish(&mut native);
    native.load_state(&file).unwrap();
    finish(&mut native);
    #[cfg(feature = "cranelift")]
    {
        let mut target = Simulation::from_sv_sources(vec![(SOURCE, Path::new("fork.sv"))], "Top")
            .build_cranelift()
            .unwrap();
        target.load_state(&file).unwrap();
        finish(&mut target);
    }
    #[cfg(feature = "wasm-runtime")]
    {
        let mut target = Simulation::from_sv_sources(vec![(SOURCE, Path::new("fork.sv"))], "Top")
            .build_wasm()
            .unwrap();
        target.load_state(&file).unwrap();
        finish(&mut target);
    }
}

#[test]
fn sparse_shuffled_file_ids_are_not_runtime_indices() {
    let mut file = saved();
    let p = &mut file.schedule.as_mut().unwrap().processes;
    let remap = |id: u64| 1000 + id * 31;
    for a in &mut p.activations {
        a.id = remap(a.id);
        a.parent = a.parent.map(remap);
        for id in &mut a.pending {
            *id = remap(*id);
        }
        if let Some((_, ids)) = &mut a.join {
            for id in ids {
                *id = remap(*id);
            }
        }
    }
    for id in p
        .wakeups
        .iter_mut()
        .map(|(id, _)| id)
        .chain(p.waiters.iter_mut().map(|w| &mut w.process))
        .chain(&mut p.ready)
        .chain(&mut p.inactive)
        .chain(&mut p.reactive)
        .chain(&mut p.re_inactive)
    {
        if let ProcessRef::Activation(value) = id {
            *value = remap(*value);
        }
    }
    p.activations.reverse();
    let mut target = Simulation::from_sv_sources(vec![(SOURCE, Path::new("fork.sv"))], "Top")
        .build_native()
        .unwrap();
    target.load_state(&bytes(&file)).unwrap();
    finish(&mut target);
}

#[test]
fn malformed_graphs_and_controls_leave_live_target_untouched() {
    let original = saved();
    let mut bad = Vec::new();
    for mutation in 0..8 {
        let mut file = original.clone();
        let p = &mut file.schedule.as_mut().unwrap().processes;
        let child = p
            .activations
            .iter()
            .position(|a| a.parent.is_some())
            .unwrap();
        match mutation {
            0 => p.activations[child].id = p.activations[0].id,
            1 => p.activations[child].parent = Some(p.activations[child].id),
            2 => p.activations[child].kernel = "foreign".into(),
            3 => {
                p.activations[child].private.pop();
            }
            4 => {
                p.activations[child].root = true;
            }
            5 => {
                p.activations[child]
                    .private
                    .iter_mut()
                    .find(|(name, _)| name.ends_with("resume"))
                    .unwrap()
                    .1
                    .value
                    .fill(255);
            }
            6 => {
                let id = p.wakeups[0].0.clone();
                p.reactive.push(id);
            }
            7 => {
                p.activations[child]
                    .private
                    .iter_mut()
                    .find(|(name, _)| name.ends_with("status"))
                    .unwrap()
                    .1
                    .value
                    .fill(255);
            }
            _ => unreachable!(),
        }
        bad.push(file);
    }
    let mut target = Simulation::from_sv_sources(vec![(SOURCE, Path::new("fork.sv"))], "Top")
        .build_native()
        .unwrap();
    target.load_state(&original).unwrap();
    for file in bad {
        assert!(target.load_state(&file).is_err());
        assert_eq!(target.time(), 1);
        assert_eq!(target.get(target.signal("q")), 0u8.into());
    }
    finish(&mut target);
}

#[test]
fn dormant_child_string_and_completed_parent_survive_restore_and_slot_reuse() {
    let source = r#"`timescale 1ns/1ns
module Top(output int q);
 initial begin
   q=0;
   for (int i=0; i<2; i++) begin
     automatic string text;
     text.itoa(i);
     text = {"child-", text};
     fork
       begin
         #3;
         if (text.getc(6) == 48) q=q+1;
         if (text.getc(6) == 49) q=q+4;
         fork begin #2; q=q+1; end join
       end
     join_none
   end
   #1;
 end
 initial begin
   string garbage;
   #2;
   for (int j=0; j<1100; j++) garbage.itoa(j);
 end
endmodule"#;
    let mut source_sim =
        Simulation::from_sv_sources(vec![(source, Path::new("strings.sv"))], "Top")
            .build_interpreter()
            .unwrap();
    source_sim.run_until(1).unwrap();
    let file = bytes(&source_sim.save_state().unwrap());
    assert!(file.strings.iter().any(|s| s == b"child-0"));
    let mut target = Simulation::from_sv_sources(vec![(source, Path::new("strings.sv"))], "Top")
        .build_native()
        .unwrap();
    for _ in 0..3 {
        target.load_state(&file).unwrap();
        target.run_until(6).unwrap();
        assert_eq!(target.get(target.signal("q")), 7u8.into());
    }
}

#[test]
fn shared_local_event_wait_and_cancellation_resume() {
    let source = r#"`timescale 1ns/1ns
module Top(output int q);
 initial begin
   automatic int trigger_value=0;
   q=0;
   fork
     begin @(trigger_value); q=q+1; end
     begin #3; trigger_value=1; end
     begin #20; q=99; end
   join_any
   #1;
   disable fork;
   wait fork;
   q=q+2;
 end
endmodule"#;
    let mut source_sim = Simulation::from_sv_sources(vec![(source, Path::new("wait.sv"))], "Top")
        .build_interpreter()
        .unwrap();
    source_sim.run_until(1).unwrap();
    let file = bytes(&source_sim.save_state().unwrap());
    let mut target = Simulation::from_sv_sources(vec![(source, Path::new("wait.sv"))], "Top")
        .build_native()
        .unwrap();
    target.load_state(&file).unwrap();
    target.run_until(30).unwrap();
    assert_eq!(target.get(target.signal("q")), 3u8.into());
}

#[test]
fn heap_queue_payloads_are_rejected_before_mutation() {
    let source = r#"`timescale 1ns/1ns
module Top(output int q);
 string text;
 initial begin text="saved"; fork begin #5; q=1; end join end
endmodule"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("heap.sv"))], "Top")
        .build_native()
        .unwrap();
    sim.run_until(1).unwrap();
    let original = bytes(&sim.save_state().unwrap());
    let path = original
        .objects
        .iter()
        .find(|o| o.path == "text")
        .unwrap()
        .path
        .clone();
    for region in 0..4 {
        let mut file = original.clone();
        let p = &mut file.schedule.as_mut().unwrap().processes;
        let update = celox_runtime::NbaRecord {
            target: path.clone(),
            lsb: 0,
            width: 32,
            value: vec![1],
            mask: vec![0],
        };
        match region {
            0 => p.nba_queue.push(update),
            1 => p.re_nba_queue.push(update),
            2 => p.host_writes.push((path.clone(), vec![1], vec![0])),
            _ => p.nba_queue.push(celox_runtime::NbaRecord {
                width: 1,
                value: vec![0],
                ..update
            }),
        }
        assert!(
            sim.load_state(&file)
                .unwrap_err()
                .to_string()
                .contains("heap-bearing queued writes")
        );
        assert_eq!(sim.time(), 1);
    }
    sim.run_until(6).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 1u8.into());
}

#[test]
fn nonempty_string_wait_baseline_is_explicitly_unsupported() {
    let source = r#"`timescale 1ns/1ns
module Top(output int q);
 string text;
 initial begin
   text="waiting";
   fork begin @(text); q=1; end begin #5; text="changed"; end join
 end
endmodule"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("heap_wait.sv"))], "Top")
        .build_native()
        .unwrap();
    sim.run_until(1).unwrap();
    assert!(
        sim.save_state()
            .unwrap_err()
            .to_string()
            .contains("heap-bearing wait baselines")
    );
    sim.run_until(6).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 1u8.into());
}

#[test]
fn child_only_class_objects_remain_unsupported() {
    let source = r#"`timescale 1ns/1ns
module Top(output int q);
 class C; int value; endclass
 initial begin
   for (int i=0; i<2; i++) begin
     automatic C object;
     if (i==0) object=new;
     fork begin #3; if (object != null) q=q+1; end join_none
   end
   #1;
 end
endmodule"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("object.sv"))], "Top")
        .build_native()
        .unwrap();
    sim.run_until(1).unwrap();
    assert!(matches!(
        sim.save_state(),
        Err(celox::StateError::Objects(_))
    ));
    sim.run_until(4).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 1u8.into());
}

#[test]
fn wide_four_state_locals_and_child_random_streams_cross_to_interpreter() {
    let source = r#"`timescale 1ns/1ns
module Top(output int q, output int r);
 initial begin
   automatic logic [69:0] value=70'bx;
   fork
     begin #3; q=(value === 70'bx); end
     begin #4; r=$urandom; end
   join
 end
endmodule"#;
    let mut origin = Simulation::from_sv_sources(vec![(source, Path::new("wide.sv"))], "Top")
        .four_state(true)
        .build_native()
        .unwrap();
    origin.run_until(1).unwrap();
    let file = bytes(&origin.save_state().unwrap());
    origin.run_until(5).unwrap();
    let mut target = Simulation::from_sv_sources(vec![(source, Path::new("wide.sv"))], "Top")
        .four_state(true)
        .build_interpreter()
        .unwrap();
    target.load_state(&file).unwrap();
    target.run_until(5).unwrap();
    assert_eq!(target.get(target.signal("q")), 1u8.into());
    assert_eq!(
        target.get(target.signal("r")),
        origin.get(origin.signal("r"))
    );
}

#[test]
fn unknown_control_bits_are_rejected_even_by_two_state_destination() {
    let mut origin = Simulation::from_sv_sources(vec![(SOURCE, Path::new("fork.sv"))], "Top")
        .four_state(true)
        .build_native()
        .unwrap();
    origin.run_until(1).unwrap();
    let original = origin.save_state().unwrap();
    let mut target = Simulation::from_sv_sources(vec![(SOURCE, Path::new("fork.sv"))], "Top")
        .four_state(false)
        .build_interpreter()
        .unwrap();
    target.run_until(1).unwrap();
    for suffix in ["resume", "status"] {
        let mut file = original.clone();
        let frame = file
            .schedule
            .as_mut()
            .unwrap()
            .processes
            .activations
            .iter_mut()
            .find(|a| a.parent.is_some())
            .unwrap();
        let value = &mut frame
            .private
            .iter_mut()
            .find(|(name, _)| name.ends_with(suffix))
            .unwrap()
            .1;
        value.mask[0] = 1;
        assert!(target.load_state(&file).is_err());
        assert_eq!(target.time(), 1);
    }
    finish(&mut target);
}

#[test]
fn malformed_shared_cells_cannot_alias_distinct_lexical_variables() {
    let source = r#"`timescale 1ns/1ns
module Top(output int q);
 initial begin
   automatic int a=1;
   automatic int b=2;
   fork begin #3; q=a+b; end join
 end
endmodule"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("cells.sv"))], "Top")
        .build_native()
        .unwrap();
    sim.run_until(1).unwrap();
    let original = sim.save_state().unwrap();
    for corrupt in 0..3 {
        let mut file = original.clone();
        let record = &mut file.schedule.as_mut().unwrap().processes;
        let frame = record
            .activations
            .iter_mut()
            .find(|a| a.parent.is_some())
            .unwrap();
        let bindings: Vec<_> = frame
            .locals
            .iter()
            .enumerate()
            .filter_map(|(i, (_, cell))| cell.map(|c| (i, c)))
            .collect();
        assert_eq!(bindings.len(), 2);
        match corrupt {
            0 => frame.locals[bindings[1].0].1 = Some(bindings[0].1),
            1 => frame.locals[bindings[1].0].1 = Some(u64::MAX),
            _ => record.cells[bindings[0].1 as usize].width += 1,
        }
        assert!(sim.load_state(&file).is_err());
        assert_eq!(sim.time(), 1);
    }
    sim.run_until(4).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 3u8.into());
}

#[test]
fn exited_program_children_round_trip_while_another_program_runs() {
    fn replay<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(2).unwrap();
        assert!(!sim.is_finished());
        let file = bytes(&sim.save_state().unwrap());
        sim.run_until(12).unwrap();
        assert_eq!(
            sim.get(sim.signal("done")),
            1u8.into(),
            "uninterrupted baseline"
        );
        // Pending join_none children and children already delayed both leave
        // dead graph nodes behind; none may keep execution storage in the file.
        sim.load_state(&file).unwrap();
        assert!(
            file.schedule
                .as_ref()
                .unwrap()
                .processes
                .activations
                .iter()
                .any(|a| !a.live && a.parent.is_some())
        );
        for activation in &file.schedule.as_ref().unwrap().processes.activations {
            if !activation.live {
                assert!(activation.private.is_empty() && activation.locals.is_empty());
            }
        }
        for _ in 0..2 {
            sim.run_until(12).unwrap();
            assert_eq!(sim.get(sim.signal("q")), 0u8.into());
            assert_eq!(sim.get(sim.signal("done")), 1u8.into());
            assert!(sim.is_finished());
            sim.load_state(&file).unwrap();
        }
    }
    for delay in ["", "#1;"] {
        let source = format!(
            r#"`timescale 1ns/1ns
module Top(output int q, output int done);
 Tb t(q);
 Other other(done);
endmodule
program Tb(output int q);
 initial begin
  automatic int value=7;
  q=0;
  fork begin #5; q=value; end join_none
  {delay}
  $exit;
 end
endprogram
program Other(output int done);
 // Leave a time step for output propagation before implicit program finish.
 initial begin done=0; #10; done=1; #1; end
endprogram"#
        );
        let build =
            || Simulation::from_sv_sources(vec![(&source, Path::new("program_exit.sv"))], "Top");
        replay(build().build_interpreter().unwrap());
        replay(build().build_native().unwrap());
        #[cfg(feature = "cranelift")]
        replay(build().build_cranelift().unwrap());
        #[cfg(feature = "wasm-runtime")]
        replay(build().build_wasm().unwrap());
    }
}

#[test]
fn dormant_child_containers_are_rejected_without_changing_execution() {
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(1).unwrap();
        assert!(matches!(
            sim.save_state(),
            Err(celox::StateError::Objects(_))
        ));
        sim.run_until(4).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 1u8.into());
    }
    for (dimension, initialize, size) in [
        ("[]", "values=new[1];", "size"),
        ("[$]", "values.push_back(7);", "size"),
        ("[int]", "values[3]=7;", "num"),
    ] {
        let source = format!(
            r#"`timescale 1ns/1ns
module Top(output int q);
 initial begin
  q=0;
  for (int i=0; i<2; i++) begin
   automatic int values{dimension};
   if (i==0) begin {initialize} end
   fork begin #3; q=q+values.{size}(); end join_none
  end
  #1;
 end
endmodule"#
        );
        let build =
            || Simulation::from_sv_sources(vec![(&source, Path::new("containers.sv"))], "Top");
        check(build().build_interpreter().unwrap());
        check(build().build_native().unwrap());
        #[cfg(feature = "cranelift")]
        check(build().build_cranelift().unwrap());
        #[cfg(feature = "wasm-runtime")]
        check(build().build_wasm().unwrap());
    }
}

#[test]
fn compound_event_baselines_restore_and_probe_entries_cannot_resume() {
    const SOURCE: &str = r#"`timescale 1ns/1ns
        module Top(output int q);
            initial begin
                automatic int value=0;
                q=0;
                fork
                    begin @(value>2); q=q+1; end
                    begin #2; value=1; #1; value=3; end
                join
                q=q+2;
            end
        endmodule
    "#;
    let mut source =
        Simulation::from_sv_sources(vec![(SOURCE, Path::new("compound_state.sv"))], "Top")
            .build_interpreter()
            .unwrap();
    source.run_until(1).unwrap();
    let probe = source
        .program()
        .runtime_schema
        .process_frames
        .iter()
        .flatten()
        .flat_map(|frame| &frame.event_probes)
        .next()
        .unwrap()
        .resume;
    let file = bytes(&source.save_state().unwrap());
    let mut target =
        Simulation::from_sv_sources(vec![(SOURCE, Path::new("compound_state.sv"))], "Top")
            .build_native()
            .unwrap();
    target.load_state(&file).unwrap();
    for mutation in 0..3 {
        let mut invalid = file.clone();
        let processes = &mut invalid.schedule.as_mut().unwrap().processes;
        let ProcessRef::Activation(waiter) = processes.waiters[0].process else {
            panic!("dynamic waiter");
        };
        let activation = processes
            .activations
            .iter_mut()
            .find(|activation| activation.id == waiter)
            .unwrap();
        let mut set = |suffix: &str, number: u64| {
            let (_, value) = activation
                .private
                .iter_mut()
                .find(|(name, _)| name.ends_with(suffix))
                .unwrap();
            let width = value.width.div_ceil(8) as usize;
            value.value = number.to_le_bytes()[..width].to_vec();
            value.mask.fill(0);
        };
        match mutation {
            0 => set("resume", u64::from(probe)),
            1 => set(
                "status",
                u64::from(celox_design::ProcessStatus::EventProbe.code()),
            ),
            2 => {
                set(
                    "status",
                    u64::from(celox_design::ProcessStatus::Fork.code()),
                );
                set("delay", u64::from(probe) | (1 << 16) | (2 << 32));
            }
            _ => unreachable!(),
        }
        let error = target.load_state(&bytes(&invalid)).unwrap_err().to_string();
        assert!(
            error.contains("invalid continuation or status")
                || error.contains("invalid control payload"),
            "{error}"
        );
        assert_eq!(target.time(), 1);
        assert_eq!(target.get(target.signal("q")), 0u8.into());
    }
    target.run_until(2).unwrap();
    assert_eq!(target.get(target.signal("q")), 0u8.into());
    target.run_until(4).unwrap();
    assert_eq!(target.get(target.signal("q")), 3u8.into());
}
