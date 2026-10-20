sv_backends! {
    fn sscanf_fields(sim) { @case "scan::sscanf_integral_fields_and_failure_counts"; }
    fn sscanf_outputs(sim) { @case "scan::sscanf_multiple_outputs_and_packed_input"; }
    fn sscanf_unknowns(sim) { @case "scan::sscanf_radix_unknowns_and_unknown_input"; }
}
