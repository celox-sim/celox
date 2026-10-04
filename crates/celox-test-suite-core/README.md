# celox-test-suite-core

Shared infrastructure for the Celox test suites
([`celox-test-suite-veryl`](../celox-test-suite-veryl) and
[`celox-test-suite-sv`](../celox-test-suite-sv)):

- the adapter contract (`Backend`, `Design`, `Simulator`) that a compiler or
  simulator implements to run a suite;
- the test script language (`script`): its reader, the interpreter that runs a
  case over a `Backend`, and the generator that turns a case into a
  self-checking SystemVerilog testbench;
- with the `external` feature, the Verilator and Icarus adapters and the
  command-line runner that verifies a suite against them. A suite supplies a
  `Frontend` that turns its designs into SystemVerilog.

Cases themselves live in the suite crates.
