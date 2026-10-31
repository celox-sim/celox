use super::*;

sv_backends! {
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
}

#[test]
fn unpacked_arrays_of_strings_are_rejected() {
    let source = r#"
        module Top;
            string names [2];
            initial names[0] = "a";
        endmodule
    "#;
    let Err(error) = Simulator::from_sv_sources(vec![(source, Path::new("strings.sv"))], "Top")
        .build_cranelift()
    else {
        panic!("an unpacked array of strings is unsupported");
    };
    let error = error.to_string();
    assert!(error.contains("unpacked array of strings"), "{error}");
}
