#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn test_single_bit_aggregate_members(sim) {
        @case "struct_constructor::test_single_bit_aggregate_members";
    }

    fn test_single_bit_struct_ff(sim) {
        @case "struct_constructor::test_single_bit_struct_ff";
    }

    fn test_struct_constructor_comb_assignment(sim) {
        @case "struct_constructor::test_struct_constructor_comb_assignment";
    }

    fn test_struct_constructor_member_width_adjustment(sim) {
        @omit_veryl;
        @case "struct_constructor::test_struct_constructor_member_width_adjustment";
    }
}
