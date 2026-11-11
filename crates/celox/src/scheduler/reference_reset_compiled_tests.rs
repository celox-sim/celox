#[test]
fn reference_process_reset_is_stable_between_compiled_runs() {
    let directory = tempfile::tempdir().unwrap();
    let memory = directory.path().join("reset-memory.hex");
    std::fs::write(&memory, "a5\n").unwrap();
    let source = r#"`timescale 1ns/1ns
module Top;
    bit [7:0] q = 0, releases = 0;
    bit clk = 0, reset_n = 0;
    wire reset = !reset_n;
    logic [7:0] mem [0:0];
    initial begin
        // #0 keeps this process in the reference loop without host state.
        #0;
        $readmemh("MEMFILE", mem);
        #3 reset_n = 1;
        #13 reset_n = 0;
        #2 reset_n = 1;
    end
    always_ff @(posedge reset_n) releases <= releases + 1;
    always #1 clk = !clk;
    always_ff @(posedge clk or posedge reset)
        if (reset) q <= 0;
        else q <= q + 1;
endmodule"#
        .replace(
            "\"MEMFILE\"",
            &serde_json::to_string(&memory.to_string_lossy()).unwrap(),
        );
    for four_state in [false, true] {
        let mut simulation = crate::Simulation::from_sv_sources(
            vec![(source.as_str(), std::path::Path::new("reference-reset.sv"))],
            "Top",
        )
        .four_state(four_state)
        .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
        .build_native()
        .unwrap();
        simulation.run_until(24).unwrap();
        let q = simulation.signal("q");
        assert_eq!(simulation.get(q), num_bigint::BigUint::from(3u8));
        let releases = simulation.signal("releases");
        assert_eq!(simulation.get(releases), num_bigint::BigUint::from(2u8));
        let mem = simulation.signal("mem");
        assert_eq!(simulation.get(mem), num_bigint::BigUint::from(0xa5u8));
        let stats = simulation.scheduler_stats();
        assert!(stats.compiled_slots > 0, "{stats:?}");
        assert!(
            stats.checked_steps > 0,
            "compiled runs must match the reference: {stats:?}"
        );
    }
}
