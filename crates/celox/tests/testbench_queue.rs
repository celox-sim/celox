#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {
    fn queue_aggregate_and_selected_returns(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "tb_queue::aggregate_and_selected_returns";
    }
    fn queue_operations(sim) {
        // The shared SystemVerilog emitter does not support native queue handles.
        // The legacy Veryl adapter does not execute native testbenches.
        @omit_veryl;
        @ignore_on(sv);
        @case "tb_queue::operations";
    }
    fn queue_four_state_values(sim) {
        // The legacy Veryl adapter does not execute native testbenches.
        @omit_veryl;
        @ignore_on(sv);
        @case "tb_queue::four_state_values";
    }
    fn queue_shared_processes(sim) {
        // The legacy Veryl adapter does not execute native testbenches.
        @omit_veryl;
        @ignore_on(sv);
        @case "tb_queue::shared_processes";
    }
}

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
