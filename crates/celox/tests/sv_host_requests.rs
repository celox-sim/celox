#![cfg(feature = "systemverilog")]

use std::path::Path;

/// Every instance starts its own site numbering at zero. Interleaved
/// operation families must retain their operands after a host round trip.
macro_rules! mixed_requests {
    ($name:ident, $backend:ident) => {
        #[test]
        fn $name() {
            let directory = tempfile::tempdir().unwrap();
            let first = directory.path().join("first.txt");
            let second = directory.path().join("second.txt");
            let source = String::from(
                r#"
                module Child #(parameter int ID = 0)
                  (output logic [7:0] value, output logic ok);
                  timeunit 1ps; timeprecision 1ps;
                  logic [2047:0] filename;
                  logic [127:0] query, file_query;
                  int fd, seed, random_value;
                  initial begin
                    seed = 19;
                    query = ID == 0 ? "A=%d" : "B=%d";
                    file_query = ID == 0 ? "A_PATH=%s" : "B_PATH=%s";
                    ok = $value$plusargs(query, value);
                    ok &= $value$plusargs(file_query, filename);
                    random_value = $dist_uniform(seed, -3, 3);
                    ok &= random_value >= -3 && random_value <= 3;
                    fd = $fopen(filename, "w");
                    $fdisplay(fd, "%m:%0d", value);
                    $fclose(fd);
                    #1 value = value + 1;
                  end
                endmodule
                module Top(output logic [7:0] a_value, b_value,
                           output logic a_ok, b_ok);
                  timeunit 1ps; timeprecision 1ps;
                  Child a(a_value, a_ok);
                  Child #(.ID(1)) b(b_value, b_ok);
                endmodule
                "#,
            );
            let mut simulation = celox::Simulation::from_sv_sources(
                vec![(&source, Path::new("mixed_host_requests.sv"))],
                "Top",
            )
            .four_state(true)
            .plusargs([
                "+A=11".to_string(),
                "+B=29".to_string(),
                format!("+A_PATH={}", first.display()),
                format!("+B_PATH={}", second.display()),
            ])
            .$backend()
            .unwrap();
            simulation.run_until(2).unwrap();
            assert_eq!(simulation.get(simulation.signal("a_ok")), 1u8.into());
            assert_eq!(simulation.get(simulation.signal("b_ok")), 1u8.into());
            assert_eq!(simulation.get(simulation.signal("a_value")), 12u8.into());
            assert_eq!(simulation.get(simulation.signal("b_value")), 30u8.into());
            assert_eq!(std::fs::read_to_string(first).unwrap(), "Top.a:11\n");
            assert_eq!(std::fs::read_to_string(second).unwrap(), "Top.b:29\n");
        }
    };
}

mixed_requests!(interpreter, build_interpreter);
mixed_requests!(cranelift, build_cranelift);
mixed_requests!(wasm, build_wasm);
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
mixed_requests!(native, build_native);

#[test]
fn external_host_state_rejects_checkpoints_and_state_files() {
    let statement = r#"$fwrite(1, "output");"#;
    let source =
        format!("module Top(output bit q); initial begin {statement} q = 1; end endmodule");
    let mut simulation =
        celox::Simulation::from_sv_sources(vec![(&source, Path::new("external_state.sv"))], "Top")
            .build_interpreter()
            .unwrap();
    assert!(matches!(
        simulation.checkpoint(),
        Err(celox::CheckpointError::ExternalSystemTasks)
    ));
    assert!(
        simulation
            .save_state()
            .unwrap_err()
            .to_string()
            .contains("SystemVerilog file I/O")
    );
}
