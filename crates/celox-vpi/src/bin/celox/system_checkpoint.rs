//! The CLI system owns composition; core checkpoint bytes know nothing of effects.
use celox::{SimBackend, Simulation};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct CliCheckpoint {
    format: String,
    checksum: String,
    time: u64,
    progress: crate::SavedStateSaves,
    core: Vec<u8>,
    waveform: celox::SourceWaveformState,
    file_effects: celox_runtime::file_io::FileIoSnapshot,
}

pub(super) fn capture<B: SimBackend>(
    simulation: &Simulation<B>,
    saves: &crate::StateSaves,
) -> Result<Vec<u8>, String> {
    let mut state = CliCheckpoint {
        format: "celox-cli-checkpoint/v4".into(),
        checksum: String::new(),
        time: simulation.time(),
        progress: saves.progress(),
        core: simulation
            .checkpoint()
            .map_err(|error| error.to_string())?
            .to_bytes()
            .map_err(|error| error.to_string())?,
        waveform: simulation.source_waveform_state(),
        file_effects: simulation
            .file_io()
            .try_snapshot()
            .map_err(|error| error.to_string())?,
    };
    let bytes = serde_json::to_vec(&state).map_err(|error| error.to_string())?;
    state.checksum = blake3::hash(&bytes).to_hex().to_string();
    serde_json::to_vec(&state).map_err(|error| error.to_string())
}

pub(super) fn restore<B: SimBackend>(
    simulation: &mut Simulation<B>,
    path: &Path,
) -> Result<crate::SavedStateSaves, String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    let mut state: CliCheckpoint =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let expected = std::mem::take(&mut state.checksum);
    let encoded = serde_json::to_vec(&state).map_err(|error| error.to_string())?;
    if blake3::hash(&encoded).to_hex().as_str() != expected {
        return Err("CLI checkpoint checksum mismatch".into());
    }
    if state.format != "celox-cli-checkpoint/v4" {
        return Err("incompatible CLI checkpoint".into());
    }
    // Safety: --restore-checkpoint, like --native-image, explicitly loads a
    // trusted artifact produced by this application. Never use it on untrusted data.
    let core = unsafe { simulation.checkpoint_from_bytes(&state.core) }
        .map_err(|error| error.to_string())?;
    let effects = simulation
        .file_io()
        .prepare_restore(&state.file_effects)
        .map_err(|error| error.to_string())?;
    // Install effects before settling the restored core can issue another effect.
    simulation.replace_file_io(effects);
    simulation
        .restore(&core)
        .map_err(|error| error.to_string())?;
    let output = if state.waveform.is_active() {
        let requested =
            std::path::absolute(state.waveform.path()).map_err(|error| error.to_string())?;
        let prefix = format!(
            "{}.resume-",
            requested.file_name().unwrap_or_default().to_string_lossy()
        );
        let segment = tempfile::Builder::new()
            .prefix(&prefix)
            .suffix(".vcd")
            .tempfile_in(requested.parent().unwrap_or(Path::new(".")))
            .map_err(|error| error.to_string())?;
        let (file, path) = segment.keep().map_err(|error| error.to_string())?;
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr().lock(),
            "Restored waveform segment: {}",
            path.display()
        );
        Some((path, file))
    } else {
        None
    };
    // SAFETY: same trusted system artifact as the core decoded above, restored
    // only after core layout compatibility has been checked.
    unsafe { simulation.restore_source_waveform(state.waveform, output) }
        .map_err(|error| error.to_string())?;
    Ok(state.progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn system_checkpoint_restores_file_effects_with_a_fresh_core() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input");
        std::fs::write(&input, b"ABCD").unwrap();
        let source = format!(
            r#"
module Top(output int value);
 timeunit 1ns; timeprecision 1ns;
 int fd = $fopen("{}", "r");
 always #1 value = $fgetc(fd);
endmodule
"#,
            input.display()
        );
        let build = |defer| {
            Simulation::from_sv_sources(vec![(&source, Path::new("io.sv"))], "Top")
                .defer_initialization(defer)
                .file_system(|| Box::<crate::checkpoint_fs::CheckpointFs>::default())
                .build_interpreter()
                .unwrap()
        };
        let mut original = build(false);
        original.run_until(1).unwrap();
        assert_eq!(original.get(original.signal("value")), 65u32.into());
        let saved = capture(&original, &crate::StateSaves::default()).unwrap();
        original.run_until(2).unwrap();
        std::fs::write(&input, b"WXYZ").unwrap();
        let path = dir.path().join("saved.checkpoint");
        std::fs::write(&path, saved).unwrap();
        let mut resumed = build(true);
        restore(&mut resumed, &path).unwrap();
        assert_eq!(resumed.time(), 1);
        resumed.run_until(2).unwrap();
        assert_eq!(resumed.get(resumed.signal("value")), 66u32.into());
        assert_eq!(std::fs::read(&input).unwrap(), b"WXYZ");
    }
    #[test]
    fn restored_cli_retains_a_previously_reported_failure() {
        let source = r#"module Top; timeunit 1ns; timeprecision 1ns;
            initial begin $error("before checkpoint"); #2; $finish; end
        endmodule"#;
        let build = |defer| {
            Simulation::from_sv_sources(vec![(source, Path::new("failure.sv"))], "Top")
                .defer_initialization(defer)
                .file_system(|| Box::<crate::checkpoint_fs::CheckpointFs>::default())
                .build_interpreter()
                .unwrap()
        };
        let mut original = build(false);
        original.run_until(1).unwrap();
        let mut failed = false;
        crate::drain_sv_output(&mut original, &mut Vec::new(), &mut failed).unwrap();
        assert!(failed);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("failed.checkpoint");
        std::fs::write(
            &path,
            capture(
                &original,
                &crate::StateSaves {
                    failed,
                    ..Default::default()
                },
            )
            .unwrap(),
        )
        .unwrap();
        let mut resumed = build(true);
        let progress = restore(&mut resumed, &path).unwrap();
        let mut saves = crate::StateSaves::default();
        saves.restore_progress(&resumed, "Top", progress).unwrap();
        let error = crate::simulate_sv(resumed, 10, saves, &mut Vec::new()).unwrap_err();
        assert!(error.contains("assertion failure or $error"), "{error}");
    }
}

#[cfg(test)]
#[test]
fn waveform_controls_survive_system_restore() {
    for pause in ["$dumpoff;", "$dumplimit(1);"] {
        let dir = tempfile::tempdir().unwrap();
        let original_path = dir.path().join("original.vcd");
        let source = format!(
            r#"
module Top(output int value, output int excluded);
 timeunit 1ns; timeprecision 1ns;
 initial begin
  $dumpfile("{}"); $dumpvars(0, Top.value);
  #1; {}; #3; $dumplimit(0); $dumpon;
 end
 always #1 begin value++; excluded++; end
endmodule
"#,
            original_path.display(),
            pause
        );
        let build = |defer| {
            Simulation::from_sv_sources(vec![(&source, Path::new("wave.sv"))], "Top")
                .defer_initialization(defer)
                .file_system(|| Box::<crate::checkpoint_fs::CheckpointFs>::default())
                .build_interpreter()
                .unwrap()
        };
        let mut original = build(false);
        original.run_until(2).unwrap();
        original.flush_vcd().unwrap();
        let checkpoint = dir.path().join("wave.checkpoint");
        std::fs::write(
            &checkpoint,
            capture(&original, &crate::StateSaves::default()).unwrap(),
        )
        .unwrap();
        let delivered = std::fs::read(&original_path).unwrap();
        let mut resumed = build(true);
        restore(&mut resumed, &checkpoint).unwrap();
        resumed.run_until(5).unwrap();
        resumed.flush_vcd().unwrap();
        assert_eq!(std::fs::read(&original_path).unwrap(), delivered);
        let segment = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("original.vcd.resume-")
            })
            .unwrap();
        let text = std::fs::read_to_string(segment.path()).unwrap();
        assert!(text.contains(" value $end"), "{pause}: {text}");
        assert!(!text.contains(" excluded $end"), "{pause}: {text}");
        assert!(!text.contains("#3\n"), "{pause}: {text}");
        assert!(
            text.contains("#4\n") && text.contains("#5\n"),
            "{pause}: {text}"
        );
        assert!(text.contains("$dumpon"), "{pause}: {text}");
    }
}

#[cfg(test)]
#[test]
fn restore_keeps_consumed_saves_cycle_count_and_edge_baseline() {
    // Capture both high and low clock phases, including immediately after a
    // counted edge. Neither start() nor the next step may count that edge twice.
    for checkpoint_time in [9, 10] {
        let dir = tempfile::tempdir().unwrap();
        let source = r#"module Top(output bit clk);
            timeunit 1ns; timeprecision 1ns;
            always #1 clk = !clk;
            initial begin #13; $finish; end
        endmodule"#;
        let build = |defer| {
            Simulation::from_sv_sources(vec![(source, Path::new("saves.sv"))], "Top")
                .defer_initialization(defer)
                .file_system(|| Box::<crate::checkpoint_fs::CheckpointFs>::default())
                .build_interpreter()
                .unwrap()
        };
        let arguments = crate::SaveStateArgs {
            save_state_on: Some("clk".into()),
            save_state_at: vec![2, 6],
            save_state_at_time: vec![5, 11],
            save_state_dir: Some(dir.path().into()),
            ..Default::default()
        };
        let mut original = build(false);
        let mut saves = crate::StateSaves::new(&original, "Top", &arguments, None).unwrap();
        saves.start(&mut original);
        while let Some(next) = original.next_event_time() {
            if next > checkpoint_time {
                break;
            }
            saves.before(&mut original, next).unwrap();
            original.step().unwrap();
            saves.after_step(&mut original).unwrap();
        }
        assert_eq!(saves.count, 5);
        assert_eq!(saves.cycles, [6]);
        assert_eq!(saves.times, [11]);
        let first_time = std::fs::read(dir.path().join("time-5.state")).unwrap();
        let first_cycle = std::fs::read(dir.path().join("clk-2.state")).unwrap();
        let checkpoint = dir.path().join("scheduled.checkpoint");
        std::fs::write(&checkpoint, capture(&original, &saves).unwrap()).unwrap();
        let mut restored = build(true);
        let progress = restore(&mut restored, &checkpoint).unwrap();
        let mut saves = crate::StateSaves::new(&restored, "Top", &arguments, None).unwrap();
        saves.restore_progress(&restored, "Top", progress).unwrap();
        crate::simulate_sv(restored, 20, saves, &mut Vec::new()).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("time-5.state")).unwrap(),
            first_time
        );
        assert_eq!(
            std::fs::read(dir.path().join("clk-2.state")).unwrap(),
            first_cycle
        );
        for name in ["clk-6.state", "time-11.state"] {
            let file = std::fs::File::open(dir.path().join(name)).unwrap();
            let state = celox::StateFile::read_from(file).unwrap();
            assert_eq!(state.schedule.unwrap().time, 11, "{name}");
        }
    }
}
