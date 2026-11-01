use super::*;

sv_backends! {
    fn fgets_line_boundaries_eof_and_aliases(sim) {
        @setup {
            let directory=tempfile::tempdir().unwrap();
            let path=directory.path().join("lines.txt");
            std::fs::write(&path,b"one\n\nlast").unwrap();
            let source=format!(r#"module Top(output bit ok);
                int fd,n; string line="initial",saved;
                logic [127:0] wide;
                initial begin
                    fd=$fopen("{}","rb");
                    n=$fgets(line,fd);
                    ok=n==4 && line=="one\n" && $ftell(fd)==4;
                    ok &= $bits($fgets(line,fd))==32;
                    saved=line;
                    wide={{96'hx,fd}};
                    n=$fgets(line,wide);
                    ok &= n==1 && line=="\n" && saved=="one\n" && $ftell(fd)==5;
                    n=$fgets(line,fd);
                    ok &= n==4 && line=="last" && $feof(fd)!=0;
                    n=$fgets(line,fd);
                    ok &= n==0 && line=="last";
                    ok &= $rewind(fd)==0;
                    $fgets(line,fd);
                    ok &= line=="one\n";
                    $fclose(fd);
                    line="keep"; n=$fgets(line,fd);
                    ok &= n==0 && line=="keep";
                end
            endmodule"#,path.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source,Path::new("fgets_lines.sv"))],"Top").four_state(true);
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("ok")),BigUint::from(1u8));
    }

    fn fgets_captures_selected_string_once_before_descriptor_effects(sim) {
        @setup {
            let directory=tempfile::tempdir().unwrap();
            let path=directory.path().join("bytes.bin");
            std::fs::write(&path,[b'0',255,0,13,10,b'T',b'A',b'I',b'L']).unwrap();
            let source=format!(r#"module Top(output bit ok);
                int fd,n,index,selected,descriptors; string words[2];
                function automatic int next_word(); selected++; return index++; endfunction
                function automatic int descriptor(); descriptors++; index=0; return fd; endfunction
                initial begin
                    fd=$fopen("{}","rb");
                    words[0]="old"; words[1]="keep";
                    ok=$fgetc(fd)==48 && $ungetc(65,fd)==0;
                    n=$fgets(words[index++],fd);
                    ok &= n==5 && index==1 && words[1]=="keep";
                    ok &= words[0].len()==4 && words[0].getc(0)==65 && words[0].getc(1)==8'hff;
                    ok &= words[0].getc(2)==13 && words[0].getc(3)==10;
                    n=$fgets(words[next_word()],descriptor());
                    ok &= n==4 && words[1]=="TAIL" && selected==1 && descriptors==1 && index==0;
                    $fclose(fd);
                end
            endmodule"#,path.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source,Path::new("fgets_selected.sv"))],"Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("ok")),BigUint::from(1u8));
    }

    fn fgets_invalid_string_array_indices_consume_without_writing(sim) {
        @setup {
            let directory=tempfile::tempdir().unwrap();
            let path=directory.path().join("invalid-indices.txt");
            std::fs::write(&path,b"first\nsecond\nlast").unwrap();
            let source=format!(r#"module Top(output bit ok);
                int fd,n,index=2; string words[2];
                initial begin
                    fd=$fopen("{}","rb");
                    words[0]="keep0"; words[1]="keep1";
                    n=$fgets(words[2],fd);
                    ok=n==6 && words[0]=="keep0" && words[1]=="keep1";
                    n=$fgets(words[index++],fd);
                    ok &= n==7 && index==3 && words[0]=="keep0" && words[1]=="keep1";
                    n=$fgets(words[0],fd);
                    ok &= n==4 && words[0]=="last" && words[1]=="keep1";
                    $fclose(fd);
                end
            endmodule"#,path.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source,Path::new("fgets_invalid_indices.sv"))],"Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("ok")),BigUint::from(1u8));
    }

    fn fgets_long_line_and_automatic_ref_argument(sim) {
        @setup {
            let directory=tempfile::tempdir().unwrap();
            let path=directory.path().join("long.txt");
            let mut bytes=vec![b'x';8192]; bytes.extend_from_slice(b"\ntail");
            std::fs::write(&path,bytes).unwrap();
            let source=format!(r#"module Top(output bit ok);
                int fd,n; string line,saved;
                task automatic next(input int fd, ref string result, output int count);
                    count=$fgets(result,fd);
                endtask
                initial begin
                    fd=$fopen("{}","rb");
                    next(fd,line,n); saved=line;
                    ok=n==8193 && line.len()==8193 && line.getc(8192)==10;
                    next(fd,line,n);
                    ok &= n==4 && line=="tail" && saved.len()==8193;
                    next(fd,line,n);
                    ok &= n==0 && line=="tail";
                    $fclose(fd);
                end
            endmodule"#,path.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source,Path::new("fgets_long.sv"))],"Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("ok")),BigUint::from(1u8));
    }

    fn fgets_write_only_and_empty_files_preserve_destination(sim) {
        @setup {
            let directory=tempfile::tempdir().unwrap();
            let path=directory.path().join("empty.txt");
            let source=format!(r#"module Top(output bit ok);
                int fd,n; string line="keep";
                initial begin
                    fd=$fopen("{}","w"); n=$fgets(line,fd);
                    ok=n==0 && line=="keep";
                    $fclose(fd);
                    fd=$fopen("{}","r"); n=$fgets(line,fd);
                    ok &= n==0 && line=="keep" && $feof(fd)!=0;
                    $fclose(fd);
                end
            endmodule"#,path.display(),path.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source,Path::new("fgets_empty.sv"))],"Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("ok")),BigUint::from(1u8));
    }

    fn fgets_invalid_descriptor_status_and_type(sim) { @case "file_io::fgets_invalid_descriptor_status_and_type"; }
}

#[test]
fn fgets_rejects_unwritable_and_unsupported_destinations() {
    for body in [
        "string s; int n; initial n=$fgets(s);",
        "int n; initial n=$fgets(\"literal\",0);",
        "logic [127:0] buffer; int n; initial n=$fgets(buffer,0);",
        "string s; initial $fgets(,0);",
        "string s; int n; always_comb n=$fgets(s,0);",
    ] {
        let source = format!("module Top; {body} endmodule");
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(&source, Path::new("fgets_unsupported.sv"))],
            "Top",
        )
        .build_cranelift() else {
            panic!("invalid fgets accepted: {body}");
        };
        assert!(error.to_string().contains("fgets"), "{body}: {error}");
    }
}

sv_backends! {
    fn fgets_unknown_descriptor_reports_runtime_error(sim) {
        @setup {
            let source=r#"module Top(output bit done);
                logic [31:0] fd='x;
                string line="keep";
                int n;
                initial begin n=$fgets(line,fd); done=1; end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source,Path::new("fgets_unknown.sv"))],"Top").four_state(true);
        let error=sim.run_until(0).unwrap_err().to_string();
        assert!(error.contains("fgets") && error.contains("X or Z"),"{error}");
        assert_eq!(sim.get(sim.signal("done")),BigUint::from(0u8));
    }
}

#[test]
fn fgets_cannot_write_through_const_ref() {
    for source in [
        "module Top; string line; task automatic read_line(const ref string target); int n; n=$fgets(target,0); endtask initial read_line(line); endmodule",
        "module Top; string lines[2]; task automatic read_line(const ref string targets[2]); int n; n=$fgets(targets[0],0); endtask initial read_line(lines); endmodule",
    ] {
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(source, Path::new("fgets_const_ref.sv"))],
            "Top",
        )
        .build_cranelift() else {
            panic!("const ref destination accepted: {source}");
        };
        assert!(error.to_string().contains("const ref"), "{source}: {error}");
    }
}

sv_backends! {
    fn fgets_from_edge_processes_resumes_after_host_read(sim) {
        @setup {
            let directory=tempfile::tempdir().unwrap();
            let path=directory.path().join("edge.txt");
            std::fs::write(&path,b"x\ny\n").unwrap();
            let source=format!(r#"module Top(output bit ok, output int count);
                timeunit 1ps; timeprecision 1ps;
                bit clk; int fd,n; string line;
                initial begin
                    fd=$fopen("{}","r");
                    #1 clk=1; #1 clk=0; #1 clk=1; #1 $fclose(fd);
                end
                always_ff @(posedge clk) begin
                    n=$fgets(line,fd); count++;
                    if (count==1) ok=n==2 && line=="x\n";
                    else ok &= n==2 && line=="y\n";
                end
            endmodule"#,path.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source,Path::new("fgets_edge.sv"))],"Top");
        sim.run_until(4).unwrap();
        assert_eq!(sim.get(sim.signal("ok")),BigUint::from(1u8));
        assert_eq!(sim.get(sim.signal("count")),BigUint::from(2u8));
    }
}
