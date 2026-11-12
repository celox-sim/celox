//! Runs celox-test-suite cases on Celox backends, one case on one backend per
//! process. `scripts/run-vtest.py` selects the cases and backends from
//! `crates/celox/vtest.toml`, runs this executor for each and reports them.

use clap::{Parser, Subcommand, ValueEnum};

#[path = "../tests/test_utils/suite.rs"]
mod suite;
#[cfg(feature = "veryl-reference")]
#[path = "../tests/test_utils/veryl_sim.rs"]
mod veryl_sim;
#[path = "../tests/test_utils/veryl_sv.rs"]
mod veryl_sv;

#[path = "../tests/frontends/systemverilog/suite.rs"]
mod sv_suite;

#[derive(Clone, Copy, ValueEnum)]
enum Language {
    Veryl,
    Sv,
}

#[derive(Parser)]
#[command(about = "Run celox-test-suite cases on Celox backends")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run one case on one backend; a failed case exits unsuccessfully.
    Run {
        #[arg(value_enum)]
        language: Language,
        /// The case, as `group::name`.
        case: String,
        backend: String,
    },
    /// Print the names of a language's cases, one per line.
    Cases {
        #[arg(value_enum)]
        language: Language,
    },
    /// Print the backends this build can run a language's cases on.
    Backends {
        #[arg(value_enum)]
        language: Language,
    },
}

#[allow(clippy::disallowed_macros)] // The listings are this command's output.
fn main() {
    match Args::parse().command {
        Command::Run {
            language,
            case,
            backend,
        } => run(language, &case, &backend),
        Command::Cases { language } => {
            let names: Vec<_> = match language {
                Language::Veryl => celox_test_suite::veryl::cases().map(|c| c.name).collect(),
                Language::Sv => celox_test_suite::sv::cases().map(|c| c.name).collect(),
            };
            for name in names {
                println!("{name}");
            }
        }
        Command::Backends { language } => {
            let backends = match language {
                Language::Veryl => suite::backends(),
                Language::Sv => sv_suite::backends(),
            };
            for backend in backends {
                println!("{backend}");
            }
        }
    }
}

fn run(language: Language, name: &str, backend: &str) {
    let run = || match language {
        Language::Veryl => celox_test_suite::veryl::case(name)
            .unwrap_or_else(|| panic!("unknown Veryl test case: {name}"))
            .run(&mut |design| suite::build(design, backend)),
        Language::Sv => celox_test_suite::sv::case(name)
            .unwrap_or_else(|| panic!("unknown SystemVerilog test case: {name}"))
            .run(&mut |design| sv_suite::build(design, backend)),
    };
    // The recursive SV parser needs more stack than a main thread may have,
    // and the shared driver adds call frames.
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn_scoped(scope, run)
            .unwrap()
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    });
}
