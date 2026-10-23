use super::*;

sv_backends! {
    fn zero_count_preserves_memory(sim) { @case "binary_file_io::zero_count_preserves_memory"; }

    fn packed_partial_reads_and_integer_descriptor_conversion(sim) {
        @setup {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("input.bin");
            std::fs::write(&path, [0xab, 0xcd, 0xef, 0x12, 0x34]).unwrap();
            let source = format!(r#"module Top(output bit ok);
                int fd, count;
                logic [23:0] data;
                initial begin
                    fd = $fopen("{}", "rb");
                    data = 0;
                    count = $fread(data, {{128'h1234, fd}});
                    ok = count == 3 && data == 24'habcdef;
                    data = 24'h987654;
                    count = $fread(data, fd, -100, -1);
                    ok &= count == 2 && data == 24'h123454 && $feof(fd) != 0;
                    count = $fread(data, fd);
                    ok &= count == 0 && data == 24'h123454;
                    count = $fread(data, 32'h8000ffff);
                    ok &= count == 0 && data == 24'h123454;
                    $fclose(fd);
                end
            endmodule"#, path.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source, Path::new("binary_packed.sv"))], "Top");
        sim.run_until(0).unwrap();
        let ok = sim.signal("ok");
        assert_eq!(sim.get(ok), BigUint::from(1u8));
    }

    fn unsigned_addresses_and_ascending_negative_bounds(sim) {
        @setup {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("input.bin");
            std::fs::write(&path, [0xab, 0xcd, 0xef]).unwrap();
            let source = format!(r#"module Top(output bit ok);
                int fd, count;
                logic [7:0] start, limit;
                logic [7:0] high [128:128];
                logic [7:0] negative [-2:-1];
                initial begin
                    fd = $fopen("{}", "rb");
                    start = 8'h80; limit = 8'hff;
                    count = $fread(high, fd, start, limit);
                    ok = count == 1 && high[128] == 8'hab;
                    count = $fread(negative, fd);
                    ok &= count == 2 && negative[-2] == 8'hcd && negative[-1] == 8'hef;
                    $fclose(fd);
                end
            endmodule"#, path.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source, Path::new("binary_addresses.sv"))], "Top");
        sim.run_until(0).unwrap();
        let ok = sim.signal("ok");
        assert_eq!(sim.get(ok), BigUint::from(1u8));
    }

    fn descending_non_byte_memory_bounds_and_partial_word(sim) {
        @setup {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("input.bin");
            std::fs::write(&path, [0xab, 0xcd, 0x12, 0x34, 0x56]).unwrap();
            let source = format!(r#"module Top(output bit ok);
                int fd, count;
                logic [8:0] mem [3:1];
                initial begin
                    fd = $fopen("{}", "rb");
                    mem[1] = 9'h155; mem[2] = 9'h155; mem[3] = 9'h155;
                    count = $fread(mem, fd,, 2);
                    ok = count == 4 && mem[1] == 9'h1cd && mem[2] == 9'h034 && mem[3] == 9'h155;
                    count = $fread(mem, fd, 3, 1);
                    ok &= count == 1 && mem[3] == 9'h055;
                    $fclose(fd);
                end
            endmodule"#, path.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source, Path::new("binary_memory.sv"))], "Top");
        sim.run_until(0).unwrap();
        let ok = sim.signal("ok");
        assert_eq!(sim.get(ok), BigUint::from(1u8));
    }

    fn destination_selector_is_captured_before_descriptor_side_effects(sim) {
        @setup {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("input.bin");
            std::fs::write(&path, [0xab]).unwrap();
            let source = format!(r#"module Top(output bit ok);
                int fd, count, index, calls;
                logic [15:0] mem [0:1];
                function automatic int select_index(); calls++; return index; endfunction
                function automatic int descriptor(); index = 1; mem[0] = 16'h1234; return fd; endfunction
                initial begin
                    fd = $fopen("{}", "rb");
                    index = 0; calls = 0; mem[0] = 0; mem[1] = 16'h5678;
                    count = $fread(mem[select_index()], descriptor());
                    ok = count == 1 && calls == 1 && index == 1 && mem[0] == 16'hab34 && mem[1] == 16'h5678;
                    $fclose(fd);
                end
            endmodule"#, path.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source, Path::new("binary_reference.sv"))], "Top");
        sim.run_until(0).unwrap();
        let ok = sim.signal("ok");
        assert_eq!(sim.get(ok), BigUint::from(1u8));
    }
}

#[test]
fn fread_rejects_read_only_and_multidimensional_destinations() {
    for destination in ["32'h0", "mem"] {
        let source = format!(
            "module Top; logic [7:0] mem [0:1][0:1]; int n; initial n = $fread({destination}, 32'h80000001); endmodule"
        );
        assert!(
            celox::Simulation::from_sv_sources(
                vec![(&source, Path::new("binary_invalid.sv"))],
                "Top"
            )
            .build_interpreter()
            .is_err(),
            "{destination}"
        );
    }
}

#[test]
fn partial_binary_reads_preserve_unknown_bits_on_every_backend() {
    fn check<B: celox::SimBackend>(mut sim: celox::Simulation<B>, four_state: bool) {
        sim.run_until(0).unwrap();
        let (value, mask) = sim.get_four_state(sim.signal("data"));
        if four_state {
            assert_eq!(mask, BigUint::from(255u16));
            assert_eq!(value >> 8, BigUint::from(0xabu8));
        } else {
            assert_eq!(value, BigUint::from(0xab00u16));
            assert_eq!(mask, BigUint::from(0u8));
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input.bin");
    std::fs::write(&path, [0xab]).unwrap();
    let source = format!(
        r#"module Top(output logic [15:0] data);
        int fd, count;
        initial begin
            fd = $fopen("{}", "rb"); data = 'x;
            count = $fread(data, fd); $fclose(fd);
        end
    endmodule"#,
        path.display()
    );
    for four_state in [false, true] {
        let build = || {
            celox::Simulation::from_sv_sources(
                vec![(&source, Path::new("binary_unknown.sv"))],
                "Top",
            )
            .four_state(four_state)
        };
        check(build().build_interpreter().unwrap(), four_state);
        check(build().build_native().unwrap(), four_state);
        check(build().build_cranelift().unwrap(), four_state);
        check(build().build_wasm().unwrap(), four_state);
    }
}
