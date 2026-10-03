//! Checked conjunction introduction for opt-in inductive proof decomposition.
//!
//! Each child proves one postcondition from the SAME complete precondition.
//! No child postcondition is available as an assumption to any other child.
//! Default solver limits are unchanged PER LEMMA; aggregate cost is reported.
use crate::{Check, QueryOptions};
use hwverify_ir::*;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet};
use std::time::Instant;

type QueryCallback<'a> = dyn FnMut(&mut Check, &str, &Term, &Env) -> Res<bool> + 'a;
const MAX_PARTS: usize = 4096;
const MAX_PLAN_NODES: usize = 100_000;

pub(crate) fn enabled() -> Res<bool> {
    let enabled = match std::env::var("HWVERIFY_CONJUNCTIVE_LEMMAS") {
        Err(std::env::VarError::NotPresent) => false,
        Ok(value) if value == "0" => false,
        Ok(value) if value == "1" => true,
        _ => return Err("HWVERIFY_CONJUNCTIVE_LEMMAS must be 0 or 1".into()),
    };
    if enabled && !crate::finite_only() {
        return Err("conjunctive proof decomposition requires finite-only mode".into());
    }
    Ok(enabled)
}

#[derive(Clone)]
struct Plan {
    pre: Term,
    post: Term,
    parts: Vec<Term>,
}
impl Plan {
    fn new(pre: Term, post: Term) -> Res<Self> {
        if pre.0.sort != Sort::Bool || post.0.sort != Sort::Bool {
            return Err("conjunctive proof requires Boolean pre/post conditions".into());
        }
        fn split(
            t: &Term,
            guards: &[Term],
            parts: &mut Vec<Term>,
            nodes: &mut usize,
            depth: usize,
        ) -> Res<()> {
            *nodes += 1;
            if *nodes > MAX_PLAN_NODES
                || depth > 256
                || guards.len() > 128
                || parts.len() >= MAX_PARTS
            {
                return Err("conjunctive proof plan limit exceeded".into());
            }
            // Equality distributes over a directly Boolean-controlled mux.
            // This exposes existing clock/stage cases without introducing ISA
            // assumptions or sharing facts between sibling postconditions.
            if t.0.op == "=" && t.0.args.len() == 2 {
                for side in 0..2 {
                    let operand = &t.0.args[side];
                    if operand.0.op != "ite" || operand.0.args.len() != 3 {
                        continue;
                    }
                    let control = &operand.0.args[0];
                    let direct = control.0.op.starts_with('@') && control.0.sort == Sort::Bool
                        || control.0.op == "not"
                            && control.0.args.len() == 1
                            && control.0.args[0].0.op.starts_with('@')
                            && control.0.args[0].0.sort == Sort::Bool;
                    if !direct {
                        continue;
                    }
                    let known = if guards.iter().any(|g| g == control) {
                        Some(true)
                    } else if guards.iter().any(|g| *g == not(control.clone())) {
                        Some(false)
                    } else {
                        None
                    };
                    for value in [true, false] {
                        if known.is_some_and(|v| v != value) {
                            continue;
                        }
                        let mut args = t.0.args.clone();
                        args[side] = operand.0.args[if value { 1 } else { 2 }].clone();
                        let mut branch_guards = guards.to_vec();
                        if known.is_none() {
                            branch_guards.push(if value {
                                control.clone()
                            } else {
                                not(control.clone())
                            });
                        }
                        split(
                            &node(Sort::Bool, "=", args),
                            &branch_guards,
                            parts,
                            nodes,
                            depth + 1,
                        )?;
                    }
                    return Ok(());
                }
            }
            if t.0.op == "and" && t.0.args.len() == 2 {
                split(&t.0.args[0], guards, parts, nodes, depth + 1)?;
                split(&t.0.args[1], guards, parts, nodes, depth + 1)?;
            } else if t.0.op == "=>" && t.0.args.len() == 2 {
                let mut g = guards.to_vec();
                g.push(t.0.args[0].clone());
                split(&t.0.args[1], &g, parts, nodes, depth + 1)?;
            } else if t.0.op == "ite" && t.0.sort == Sort::Bool && t.0.args.len() == 3 {
                let mut yes = guards.to_vec();
                yes.push(t.0.args[0].clone());
                split(&t.0.args[1], &yes, parts, nodes, depth + 1)?;
                let mut no = guards.to_vec();
                no.push(not(t.0.args[0].clone()));
                split(&t.0.args[2], &no, parts, nodes, depth + 1)?;
            } else {
                let part = guards.iter().rev().fold(t.clone(), |body, guard| {
                    node(Sort::Bool, "=>", vec![guard.clone(), body])
                });
                parts.push(part);
            }
            Ok(())
        }
        let mut parts = vec![];
        split(&post, &[], &mut parts, &mut 0, 0)?;
        Ok(Self { pre, post, parts })
    }
    fn check(&self, pre: &Term, post: &Term, parts: &[Term]) -> Res<()> {
        if &self.pre != pre || &self.post != post || self.parts != parts {
            return Err(
                "stale, missing, duplicated or reordered conjunctive proof coverage".into(),
            );
        }
        Ok(())
    }
    fn query(&self, index: usize) -> Term {
        // The actual finite query, not just a diagnostic residual, exposes the
        // exact guards of a negated implication. The complete pre remains an
        // unchanged conjunct; sibling postconditions are never assumptions.
        let mut body = &self.parts[index];
        let mut guards = vec![];
        while body.0.op == "=>" {
            guards.push(body.0.args[0].clone());
            body = &body.0.args[1];
        }
        let bad = guards
            .into_iter()
            .rev()
            .fold(not(body.clone()), |rest, guard| and(guard, rest));
        and(self.pre.clone(), bad)
    }
}

/// Read-only planning for editor requests. These queries confer no proof authority.
/// Uses the exact same guard-preserving decomposition as live refinement checking.
pub fn implication_queries(pre: Term, post: Term) -> Res<Vec<Term>> {
    let plan = Plan::new(pre, post)?;
    Ok((0..plan.parts.len()).map(|i| plan.query(i)).collect())
}

fn validated_children(names: &[String], reports: &[Value]) -> Res<bool> {
    if names.len() != reports.len()
        || names.iter().collect::<BTreeSet<_>>().len() != names.len()
        || names
            .iter()
            .zip(reports)
            .any(|(name, report)| report["name"] != *name)
    {
        return Err("missing, duplicated or reordered conjunctive proof reports".into());
    }
    for report in reports {
        if report["backend"] == "checked_proof_bundle" {
            crate::proof_bundle::validate_report(report)?;
            continue;
        }
        if !matches!(
            report["backend"].as_str(),
            Some("finite_bv" | "structural_kernel")
        ) {
            return Err("conjunctive proof only accepts finite or structural children".into());
        }
        match report["solver_result"].as_str() {
            Some("unsat") if report["status"] == "passed" => {}
            Some("unknown") if report["status"] == "unknown" => {}
            Some("sat")
                if report["status"] == "counterexample"
                    && report["finite"]["original_formula_validated"] == true => {}
            _ => return Err("contradictory or unvalidated conjunctive proof result".into()),
        }
    }
    Ok(reports
        .iter()
        .all(|r| r["solver_result"] == "unsat" && r["status"] == "passed"))
}

/// Preserve all original input/prestate variables for witnesses, without forcing
/// unused NEXT-state debug cones into every child CNF. The actual child formula
/// remains unchanged; the full original query is rerun before reporting SAT.
fn primitive_context(context: &Env) -> Env {
    let mut env = Env::new();
    let mut seen = HashSet::new();
    let mut todo = context.values().cloned().collect::<Vec<_>>();
    while let Some(t) = todo.pop() {
        if !seen.insert(t.clone()) {
            continue;
        }
        if let Some(name) = t.0.op.strip_prefix('@') {
            env.insert(name.to_string(), t.clone());
        }
        todo.extend(t.0.args.iter().cloned());
    }
    env
}

#[derive(Clone, Copy)]
struct Timeouts {
    original: u64,
    child: u64,
    replay: u64,
}
impl Default for Timeouts {
    fn default() -> Self {
        Self {
            original: 10_000,
            child: 10_000,
            replay: 10_000,
        }
    }
}

impl Check {
    pub fn query_implication(
        &mut self,
        name: &str,
        pre: Term,
        post: Term,
        context: &Env,
    ) -> Res<()> {
        let enabled = enabled()? || crate::automatic::mode()?.is_some();
        self.query_implication_mode(name, pre, post, context, enabled)
    }

    /// Explicit flag for tests; ordinary callers use the checked opt-in above.
    pub fn query_implication_mode(
        &mut self,
        name: &str,
        pre: Term,
        post: Term,
        context: &Env,
        enabled: bool,
    ) -> Res<()> {
        self.query_implication_timed(name, pre, post, context, enabled, Timeouts::default())
    }

    fn query_implication_timed(
        &mut self,
        name: &str,
        pre: Term,
        post: Term,
        context: &Env,
        enabled: bool,
        timeouts: Timeouts,
    ) -> Res<()> {
        self.query_implication_timed_hook(
            name,
            pre,
            post,
            context,
            (enabled, timeouts, false),
            None,
        )
    }
    pub fn query_implication_with_callback(
        &mut self,
        name: &str,
        pre: Term,
        post: Term,
        context: &Env,
        callback: &mut QueryCallback<'_>,
    ) -> Res<()> {
        self.query_implication_timed_hook(
            name,
            pre,
            post,
            context,
            (true, Timeouts::default(), false),
            Some(callback),
        )
    }
    /// Independent-lemma fallback preserves the ordinary child attempt as
    /// separately accounted evidence. Shared-query programs must run before it.
    pub fn query_implication_with_fallback(
        &mut self,
        name: &str,
        pre: Term,
        post: Term,
        context: &Env,
        callback: &mut QueryCallback<'_>,
    ) -> Res<()> {
        self.query_implication_timed_hook(
            name,
            pre,
            post,
            context,
            (true, Timeouts::default(), true),
            Some(callback),
        )
    }
    fn query_implication_timed_hook(
        &mut self,
        name: &str,
        pre: Term,
        post: Term,
        context: &Env,
        settings: (bool, Timeouts, bool),
        mut callback: Option<&mut QueryCallback<'_>>,
    ) -> Res<()> {
        let (enabled, timeouts, after_unknown) = settings;
        let automatic_mode = crate::automatic::mode()?;
        if callback.is_some()
            && automatic_mode.is_some_and(|mode| {
                (mode == crate::CutBudgetMode::IndependentLemmas) != after_unknown
            })
        {
            return Err(
                "automatic proof budget mode must match the explicit proof callback".into(),
            );
        }
        let after_unknown =
            after_unknown || automatic_mode == Some(crate::CutBudgetMode::IndependentLemmas);
        let overall_start = Instant::now();
        let bad = and(pre.clone(), not(post.clone()));
        if enabled && !crate::finite_only() {
            return Err("conjunctive proof decomposition requires finite-only mode".into());
        }
        let validation_start = Instant::now();
        let validation = if enabled {
            Some(crate::finite::validate_original_context(&bad, context)?)
        } else {
            None
        };
        let validation_seconds = validation_start.elapsed().as_secs_f64();
        self.query_with_timeout(name, bad.clone(), false, context, timeouts.original)?;
        if let Some((nodes, work)) = validation {
            let report = self.reports.last_mut().unwrap();
            report["original_source_validation"] = json!({"nodes":nodes,"work":work,"seconds":validation_seconds,"includes_derived_context":true});
            report["seconds"] =
                json!(report["seconds"].as_f64().unwrap_or(0.0) + validation_seconds);
        }
        if !enabled || self.reports.last().unwrap()["status"] != "unknown" {
            return Ok(());
        }
        if !crate::finite_only() {
            return Err("conjunctive proof decomposition requires finite-only mode".into());
        }
        let original = self.reports.pop().unwrap();
        let plan = match Plan::new(pre.clone(), post.clone()) {
            Ok(plan) => plan,
            Err(reason) => {
                let mut report = original;
                report["conjunctive_plan_error"] = json!(reason);
                self.reports.push(report);
                return Ok(());
            }
        };
        let frozen_parts = plan.parts.clone();
        let names = (0..plan.parts.len())
            .map(|i| format!("{name}_lemma_{i:04}"))
            .collect::<Vec<_>>();
        let child_context = primitive_context(context);
        let mut children = vec![];
        for (index, child_name) in names.iter().enumerate() {
            plan.check(&pre, &post, &frozen_parts)?;
            let bad_child = plan.query(index);
            let original_child = if after_unknown {
                self.query_with_timeout(
                    &format!("{child_name}_uncut"),
                    bad_child.clone(),
                    false,
                    &child_context,
                    timeouts.child,
                )?;
                let mut result = self.reports.pop().unwrap();
                result["name"] = json!(child_name);
                if result["solver_result"] != "unknown" {
                    children.push(result);
                    continue;
                }
                Some(result)
            } else {
                None
            };
            let before = self.reports.len();
            let mut handled = if let Some(hook) = callback.as_deref_mut() {
                match hook(self, child_name, &bad_child, context) {
                    Ok(handled) => handled,
                    Err(error)
                        if self.reports.len() == before + 1
                            && self
                                .reports
                                .last()
                                .is_some_and(|r| r["backend"] == "checked_proof_bundle") =>
                    {
                        self.reports.last_mut().unwrap()["proof_program_error"] = json!(error);
                        true
                    }
                    Err(error) => return Err(error),
                }
            } else {
                false
            };
            let mut automatic_search = Value::Null;
            if !handled {
                if let Some(mode) = automatic_mode {
                    (handled, automatic_search) = self.query_automatic_proof_detailed(
                        child_name,
                        bad_child.clone(),
                        context,
                        mode,
                    )?;
                }
            }
            if handled {
                if after_unknown
                    && self
                        .reports
                        .last()
                        .is_some_and(|r| r["cost"]["mode"] != "independent_lemmas")
                {
                    return Err("post-Unknown fallback requires independent lemma budgets".into());
                }
                if self.reports.len() != before + 1
                    || self.reports.last().unwrap()["name"] != *child_name
                    || self.reports.last().unwrap()["backend"] != "checked_proof_bundle"
                {
                    return Err(
                        "proof callback did not return one exact current-query bundle".into(),
                    );
                }
            } else {
                if self.reports.len() != before {
                    return Err("unmatched proof callback changed reports".into());
                }
                if let Some(original) = original_child.clone() {
                    self.reports.push(original);
                } else if automatic_mode == Some(crate::CutBudgetMode::SharedQuery)
                    && !automatic_search.is_null()
                {
                    // An abandoned search still consumes the strict shared
                    // query budget before the ordinary finite fallback.
                    let mut limits = crate::finite::Limits::default();
                    limits.max_work = limits
                        .max_work
                        .saturating_sub(automatic_search["search_work"].as_u64().unwrap_or(0));
                    let search_ms = (automatic_search["search_seconds"].as_f64().unwrap_or(0.0)
                        * 1000.0)
                        .ceil() as u64;
                    limits.timeout_ms = limits
                        .timeout_ms
                        .min(timeouts.child)
                        .saturating_sub(search_ms);
                    self.query_limited(
                        child_name,
                        bad_child,
                        false,
                        &child_context,
                        crate::QueryOptions {
                            timeout_ms: limits.timeout_ms,
                            ..Default::default()
                        },
                        Some(crate::z3::QueryBudget {
                            limits,
                            allow_kernel: false,
                        }),
                    )?;
                } else {
                    self.query_with_timeout(
                        child_name,
                        bad_child,
                        false,
                        &child_context,
                        timeouts.child,
                    )?;
                }
                if !automatic_search.is_null() {
                    self.reports.last_mut().unwrap()["automatic_search_attempt"] = automatic_search;
                }
            }
            if handled {
                if let Some(original) = original_child {
                    self.reports.last_mut().unwrap()["original_attempt"] = original;
                }
            }
            children.push(self.reports.pop().unwrap());
        }
        plan.check(&pre, &post, &frozen_parts)?;
        let passed = validated_children(&names, &children)?;
        if children.iter().any(|r| r["solver_result"] == "sat") {
            // A child SAT is logically a parent counterexample, but preserve the
            // stronger policy: require independent replay of the ORIGINAL full
            // query before reporting a parent counterexample.
            self.query_with_options(
                &format!("{name}_original_sat_recheck"),
                bad,
                false,
                context,
                QueryOptions {
                    finite_search_hint: Some(crate::finite::SearchHint::Sat),
                    timeout_ms: timeouts.replay,
                    ..Default::default()
                },
            )?;
            let report = self.reports.last_mut().unwrap();
            report["name"] = json!(name);
            if report["solver_result"] == "unsat" {
                return Err("conjunctive child SAT contradicts original-query UNSAT".into());
            }
            report["conjunctive_children"] = json!(children);
            report["original_source_validation"] = original["original_source_validation"].clone();
            report["conjunctive_total_seconds"] = json!(overall_start.elapsed().as_secs_f64());
            report["original_attempt"] = original;
            return Ok(());
        }
        let primitives = primitive_query_reports(&children);
        let total_work = primitives
            .iter()
            .map(|r| r["finite"]["work"].as_u64().unwrap_or(0))
            .sum::<u64>();
        let total_clauses = primitives
            .iter()
            .map(|r| r["finite"]["clauses"].as_u64().unwrap_or(0))
            .sum::<u64>();
        let total_seconds = primitives
            .iter()
            .map(|r| r["seconds"].as_f64().unwrap_or(0.0))
            .sum::<f64>();
        self.reports.push(json!({
            "name":name, "status":if passed {"passed"} else {"unknown"},
            "solver_result":if passed {"unsat"} else {"unknown"}, "backend":"conjunctive_lemmas",
            "logical_expectation":"unsat", "seconds":overall_start.elapsed().as_secs_f64(), "z3_seconds":0.0,
            "original_attempt":original, "children":children,
            "original_source_validation":{"nodes":validation.unwrap().0,"work":validation.unwrap().1,"seconds":validation_seconds,"includes_derived_context":true},
            "coverage":{"rule":"exact-conjunction-introduction-v1", "ordered_names":names,
                "complete":true,"same_full_precondition":true,"postconditions_used_as_assumptions":false,
                "transition_binding":"all post terms lowered from the same validated transition document",
                "transformations":["conjunction flattening","implication distributes over conjunction","Boolean ITE is both guarded branches","equality distributes over directly Boolean-controlled ITE","negated implication is asserted guard and negated consequent"]},
            "cost":{"per_lemma_work_limit":100_000_000u64,"per_lemma_clause_limit":1_000_000u64,
                "per_lemma_timeout_ms":10_000u64,"total_child_work":total_work,"total_child_clauses":total_clauses,"total_child_seconds":total_seconds,
                "original_monolithic_result":"unknown","whole_bundle_budget_is_not_a_single_query_budget":true},
            "trusted":"Rust exact conjunction coverage and original validated transition binding; each child is independently checked with unchanged limits"
        }));
        Ok(())
    }
}

/// Every actual backend invocation, including an unsuccessful original query.
/// The derived conjunction parent is not itself a solver invocation.
pub fn primitive_query_reports(reports: &[Value]) -> Vec<&Value> {
    fn visit<'a>(report: &'a Value, result: &mut Vec<&'a Value>) {
        if let Some(original) = report.get("original_attempt") {
            visit(original, result);
        }
        for key in ["children", "conjunctive_children"] {
            if let Some(children) = report.get(key).and_then(Value::as_array) {
                for child in children {
                    visit(child, result);
                }
            }
        }
        if !matches!(
            report["backend"].as_str(),
            Some("conjunctive_lemmas" | "checked_proof_bundle" | "checked_congruence")
        ) {
            result.push(report);
        }
    }
    let mut result = vec![];
    for report in reports {
        visit(report, &mut result);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn v(name: &str) -> Term {
        node(Sort::Bool, format!("@{name}"), vec![])
    }
    fn evaluate(t: &Term, values: &[bool; 3]) -> bool {
        match t.0.op.as_str() {
            "@a" => values[0],
            "@b" => values[1],
            "@c" => values[2],
            "true" => true,
            "false" => false,
            "and" => evaluate(&t.0.args[0], values) && evaluate(&t.0.args[1], values),
            "=" => evaluate(&t.0.args[0], values) == evaluate(&t.0.args[1], values),
            "not" => !evaluate(&t.0.args[0], values),
            "=>" => !evaluate(&t.0.args[0], values) || evaluate(&t.0.args[1], values),
            "ite" => evaluate(
                &t.0.args[if evaluate(&t.0.args[0], values) { 1 } else { 2 }],
                values,
            ),
            _ => panic!("unhandled independent Boolean oracle"),
        }
    }
    #[test]
    fn exact_guarded_cover_and_same_precondition() {
        let (a, b, c) = (v("a"), v("b"), v("c"));
        let post = and(
            node(Sort::Bool, "=>", vec![a.clone(), and(b.clone(), c.clone())]),
            ite(b.clone(), and(a.clone(), c.clone()), not(a.clone())),
        );
        let plan = Plan::new(a.clone(), post.clone()).unwrap();
        for bits in 0..8 {
            let values = [bits & 1 != 0, bits & 2 != 0, bits & 4 != 0];
            assert_eq!(
                evaluate(&post, &values),
                plan.parts.iter().all(|p| evaluate(p, &values))
            );
            for i in 0..plan.parts.len() {
                assert_eq!(plan.query(i).0.args[0], a);
                assert_eq!(
                    evaluate(&plan.query(i), &values),
                    evaluate(&and(a.clone(), not(plan.parts[i].clone())), &values)
                );
            }
        }
        assert!(plan.check(&a, &post, &plan.parts).is_ok());
        assert!(plan.check(&b, &post, &plan.parts).is_err());
        assert!(plan.check(&a, &b, &plan.parts).is_err());
        let mut parts = plan.parts.clone();
        parts.pop();
        assert!(plan.check(&a, &post, &parts).is_err());
        let mut parts = plan.parts.clone();
        parts.push(parts[0].clone());
        assert!(plan.check(&a, &post, &parts).is_err());
        let mut parts = plan.parts.clone();
        parts.reverse();
        assert!(plan.check(&a, &post, &parts).is_err());
    }
    #[test]
    fn equality_mux_case_coverage_is_exact() {
        let (a, b, c) = (v("a"), v("b"), v("c"));
        for post in [
            eq(
                ite(a.clone(), b.clone(), c.clone()),
                ite(a.clone(), c.clone(), b.clone()),
            ),
            eq(
                ite(
                    not(a.clone()),
                    b.clone(),
                    ite(c.clone(), a.clone(), b.clone()),
                ),
                c.clone(),
            ),
        ] {
            let plan = Plan::new(b.clone(), post.clone()).unwrap();
            assert!(plan.parts.len() > 1);
            for bits in 0..8 {
                let values = [bits & 1 != 0, bits & 2 != 0, bits & 4 != 0];
                assert_eq!(
                    evaluate(&post, &values),
                    plan.parts.iter().all(|p| evaluate(p, &values))
                );
                for i in 0..plan.parts.len() {
                    assert_eq!(plan.query(i).0.args[0], b);
                    assert_eq!(
                        evaluate(&plan.query(i), &values),
                        evaluate(&and(b.clone(), not(plan.parts[i].clone())), &values)
                    );
                }
            }
        }
    }
    #[test]
    fn limits_and_literal_postconditions_fail_closed() {
        let mut deep = boolv(true);
        for _ in 0..300 {
            deep = and(v("a"), deep);
        }
        assert!(Plan::new(v("a"), deep).is_err());
        for post in [boolv(false), boolv(true)] {
            let plan = Plan::new(v("a"), post.clone()).unwrap();
            assert_eq!(plan.parts, vec![post]);
        }
        assert!(Plan::new(bv(1, 0), boolv(true)).is_err());
    }
    #[test]
    fn projection_cannot_hide_unsupported_or_inconsistent_context() {
        let dead = node(Sort::Bv(8), "unsupported", vec![bv(8, 0)]);
        let mut context = Env::new();
        context.insert("derived".into(), dead);
        assert!(crate::finite::validate_original_context(&boolv(false), &context).is_err());
        let mut context = Env::new();
        context.insert("bool".into(), v("same"));
        context.insert("word".into(), node(Sort::Bv(8), "@same", vec![]));
        assert!(crate::finite::validate_original_context(&boolv(false), &context).is_err());
        let mut context = Env::new();
        context.insert("word".into(), bv(8, 3));
        assert!(crate::finite::validate_original_context(&boolv(false), &context).is_ok());
    }
    #[test]
    fn actual_fallback_subprocess() {
        for mode in ["finite", "nonfinite"] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "conjunctive::tests::actual_fallback_worker",
                    "--nocapture",
                ])
                .env("HWVERIFY_CONJ_TEST_WORKER", mode)
                .env("HWVERIFY_KERNEL", "off");
            if mode == "finite" {
                command
                    .env("HWVERIFY_SOLVER", "finite")
                    .env("HWVERIFY_CONJUNCTIVE_LEMMAS", "0");
            } else {
                command
                    .env_remove("HWVERIFY_SOLVER")
                    .env("HWVERIFY_CONJUNCTIVE_LEMMAS", "1");
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    #[test]
    fn actual_fallback_worker() {
        let Ok(mode) = std::env::var("HWVERIFY_CONJ_TEST_WORKER") else {
            return;
        };
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let folder =
            std::env::temp_dir().join(format!("hwverify-conj-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&folder).unwrap();
        let mut check = Check {
            z3: "must-not-invoke-z3".into(),
            out: folder.clone(),
            reports: vec![],
        };
        if mode == "nonfinite" {
            let error = check
                .query("blocked_before_backend", boolv(false), false, &Env::new())
                .unwrap_err();
            assert!(error.contains("finite-only"));
            assert!(check.reports.is_empty());
            std::fs::remove_dir_all(folder).unwrap();
            return;
        }
        let x = node(Sort::Bv(8), "@x", vec![]);
        let y = node(Sort::Bv(8), "@y", vec![]);
        let add = |a, b| node(Sort::Bv(8), "bvadd", vec![a, b]);
        let sub = |a, b| node(Sort::Bv(8), "bvsub", vec![a, b]);
        let one = bv(8, 1);
        let identity = and(
            eq(
                add(add(x.clone(), one.clone()), y.clone()),
                add(x.clone(), add(y.clone(), one)),
            ),
            eq(sub(add(x.clone(), y.clone()), y.clone()), x.clone()),
        );
        let context = Env::from([("x".into(), x.clone()), ("y".into(), y)]);
        check
            .query_implication_timed(
                "closed",
                boolv(true),
                identity.clone(),
                &context,
                true,
                Timeouts {
                    original: 0,
                    ..Default::default()
                },
            )
            .unwrap();
        let result = check.reports.last().unwrap();
        assert_eq!(result["backend"], "conjunctive_lemmas");
        assert_eq!(result["status"], "passed");
        assert_eq!(result["original_attempt"]["status"], "unknown");
        assert!(result["children"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["backend"] == "finite_bv"));
        check
            .query_implication_timed(
                "child_unknown",
                boolv(true),
                identity,
                &context,
                true,
                Timeouts {
                    original: 0,
                    child: 0,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(check.reports.last().unwrap()["status"], "unknown");
        let bad = and(eq(x.clone(), bv(8, 0)), eq(x, bv(8, 1)));
        check
            .query_implication_timed(
                "counterexample",
                boolv(true),
                bad.clone(),
                &context,
                true,
                Timeouts {
                    original: 0,
                    ..Default::default()
                },
            )
            .unwrap();
        let result = check.reports.last().unwrap();
        assert_eq!(result["status"], "counterexample");
        assert_eq!(result["finite"]["original_formula_validated"], true);
        assert!(result["evidence"]
            .as_str()
            .unwrap()
            .contains("original_sat_recheck"));
        check
            .query_implication_timed(
                "replay_unknown",
                boolv(true),
                bad,
                &context,
                true,
                Timeouts {
                    original: 0,
                    replay: 0,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(check.reports.last().unwrap()["status"], "unknown");
        let mut context = context;
        context.insert(
            "dead_unsupported".into(),
            node(Sort::Bv(8), "unsupported", vec![bv(8, 0)]),
        );
        assert!(check
            .query_implication_timed(
                "unsupported",
                boolv(true),
                boolv(true),
                &context,
                true,
                Timeouts::default()
            )
            .is_err());
        std::fs::remove_dir_all(folder).unwrap();
    }
    #[test]
    fn missing_unknown_sat_and_duplicate_reports_never_pass() {
        let names = vec!["first".into(), "second".into()];
        let good = |name| json!({"name":name,"status":"passed","solver_result":"unsat","backend":"finite_bv"});
        let reports = vec![good("first"), good("second")];
        assert!(validated_children(&names, &reports).unwrap());
        assert!(validated_children(&names, &reports[..1]).is_err());
        assert!(validated_children(&names, &[reports[0].clone(), reports[0].clone()]).is_err());
        assert!(validated_children(&names, &[reports[1].clone(), reports[0].clone()]).is_err());
        let mut changed = reports.clone();
        changed[1]["status"] = json!("unknown");
        changed[1]["solver_result"] = json!("unknown");
        assert!(!validated_children(&names, &changed).unwrap());
        changed[1]["status"] = json!("counterexample");
        changed[1]["solver_result"] = json!("sat");
        assert!(validated_children(&names, &changed).is_err());
        changed[1]["finite"] = json!({"original_formula_validated":true});
        assert!(!validated_children(&names, &changed).unwrap());
        changed[1]["backend"] = json!("z3");
        assert!(validated_children(&names, &changed).is_err());
    }
}
