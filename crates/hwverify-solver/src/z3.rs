//! SMT-LIB emission and isolated Z3 subprocess/evidence I/O.
use hwverify_ir::*;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    time::Instant,
};
#[derive(Default)]
pub struct Emitter {
    ids: HashMap<Term, String>,
    lines: Vec<String>,
    count: u64,
}
impl Emitter {
    fn emit(&mut self, t: &Term) -> String {
        if let Some(n) = self.ids.get(t) {
            return n.clone();
        }
        if let Some(n) = t.0.op.strip_prefix('@') {
            self.lines
                .push(format!("(declare-fun {n} () {})", t.0.sort.smt()));
            self.ids.insert(t.clone(), n.into());
            return n.into();
        }
        let args = t.0.args.iter().map(|x| self.emit(x)).collect::<Vec<_>>();
        let n = format!("t{}", self.count);
        self.count += 1;
        let expr = if args.is_empty() {
            t.0.op.clone()
        } else {
            format!("({} {})", t.0.op, args.join(" "))
        };
        self.lines
            .push(format!("(define-fun {n} () {} {expr})", t.0.sort.smt()));
        self.ids.insert(t.clone(), n.clone());
        n
    }
}
pub(crate) fn finite_only() -> bool {
    std::env::var("HWVERIFY_SOLVER").as_deref() == Ok("finite")
}
pub fn solver(z3: &str, script: &str) -> Res<String> {
    // Defense in depth: opting into the finite route must never launch Z3,
    // including through a caller that has not implemented finite reporting.
    if finite_only() {
        return Err("external SMT subprocess disabled by HWVERIFY_SOLVER=finite".into());
    }
    let mut p = Command::new(z3)
        .args(["-in", "-smt2"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("start Z3: {e}"))?;
    p.stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .map_err(|e| e.to_string())?;
    let out = p.wait_with_output().map_err(|e| e.to_string())?;
    let s = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() || s.contains("(error") {
        return Err(format!(
            "Z3 error: {} {}",
            s,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(s)
}
pub struct Check {
    pub z3: String,
    pub out: PathBuf,
    pub reports: Vec<Value>,
}
impl Check {
    pub fn query(&mut self, name: &str, bad: Term, expect_sat: bool, context: &Env) -> Res<()> {
        self.query_with_timeout(name, bad, expect_sat, context, 10000)
    }
    /// Capture a SAT witness even when SAT is the expected outcome (for finite examples).
    pub fn query_with_witness(
        &mut self,
        name: &str,
        formula: Term,
        expect_sat: bool,
        context: &Env,
    ) -> Res<()> {
        self.query_options(name, formula, expect_sat, context, 10000, true)
    }
    pub fn query_with_timeout(
        &mut self,
        name: &str,
        bad: Term,
        expect_sat: bool,
        context: &Env,
        timeout_ms: u64,
    ) -> Res<()> {
        self.query_options(name, bad, expect_sat, context, timeout_ms, false)
    }
    fn query_options(
        &mut self,
        name: &str,
        bad: Term,
        expect_sat: bool,
        context: &Env,
        timeout_ms: u64,
        capture_sat: bool,
    ) -> Res<()> {
        let start = Instant::now();
        let mut e = Emitter::default();
        let b = e.emit(&bad);
        let ctx = context
            .iter()
            .map(|(n, t)| (n.clone(), e.emit(t)))
            .collect::<BTreeMap<_, _>>();
        let base=format!("(set-option :timeout {timeout_ms})\n(set-option :produce-models true)\n(set-logic QF_AUFBV)\n{}\n(assert {b})\n(check-sat)\n",e.lines.join("\n"));
        let emission_seconds = start.elapsed().as_secs_f64();
        let kernel_start = Instant::now();
        let use_kernel = !expect_sat && std::env::var("HWVERIFY_KERNEL").as_deref() != Ok("off");
        let mut kernel_report = json!({"enabled":use_kernel});
        if use_kernel {
            let attempt = crate::kernel::refute(&bad);
            let kernel_compute_seconds = kernel_start.elapsed().as_secs_f64();
            let mut residual_emitter = Emitter::default();
            let residual_name = residual_emitter.emit(&attempt.residual);
            let residual_text = format!("; DIAGNOSTIC ONLY: contextual residual may omit assumptions; not equivalent standalone to original query.\n{}\n; residual {}\n",residual_emitter.lines.join("\n"),residual_name);
            fs::write(
                self.out.join(format!("{name}.kernel-residual.smt2")),
                residual_text,
            )
            .map_err(|e| e.to_string())?;
            kernel_report = json!({"enabled":true,"closed":attempt.closed,"rounds":attempt.rounds,
                "rules":attempt.rules,"seconds":kernel_compute_seconds,
                "diagnostics_seconds":kernel_start.elapsed().as_secs_f64()-kernel_compute_seconds,
                "original_complexity":crate::kernel::complexity(&bad),
                "residual_complexity":crate::kernel::complexity(&attempt.residual),
                "residual":format!("{name}.kernel-residual.smt2"),"kind":"diagnostic rule counts, not independently checkable proof certificate","trusted":"Rust rewrite/contradiction implementation; not a Lean certificate"});
            fs::write(
                self.out.join(format!("{name}.kernel.json")),
                serde_json::to_string_pretty(&kernel_report).unwrap(),
            )
            .map_err(|e| e.to_string())?;
            if attempt.closed {
                // Keep the ORIGINAL obligation replayable by an independent SMT solver.
                fs::write(self.out.join(format!("{name}.smt2")), &base)
                    .map_err(|e| e.to_string())?;
                fs::write(
                    self.out.join(format!("{name}.out")),
                    "unsat\n; closed by hwverify structural kernel; Z3 not invoked\n",
                )
                .map_err(|e| e.to_string())?;
                self.reports.push(json!({"name":name,"status":"passed","solver_result":"unsat",
                    "backend":"structural_kernel","seconds":start.elapsed().as_secs_f64(),
                    "emission_seconds":emission_seconds,"z3_seconds":0.0,"kernel":kernel_report,
                    "context_symbols":ctx,"evidence":format!("{name}.smt2"),"solver_output":format!("{name}.out")}));
                return Ok(());
            }
        }
        if finite_only() {
            let finite_start = Instant::now();
            let result = crate::finite::solve(
                &bad,
                context,
                crate::finite::Limits {
                    timeout_ms,
                    ..Default::default()
                },
            );
            let finite_seconds = finite_start.elapsed().as_secs_f64();
            let verdict = result.verdict.as_str();
            let status = match verdict {
                "unsat" if expect_sat => "failed_nonvacuity",
                "unsat" => "passed",
                "sat" if expect_sat => "passed",
                "sat" => "counterexample",
                _ => "unknown",
            };
            let mut script = base;
            let mut raw = format!("{verdict}\n; hwverify finite Bool/BV backend; Z3 not invoked\n");
            if let Some(reason) = &result.reason {
                raw.push_str(&format!("; {reason}\n"));
            }
            if result.verdict == crate::finite::Verdict::Sat {
                // Every finite SAT result includes a validated original-formula
                // witness, even nonvacuity checks that did not request a model.
                script.push_str("(get-model)\n");
                raw.push_str("; original-formula witness independently evaluated true\n(\n");
                for (symbol, value) in &result.assignments {
                    let sort = match value {
                        crate::finite::Scalar::Bool(_) => "Bool".into(),
                        crate::finite::Scalar::Bv { width, .. } => format!("(_ BitVec {width})"),
                    };
                    raw.push_str(&format!(
                        "  (define-fun {symbol} () {sort} {})\n",
                        value.smt()
                    ));
                }
                raw.push_str(")\n");
                if !ctx.is_empty() {
                    script.push_str(&format!(
                        "(get-value ({}))\n",
                        ctx.values().cloned().collect::<Vec<_>>().join(" ")
                    ));
                    raw.push('(');
                    for (label, symbol) in &ctx {
                        raw.push_str(&format!(
                            "({symbol} {})",
                            result.context_values[label].smt()
                        ));
                    }
                    raw.push_str(")\n");
                }
            }
            let diagnostics = result.diagnostics();
            let diagnostics_path = format!("{name}.finite.json");
            fs::write(
                self.out.join(&diagnostics_path),
                serde_json::to_string_pretty(&diagnostics).unwrap(),
            )
            .map_err(|e| e.to_string())?;
            fs::write(self.out.join(format!("{name}.smt2")), &script).map_err(|e| e.to_string())?;
            fs::write(self.out.join(format!("{name}.out")), &raw).map_err(|e| e.to_string())?;
            self.reports.push(json!({"name":name,"status":status,"solver_result":verdict,"backend":"finite_bv",
                "seconds":start.elapsed().as_secs_f64(),"emission_seconds":emission_seconds,"z3_seconds":0.0,
                "finite_seconds":finite_seconds,"finite":diagnostics,"finite_diagnostics":diagnostics_path,
                "kernel":kernel_report,"context_symbols":ctx,"concrete_model":result.original_formula_validated,
                "evidence":format!("{name}.smt2"),"solver_output":format!("{name}.out")}));
            return Ok(());
        }
        let z3_start = Instant::now();
        let mut raw = solver(&self.z3, &base)?;
        let mut z3_seconds = z3_start.elapsed().as_secs_f64();
        let verdict = raw.trim().to_string();
        let status = match verdict.as_str() {
            "unsat" => {
                if expect_sat {
                    "failed_nonvacuity"
                } else {
                    "passed"
                }
            }
            "sat" => {
                if expect_sat {
                    "passed"
                } else {
                    "counterexample"
                }
            }
            "unknown" => "unknown",
            _ => return Err(format!("unexpected Z3 output {raw}")),
        };
        let mut script = base;
        if verdict == "sat" && (!expect_sat || capture_sat) {
            script.push_str("(get-model)\n");
            if !ctx.is_empty() {
                script.push_str(&format!(
                    "(get-value ({}))\n",
                    ctx.values().cloned().collect::<Vec<_>>().join(" ")
                ))
            }
            let witness_start = Instant::now();
            raw = solver(&self.z3, &script)?;
            z3_seconds += witness_start.elapsed().as_secs_f64();
            if raw.lines().next().map(str::trim) != Some("sat") {
                return Err(
                    "SAT witness recheck did not return sat; no SAT result certified".into(),
                );
            }
        }
        fs::write(self.out.join(format!("{name}.smt2")), &script).map_err(|e| e.to_string())?;
        fs::write(self.out.join(format!("{name}.out")), &raw).map_err(|e| e.to_string())?;
        self.reports.push(json!({"name":name,"status":status,"solver_result":verdict,"backend":"z3","seconds":start.elapsed().as_secs_f64(),"emission_seconds":emission_seconds,"z3_seconds":z3_seconds,"kernel":kernel_report,"context_symbols":ctx,"evidence":format!("{name}.smt2"),"solver_output":format!("{name}.out")}));
        Ok(())
    }
}
