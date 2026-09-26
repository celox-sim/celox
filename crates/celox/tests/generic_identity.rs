#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn nested_value_generics_keep_distinct_bodies(sim) {
        @case "generic_identity::nested_value_generics_keep_distinct_bodies";
    }

    fn default_and_explicit_value_generics_preserve_instance_inputs(sim) {
        @case "generic_identity::default_and_explicit_value_generics_preserve_instance_inputs";
    }
}
