use super::*;

sv_backends! {
    fn byte_reads_pushback_position_and_eof(sim) {
        @setup {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("input.bin");
            std::fs::write(&path, [0, 255, b'A', b'B']).unwrap();
            let source = format!(r#"module Top(output bit ok);
                int fd;
                initial begin
                    ok = 1;
                    fd = $fopen("{}", "rb");
                    ok &= fd != 0 && $feof(fd) == 0 && $ftell(fd) == 0;
                    ok &= $fgetc(fd) == 0 && $ftell(fd) == 1;
                    ok &= $fgetc(fd) == 255 && $ftell(fd) == 2;
                    ok &= $ungetc(127, fd) == 0 && $ftell(fd) == 1;
                    ok &= $fgetc(fd) == 127 && $ftell(fd) == 2;
                    ok &= $fseek(fd, -1, 1) == 0 && $fgetc(fd) == 255;
                    ok &= $fseek(fd, -1, 2) == 0 && $fgetc(fd) == 66;
                    ok &= $feof(fd) == 0 && $fgetc(fd) == -1 && $feof(fd) != 0;
                    ok &= $ungetc(90, fd) == 0 && $feof(fd) == 0 && $ftell(fd) == 3;
                    ok &= $fseek(fd, 0, 1) == 0 && $fgetc(fd) == 66;
                    ok &= $fgetc(fd) == -1 && $rewind(fd) == 0;
                    ok &= $feof(fd) == 0 && $ftell(fd) == 0 && $fgetc(fd) == 0;
                    $fclose(fd);
                end
            endmodule"#, path.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source, Path::new("file_core_bytes.sv"))], "Top");
        sim.run_until(0).unwrap();
        let ok = sim.signal("ok");
        assert_eq!(sim.get(ok), BigUint::from(1u8));
        assert_eq!(std::fs::read(path).unwrap(), [0, 255, b'A', b'B']);
    }

    fn flush_update_seek_gap_and_append_modes(sim) {
        @setup {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("output.bin");
            let source = format!(r#"module Top(output bit ok);
                int fd;
                initial begin
                    ok = 1;
                    fd = $fopen("{}", "w+b");
                    $fwrite(fd, "a");
                    $fflush(fd);
                    ok &= $ftell(fd) == 1 && $fseek(fd, 3, 0) == 0;
                    $fwrite(fd, "z");
                    $fflush();
                    ok &= $rewind(fd) == 0;
                    ok &= $fgetc(fd) == 97 && $fgetc(fd) == 0 && $fgetc(fd) == 0 && $fgetc(fd) == 122;
                    $fclose(fd);
                    fd = $fopen("{}", "a+b");
                    ok &= $ftell(fd) == 4 && $feof(fd) == 0;
                    ok &= $fgetc(fd) == -1 && $feof(fd) != 0;
                    ok &= $fseek(fd, 0, 0) == 0 && $feof(fd) == 0;
                    $fwrite(fd, "!");
                    $fflush(fd);
                    ok &= $ftell(fd) == 5 && $rewind(fd) == 0 && $fgetc(fd) == 97;
                    $fclose(fd);
                end
            endmodule"#, path.display(), path.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source, Path::new("file_core_update.sv"))], "Top");
        sim.run_until(0).unwrap();
        let ok = sim.signal("ok");
        assert_eq!(sim.get(ok), BigUint::from(1u8));
        assert_eq!(std::fs::read(path).unwrap(), b"a\0\0z!");
    }

    fn input_and_position_failures_return_status_without_stopping_process(sim) {
        @setup {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("writeonly.bin");
            let source = format!(r#"module Top(output bit ok, done);
                int fd;
                initial begin
                    fd = $fopen("{}", "w");
                    ok = $fgetc(fd) == -1 && $ungetc(65, fd) == -1 && $feof(fd) == 0;
                    ok &= $fseek(fd, -1, 0) == -1 && $ftell(fd) == 0;
                    ok &= $fseek(fd, 0, 9) == -1 && $ftell(fd) == 0;
                    $fclose(fd);
                    ok &= $fgetc(fd) == -1 && $ungetc(65, fd) == -1;
                    ok &= $ftell(fd) == -1 && $rewind(fd) == -1 && $fseek(fd, 0, 0) == -1;
                    ok &= $fgetc(1) == -1 && $ungetc(-1, 32'h80000000) == -1;
                    done = 1;
                end
            endmodule"#, path.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source, Path::new("file_core_status.sv"))], "Top");
        sim.run_until(0).unwrap();
        let ok = sim.signal("ok");
        let done = sim.signal("done");
        assert_eq!(sim.get(ok), BigUint::from(1u8));
        assert_eq!(sim.get(done), BigUint::from(1u8));
    }

    fn unsigned_byte_seek_offsets_zero_extend_and_signed_offsets_sign_extend(sim) {
        @setup {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("offset.bin");
            std::fs::write(&path, []).unwrap();
            let source = format!(r#"module Top(output bit ok);
                int fd;
                byte unsigned positive = 8'h80;
                byte negative = -1;
                initial begin
                    fd = $fopen("{}", "r+");
                    ok = $fseek(fd, positive, 0) == 0 && $ftell(fd) == 128;
                    ok &= $fseek(fd, negative, 1) == 0 && $ftell(fd) == 127;
                    ok &= $fseek(fd, 8'hff, 0) == 0 && $ftell(fd) == 255;
                    ok &= $fseek(fd, 8'shff, 1) == 0 && $ftell(fd) == 254;
                    $fclose(fd);
                end
            endmodule"#, path.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source, Path::new("file_core_offset_types.sv"))], "Top");
        sim.run_until(0).unwrap();
        let ok = sim.signal("ok");
        assert_eq!(sim.get(ok), BigUint::from(1u8));
    }

    fn new_file_functions_truncate_descriptor_actuals_before_testing_unknown_bits(sim) {
        @setup {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("descriptor.bin");
            std::fs::write(&path, b"a").unwrap();
            let source = format!(r#"module Top(output bit ok);
                int fd;
                logic [127:0] wide;
                initial begin
                    fd = $fopen("{}", "r");
                    wide = {{96'hx, fd}};
                    ok = $ftell(wide) == 0;
                    $fflush(wide);
                    ok &= $fgetc(wide) == 97 && $feof(wide) == 0;
                    ok &= $fseek(wide, 0, 0) == 0 && $ungetc(65, wide) == 0;
                    ok &= $rewind(wide) == 0 && $fgetc(wide) == 97;
                    $fclose(fd);
                end
            endmodule"#, path.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source, Path::new("file_core_descriptor_types.sv"))], "Top").four_state(true);
        sim.run_until(0).unwrap();
        let ok = sim.signal("ok");
        assert_eq!(sim.get(ok), BigUint::from(1u8));
    }

    fn flush_and_file_function_types(sim) { @case "file_core::flush_preserves_stdout_order_and_integral_return_types"; }
}

#[test]
fn invalid_flush_reports_a_runtime_error() {
    let source = "module Top; initial $fflush(32'h8000ffff); endmodule";
    let mut simulation =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("invalid_flush.sv"))], "Top")
            .build_interpreter()
            .unwrap();
    assert!(
        simulation
            .run_until(0)
            .unwrap_err()
            .to_string()
            .contains("$fflush: invalid or closed")
    );
}

#[test]
fn byte_function_calls_reject_invalid_arity_and_combinational_execution() {
    for call in [
        "$fgetc()",
        "$ungetc(1)",
        "$ftell(1, 2)",
        "$fseek(1, 2)",
        "$rewind()",
        "$feof()",
    ] {
        let source = format!("module Top(output int result); initial result = {call}; endmodule");
        assert!(
            celox::Simulation::from_sv_sources(vec![(&source, Path::new("arity.sv"))], "Top")
                .build_interpreter()
                .is_err(),
            "{call}"
        );
    }
    let source =
        "module Top(output int result); always_comb result = $ftell(32'h80000001); endmodule";
    let message = Simulator::from_sv_sources(vec![(source, Path::new("comb.sv"))], "Top")
        .build_interpreter()
        .unwrap_err()
        .to_string();
    assert!(message.contains("$ftell"), "{message}");
}
