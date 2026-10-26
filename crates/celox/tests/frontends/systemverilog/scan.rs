sv_backends! {
    fn sscanf_string_format(sim) { @case "scan::sscanf_string_format_expression"; }
    fn sscanf_discarded_return(sim) { @case "scan::sscanf_string_inputs_and_discarded_return"; }
    fn sscanf_real_fields(sim) { @case "scan::sscanf_real_fields_width_and_suppression"; }
    fn sscanf_fields(sim) { @case "scan::sscanf_integral_fields_and_failure_counts"; }
    fn sscanf_outputs(sim) { @case "scan::sscanf_multiple_outputs_and_packed_input"; }
    fn sscanf_unknowns(sim) { @case "scan::sscanf_radix_unknowns_and_unknown_input"; }
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn sscanf_strings_native_image() {
    for case in [
        "scan::sscanf_string_inputs_and_discarded_return",
        "scan::sscanf_string_format_expression",
    ] {
        crate::suite::run_case(case, "native-image");
    }
}
