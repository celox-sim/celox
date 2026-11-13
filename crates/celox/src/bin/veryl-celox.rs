//! `veryl celox ...`: Veryl runs `veryl-<name>` from `PATH` for a subcommand
//! it does not provide, and this runs the `celox` command installed beside it.

use std::{
    env,
    ffi::OsString,
    io::Write,
    path::PathBuf,
    process::{Command, ExitCode},
};

/// The line `veryl --list` shows for the subcommand (`veryl-celox --info`).
const INFO: &str = "Run Veryl and SystemVerilog testbenches and cocotb tests with Celox";

fn main() -> ExitCode {
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    if arguments.len() == 1 && arguments[0] == "--info" {
        println!("{INFO}");
        return ExitCode::SUCCESS;
    }
    let mut command = Command::new(celox_command());
    command.args(arguments);
    run(command)
}

/// The `celox` executable beside this one, or `celox` from `PATH`.
fn celox_command() -> PathBuf {
    let name = format!("celox{}", env::consts::EXE_SUFFIX);
    env::current_exe()
        .ok()
        .map(|executable| executable.with_file_name(&name))
        .filter(|sibling| sibling.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

#[cfg(unix)]
fn run(mut command: Command) -> ExitCode {
    use std::os::unix::process::CommandExt as _;

    // Replace this process, so that signals such as the checkpoint request
    // (`SIGUSR1`) reach `celox` itself.
    let error = command.exec();
    failure(&command, &error)
}

#[cfg(not(unix))]
fn run(mut command: Command) -> ExitCode {
    match command.status() {
        Ok(status) => status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map_or(ExitCode::FAILURE, ExitCode::from),
        Err(error) => failure(&command, &error),
    }
}

fn failure(command: &Command, error: &std::io::Error) -> ExitCode {
    let _ = writeln!(
        std::io::stderr(),
        "veryl-celox: failed to run {}: {error}; install it with `cargo install celox`",
        command.get_program().display()
    );
    ExitCode::FAILURE
}
