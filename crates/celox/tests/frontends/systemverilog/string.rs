use super::*;

sv_backends! {
    fn string_fixed_array_mutations(sim) { @case "string::fixed_array_mutations"; }
    fn string_fixed_array_mutating_methods(sim) { @case "string::fixed_array_mutating_methods"; }
    fn string_fixed_array_mutation_indices(sim) { @case "string::fixed_array_mutation_indices"; }
    fn string_fixed_array_mutations_invalid_indices(sim) { @case "string::fixed_array_mutations_invalid_indices"; }

    fn string_fixed_array_pattern_rows(sim) { @case "string::fixed_array_pattern_rows"; }
    fn string_fixed_array_continuous_elements(sim) { @case "string::fixed_array_continuous_elements"; }

    fn string_fixed_array_loops_and_copy(sim) { @case "string::fixed_array_loops_and_copy"; }
    fn string_fixed_array_multidimensional_slices(sim) { @case "string::fixed_array_multidimensional_slices"; }
    fn string_fixed_array_invalid_indices(sim) { @case "string::fixed_array_invalid_indices"; }
    fn string_fixed_array_dynamic_indices(sim) { @case "string::fixed_array_dynamic_indices"; }
    fn string_fixed_array_unknown_indices(sim) { @case "string::fixed_array_unknown_indices"; }
    fn string_fixed_array_survives_heap_collections(sim) { @case "string::fixed_array_survives_heap_collections"; }
    fn string_fixed_array_ports_and_local_lifetime(sim) { @case "string::fixed_array_ports_and_local_lifetime"; }
    fn string_assignment_and_display(sim) { @case "string::assignment_and_display"; }
    fn string_comparisons(sim) { @case "string::comparisons"; }
    fn string_concatenation_and_replication(sim) { @case "string::concatenation_and_replication"; }
    fn string_character_access(sim) { @case "string::character_access"; }
    fn string_methods(sim) { @case "string::methods"; }
    fn string_characters_are_signed_bytes(sim) { @case "string::characters_are_signed_bytes"; }
    fn string_casts_from_integral_values(sim) { @case "string::casts_from_integral_values"; }
    fn strings_across_clock_edges(sim) { @case "string::strings_across_clock_edges"; }
}

sv_backends! {
    fn strings_survive_heap_collections(sim) { @case "string::strings_survive_heap_collections"; }
}

sv_backends! {
    fn string_parameters_keep_their_text(sim) { @case "string::string_parameters_keep_their_text"; }
    fn string_foreach_iterates_the_characters(sim) { @case "string::foreach_iterates_the_characters"; }
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
