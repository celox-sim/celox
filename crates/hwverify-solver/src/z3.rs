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
    visits: u64,
}
struct EmissionBudget {
    work: u64,
    start: Instant,
    timeout_ms: u64,
}
impl Emitter {
    fn emit(&mut self, t: &Term) -> String {
        self.emit_inner(t, &mut None).expect("unbounded emitter")
    }
    fn emit_inner(&mut self, t: &Term, budget: &mut Option<EmissionBudget>) -> Res<String> {
        self.visits += 1;
        if let Some(b) = budget {
            if self.visits > b.work || b.start.elapsed().as_millis() >= b.timeout_ms as u128 {
                return Err("checked query emission budget exhausted".into());
            }
        }
        if let Some(n) = self.ids.get(t) {
            return Ok(n.clone());
        }
        if let Some(n) = t.0.op.strip_prefix('@') {
            self.lines
                .push(format!("(declare-fun {n} () {})", t.0.sort.smt()));
            self.ids.insert(t.clone(), n.into());
            return Ok(n.into());
        }
        let args =
            t.0.args
                .iter()
                .map(|x| self.emit_inner(x, budget))
                .collect::<Res<Vec<_>>>()?;
        let n = format!("t{}", self.count);
        self.count += 1;
        let expression = if args.is_empty() {
            t.0.op.clone()
        } else {
            format!("({} {})", t.0.op, args.join(" "))
        };
        self.lines.push(format!(
            "(define-fun {n} () {} {expression})",
            t.0.sort.smt()
        ));
        self.ids.insert(t.clone(), n.clone());
        Ok(n)
    }
}

pub(crate) fn write_original_query(
    out: &std::path::Path,
    name: &str,
    bad: &Term,
    context: &Env,
    limits: Option<crate::finite::Limits>,
) -> (Res<()>, usize) {
    let mut emitter = Emitter::default();
    let mut budget = limits.map(|l| EmissionBudget {
        work: l.max_work,
        start: Instant::now(),
        timeout_ms: l.timeout_ms,
    });
    let result = (|| -> Res<()> {
        let root = emitter.emit_inner(bad, &mut budget)?;
        for term in context.values() {
            emitter.emit_inner(term, &mut budget)?;
        }
        let text = format!(
            "(set-logic QF_AUFBV)\n{}\n(assert {root})\n(check-sat)\n",
            emitter.lines.join("\n")
        );
        fs::write(out.join(format!("{name}.smt2")), text).map_err(|e| e.to_string())
    })();
    (result, emitter.visits as usize)
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
/// Search and evidence options, separate from the query's logical expectation.
#[derive(Clone, Debug)]
pub struct QueryOptions {
    pub timeout_ms: u64,
    pub capture_sat: bool,
    /// None derives a hint from `expect_sat`, unless an explicit environment
    /// override is present. Some(...) overrides that environment as well.
    pub finite_search_hint: Option<crate::finite::SearchHint>,
}
impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            timeout_ms: 10_000,
            capture_sat: false,
            finite_search_hint: None,
        }
    }
}
/// Validate the CLI/environment setting. `query` uses each logical expectation.
pub fn parse_finite_search_hint(value: &str) -> Res<Option<crate::finite::SearchHint>> {
    match value {
        "query" => Ok(None),
        "sat" => Ok(Some(crate::finite::SearchHint::Sat)),
        "unsat" => Ok(Some(crate::finite::SearchHint::Unsat)),
        _ => Err("finite search hint must be query, sat or unsat".into()),
    }
}
pub(crate) fn resolve_finite_search_hint(
    expect_sat: bool,
    explicit: Option<crate::finite::SearchHint>,
) -> Res<(crate::finite::SearchHint, &'static str)> {
    if let Some(hint) = explicit {
        return Ok((hint, "query_options"));
    }
    match std::env::var("HWVERIFY_FINITE_SEARCH_HINT") {
        Ok(value) => {
            if let Some(hint) = parse_finite_search_hint(&value)? {
                return Ok((hint, "environment"));
            }
        }
        Err(std::env::VarError::NotPresent) => {}
        Err(_) => return Err("HWVERIFY_FINITE_SEARCH_HINT is not valid UTF-8".into()),
    }
    Ok((
        if expect_sat {
            crate::finite::SearchHint::Sat
        } else {
            crate::finite::SearchHint::Unsat
        },
        "logical_expectation",
    ))
}
pub(crate) struct QueryBudget {
    pub limits: crate::finite::Limits,
    pub allow_kernel: bool,
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
        self.query_with_options(
            name,
            formula,
            expect_sat,
            context,
            QueryOptions {
                capture_sat: true,
                ..QueryOptions::default()
            },
        )
    }
    pub fn query_with_timeout(
        &mut self,
        name: &str,
        bad: Term,
        expect_sat: bool,
        context: &Env,
        timeout_ms: u64,
    ) -> Res<()> {
        self.query_with_options(
            name,
            bad,
            expect_sat,
            context,
            QueryOptions {
                timeout_ms,
                ..QueryOptions::default()
            },
        )
    }
    /// `expect_sat` controls pass/fail only. The optional finite search hint
    /// controls search order only; neither can substitute for a solver result.
    pub fn query_with_options(
        &mut self,
        name: &str,
        bad: Term,
        expect_sat: bool,
        context: &Env,
        options: QueryOptions,
    ) -> Res<()> {
        self.query_limited(name, bad, expect_sat, context, options, None)
    }

    /// Internal bounded route for checked query decompositions. No external
    /// solver or proof rule is introduced by selecting smaller finite limits.
    pub(crate) fn query_limited(
        &mut self,
        name: &str,
        bad: Term,
        expect_sat: bool,
        context: &Env,
        options: QueryOptions,
        budget: Option<QueryBudget>,
    ) -> Res<()> {
        // Reject an invalid/non-finite opt-in before ANY backend invocation,
        // including nonvacuity/example queries preceding the inductive query.
        crate::conjunctive::enabled()?;
        crate::automatic::mode()?;
        let bounded = budget.is_some();
        if bounded && !finite_only() {
            return Err("bounded query cannot invoke an external solver".into());
        }
        let finite_mode = bounded || finite_only();
        let (mut limits, allow_kernel) = match budget {
            Some(b) => (Some(b.limits), b.allow_kernel),
            None => (None, true),
        };
        let QueryOptions {
            timeout_ms,
            capture_sat,
            finite_search_hint,
        } = options;
        let logical_expectation = if expect_sat { "sat" } else { "unsat" };
        let finite_hint = if finite_mode {
            Some(resolve_finite_search_hint(expect_sat, finite_search_hint)?)
        } else {
            None
        };
        let start = Instant::now();
        let mut e = Emitter::default();
        let strict = limits.is_some() && !allow_kernel;
        let mut emission_budget = if strict {
            limits.as_ref().map(|l| EmissionBudget {
                work: l.max_work,
                start,
                timeout_ms: l.timeout_ms,
            })
        } else {
            None
        };
        let emitted = (|| -> Res<(String, BTreeMap<String, String>)> {
            let root = e.emit_inner(&bad, &mut emission_budget)?;
            let context = context
                .iter()
                .map(|(name, t)| Ok((name.clone(), e.emit_inner(t, &mut emission_budget)?)))
                .collect::<Res<BTreeMap<_, _>>>()?;
            Ok((root, context))
        })();
        let (b, ctx) = match emitted {
            Ok(value) => value,
            Err(reason) => {
                self.reports.push(json!({"name":name,"status":"unknown","solver_result":"unknown","backend":"finite_bv","logical_expectation":logical_expectation,
                "seconds":start.elapsed().as_secs_f64(),"z3_seconds":0.0,"emission_seconds":start.elapsed().as_secs_f64(),"emission_nodes":e.ids.len(),"emission_work":e.visits,
                "kernel":{"enabled":false},"finite":{"solver_result":"unknown","work":0,"clauses":0,"terms":0,"variables":0,"reason":reason,"original_formula_validated":false}}));
                return Ok(());
            }
        };
        if strict {
            if let Some(l) = limits.as_mut() {
                l.max_work = l.max_work.saturating_sub(e.visits);
                l.timeout_ms = l
                    .timeout_ms
                    .saturating_sub(start.elapsed().as_millis() as u64);
            }
        }
        let base=format!("(set-option :timeout {timeout_ms})\n(set-option :produce-models true)\n(set-logic QF_AUFBV)\n{}\n(assert {b})\n(check-sat)\n",e.lines.join("\n"));
        let emission_seconds = start.elapsed().as_secs_f64();
        let kernel_start = Instant::now();
        let use_kernel =
            allow_kernel && !expect_sat && std::env::var("HWVERIFY_KERNEL").as_deref() != Ok("off");
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
                    "backend":"structural_kernel","logical_expectation":logical_expectation,
                    "finite_search_hint":finite_hint.map(|(hint, _)| hint.as_str()),
                    "search_hint_source":finite_hint.map(|(_, source)| source),
                    "search_strategy":"structural_kernel","seconds":start.elapsed().as_secs_f64(),
                    "emission_seconds":emission_seconds,"emission_nodes":e.ids.len(),"emission_work":e.visits,"z3_seconds":0.0,"kernel":kernel_report,
                    "context_symbols":ctx,"evidence":format!("{name}.smt2"),"solver_output":format!("{name}.out")}));
                return Ok(());
            }
        }
        if finite_mode {
            let finite_start = Instant::now();
            let (search_hint, search_hint_source) = finite_hint.unwrap();
            let result = crate::finite::solve_with_hint(
                &bad,
                context,
                limits.unwrap_or(crate::finite::Limits {
                    timeout_ms,
                    ..Default::default()
                }),
                search_hint,
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
            let mut raw = format!(
                "{verdict}\n; hwverify finite Bool/BV/readonly-array backend; Z3 not invoked\n"
            );
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
                for (symbol, value) in &result.array_assignments {
                    raw.push_str(&format!(
                        "  (define-fun {symbol} () {} {})\n",
                        hwverify_ir::Sort::Mem(value.address_width, value.value_width).smt(),
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
                            result
                                .context_values
                                .get(label)
                                .map(|v| v.smt())
                                .unwrap_or_else(|| result.array_context_values[label].smt())
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
                "logical_expectation":logical_expectation,"search_hint_source":search_hint_source,
                "seconds":start.elapsed().as_secs_f64(),"emission_seconds":emission_seconds,"emission_nodes":e.ids.len(),"emission_work":e.visits,"z3_seconds":0.0,
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
        self.reports.push(json!({"name":name,"status":status,"solver_result":verdict,"backend":"z3","logical_expectation":logical_expectation,"seconds":start.elapsed().as_secs_f64(),"emission_seconds":emission_seconds,"emission_nodes":e.ids.len(),"emission_work":e.visits,"z3_seconds":z3_seconds,"kernel":kernel_report,"context_symbols":ctx,"evidence":format!("{name}.smt2"),"solver_output":format!("{name}.out")}));
        Ok(())
    }
}
