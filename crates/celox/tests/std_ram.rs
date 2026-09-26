#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Dual-port RAM: write via port A, read via port B (BUFFER_OUT=false, combinational read)
    fn test_ram_write_read(sim) {
        @ignore_on(sv);
        @case "std_ram::test_ram_write_read";
    }

    // Overwrite same address and verify latest value
    fn test_ram_overwrite(sim) {
        @ignore_on(sv);
        @case "std_ram::test_ram_overwrite";
    }

    // RAM with USE_RESET=true: clear via i_clr
    fn test_ram_reset_and_clear(sim) {
        @ignore_on(sv);
        @case "std_ram::test_ram_reset_and_clear";
    }
}
