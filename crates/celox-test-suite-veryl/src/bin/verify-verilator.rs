fn main() -> celox_test_suite_veryl::Result<()> {
    celox_test_suite_veryl::verification::run(
        "verilator",
        "verilator",
        "--version",
        false,
        |design, directory| {
            Ok(Box::new(
                celox_test_suite_veryl::verilator::Verilator::build(design, directory)?,
            ))
        },
    )
}
