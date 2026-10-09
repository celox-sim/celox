//! Executes the reusable suite cases, backed exclusively by proved reads.
//! No AST assertion transformation, simulator, or expected-value extraction.
use celox_test_suite::{
    Backend, BigUint, CompilationRejected, Design, Result, SignalPath, veryl::cases,
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex},
};
struct ProofBackend {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    sticky: Arc<Mutex<Vec<String>>>,
    rejected: bool,
    rejection_message: Option<String>,
}
/// Celox refused a construct it does not implement yet (a typed `Unsupported`
/// lowering error). It is not a source rejection, so a `CompilationError` case
/// still fails, and not a backend fault: the case fails with this message and
/// the coverage gate accepts it only as a reviewed `celox_unsupported` exception.
#[derive(Debug)]
struct FrontendUnsupported(String);
impl std::fmt::Display for FrontendUnsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for FrontendUnsupported {}
fn path(p: &SignalPath) -> Value {
    json!({"name":p.name,"instances":p.instances.iter().map(|i|json!({"name":i.name,"index":i.index})).collect::<Vec<_>>()})
}
impl ProofBackend {
    fn exchange(&mut self, value: Value) -> Result<Value> {
        let result = self.exchange_raw(value);
        if let Err(error) = &result {
            if error.downcast_ref::<CompilationRejected>().is_none()
                && error.downcast_ref::<FrontendUnsupported>().is_none()
            {
                self.sticky.lock().unwrap().push(error.to_string());
            }
        }
        result
    }
    fn exchange_raw(&mut self, value: Value) -> Result<Value> {
        let command = value["command"]
            .as_str()
            .ok_or("missing command")?
            .to_owned();
        writeln!(self.input, "{value}")?;
        self.input.flush()?;
        let mut line = String::new();
        self.output.read_line(&mut line)?;
        if line.is_empty() {
            return Err("proof backend terminated unexpectedly".into());
        }
        let reply: Value = serde_json::from_str(&line)?;
        if let Some(error) = reply["error"].as_str() {
            if command == "compile" && reply["compilation_rejected"] == true {
                self.rejected = true;
                self.rejection_message = Some(error.to_string());
                return Err(CompilationRejected(error.to_string()).into());
            }
            if command == "compile" && reply["frontend_unsupported"] == true {
                self.rejected = true;
                self.rejection_message = Some(error.to_string());
                return Err(FrontendUnsupported(error.to_string()).into());
            }
            self.sticky.lock().unwrap().push(error.to_string());
            return Err(error.to_string().into());
        }
        if !reply["error"].is_null() {
            return Err("malformed backend error response".into());
        }
        match command.as_str() {
            "compile" if reply["status"] != "ready" => return Err("missing ready verdict".into()),
            "write" | "eval_comb" | "tick" if reply["status"] != "ok" => {
                return Err("missing operation verdict".into());
            }
            "read" if !reply["payload"].is_string() || !reply["mask"].is_string() => {
                return Err("malformed read response".into());
            }
            _ => {}
        }
        Ok(reply)
    }
    fn build(
        design: &Design,
        out: &Path,
        sticky: Arc<Mutex<Vec<String>>>,
    ) -> Result<Box<dyn Backend>> {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../lydite/conformance/veryl-proof/server.py");
        let mut child = Command::new("python3")
            .arg(script)
            .arg(out)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut backend = Self {
            child,
            input,
            output,
            sticky,
            rejected: false,
            rejection_message: None,
        };
        backend.exchange(json!({"command":"compile","design":{"top":design.top,"four_state":design.four_state,"sources":design.sources.iter().map(|s|json!({"text":s.text,"path":s.path})).collect::<Vec<_>>()}}))?;
        Ok(Box::new(backend))
    }
}
impl Backend for ProofBackend {
    fn write(&mut self, signal: &SignalPath, payload: BigUint, mask: BigUint) -> Result<()> {
        self.exchange(json!({"command":"write","path":path(signal),"payload":payload.to_string(),"mask":mask.to_string()}))?;
        Ok(())
    }
    fn read(&mut self, signal: &SignalPath) -> Result<(BigUint, BigUint)> {
        let result = (|| {
            let r = self.exchange(json!({"command":"read","path":path(signal)}))?;
            Ok((
                r["payload"].as_str().ok_or("payload")?.parse()?,
                r["mask"].as_str().ok_or("mask")?.parse()?,
            ))
        })();
        if let Err(error) = &result {
            let error: &celox_test_suite::Error = error;
            self.sticky.lock().unwrap().push(error.to_string());
        }
        result
    }
    fn eval_comb(&mut self) -> Result<()> {
        self.exchange(json!({"command":"eval_comb"}))?;
        Ok(())
    }
    fn tick(&mut self, event: &str) -> Result<()> {
        self.exchange(json!({"command":"tick","event":event}))?;
        Ok(())
    }
}
impl Drop for ProofBackend {
    fn drop(&mut self) {
        // A compile rejection or frontend Unsupported already returned as its typed
        // marker; closing that unconstructed backend must not turn it into a
        // runtime success.
        let _ = writeln!(self.input, "{{\"command\":\"close\"}}");
        let _ = self.input.flush();
        let mut line = String::new();
        let _ = self.output.read_line(&mut line);
        if let Ok(value) = serde_json::from_str::<Value>(&line) {
            let rejection_close = self.rejected
                && value["status"] == "failed"
                && value["error"].as_str() == self.rejection_message.as_deref();
            let success_close = value["status"] == "passed" && value["error"].is_null();
            if !rejection_close && !success_close {
                self.sticky
                    .lock()
                    .unwrap()
                    .push(format!("invalid or failed close verdict: {value}"));
            }
        } else {
            self.sticky
                .lock()
                .unwrap()
                .push("backend close did not produce a verdict".into());
        }
        if self.child.wait().map_or(true, |s| !s.success()) {
            self.sticky
                .lock()
                .unwrap()
                .push("backend process failed".into());
        }
    }
}
fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.as_slice() == ["--list"] {
        // The coverage gate hashes each case's text from `source` onward, so
        // edited cases need a new review.
        let listed = cases()
            .map(|c| {
                let group = c.name.split("::").next().unwrap_or(c.name);
                json!({
                    "case": c.name,
                    "expectation": format!("{:?}", c.expectation),
                    "category": format!("{:?}", c.category),
                    "source": {"file": format!("src/veryl/cases/{group}.vtest"), "line": c.script().pos.line},
                })
            })
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string(&listed).unwrap());
        return;
    }
    if args.is_empty() {
        eprintln!("usage: proof-suite OUT [CASE...]");
        std::process::exit(2)
    }
    let known = cases()
        .map(|case| case.name)
        .collect::<std::collections::HashSet<_>>();
    let mut requested = std::collections::HashSet::new();
    for name in &args[1..] {
        if !known.contains(name.as_str()) || !requested.insert(name) {
            eprintln!("unknown or duplicate case: {name}");
            std::process::exit(2)
        }
    }
    let expected_count = if args.len() == 1 {
        known.len()
    } else {
        requested.len()
    };
    if expected_count == 0 {
        eprintln!("empty case selection");
        std::process::exit(2)
    }
    let out = PathBuf::from(&args[0]);
    std::fs::create_dir(&out).unwrap();
    let mut rows = vec![];
    for case in
        cases().filter(|case| args.len() == 1 || args[1..].iter().any(|name| name == case.name))
    {
        let start = std::time::Instant::now();
        let sticky = Arc::new(Mutex::new(vec![]));
        let mut designs = 0;
        let case_out = out.join(case.name.replace("::", "__"));
        std::fs::create_dir(&case_out).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            case.run(&mut |design| {
                designs += 1;
                ProofBackend::build(
                    design,
                    &case_out.join(format!("design-{designs}")),
                    sticky.clone(),
                )
            })
        }));
        let panic = result.err().map(|p| {
            p.downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or("nonstring panic".into())
        });
        let errors = sticky.lock().unwrap().clone();
        let passed = panic.is_none() && errors.is_empty();
        rows.push(json!({"case":case.name,"status":if passed{"passed"}else{"failed"},"expectation":format!("{:?}",case.expectation),"panic":panic,"errors":errors,"designs":designs,"elapsed_ms":start.elapsed().as_millis()}));
        std::fs::write(
            out.join("summary.json"),
            serde_json::to_string_pretty(&rows).unwrap(),
        )
        .unwrap();
        println!("{} {}", case.name, if passed { "passed" } else { "failed" });
    }
    if rows.len() != expected_count {
        eprintln!("case coverage changed");
        std::process::exit(2)
    }
    if rows.iter().any(|r| r["status"] != "passed") {
        std::process::exit(1)
    }
}
