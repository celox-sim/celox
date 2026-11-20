//! Native `$tb::queue` testbench values (the shared cases run from
//! `tb_queue.vtest`; these cover the compiled-testbench runtime itself).

#[test]
fn compound_four_state_queue_arguments_do_not_silently_lose_unknowns() {
    let result = celox::Simulator::builder(
        r#"#[test(Top)] module Top {
            gen element: type = logic<4>;
            var q: $tb::queue::<element>;
            var data: element;
            initial { q.push_back(data | 4'b0000); $finish(); }
        }"#,
        "Top",
    )
    .four_state(true)
    .build();
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("unsupported four-state expression must be diagnosed"),
    };
    assert!(
        error
            .to_string()
            .contains("four-state queue argument expression"),
        "{error}"
    );
}

#[test]
fn rerunning_compiled_testbench_resets_queue_handles() {
    let mut sim = celox::Simulator::builder(
        r#"#[test(Top)] module Top {
            var q: $tb::queue::<u8>;
            initial {
                $assert(q.size() == 0);
                q.push_back(7);
                $finish();
            }
        }"#,
        "Top",
    )
    .build()
    .unwrap();
    let program = celox::testbench::compile_initial_testbench(&sim).unwrap();
    for _ in 0..2 {
        assert_eq!(
            celox::testbench::run_compiled_testbench(&mut sim, &program),
            celox::TestResult::Pass
        );
    }
}

#[test]
fn empty_logic_queue_coerces_unknowns_in_two_state_simulation() {
    let result = celox::Simulator::builder(
        r#"#[test(Top)] module Top {
            gen element: type = logic<4>;
            var q: $tb::queue::<element>;
            var value: element;
            initial {
                value = q.pop_back();
                $assert(value == 0);
                $finish();
            }
        }"#,
        "Top",
    )
    .four_state(false)
    .run_test()
    .unwrap();
    assert_eq!(result, celox::TestResult::Pass);
}

#[test]
fn queue_reads_invalid_indices_as_unknown_without_crossing_rows() {
    let mut sim = celox::Simulator::builder(
        r#"#[test(Top)] module Top {
            gen element: type = logic<8>;
            var q: $tb::queue::<element>;
            var data: element[2, 2];
            var row: logic<2>;
            var col: logic<2>;
            var valid: element;
            var invalid_column: element;
            var invalid_row: element;
            var unknown_column: element;
            initial {
                data[0][0] = 8'h11;
                data[0][1] = 8'h22;
                data[1][0] = 8'h33;
                data[1][1] = 8'h44;
                row = 0;
                col = 1;
                q.push_back(data[row][col]);
                valid = q.pop_front();
                col = 2;
                q.push_back(data[row][col]);
                invalid_column = q.pop_front();
                row = 2;
                col = 0;
                q.push_back(data[row][col]);
                invalid_row = q.pop_front();
                row = 0;
                col = 2'b0x;
                q.push_back(data[row][col]);
                unknown_column = q.pop_front();
                $finish();
            }
        }"#,
        "Top",
    )
    .four_state(true)
    .build()
    .unwrap();
    let program = celox::testbench::compile_initial_testbench(&sim).unwrap();
    assert_eq!(
        celox::testbench::run_compiled_testbench(&mut sim, &program),
        celox::TestResult::Pass
    );
    assert_eq!(
        sim.get_four_state(sim.signal("valid")),
        (0x22u32.into(), 0u32.into())
    );
    for signal in ["invalid_column", "invalid_row", "unknown_column"] {
        assert_eq!(
            sim.get_four_state(sim.signal(signal)),
            (0xffu32.into(), 0xffu32.into()),
            "{signal}"
        );
    }
}
