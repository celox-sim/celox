use crate::*;

sv_backends! {
    fn monitor_observes_idle_host_writes(sim) {
        @setup { let source=r#"module Top;
            timeunit 1ps; timeprecision 1ps;
            int value;
            initial $monitor("%0d",value);
        endmodule"#; }
        @build celox::Simulation::from_sv_sources(vec![(source,Path::new("monitor_host.sv"))],"Top");
        sim.run_until(0).unwrap();
        sim.drain_runtime_events();
        let value=sim.signal("value");
        sim.modify(|core| core.set(value,7u32)).unwrap();
        sim.run_until(1).unwrap();
        assert_eq!(sim.drain_runtime_events(),vec![celox::RuntimeEvent::Display { message:b"7".to_vec() }]);
        sim.run_until(2).unwrap();
        assert!(sim.drain_runtime_events().is_empty());
    }
}

#[test]
fn monitor_rejects_expression_arguments() {
    for body in [
        "int value; initial $monitor(value+1);",
        "int value; initial $monitor(value++);",
        "string value; initial $fmonitor(1,value);",
    ] {
        let source = format!("module Top; {body} endmodule");
        let result = celox::Simulation::from_sv_sources(
            vec![(&source, Path::new("monitor_invalid.sv"))],
            "Top",
        )
        .build_cranelift();
        assert!(result.is_err(), "accepted {body}");
    }
}

sv_backends! {
    fn fmonitor_captures_fd_and_close_cancels_each_list(sim) {
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
                    fd=a; $fmonitor(fd,"value=%0d",value); fd=b;
                    $fmonitor(b,"cancelled"); $fclose(b); b=$fopen("{}","a");
                    $fmonitor(c|d,"channels=%0d",value); $fclose(c);
                    value=7;
                    #1 value=8;
                    #1 $fclose(a); $fclose(b); $fclose(d);
                    value=9;
                end
            endmodule"#,a.display(),b.display(),c.display(),d.display(),b.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source,Path::new("fmonitor_files.sv"))],"Top");
        sim.run_until(2).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(),b"value=7\nvalue=8\n");
        assert_eq!(std::fs::read(&b).unwrap(),b"");
        assert_eq!(std::fs::read(&c).unwrap(),b"");
        assert_eq!(std::fs::read(&d).unwrap(),b"channels=7\nchannels=8\n");
    }

    fn monitor_tracks_four_state_mask_changes(sim) {
        @setup { let source=r#"module Top;
            timeunit 1ps; timeprecision 1ps;
            logic value;
            initial begin value=1'bx; $monitor("%b",value);
                #1 value=1'bz; #1 value=0; #1;
            end
        endmodule"#; }
        @build celox::Simulation::from_sv_sources(vec![(source,Path::new("monitor_masks.sv"))],"Top").four_state(true);
        sim.run_until(3).unwrap();
        assert_eq!(sim.drain_runtime_events(),["x","z","0"].map(|s| celox::RuntimeEvent::Display { message:s.as_bytes().to_vec() }).to_vec());
    }
}
