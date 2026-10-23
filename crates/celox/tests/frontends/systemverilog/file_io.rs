use super::*;

sv_backends! {
    fn closing_standard_streams_twice_is_a_runtime_error(sim) {
        @setup {
            let source = r#"module Top(output bit done);
                initial begin
                    $fclose(32'h8000_0001);
                    $fclose(32'h8000_0001);
                    done = 1;
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("repeat_standard_close.sv"))], "Top");
        assert!(sim.run_until(0).unwrap_err().to_string().contains("invalid or closed"));
        let done = sim.signal("done");
        assert_eq!(sim.get(done), BigUint::from(0u8));
    }

    fn scheduled_resets_keep_their_associated_clock_in_reflection(sim) {
        @setup {
            let source = r#"module Top(input bit clk, rst_n, rst, reference_rst_n,
                                      output bit low, high, reference);
                always_ff @(posedge clk or negedge rst_n) begin
                    $fwrite(1, "");
                    if (!rst_n) low <= 0; else low <= 1;
                end
                always_ff @(posedge clk or posedge rst) begin
                    $fwrite(1, "");
                    if (rst) high <= 0; else high <= 1;
                end
                always_ff @(posedge clk or negedge reference_rst_n)
                    if (!reference_rst_n) reference <= 0; else reference <= 1;
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("scheduled_reset_reflection.sv"))], "Top");
        let signals = sim.named_signals();
        for name in ["rst_n", "rst", "reference_rst_n"] {
            let signal = signals.iter().find(|signal| signal.name == name).unwrap();
            assert_eq!(signal.associated_clock.as_deref(), Some("clk"), "{name}");
        }
        assert!(signals.iter().find(|signal| signal.name == "clk").unwrap().associated_clock.is_none());
    }

    fn memory_snapshots_preserve_non_byte_element_layout(sim) {
        @setup {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("narrow_memory");
            let source = format!(r#"
                module Top;
                    logic [4:0] memory [3:1];
                    initial begin
                        memory[3] = 5'b10101;
                        memory[2] = 5'bxz101;
                        memory[1] = 5'b00111;
                        $writememb("{}", memory, 3, 1);
                    end
                endmodule
            "#, path.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source, Path::new("narrow_memory.sv"))], "Top").four_state(true);
        sim.run_until(0).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"10101\nxz101\n00111\n");
    }
    fn file_output_formats_each_instance_scope(sim) {
        @setup { let source = r#"module Child; initial $fdisplay(1, "%m"); endmodule module Top; Child first(); Child second(); endmodule"#; }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("file_scope.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: b"Top.first".to_vec() },
            celox::RuntimeEvent::Display { message: b"Top.second".to_vec() },
        ]);
    }
    fn edge_procedures_execute_requests_in_statement_order(sim) { @case "file_io::edge_procedures_execute_requests_in_statement_order"; }
    fn stdout_preserves_format_and_call_order(sim) { @case "file_io::stdout_preserves_format_and_call_order"; }
    fn descriptors_distinguish_fd_mcd_and_reuse_channels(sim) {
        @setup {
            let dir = tempfile::tempdir().unwrap();
            let a = dir.path().join("mcd_a");
            let b = dir.path().join("mcd_b");
            let c = dir.path().join("mcd_c");
            let fd = dir.path().join("fd");
            let source = format!(r#"
                module Top(output bit ok);
                  int a, b, c, fd;
                  initial begin
                    a = $fopen("{}");
                    b = $fopen("{}");
                    fd = $fopen("{}", "wb");
                    ok = a != 0 && b != 0 && (a & b) == 0 && !a[31] && !b[31] && fd[31];
                    $fdisplay(a | b, "shared:%0d", 37);
                    $fwrite(fd, "fd");
                    $fclose(a);
                    c = $fopen("{}");
                    ok = ok && a == c;
                    $fclose(b | c);
                    $fclose(fd);
                  end
                endmodule
            "#, a.display(), b.display(), fd.display(), c.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source, Path::new("file_descriptors.sv"))], "Top");
        sim.run_until(0).unwrap();
        let ok = sim.signal("ok");
        assert_eq!(sim.get(ok), BigUint::from(1u8));
        assert_eq!(std::fs::read(&a).unwrap(), b"shared:37\n");
        assert_eq!(std::fs::read(&b).unwrap(), b"shared:37\n");
        assert_eq!(std::fs::read(&fd).unwrap(), b"fd");
        assert!(std::fs::read(&c).unwrap().is_empty());
    }

    fn files_and_memory_are_written_at_runtime(sim) {
        @setup {
            let dir = tempfile::tempdir().unwrap();
            let output = dir.path().join("output");
            let hex = dir.path().join("hex");
            let bin = dir.path().join("bin");
            let source = format!(r#"
                module Top(output bit done);
                  timeunit 1ps; timeprecision 1ps;
                  int fd;
                  logic [7:0] mem [5:3];
                  initial begin
                    mem[5] = 8'h12; mem[4] = 8'h34; mem[3] = 8'h56;
                    #1;
                    fd = $fopen("{}", "w");
                    $fdisplay(fd, "value=%h", mem[5]);
                    $fclose(fd);
                    fd = $fopen("{}", "a");
                    $fwrite(fd, "tail");
                    $fclose(fd);
                    $writememh("{}", mem, 5, 3);
                    mem[4] = 8'bxz010101;
                    $writememb("{}", mem, 4, 3);
                    done = 1;
                  end
                endmodule
            "#, output.display(), output.display(), hex.display(), bin.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source, Path::new("runtime_files.sv"))], "Top").four_state(true);
        assert!(!output.exists());
        sim.run_until(0).unwrap();
        assert!(!output.exists());
        sim.run_until(1).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"value=12\ntail");
        assert_eq!(std::fs::read(&hex).unwrap(), b"12\n34\n56\n");
        assert_eq!(std::fs::read(&bin).unwrap(), b"xz010101\n01010110\n");
    }
}

#[test]
fn edge_host_requests_require_the_timed_api() {
    let source = r#"module Top(input bit clk, output bit assigned);
      always_ff @(posedge clk) begin
        $fwrite(1, "edge");
        assigned = 1;
      end
    endmodule"#;
    let build = || Simulator::from_sv_sources(vec![(source, Path::new("edge_request.sv"))], "Top");
    let results = [
        build().build_interpreter().map(|_| ()),
        build().build_cranelift().map(|_| ()),
        build().build_wasm().map(|_| ()),
    ];
    for result in results {
        let error = result.unwrap_err();
        assert!(
            matches!(
                error.kind(),
                celox::SimulatorErrorKind::ProcessRequiresSimulation(_)
            ),
            "{error}"
        );
    }
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        let error = build().build_native().map(|_| ()).unwrap_err();
        assert!(
            matches!(
                error.kind(),
                celox::SimulatorErrorKind::ProcessRequiresSimulation(_)
            ),
            "{error}"
        );
    }
}

#[test]
fn invalid_file_descriptors_report_runtime_errors() {
    let source = r#"module Top; initial $fwrite(32'h8000ffff, "bad"); endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("invalid_fd.sv"))], "Top")
            .build_interpreter()
            .unwrap();
    assert!(
        sim.step()
            .unwrap_err()
            .to_string()
            .contains("invalid or closed")
    );
}
