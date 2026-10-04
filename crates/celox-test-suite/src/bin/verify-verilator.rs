fn main() -> celox_test_suite::Result<()> {
    celox_test_suite::veryl::verification::run(celox_test_suite::verification::Tool::Verilator)
}
