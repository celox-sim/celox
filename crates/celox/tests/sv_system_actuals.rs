#![cfg(feature = "systemverilog")]
use std::path::Path;
const SOURCE: &str = r#"
module Top(output logic ok);
  int calls;
  int seeds[0:3];
  longint wide;
  int value, found;
  function automatic int index();
    calls = calls + 1;
    return calls - 1;
  endfunction
  initial begin
    seeds[0]=17; seeds[1]=91; seeds[2]=92; seeds[3]=93; calls=0;
    value = $dist_uniform(seeds[index()], -17, 23);
    ok = calls == 1 && value == -17 && seeds[0] == 1174174;
    ok &= seeds[1] == 91 && seeds[2] == 92 && seeds[3] == 93;
    wide = 64'h12345678abcdef01;
    value = $dist_uniform(wide, 7, 7);
    ok &= value == 7 && wide == 64'h12345678abcdef01;
    value = $dist_uniform(wide, 9, -9);
    ok &= value == 9 && wide == 64'h12345678abcdef01;
    calls = 0;
    found = $value$plusargs("number=%d", seeds[index()]);
    ok &= calls == 1 && found == 1 && seeds[0] == 42 && seeds[1] == 91;
    calls = 0;
    found = $value$plusargs("missing=%d", seeds[index()]);
    ok &= calls == 1 && found == 0 && seeds[0] == 42 && seeds[1] == 91;
  end
endmodule
"#;
macro_rules! actuals_backend {
    ($name:ident,$backend:ident) => {
        #[test]
        fn $name() {
            let mut sim = celox::Simulation::from_sv_sources(
                vec![(SOURCE, Path::new("system_actuals.sv"))],
                "Top",
            )
            .plusargs(["+number=42"])
            .$backend()
            .unwrap();
            sim.run_until(0).unwrap();
            let ok = sim.signal("ok");
            assert_eq!(sim.get(ok), 1u8.into());
        }
    };
}
actuals_backend!(system_actuals_interpreter, build_interpreter);
actuals_backend!(system_actuals_cranelift, build);
#[cfg(target_arch = "x86_64")]
actuals_backend!(system_actuals_native, build_native);
actuals_backend!(system_actuals_wasm, build_wasm);
