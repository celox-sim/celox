#![cfg(feature = "systemverilog")]

use celox::{SimBackend, Simulation};
use std::path::Path;

const SOURCE: &str = r#"module Top(output int count,index,order,first,second,mismatch,eof,incomplete,real_index,
    output logic[63:0] parsed,negative_zero,kept,alias_value,
    output logic[31:0] rounded,overflow);
    real value,keep; real real_values[0:1]; shortreal narrow; int values[0:1];
    function automatic [127:0] source(); order=order*10+1;source="7 1.25 8";endfunction
    function automatic [63:0] format_string(); order=order*10+2;format_string="%d%f%d";endfunction
    function automatic int next_index();order=order*10+3;next_index=index;index+=1;endfunction
    function automatic int next_real_index(); next_real_index=real_index;real_index+=1;endfunction
    initial begin
        index=0;order=0;real_index=0;
        count=$sscanf(source(),format_string(),values[next_index()],value,values[next_index()]);
        first=values[0];second=values[1];parsed=$realtobits(value);
        count=count+$sscanf("3.25 -4.5","%f%f",real_values[next_real_index()],real_values[next_real_index()])-2;
        count=count+$sscanf("1.0000000596046447753906250000001","%f",narrow)-1;
        rounded=$shortrealtobits(narrow);
        count=count+$sscanf("-0","%g",value)-1;negative_zero=$realtobits(value);
        count=count+$sscanf("1e9999","%e",narrow)-1;overflow=$shortrealtobits(narrow);
        keep=9.5;mismatch=$sscanf("bad","%f",keep);eof=$sscanf("","%e",keep);
        incomplete=$sscanf("1e+","%g",keep);kept=$realtobits(keep);
        count=count+$sscanf("2.5","%f",value,value)-1;alias_value=$realtobits(value);
    end endmodule"#;

fn check<B: SimBackend>(mut sim: Simulation<B>) {
    sim.run_until(0).unwrap();
    for (name, expected) in [
        ("count", 3u64),
        ("index", 2),
        ("real_index", 2),
        ("order", 1233),
        ("first", 7),
        ("second", 8),
        ("mismatch", 0),
        ("eof", u32::MAX as u64),
        ("incomplete", 0),
        ("parsed", 1.25f64.to_bits()),
        ("negative_zero", (-0.0f64).to_bits()),
        ("kept", 9.5f64.to_bits()),
        ("alias_value", 2.5f64.to_bits()),
        ("rounded", 0x3f800001),
        ("overflow", f32::INFINITY.to_bits() as u64),
    ] {
        let signal = sim.signal(name);
        assert_eq!(sim.get(signal), expected.into(), "{name}");
    }
    let real_values = sim.signal("real_values");
    let expected = num_bigint::BigUint::from(3.25f64.to_bits())
        | (num_bigint::BigUint::from((-4.5f64).to_bits()) << 64);
    assert_eq!(sim.get(real_values), expected);
}

macro_rules! backends {
    ($($name:ident:$build:ident),* $(,)?) => {$ (
        #[test]
        fn $name() {
            check(Simulation::from_sv_sources(vec![(SOURCE,Path::new("scan_real.sv"))],"Top").four_state(true).$build().unwrap());
        }
    )*};
}
backends!(interpreter:build_interpreter, cranelift:build_cranelift, wasm:build_wasm);
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
backends!(native:build_native);

#[test]
fn fscanf_real_lookahead_eof_and_failed_field_preserve_output() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("real.txt");
    std::fs::write(&input, b"1.25Q -0 2.5E+1 1e+X").unwrap();
    let source = format!(
        r#"module Top(output int count,ch,eof,failed,tail,ended,output logic[63:0] value_bits,zero_bits,kept,output logic[31:0] narrow_bits);
        int fd;real value,zero;shortreal narrow;
        initial begin fd=$fopen("{}","r");
            count=$fscanf(fd,"%4f%c%g%e",value,ch,zero,narrow);
            value_bits=$realtobits(value);zero_bits=$realtobits(zero);narrow_bits=$shortrealtobits(narrow);
            failed=$fscanf(fd,"%f",value);tail=$fgetc(fd);kept=$realtobits(value);
            eof=$fscanf(fd,"%e",value);ended=$feof(fd);$fclose(fd);
        end endmodule"#,
        input.display()
    );
    fn verify<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(0).unwrap();
        for (name, expected) in [
            ("count", 4u64),
            ("ch", b'Q' as u64),
            ("failed", 0),
            ("tail", b'X' as u64),
            ("eof", u32::MAX as u64),
            ("ended", 1),
            ("value_bits", 1.25f64.to_bits()),
            ("zero_bits", (-0.0f64).to_bits()),
            ("narrow_bits", 25.0f32.to_bits() as u64),
            ("kept", 1.25f64.to_bits()),
        ] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), expected.into(), "{name}");
        }
    }
    let build = || {
        Simulation::from_sv_sources(vec![(&source, Path::new("fscanf_real.sv"))], "Top")
            .four_state(true)
    };
    verify(build().build_interpreter().unwrap());
    verify(build().build_cranelift().unwrap());
    verify(build().build_wasm().unwrap());
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    verify(build().build_native().unwrap());
}

#[test]
fn mismatched_destination_types_are_diagnosed() {
    for (declaration, format, expected) in [
        ("int value;", "%f", "real or shortreal destination"),
        ("real value;", "%d", "integral destination"),
    ] {
        let source = format!(
            "module Top;{declaration}int n;initial n=$sscanf(\"1\",\"{format}\",value);endmodule"
        );
        let mut sim =
            Simulation::from_sv_sources(vec![(&source, Path::new("scan_type.sv"))], "Top")
                .build_interpreter()
                .unwrap();
        assert!(format!("{:?}", sim.run_until(0).unwrap_err()).contains(expected));
    }
}
