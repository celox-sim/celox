#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn test_fifo_issue5_subtract_overflow(sim) {
        @ignore_on(sv);
        @case "fifo_issue5::test_fifo_issue5_subtract_overflow";
    }
}
