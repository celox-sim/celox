#![cfg(feature = "external")]

use celox_test_suite::verification::{Builders, run_case};
use celox_test_suite::{Backend, BigUint, Result, SignalPath};

struct FailedTestbench;

impl Backend for FailedTestbench {
    fn write(&mut self, _: &SignalPath, _: BigUint, _: BigUint) -> Result<()> {
        unreachable!("generated script runs as a testbench")
    }

    fn read(&mut self, _: &SignalPath) -> Result<(BigUint, BigUint)> {
        unreachable!("generated script runs as a testbench")
    }

    fn eval_comb(&mut self) -> Result<()> {
        unreachable!("generated script runs as a testbench")
    }

    fn tick(&mut self, _: &str) -> Result<()> {
        unreachable!("generated script runs as a testbench")
    }

    fn run_testbench(&mut self) -> Result<()> {
        Err("testbench exited with failure".into())
    }
}

fn failed_transcript(name: &str, log: &[u8]) -> serde_json::Value {
    let output = std::env::temp_dir().join(format!(
        "celox-verification-failure-{name}-{}",
        std::process::id()
    ));
    let case = celox_test_suite::sv::case(
        "system_functions::time_format_consumes_its_expression_argument",
    )
    .unwrap();
    let row = run_case(
        case,
        &output,
        true,
        &Builders {
            design: &|_, _| unreachable!("generated script uses the script builder"),
            script: &|_, directory| {
                std::fs::write(directory.join("protocol.log"), log)?;
                Ok(Box::new(FailedTestbench))
            },
        },
        None,
        None,
    );
    std::fs::remove_dir_all(output).unwrap();
    row
}

#[test]
fn failed_binary_transcripts_preserve_assertion_mismatches() {
    let row = failed_transcript(
        "assertions",
        b"\xff\0\x80\n \t@suite assert first: expected 1, got 0\r\n\xfe\n@suite assert second: failed\n",
    );
    assert_eq!(row["status"], "mismatch");
    assert_eq!(row["phase"], "execute");
    assert_eq!(row["detail"], "first: expected 1, got 0\nsecond: failed");
}

#[test]
fn failed_binary_transcripts_without_markers_remain_runtime_errors() {
    let row = failed_transcript(
        "runtime",
        b"\xff\0\x80\noutput includes @suite assert but is not a marker\n",
    );
    assert_eq!(row["status"], "runtime_error");
    assert_eq!(row["phase"], "execute");
    assert_eq!(row["detail"], "testbench exited with failure");
}
