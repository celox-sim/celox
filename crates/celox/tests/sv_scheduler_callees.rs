//! The compiled scheduler of a native image calls the larger bodies it runs
//! (the settle, the FF domains) as functions of their own instead of
//! inlining them (`SirPass::SchedulerCallees`). A design simulates the same
//! with and without them, and from a saved native image.
use celox::{RuntimeEvent, Simulation, SirPass};
use std::path::Path;

/// A combinational network wide enough that the settle is a callee, two
/// clock domains whose FF units are, and a testbench that prints a digest.
fn source() -> String {
    let mut source = String::from(
        "`timescale 1ns/1ns\nmodule Top;\n  bit clk = 0, slow = 0;\n  always #1 clk = ~clk;\n  always #3 slow = ~slow;\n",
    );
    for index in 0..72 {
        source += &format!("  logic [31:0] r{index} = 32'd{index};\n  logic [31:0] w{index};\n");
    }
    for index in 0..72 {
        let next = (index + 1) % 72;
        let other = (index + 7) % 72;
        source += &format!(
            "  assign w{index} = (r{index} ^ (r{next} << 3)) + (r{other} >> 2) + (r{index}[0] ? r{next} : 32'h9e37_79b9);\n"
        );
    }
    source += "  always_ff @(posedge clk) begin\n";
    for index in 0..36 {
        source += &format!("    r{index} <= w{index} ^ {{w{index}[15:0], w{index}[31:16]}};\n");
    }
    source += "  end\n  always_ff @(posedge slow) begin\n";
    for index in 36..72 {
        source += &format!("    r{index} <= w{index} + w{};\n", index - 36);
    }
    source += "  end\n  logic [31:0] digest;\n  always_comb begin\n    digest = 0;\n";
    for index in 0..72 {
        source += &format!("    digest = digest ^ w{index};\n");
    }
    source += "  end\n  initial begin\n    repeat (6) begin\n      #10 $display(\"%0t %h\", $time, digest);\n    end\n    $finish;\n  end\nendmodule\n";
    source
}

fn lines<B: celox::SimBackend>(mut sim: Simulation<B>) -> Vec<String> {
    sim.run_until(100).unwrap();
    sim.drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            RuntimeEvent::Display { message } => {
                Some(String::from_utf8_lossy(&message).into_owned())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn scheduler_callees_simulate_as_inlined_bodies() {
    let source = source();
    let builder =
        || Simulation::from_sv_sources(vec![(source.as_str(), Path::new("callees.sv"))], "Top");
    let reference = lines(builder().build_interpreter().unwrap());
    assert_eq!(reference.len(), 6, "{reference:?}");
    assert_eq!(lines(builder().build_native().unwrap()), reference);
    assert_eq!(
        lines(
            builder()
                .disable_pass(SirPass::SchedulerCallees)
                .build_native()
                .unwrap()
        ),
        reference
    );
    let image = builder().compile_native().unwrap().into_program_image();
    let image =
        celox::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
            .unwrap();
    assert_eq!(
        lines(
            Simulation::from_sv_sources(Vec::new(), "Top")
                .build_native_from_image(image)
                .unwrap()
        ),
        reference
    );
}

/// A callee that calls a DPI-C import the design was elaborated without
/// fails with the import's run-time error, as the inlined body does.
#[test]
fn outlined_calls_of_unlinked_imports_are_runtime_errors() {
    let mut source = String::from(
        "`timescale 1ns/1ns\nmodule Top;\n  import \"DPI-C\" function int unlinked_sum(input int x, input int y);\n  bit clk = 0;\n  always #1 clk = ~clk;\n  int y;\n",
    );
    for index in 0..48 {
        source += &format!("  logic [31:0] r{index} = 32'd{index};\n");
    }
    source += "  always_ff @(posedge clk) begin\n";
    for index in 0..48 {
        source += &format!(
            "    r{index} <= (r{index} ^ (r{} << 3)) + (r{} >> 2);\n",
            (index + 1) % 48,
            (index + 7) % 48
        );
    }
    source += "    if (r0[3]) y <= unlinked_sum(r1, r2);\n  end\nendmodule\n";
    let error =
        Simulation::from_sv_sources(vec![(source.as_str(), Path::new("unlinked.sv"))], "Top")
            .allow_unlinked_dpi_for_elaboration()
            .build_native()
            .unwrap()
            .run_until(200)
            .expect_err("the call of an unlinked import fails");
    assert!(
        error.to_string().contains(
            "DPI-C import `unlinked_sum` was elaborated without its C body (allow_unlinked_dpi_for_elaboration) and cannot be called"
        ),
        "{error}"
    );
}
