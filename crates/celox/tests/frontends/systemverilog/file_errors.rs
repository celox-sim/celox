use crate::*;

sv_backends! {
    fn ferror_success_clears_description(sim) { @case "file_io::ferror_success_clears_description"; }

    fn ferror_tracks_status_and_clears_outputs_on_success(sim) {
        @setup {
            let directory=tempfile::tempdir().unwrap();
            let path=directory.path().join("input.txt");
            std::fs::write(&path,b"x").unwrap();
            let source=format!(r#"module Top(output bit ok);
                int fd,other,n,code,again; string text="old",saved; logic [639:0] packed_text='1;
                initial begin
                    fd=$fopen("{}","r"); other=$fopen("{}","r");
                    code=$ferror({{96'hx,fd}},text);
                    ok=code==0 && text=="" && $bits($ferror(fd,text))==32;
                    n=$fseek(fd,-1,0);
                    code=$ferror(fd,text); saved=text;
                    again=$ferror(fd,text);
                    ok &= n==-1 && code!=0 && text.len()>0 && again==code && text==saved;
                    n=$fgetc(other);
                    again=$ferror(fd,packed_text);
                    ok &= n==120 && again==code && packed_text!=0;
                    n=$fgetc(fd); code=$ferror(fd,text);
                    ok &= n==120 && code==0 && text=="";
                    n=$fgetc(fd); code=$ferror(fd,text);
                    ok &= n==-1 && code==0 && text=="";
                    packed_text='1; code=$ferror(fd,packed_text);
                    ok &= code==0 && packed_text==0;
                    $fclose(fd); code=$ferror(fd,text);
                    ok &= code!=0 && text.len()>0;
                    $fclose(other);
                end
            endmodule"#,path.display(),path.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source,Path::new("ferror_status.sv"))],"Top").four_state(true);
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("ok")),BigUint::from(1u8));
    }

    fn ferror_output_indices_follow_descriptor_effects_once(sim) {
        @setup {
            let directory=tempfile::tempdir().unwrap();
            let path=directory.path().join("input.txt");
            std::fs::write(&path,b"x").unwrap();
            let source=format!(r#"module Top(output bit ok);
                int fd,n,index=0,calls; string words[2],result;
                function automatic int descriptor(); calls++; index=1; return fd; endfunction
                task automatic query(input int descriptor, ref string result, output int code);
                    code=$ferror(descriptor,result);
                endtask
                initial begin
                    fd=$fopen("{}","r"); words[0]="keep"; words[1]="old";
                    n=$ferror(descriptor(),words[index++]);
                    ok=n==0 && words[0]=="keep" && words[1]=="" && index==2 && calls==1;
                    if (!ok) $fatal(1,"selection n=%0d index=%0d calls=%0d w0=%s w1=%s",n,index,calls,words[0],words[1]);
                    n=$ferror(fd,words[2]);
                    ok &= n==0 && words[0]=="keep";
                    n=$fseek(fd,-1,0); query(fd,result,n);
                    ok &= n!=0 && result.len()>0;
                    if (!ok) $fatal(1,"query n=%0d text=%s",n,result);
                    $fclose(fd);
                end
            endmodule"#,path.display());
        }
        @build celox::Simulation::from_sv_sources(vec![(&source,Path::new("ferror_outputs.sv"))],"Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("ok")),BigUint::from(1u8));
    }
}

#[test]
fn ferror_rejects_unwritable_or_small_outputs() {
    for body in [
        "string text; initial $ferror(0);",
        "initial $ferror(0,\"literal\");",
        "int n; initial $ferror(0,n);",
        "string text; always_comb $ferror(0,text);",
    ] {
        let source = format!("module Top; {body} endmodule");
        let Err(error) = celox::Simulation::from_sv_sources(
            vec![(&source, Path::new("ferror_invalid.sv"))],
            "Top",
        )
        .build_cranelift() else {
            panic!("accepted {body}");
        };
        assert!(error.to_string().contains("ferror"), "{error}");
    }
    let source = "module Top; string text; task automatic query(const ref string dest); int n; n=$ferror(0,dest); endtask initial query(text); endmodule";
    let Err(error) =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("ferror_const.sv"))], "Top")
            .build_cranelift()
    else {
        panic!("const ref accepted");
    };
    assert!(error.to_string().contains("const ref"), "{error}");
}

sv_backends! {
    fn ferror_unknown_descriptor_is_runtime_error(sim) {
        @setup { let source="module Top(output bit done); logic [31:0] fd='x; string text; initial begin $ferror(fd,text); done=1; end endmodule"; }
        @build celox::Simulation::from_sv_sources(vec![(source,Path::new("ferror_unknown.sv"))],"Top").four_state(true);
        let error=sim.run_until(0).unwrap_err().to_string();
        assert!(error.contains("ferror") && error.contains("X or Z"),"{error}");
        assert_eq!(sim.get(sim.signal("done")),BigUint::from(0u8));
    }
}
