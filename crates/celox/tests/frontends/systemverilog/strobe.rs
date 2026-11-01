use crate::*;

sv_backends! {
    fn strobe_reads_after_reactive_program(sim) { @case "file_io::strobe_reads_after_reactive_program"; }
    fn strobe_radix_variants(sim) { @case "file_io::strobe_radix_variants"; }
    fn strobe_reads_after_nba(sim) { @case "file_io::strobe_reads_after_nba"; }
    fn fstrobe_stdout_reads_final_value(sim) { @case "file_io::fstrobe_stdout_reads_final_value"; }
}

sv_backends! {
    fn strobe_observes_zero_delay_nba_and_sampled_values(sim) {
        @setup { let source=r#"module Top;
            timeunit 1ps; timeprecision 1ps;
            int value;
            initial begin
                value=1;
                $strobe("late=%0d sampled=%0d",value,$sampled(value));
                value<=3;
                #0 value=2;
                #1;
            end
        endmodule"#; }
        @build celox::Simulation::from_sv_sources(vec![(source,Path::new("strobe_inactive.sv"))],"Top");
        sim.run_until(1).unwrap();
        assert_eq!(sim.drain_runtime_events(),vec![celox::RuntimeEvent::Display { message:b"late=3 sampled=0".to_vec() }]);
    }

    fn strobe_repeated_calls_read_each_time_slot_end(sim) {
        @setup { let source=r#"module Top;
            timeunit 1ps; timeprecision 1ps;
            int value,index;
            initial begin
                while (index<2) begin $strobe("late=%0d",value); index++; end
                value=9;
                #1 value=10; $strobe("late=%0d",value);
                #1;
            end
        endmodule"#; }
        @build celox::Simulation::from_sv_sources(vec![(source,Path::new("strobe_repeat.sv"))],"Top");
        sim.run_until(2).unwrap();
        assert_eq!(sim.get(sim.signal("value")),BigUint::from(10u8));
        assert_eq!(sim.drain_runtime_events(),["late=9","late=9","late=10"].map(|text| celox::RuntimeEvent::Display { message:text.as_bytes().to_vec() }).to_vec());
    }

    fn fstrobe_captures_fd_and_close_cancels_pending_output(sim) {
        @setup {
            let dir=tempfile::tempdir().unwrap();
            let a=dir.path().join("a"); let b=dir.path().join("b");
            let c=dir.path().join("c"); let d=dir.path().join("d");
            let source=format!(r#"module Top;
            timeunit 1ps; timeprecision 1ps;
                int a,b,c,d,fd,value;
                initial begin
                    a=$fopen("{}","w"); b=$fopen("{}","w");
                    c=$fopen("{}"); d=$fopen("{}");
                    fd=a; $fstrobe(fd,"value=%0d",value); fd=b;
                    $fstrobe(b,"cancelled"); $fclose(b); b=$fopen("{}","a");
                    $fstrobe(c|d,"channels=%0d",value); $fclose(c);
                    value=7;
                    #1 $fclose(a); $fclose(b); $fclose(d);
                end
            endmodule"#,a.display(),b.display(),c.display(),d.display(),b.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source,Path::new("fstrobe_files.sv"))],"Top");
        sim.run_until(1).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(),b"value=7\n");
        assert_eq!(std::fs::read(&b).unwrap(),b"");
        assert_eq!(std::fs::read(&c).unwrap(),b"");
        assert_eq!(std::fs::read(&d).unwrap(),b"channels=7\n");
    }
}

sv_backends! {
    fn fstrobe_preserves_file_writes_beyond_the_report_ring_capacity(sim) {
        @setup {
            let dir=tempfile::tempdir().unwrap(); let file=dir.path().join("many");
            let source=format!(r#"module Top;
                timeunit 1ps; timeprecision 1ps;
                int fd,index;
                initial begin
                    fd=$fopen("{}","w");
                    while (index<1100) begin $fstrobe(fd,"%0d",index); index++; end
                    #1 $fclose(fd);
                end
            endmodule"#,file.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source,Path::new("fstrobe_many.sv"))],"Top");
        sim.run_until(1).unwrap();
        assert_eq!(std::fs::read(file).unwrap(),b"1100\n".repeat(1100));
    }
}

#[test]
fn strobe_rejects_unsupported_late_evaluation_contexts() {
    for body in [
        "int i; initial $strobe(i++);",
        "int i; initial $strobe((i=2));",
        "int i; function int f(); return i; endfunction initial $strobe(f());",
        "task t(input int i); $strobe(i); endtask initial t(1);",
        "initial begin automatic int i=2; $strobe(i); end",
    ] {
        let source = format!("module Top; {body} endmodule");
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(&source, Path::new("strobe_invalid.sv"))],
            "Top",
        )
        .build_cranelift() else {
            panic!("accepted {body}");
        };
        assert!(error.to_string().contains("strobe"), "{body}: {error}");
    }
}
