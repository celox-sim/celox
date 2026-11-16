use super::*;

sv_backends! {
    // An omitted index skips the string's only dimension, so the body runs
    // once whatever the string's length (12.7.3).
    fn string_foreach_omitted_index_runs_once(sim) {
        @setup {
            let source = r#"
                module Top;
                    string s = "abc";
                    string empty;
                    int n, m;
                    initial begin
                        foreach (s[]) n++;
                        foreach (empty[]) m++;
                        $display("%0d %0d", n, m);
                    end
                endmodule
            "#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("omitted.sv"))], "Top");
        sim.run_until(0).unwrap();
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: b"1 1".to_vec() },
        ]);
    }
}

#[test]
fn string_array_mutations_require_a_complete_element() {
    for receiver in ["names", "names[0]", "names[0:1]"] {
        let source =
            format!("module Top; string names[2][2]; initial {receiver}.putc(0, \"a\"); endmodule");
        assert!(
            Simulator::from_sv_sources(vec![(&source, Path::new("strings.sv"))], "Top")
                .build_cranelift()
                .is_err(),
            "array-valued receiver accepted: {source}"
        );
    }
}

#[test]
fn string_arrays_in_subroutine_interfaces_remain_unsupported() {
    let source = r#"
        module Top;
            function automatic int count(input string words[2]);
                return words[0].len();
            endfunction
        endmodule
    "#;
    let Err(error) = Simulator::from_sv_sources(vec![(source, Path::new("strings.sv"))], "Top")
        .build_cranelift()
    else {
        panic!("array-valued string subroutine interfaces are unsupported");
    };
    assert!(
        error
            .to_string()
            .contains("fixed string arrays in subroutine interfaces"),
        "{error}"
    );
}

#[test]
fn fixed_string_array_state_roundtrip() {
    let source = r#"
        module Top(output bit same);
            timeunit 1ns / 1ns;
            string words[3];
            initial begin
                words[0] = {"ke", "pt"};
                words[1] = {"al", "so"};
                words[2] = {"sa", "fe"};
                #2;
                same = words[0] == "kept" && words[1] == "also" && words[2] == "safe";
            end
        endmodule
    "#;
    for four_state in [false, true] {
        let build = || {
            celox::Simulation::from_sv_sources(vec![(source, Path::new("strings.sv"))], "Top")
                .four_state(four_state)
                .build()
                .unwrap()
        };
        let mut first = build();
        first.run_until(1).unwrap();
        let checkpoint = first.save_state().unwrap();
        let mut bytes = Vec::new();
        checkpoint.write_to(&mut bytes).unwrap();
        let checkpoint = celox::StateFile::read_from(bytes.as_slice()).unwrap();
        let mut loaded = build();
        loaded.load_state(&checkpoint).unwrap();
        loaded.run_until(2).unwrap();
        let same = loaded.signal("same");
        assert_eq!(loaded.get(same), 1u8.into());
    }
}

#[test]
fn nonblocking_string_array_character_assignments_remain_unsupported() {
    let source = r#"
        module Top;
            string words[2];
            initial begin
                words[0][0] <= "x";
                words[0][1] <= "y";
            end
        endmodule
    "#;
    let Err(error) = Simulator::from_sv_sources(vec![(source, Path::new("strings.sv"))], "Top")
        .build_cranelift()
    else {
        panic!("nonblocking string array character assignments accepted");
    };
    assert!(
        error
            .to_string()
            .contains("nonblocking assignment to a string array character"),
        "{error}"
    );
}

#[test]
fn string_literals_replicated_by_a_variable_count() {
    // A replication of string literals by a count that is not constant is a
    // string replication (IEEE 1800-2023 Table 6-9, 11.4.12.2): the result
    // is that many copies, none for a zero count. A negative count, which
    // the multiplier must not be, gives none too. A constant count keeps the
    // integral replication.
    let source = r#"
        module Top(output bit ok, output int dash_len, output bit [23:0] packed_value);
            int three = 3;
            int zero = 0;
            int negative = -2;
            initial begin
                string dash = {three{"-"}};
                string pair = {three{"ab", "c"}};
                string none = {zero{"x"}};
                string below = {negative{"x"}};
                string framed = {"<", {three - 1{" "}}, ">"};
                packed_value = {3{"a"}};
                dash_len = dash.len();
                ok = dash == "---" && pair == "abcabcabc" && none == "" && below == ""
                    && framed == "<  >";
            end
        endmodule
    "#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("strings.sv"))], "Top")
                .four_state(four_state)
                .build()
                .unwrap();
        sim.run_until(0).unwrap();
        let ok = sim.signal("ok");
        let dash_len = sim.signal("dash_len");
        let packed_value = sim.signal("packed_value");
        assert_eq!(sim.get(ok), 1u8.into(), "four-state {four_state}");
        assert_eq!(sim.get(dash_len), 3u8.into(), "four-state {four_state}");
        assert_eq!(
            sim.get(packed_value),
            0x61_61_61u32.into(),
            "four-state {four_state}"
        );
    }
}
