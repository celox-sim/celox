fn main() -> celox_test_suite::Result<()> {
    celox_test_suite::sv::verification::run(celox_test_suite::verification::Tool::Verilator)
}
