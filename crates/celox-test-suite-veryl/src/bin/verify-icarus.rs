fn main() -> celox_test_suite_veryl::Result<()> {
    celox_test_suite_veryl::verification::run(
        "icarus",
        "iverilog",
        "-V",
        true,
        |design, directory| {
            Ok(Box::new(celox_test_suite_veryl::icarus::Icarus::build(
                design, directory,
            )?))
        },
    )
}
