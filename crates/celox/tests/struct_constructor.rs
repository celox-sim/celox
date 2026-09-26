#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn test_struct_constructor_comb_assignment(sim) {
        @ignore_on(sv);
        @case "struct_constructor::test_struct_constructor_comb_assignment";
    }

    fn test_struct_constructor_member_width_adjustment(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "struct_constructor::test_struct_constructor_member_width_adjustment";
    }
}
