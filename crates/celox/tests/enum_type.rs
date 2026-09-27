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
    // and compared against enum members.
    fn test_enum_assign_and_compare(sim) {
        @case "enum_type::test_enum_assign_and_compare";
    }
}
