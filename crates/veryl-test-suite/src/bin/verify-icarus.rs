fn main() -> veryl_test_suite::Result<()> {
    veryl_test_suite::verification::run("icarus", "iverilog", "-V", true, |design, directory| {
        Ok(Box::new(veryl_test_suite::icarus::Icarus::build(
            design, directory,
        )?))
    })
}
