#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Build-only smoke test: mux with selector_pkg requires `calc_select_width`
    // evaluation while resolving module parameters and widths.
    fn test_mux_build_smoke(sim) {
        @ignore_on(veryl, sv);
        @case "std_mux::test_mux_build_smoke";
    }

    // Build-only smoke test: binary demux also depends on selector_pkg compile-time
    // width resolution through `calc_select_width`.
    fn test_demux_build_smoke(sim) {
        @ignore_on(veryl, sv);
        @case "std_mux::test_demux_build_smoke";
    }
}
