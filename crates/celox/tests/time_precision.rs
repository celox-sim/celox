#![cfg(feature = "systemverilog")]

use std::path::Path;

use celox::{SimBackend, Simulator, SimulatorBuilder};

fn check_vcd<B: SimBackend>(mut sim: Simulator<B>, path: &Path, expected: &str) {
    let signal = sim.signal("y");
    sim.modify(|io| io.set(signal, 1u8)).unwrap();
    sim.dump(123);
    sim.flush_vcd().unwrap();
    let first = std::fs::read_to_string(path).unwrap();
    assert!(
        first.contains(&format!("$timescale {expected} $end")),
        "{first}"
    );
    assert!(
        first.contains("#123\n"),
        "timestamps must remain global ticks"
    );
    let next = path.with_extension("next.vcd");
    sim.switch_vcd(&next).unwrap();
    sim.dump(130);
    sim.flush_vcd().unwrap();
    assert!(
        std::fs::read_to_string(next)
            .unwrap()
            .contains(&format!("$timescale {expected} $end"))
    );
}

fn check_backends<'source>(
    build: impl Fn(&Path) -> SimulatorBuilder<'source, Simulator>,
    expected: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("wave.vcd");
    check_vcd(build(&path).build_native().unwrap(), &path, expected);
    check_vcd(build(&path).build_cranelift().unwrap(), &path, expected);
    check_vcd(build(&path).build_wasm().unwrap(), &path, expected);
    check_vcd(build(&path).build_interpreter().unwrap(), &path, expected);
    check_vcd(build(&path).build_tiered().unwrap(), &path, expected);
    check_vcd(
        build(&path)
            .threads(4)
            .parallel_partition(celox::ParallelPartition::Always)
            .build_native()
            .unwrap(),
        &path,
        expected,
    );
    check_vcd(
        build(&path)
            .threads(4)
            .parallel_partition(celox::ParallelPartition::Always)
            .build_cranelift()
            .unwrap(),
        &path,
        expected,
    );
}

#[test]
fn sv_waveforms_use_global_precision_across_backends() {
    for (declarations, expected) in [
        ("", "1ps"),
        ("timeunit 1ns; timeprecision 1fs;", "1fs"),
        ("timeunit 1us; timeprecision 100ns;", "100ns"),
    ] {
        let source = format!("module Top(output logic y); {declarations} endmodule");
        check_backends(
            |path| {
                Simulator::from_sv_sources(vec![(&source, Path::new("top.sv"))], "Top").vcd(path)
            },
            expected,
        );
    }
}

#[test]
fn package_precision_is_retained_by_native_images() {
    let source = "package P; timeunit 1ns; timeprecision 10fs; endpackage
                  module Top(output logic y); timeunit 1ns; timeprecision 1ps; endmodule";
    let build = |path: &Path| {
        Simulator::from_sv_sources(vec![(source, Path::new("packages.sv"))], "Top").vcd(path)
    };
    check_backends(build, "10fs");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image.vcd");
    let image = build(&path).compile_native().unwrap().into_program_image();
    let image =
        celox::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
            .unwrap();
    assert_eq!(image.time_precision().exponent(), -14);
    let sim = Simulator::from_sources(Vec::new(), "Top")
        .vcd(&path)
        .build_native_from_image(image)
        .unwrap();
    check_vcd(sim, &path, "10fs");
}

#[test]
fn veryl_waveforms_use_the_default_picosecond_tick() {
    let source = "module Top(y: output logic) {}";
    check_backends(|path| Simulator::builder(source, "Top").vcd(path), "1ps");
}

#[test]
fn mixed_designs_keep_sv_precision_through_the_veryl_frontend() {
    let veryl = "module Top(y: output logic) { inst child: $sv::Child (y); }";
    for (precision, expected) in [("1fs", "1fs"), ("100ns", "100ns")] {
        let sv = format!(
            "module Child(output logic y); timeunit 1us; timeprecision {precision}; assign y = 1; endmodule"
        );
        check_backends(
            |path| {
                Simulator::from_mixed_sources(
                    vec![(veryl, Path::new("top.veryl"))],
                    vec![(&sv, Path::new("child.sv"))],
                    "Top",
                )
                .vcd(path)
            },
            expected,
        );
    }
}
