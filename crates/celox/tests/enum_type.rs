#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn test_enum_case_match(sim) {
        @case "enum_type::test_enum_case_match";
    }

    fn test_enum_ff_state_machine(sim) {
        @case "enum_type::test_enum_ff_state_machine";
    }

    // Enum-typed variables can be assigned from logic inputs
    // and compared against enum members. Veryl emits the assignment of
    // the logic input to the enum without a cast, which SystemVerilog
    // requires (IEEE 1800-2023 6.19.3; the `sv_enum_conversion`
    // limitation), so the emitted design is not run.
    fn test_enum_assign_and_compare(sim) {
        @omit_sv;
        @case "enum_type::test_enum_assign_and_compare";
    }
}
