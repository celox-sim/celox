#![cfg(feature = "systemverilog")]

use std::path::Path;

/// Every instance starts its own site numbering at zero. Interleaved
/// operation families must retain their operands after a host round trip.
macro_rules! mixed_requests {
    ($name:ident, $backend:ident) => {
        #[test]
        fn $name() {
            let directory = tempfile::tempdir().unwrap();
            let first = directory.path().join("first.txt");
            let second = directory.path().join("second.txt");
            let waveform = directory.path().join("trace.vcd");
            let source = format!(
                r#"
                module Child #(parameter int ID = 0)
                  (output logic [7:0] value, output logic ok);
                  timeunit 1ps; timeprecision 1ps;
                  logic [2047:0] filename;
                  logic [127:0] query, file_query;
                  int fd, seed, random_value;
                  initial begin
                    seed = 19;
                    query = ID == 0 ? "A=%d" : "B=%d";
                    file_query = ID == 0 ? "A_PATH=%s" : "B_PATH=%s";
                    ok = $value$plusargs(query, value);
                    ok &= $value$plusargs(file_query, filename);
                    random_value = $dist_uniform(seed, -3, 3);
                    ok &= random_value >= -3 && random_value <= 3;
                    fd = $fopen(filename, "w");
                    $fdisplay(fd, "%m:%0d", value);
                    $fclose(fd);
                    #1 value = value + 1;
                  end
                endmodule
                module Top(output logic [7:0] a_value, b_value,
                           output logic a_ok, b_ok);
                  timeunit 1ps; timeprecision 1ps;
                  Child a(a_value, a_ok);
                  Child #(.ID(1)) b(b_value, b_ok);
                  initial begin
                    $dumpfile("{}");
                    $dumpvars(0, Top);
                    #2 $dumpflush;
                  end
                endmodule
                "#,
                waveform.display()
            );
            let mut simulation = celox::Simulation::from_sv_sources(
                vec![(&source, Path::new("mixed_host_requests.sv"))],
                "Top",
            )
            .four_state(true)
            .plusargs([
                "+A=11".to_string(),
                "+B=29".to_string(),
                format!("+A_PATH={}", first.display()),
                format!("+B_PATH={}", second.display()),
            ])
            .$backend()
            .unwrap();
            simulation.run_until(2).unwrap();
            simulation.flush_vcd().unwrap();
            assert_eq!(simulation.get(simulation.signal("a_ok")), 1u8.into());
            assert_eq!(simulation.get(simulation.signal("b_ok")), 1u8.into());
            assert_eq!(simulation.get(simulation.signal("a_value")), 12u8.into());
            assert_eq!(simulation.get(simulation.signal("b_value")), 30u8.into());
            assert_eq!(std::fs::read_to_string(first).unwrap(), "Top.a:11\n");
            assert_eq!(std::fs::read_to_string(second).unwrap(), "Top.b:29\n");
            let trace = std::fs::read_to_string(waveform).unwrap();
            assert!(trace.contains("$dumpvars"));
            assert!(trace.contains("#1\n"));
        }
    };
}

mixed_requests!(interpreter, build_interpreter);
mixed_requests!(cranelift, build_cranelift);
mixed_requests!(wasm, build_wasm);
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
mixed_requests!(native, build_native);

#[test]
fn core_checkpoint_does_not_own_external_host_state() {
    for statement in ["$fwrite(1, \"output\");", "$dumpvars;"] {
        let source =
            format!("module Top(output bit q); initial begin {statement} q = 1; end endmodule");
        let mut simulation = celox::Simulation::from_sv_sources(
            vec![(&source, Path::new("external_state.sv"))],
            "Top",
        )
        .build_interpreter()
        .unwrap();
        let checkpoint = simulation.checkpoint().unwrap();
        let file = simulation.save_state().unwrap();
        simulation.restore(&checkpoint).unwrap();
        assert!(simulation.load_state(&file).is_err());
    }
}

#[path = "test_utils/file_system.rs"]
mod file_system;

const IO_SNAPSHOT_SOURCE: &str = r#"
module Top(output int value, count);
    timeunit 1ps; timeprecision 1ps;
    int fd = $fopen("input", "r");
    int n;
    initial begin
        n = $fscanf(fd, "%d", value);
        #1;
        value = $fgetc(fd);
        count++;
        $fclose(fd);
        fd = $fopen("output", "w");
        $fwrite(fd, "%c", value);
        $fclose(fd);
        #1;
    end
endmodule
"#;

macro_rules! io_snapshot {
    ($name:ident, $backend:ident) => {
        #[test]
        fn $name() {
            let build = || {
                celox::Simulation::from_sv_sources(
                    vec![(IO_SNAPSHOT_SOURCE, Path::new("io_snapshot.sv"))],
                    "Top",
                )
                .file_system(|| Box::<file_system::TestIo>::default())
            };
            let mut sim = build().$backend().unwrap();
            sim.run_until(0).unwrap();
            assert_eq!(sim.get(sim.signal("value")), 12u32.into());
            let checkpoint = sim.checkpoint().unwrap();
            let effects = sim.file_io().snapshot();
            for _ in 0..2 {
                sim.run_until(1).unwrap();
                assert_eq!(sim.get(sim.signal("value")), 81u32.into());
                assert_eq!(sim.get(sim.signal("count")), 1u32.into());
                // The old input descriptor was closed and its HDL number reused
                // for output. Both the mapping and fscanf lookahead must return.
                let restored = sim.file_io().prepare_restore(&effects).unwrap();
                sim.replace_file_io(restored);
                sim.restore(&checkpoint).unwrap();
                assert_eq!(sim.time(), 0);
                assert_eq!(sim.get(sim.signal("value")), 12u32.into());
            }
            let mut fork = build().$backend().unwrap();
            let restored = fork.file_io().prepare_restore(&effects).unwrap();
            fork.replace_file_io(restored);
            fork.restore(&checkpoint).unwrap();
            fork.run_until(1).unwrap();
            assert_eq!(fork.get(fork.signal("value")), 81u32.into());
            let mut rejected = celox::Simulation::from_sv_sources(
                vec![(IO_SNAPSHOT_SOURCE, Path::new("io_snapshot.sv"))],
                "Top",
            )
            .file_system(|| {
                let mut host = file_system::TestIo::default();
                host.reject_restore = true;
                Box::new(host)
            })
            .$backend()
            .unwrap();
            rejected.run_until(1).unwrap();
            let before = rejected.save_state().unwrap();
            assert!(
                rejected
                    .file_io()
                    .prepare_restore(&effects)
                    .unwrap_err()
                    .to_string()
                    .contains("host rejected restore")
            );
            assert_eq!(before, rejected.save_state().unwrap());
            assert_eq!(rejected.time(), 1);
        }
    };
}
io_snapshot!(io_snapshot_interpreter, build_interpreter);
io_snapshot!(io_snapshot_cranelift, build_cranelift);
io_snapshot!(io_snapshot_wasm, build_wasm);
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
io_snapshot!(io_snapshot_native, build_native);
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
io_snapshot!(io_snapshot_tiered, build_tiered);

#[test]
fn io_manager_restores_eof_pushback_channels_and_standard_streams() {
    use celox_runtime::file_io::FileIo;
    let mut io = FileIo::new(Box::<file_system::TestIo>::default());
    let fd = io.open("input", Some("r"));
    while io.read_byte(fd).unwrap().is_some() {}
    assert!(io.eof(fd).unwrap());
    let eof = io.snapshot();
    io.rewind(fd).unwrap();
    io = io.prepare_restore(&eof).unwrap();
    assert!(io.eof(fd).unwrap());
    io.unread_byte(fd, b'X').unwrap();
    assert_eq!(io.read_byte(0x8000_0000).unwrap(), Some(b's'));
    let channel = io.open("output", None);
    io.write(channel, b"before").unwrap();
    let saved = io.snapshot();
    io.write(channel, b"after").unwrap();
    io.close(fd).unwrap();
    io.close(channel).unwrap();
    io.close(0x8000_0000).unwrap();
    io = io.prepare_restore(&saved).unwrap();
    assert!(!io.eof(fd).unwrap());
    assert_eq!(io.read_byte(fd).unwrap(), Some(b'X'));
    assert_eq!(io.read_byte(0x8000_0000).unwrap(), Some(b't'));
    io.write(channel, b"!").unwrap();
    let output = io.open("output", Some("r"));
    let mut bytes = Vec::new();
    while let Some(byte) = io.read_byte(output).unwrap() {
        bytes.push(byte);
    }
    assert_eq!(bytes, b"before!");
    let mut incompatible = file_system::TestIo::default();
    incompatible.format = "other-host/v2".into();
    let incompatible = FileIo::new(Box::new(incompatible));
    assert!(
        incompatible
            .prepare_restore(&saved)
            .unwrap_err()
            .to_string()
            .contains("format mismatch")
    );
}

#[test]
fn rejected_io_restore_preserves_live_stream_position() {
    use celox_runtime::file_io::FileIo;
    let mut host = file_system::TestIo::default();
    host.reject_restore = true;
    let mut io = FileIo::new(Box::new(host));
    let fd = io.open("input", Some("r"));
    assert_eq!(io.read_byte(fd).unwrap(), Some(b'1'));
    let saved = io.snapshot();
    assert_eq!(io.read_byte(fd).unwrap(), Some(b'2'));
    assert!(io.prepare_restore(&saved).is_err());
    assert_eq!(io.read_byte(fd).unwrap(), Some(b'Q'));
}

#[test]
fn writemem_uses_injected_io() {
    let source = r#"module Top(output int value);
        logic [7:0] words [0:1]; int fd;
        initial begin
            words[0]=8'h41; words[1]=8'h42;
            $writememh("output", words);
            fd=$fopen("output", "r");
            value=$fgetc(fd);
        end
    endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("write.sv"))], "Top")
        .file_system(|| Box::<file_system::TestIo>::default())
        .build_interpreter()
        .unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(sim.signal("value")), u32::from(b'4').into());
}
