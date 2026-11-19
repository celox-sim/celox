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
