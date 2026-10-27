#![cfg(feature = "systemverilog")]
use celox::{SimBackend, Simulation};
use std::path::Path;
fn exercise<B: SimBackend>(mut simulation: Simulation<B>) {
    simulation.run_until(1).unwrap();
    for (name, expected) in [
        ("count", 3u32),
        ("a", 12),
        ("b", 255),
        ("c", b'Q' as u32),
        ("index", 2),
        ("order", 1233),
        ("first", 7),
        ("second", 8),
        ("aliased", 9),
        ("alias_count", 1),
        ("eof", u32::MAX),
        ("failure", 0),
        ("partial", 1),
        ("keep", 77),
        ("two_state", 0),
    ] {
        let signal = simulation.signal(name);
        assert_eq!(simulation.get(signal), expected.into(), "{name}");
    }
    let wide = simulation.signal("wide");
    assert_eq!(
        simulation.get(wide),
        (num_bigint::BigUint::from(1u8) << 68) - num_bigint::BigUint::from(1u8)
    );
    let unknown = simulation.signal("unknown");
    assert_eq!(
        simulation.get_four_state(unknown),
        (16u8.into(), 15u8.into())
    );
}
const SOURCE: &str = r#"module Top(output int count,a,b,c,index,order,first,second,aliased,alias_count,eof,failure,partial,keep,
    output logic [67:0] wide,output logic [7:0] unknown,output bit [7:0] two_state);
    int values[0:1];
    string source = " 12,99 ffQ";
    function automatic [31:0] scan_source(); order=order*10+1; scan_source="7 8"; endfunction
    function automatic [31:0] scan_format(); order=order*10+2; scan_format="%d%d"; endfunction
    function automatic int next_index(); order=order*10+3; next_index=index; index+=1; endfunction
    initial begin
        count=$sscanf(source,"%2d,%*d %h%c",a,b,c);
        index=0; order=0; count=count+$sscanf(scan_source(),scan_format(),values[next_index()],values[next_index()])-2;
        first=values[0];second=values[1];
        aliased=1;alias_count=$sscanf("9","%d",aliased,aliased);
        count=count+$sscanf("fffffffffffffffff","%h",wide)-1;
        count=count+$sscanf("1z","%h",unknown)-1;
        count=count+$sscanf("z","%h",two_state)-1;
        keep=77;eof=$sscanf("","%d",keep);failure=$sscanf("bad","%d",keep);
        partial=$sscanf("5bad","%d%d",a,keep);
        a=12;
    end endmodule"#;
macro_rules! backend {
    ($name:ident,$build:ident) => {
        #[test]
        fn $name() {
            exercise(
                Simulation::from_sv_sources(vec![(SOURCE, Path::new("scan.sv"))], "Top")
                    .four_state(true)
                    .$build()
                    .unwrap(),
            );
        }
    };
}
backend!(interpreter, build_interpreter);
backend!(cranelift, build_cranelift);
backend!(wasm, build_wasm);
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
backend!(native, build_native);

#[test]
fn fscanf_preserves_mismatch_lookahead_and_allows_checkpoint() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input.txt");
    std::fs::write(&path, b"12 34Q 7").unwrap();
    let source = format!(
        r#"module Top(output int n,a,b,c,fail,last,eof);int fd;string fmt="%d";
        initial begin fd=$fopen("{}","r");n=$fscanf({{32'h12345678,fd}},fmt,a);
        n=n+$fscanf({{96'h123456789abcdef123456789,fd}},"%d",b);
        fail=$fscanf(fd,"%d",b);n=n+$fscanf(fd,"%c%d",c,last);
        eof=$fscanf(fd,"%d",b);end endmodule"#,
        path.display()
    );
    fn check<B: SimBackend>(mut simulation: Simulation<B>) {
        simulation.run_until(1).unwrap();
        for (name, value) in [
            ("n", 4u32),
            ("b", 34),
            ("a", 12),
            ("c", 81),
            ("last", 7),
            ("fail", 0),
            ("eof", u32::MAX),
        ] {
            let signal = simulation.signal(name);
            assert_eq!(simulation.get(signal), value.into(), "{name}");
        }
        simulation.checkpoint().unwrap();
        simulation.save_state().unwrap();
    }
    let build = || Simulation::from_sv_sources(vec![(&source, Path::new("scan.sv"))], "Top");
    check(build().build_interpreter().unwrap());
    check(build().build_cranelift().unwrap());
    check(build().build_wasm().unwrap());
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    check(build().build_native().unwrap());
}
#[test]
fn pure_sscanf_allows_checkpoint_and_untimed_build_is_rejected() {
    let source =
        "module Top(output int result);int a;initial result=$sscanf(\"12\",\"%d\",a);endmodule";
    let mut simulation = Simulation::from_sv_sources(vec![(source, Path::new("scan.sv"))], "Top")
        .build_interpreter()
        .unwrap();
    simulation.run_until(1).unwrap();
    assert!(simulation.checkpoint().is_ok());
    assert!(simulation.save_state().is_ok());
    let error = celox::Simulator::from_sv_sources(vec![(source, Path::new("scan.sv"))], "Top")
        .build_interpreter()
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            celox::SimulatorErrorKind::ProcessRequiresSimulation(_)
        ),
        "{error}"
    );
}
#[test]
fn rejected_formats_are_explicit() {
    for (format, expected) in [
        ("%s", "string output"),
        ("%t", "$timeformat"),
        ("%q", "unsupported scan conversion"),
    ] {
        let source = format!(
            r#"module Top;int a,n;initial n=$sscanf("1","{}",a);endmodule"#,
            format
        );
        let mut simulation =
            Simulation::from_sv_sources(vec![(&source, Path::new("scan.sv"))], "Top")
                .build_interpreter()
                .unwrap();
        assert!(format!("{:?}", simulation.run_until(1).unwrap_err()).contains(expected));
    }
}

#[test]
fn fscanf_on_standard_input_allows_snapshots_before_execution() {
    let source = r#"module Top; int n, value;
        initial begin #1; n = $fscanf(32'h80000000, "%d", value); end
    endmodule"#;
    let mut simulation =
        Simulation::from_sv_sources(vec![(source, Path::new("scan_stdin.sv"))], "Top")
            .build_interpreter()
            .unwrap();
    simulation.checkpoint().unwrap();
    simulation.save_state().unwrap();
}
