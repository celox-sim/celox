#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Tests that two different packages can instantiate the same generic module,
    // each getting a unique ModuleId.
    fn test_generic_module_instantiation(sim) {
        @ignore_on(sv);
        @case "compare_matrix::test_generic_module_instantiation";
    }

    // Tests proto package function resolution (E::gt → IntElement::gt).
    fn test_proto_function_basic(sim) {
        @ignore_on(sv);
        @case "compare_matrix::test_proto_function_basic";
    }

    // Tests proto constant (E::max_value) resolution.
    fn test_proto_const_max_value(sim) {
        @ignore_on(sv);
        @case "compare_matrix::test_proto_const_max_value";
    }

    // Tests the compare matrix scoring module (CompareMatrixStage1CM).
    // Input 4 values, verify scores reflect sorted order.
    fn test_compare_matrix_stage1cm(sim) {
        @ignore_on(veryl, sv);
        @case "compare_matrix::test_compare_matrix_stage1cm";
    }

    // Tests the compare matrix selector module (through wrapper, verifying parameter forwarding).
    fn test_compare_matrix_selector(sim) {
        @ignore_on(veryl, sv);
        @case "compare_matrix::test_compare_matrix_selector";
    }

    // Tests full sorting via CompareMatrixStage1 (scoring + selection through wrapper chain).
    // Input unsorted values, output sorted ascending.
    fn test_compare_matrix_stage1_sort(sim) {
        @ignore_on(veryl, sv);
        @case "compare_matrix::test_compare_matrix_stage1_sort";
    }

    // Tests the compare matrix merger.
    // Two sorted ascending arrays in, one merged sorted ascending array out.
    fn test_compare_matrix_merger(sim) {
        @ignore_on(veryl, sv);
        @case "compare_matrix::test_compare_matrix_merger";
    }
}
