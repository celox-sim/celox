#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn test_interface_connection(sim) {
        @ignore_on(sv);
        @case "interface::test_interface_connection";
    }
}
