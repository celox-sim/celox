//! One case per process; libtest/nextest owns selection, ignores and scheduling.

use clap::{Parser, ValueEnum};

#[path = "../tests/test_utils/suite.rs"]
mod suite;
#[path = "../tests/test_utils/veryl_sim.rs"]
mod veryl_sim;
#[path = "../tests/test_utils/veryl_sv.rs"]
mod veryl_sv;
#[path = "../tests/test_utils/vtest_exec.rs"]
mod vtest_exec;

#[cfg(feature = "systemverilog")]
#[allow(dead_code)]
#[path = "../tests/frontends/systemverilog/suite.rs"]
mod sv_suite;

#[derive(Clone, ValueEnum)]
enum Language {
    Veryl,
    #[cfg(feature = "systemverilog")]
    Sv,
}

#[derive(Parser)]
#[command(about = "Execute a runtime vtest case using the shared test adapters")]
struct Args {
    #[arg(value_enum)]
    language: Language,
    case: String,
    backend: String,
}

fn main() {
    let args = Args::parse();
    match args.language {
        Language::Veryl => suite::run_case(&args.case, &args.backend),
        #[cfg(feature = "systemverilog")]
        Language::Sv => sv_suite::run_case(&args.case, &args.backend),
    }
}
