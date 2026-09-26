fn main() -> veryl_test_suite::Result<()> {
    veryl_test_suite::verification::run(
        "verilator",
        "verilator",
        "--version",
        false,
        |design, directory| {
            Ok(Box::new(veryl_test_suite::verilator::Verilator::build(
                design, directory,
            )?))
        },
    )
}
