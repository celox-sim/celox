//! Opt-in, current-query acyclic equality cuts. Proposals carry no authority.
//! Independent lemma budgets and strict shared query budgets are distinct.
use crate::{finite, Check, QueryOptions};
use hwverify_ir::*;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    time::Instant,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutBudgetMode {
    SharedQuery,
    IndependentLemmas,
}
impl CutBudgetMode {
    fn name(self) -> &'static str {
        match self {
            Self::SharedQuery => "shared_query",
            Self::IndependentLemmas => "independent_lemmas",
        }
    }
}
const MAX_CUTS: usize = 2;
const MAX_PLAN_WORK: u64 = 100_000;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Plan {
    original: Term,
    prefixes: Vec<Term>,
    goal: Term,
    guard: Term,
    branch_value: Term,
    opposite: Term,
    ite_side: usize,
    cuts: Vec<(Term, Term)>,
}
impl Plan {
    fn new(original: &Term) -> Res<Option<(Self, u64)>> {
        let mut tail = original;
        let mut prefixes = vec![];
        while tail.0.op == "and" && tail.0.args.len() == 2 {
            if prefixes.len() >= 128 {
                return Ok(None);
            }
            prefixes.push(tail.0.args[0].clone());
            tail = &tail.0.args[1];
        }
        if tail.0.op != "not" || tail.0.args.len() != 1 {
            return Ok(None);
        }
        let goal = &tail.0.args[0];
        if goal.0.op != "=" || goal.0.args.len() != 2 {
            return Ok(None);
        }
        let mut work = prefixes.len() as u64;
        for side in 0..2 {
            let mux = &goal.0.args[side];
            let opposite = &goal.0.args[1 - side];
            if mux.0.op != "ite" || mux.0.args.len() != 3 || mux.0.args[0].0.sort != Sort::Bool {
                continue;
            }
            for branch in 1..=2 {
                let value = &mux.0.args[branch];
                if value.0.args.is_empty()
                    || value.0.op != opposite.0.op
                    || value.0.sort != opposite.0.sort
                    || value.0.args.len() != opposite.0.args.len()
                {
                    continue;
                }
                let mut pending = vec![(value.clone(), opposite.clone(), 0usize)];
                let mut seen = HashSet::new();
                let mut cuts = vec![];
                let mut valid = true;
                while let Some((target, source, depth)) = pending.pop() {
                    work += 1;
                    if work > MAX_PLAN_WORK || depth > 512 {
                        return Err("checked congruence plan budget exhausted".into());
                    }
                    if target == source || !seen.insert((target.clone(), source.clone())) {
                        continue;
                    }
                    if target.0.sort != source.0.sort {
                        valid = false;
                        break;
                    }
                    if target.0.op == source.0.op
                        && target.0.args.len() == source.0.args.len()
                        && !source.0.args.is_empty()
                    {
                        pending.extend(
                            target
                                .0
                                .args
                                .iter()
                                .zip(&source.0.args)
                                .map(|(a, b)| (a.clone(), b.clone(), depth + 1)),
                        );
                    } else {
                        // Never offer the entire goal operand as its own lemma.
                        if source == *opposite
                            || cuts.iter().any(|(a, b)| *a == source && *b != target)
                        {
                            valid = false;
                            break;
                        }
                        if !cuts.iter().any(|(a, b)| *a == source && *b == target) {
                            cuts.push((source, target));
                        }
                        if cuts.len() > MAX_CUTS {
                            valid = false;
                            break;
                        }
                    }
                }
                if valid && !cuts.is_empty() {
                    return Ok(Some((
                        Self {
                            original: original.clone(),
                            prefixes: prefixes.clone(),
                            goal: goal.clone(),
                            guard: if branch == 1 {
                                mux.0.args[0].clone()
                            } else {
                                not(mux.0.args[0].clone())
                            },
                            branch_value: value.clone(),
                            opposite: opposite.clone(),
                            ite_side: side,
                            cuts,
                        },
                        work,
                    )));
                }
            }
        }
        Ok(None)
    }
    fn antecedent(&self, positive: bool) -> Term {
        self.prefixes
            .iter()
            .cloned()
            .chain(std::iter::once(if positive {
                self.guard.clone()
            } else {
                not(self.guard.clone())
            }))
            .reduce(and)
            .unwrap()
    }
    fn equality_query(&self, index: usize) -> Term {
        let (a, b) = &self.cuts[index];
        and(self.antecedent(true), not(eq(a.clone(), b.clone())))
    }
    fn original_query(&self) -> Term {
        self.prefixes
            .iter()
            .rev()
            .fold(not(self.goal.clone()), |tail, p| and(p.clone(), tail))
    }
}

// Private, process-local objects: neither reports nor serialized expressions can
// mint these. All equalities remain independent of other equality handles.
struct Handle {
    owner: Rc<()>,
    index: usize,
    equality: (Term, Term),
}
struct Session {
    plan: Plan,
    frozen: Plan,
    context: Env,
    frozen_context: Env,
    owner: Rc<()>,
    mode: CutBudgetMode,
    start: Instant,
    limits: finite::Limits,
    work: u64,
    clauses: usize,
    terms: usize,
    variables: usize,
    children: Vec<Value>,
    validation: Value,
}
impl Session {
    fn check(&self) -> Res<()> {
        if self.plan != self.frozen
            || self.context != self.frozen_context
            || self.plan.original_query() != self.plan.original
        {
            return Err("stale checked congruence query/context/plan".into());
        }
        Ok(())
    }
    fn remaining(&self) -> Res<finite::Limits> {
        if self.mode == CutBudgetMode::IndependentLemmas {
            return Ok(self.limits.clone());
        }
        let elapsed = self.start.elapsed().as_millis() as u64;
        if self.work >= self.limits.max_work
            || self.clauses >= self.limits.max_clauses
            || elapsed >= self.limits.timeout_ms
            || self.terms >= self.limits.max_terms
            || self.variables >= self.limits.max_variables
        {
            return Err("checked congruence shared query budget exhausted".into());
        }
        Ok(finite::Limits {
            max_work: self.limits.max_work - self.work,
            max_clauses: self.limits.max_clauses - self.clauses,
            timeout_ms: self.limits.timeout_ms - elapsed,
            max_terms: self.limits.max_terms - self.terms,
            max_variables: self.limits.max_variables - self.variables,
            ..self.limits.clone()
        })
    }
    fn query(
        &mut self,
        check: &mut Check,
        name: &str,
        formula: Term,
        sat_hint: bool,
    ) -> Res<Value> {
        self.check()?;
        if !crate::finite_only() {
            return Err("checked congruence solver mode changed".into());
        }
        let limits = self.remaining()?;
        check.query_limited(
            name,
            formula,
            false,
            &self.context,
            QueryOptions {
                timeout_ms: limits.timeout_ms,
                finite_search_hint: Some(if sat_hint {
                    finite::SearchHint::Sat
                } else {
                    finite::SearchHint::Unsat
                }),
                ..Default::default()
            },
            Some(crate::z3::QueryBudget {
                limits,
                allow_kernel: self.mode == CutBudgetMode::IndependentLemmas,
            }),
        )?;
        let report = check
            .reports
            .pop()
            .ok_or("missing fresh checked congruence query result")?;
        self.work = self
            .work
            .saturating_add(report["finite"]["work"].as_u64().unwrap_or(0))
            .saturating_add(report["emission_work"].as_u64().unwrap_or(0));
        self.clauses = self
            .clauses
            .saturating_add(report["finite"]["clauses"].as_u64().unwrap_or(0) as usize);
        self.terms = self
            .terms
            .saturating_add(report["finite"]["terms"].as_u64().unwrap_or(0) as usize);
        self.variables = self
            .variables
            .saturating_add(report["finite"]["variables"].as_u64().unwrap_or(0) as usize);
        self.children.push(report.clone());
        self.check()?;
        Ok(report)
    }
    fn rewrite(&mut self, handles: &[Handle]) -> Res<Term> {
        self.check()?;
        if handles.len() != self.plan.cuts.len() {
            return Err("missing checked equality handle".into());
        }
        let mut indices = HashSet::new();
        for h in handles {
            if !Rc::ptr_eq(&h.owner, &self.owner)
                || !indices.insert(h.index)
                || self.plan.cuts.get(h.index) != Some(&h.equality)
            {
                return Err("foreign, duplicate, or changed checked equality handle".into());
            }
        }
        let rules = handles
            .iter()
            .map(|h| h.equality.clone())
            .collect::<HashMap<_, _>>();
        let root = self.plan.opposite.clone();
        let mut memo = HashMap::new();
        let mut pending = vec![(root.clone(), false, 0usize)];
        let mut rewrite_work = 0;
        while let Some((t, exit, depth)) = pending.pop() {
            self.work += 1;
            rewrite_work += 1;
            if depth > self.limits.max_depth
                || memo.len() > self.limits.max_terms
                || rewrite_work > self.limits.max_work
                || (self.mode == CutBudgetMode::SharedQuery && self.work > self.limits.max_work)
            {
                return Err("checked congruence rewrite budget exhausted".into());
            }
            if memo.contains_key(&t) {
                continue;
            }
            if let Some(rhs) = rules.get(&t) {
                memo.insert(t, rhs.clone());
                continue;
            }
            if exit {
                let args = t.0.args.iter().map(|x| memo[x].clone()).collect();
                memo.insert(t.clone(), node(t.0.sort.clone(), t.0.op.clone(), args));
            } else {
                pending.push((t.clone(), true, depth));
                pending.extend(t.0.args.iter().map(|x| (x.clone(), false, depth + 1)));
            }
        }
        self.terms = self.terms.saturating_add(memo.len());
        let other = memo.remove(&root).unwrap();
        Ok(if self.plan.ite_side == 0 {
            eq(self.plan.branch_value.clone(), other)
        } else {
            eq(other, self.plan.branch_value.clone())
        })
    }
    fn within_shared_budget(&self) -> bool {
        self.mode != CutBudgetMode::SharedQuery
            || (self.work <= self.limits.max_work
                && self.clauses <= self.limits.max_clauses
                && self.terms <= self.limits.max_terms
                && self.variables <= self.limits.max_variables
                && self.start.elapsed().as_millis() < (self.limits.timeout_ms as u128))
    }
}

impl Check {
    /// Returns false without changing reports if this bounded structural plan
    /// does not match. No saved report, candidate list, or foreign handle is an input.
    pub fn query_checked_congruence(
        &mut self,
        name: &str,
        original: Term,
        context: &Env,
        mode: CutBudgetMode,
    ) -> Res<bool> {
        self.query_checked_congruence_limited(
            name,
            original,
            context,
            mode,
            finite::Limits::default(),
        )
    }
    fn query_checked_congruence_limited(
        &mut self,
        name: &str,
        original: Term,
        context: &Env,
        mode: CutBudgetMode,
        limits: finite::Limits,
    ) -> Res<bool> {
        if !crate::finite_only() {
            return Err("checked congruence requires finite-only mode".into());
        }
        let start = Instant::now();
        let Some((plan, work)) = Plan::new(&original)? else {
            return Ok(false);
        };
        let mut session = Session {
            frozen: plan.clone(),
            plan,
            context: context.clone(),
            frozen_context: context.clone(),
            owner: Rc::new(()),
            mode,
            start,
            limits,
            work,
            clauses: 0,
            terms: 0,
            variables: 0,
            children: vec![],
            validation: Value::Null,
        };
        let validation_start = Instant::now();
        let result = (|| -> Res<bool> {
            let (nodes, validation_work) = finite::validate_original_context_with_limits(
                &original,
                context,
                session.remaining()?,
            )?;
            session.work += validation_work;
            session.terms += nodes;
            session.validation = json!({"complete":true,"includes_derived_context":true,"nodes":nodes,"work":validation_work,"seconds":validation_start.elapsed().as_secs_f64()});
            let (written, emitted) = crate::z3::write_original_query(
                &self.out,
                name,
                &original,
                context,
                Some(session.remaining()?),
            );
            session.work += emitted as u64;
            written?;
            let mut handles = vec![];
            for index in 0..session.plan.cuts.len() {
                let report = session.query(
                    self,
                    &format!("{name}_cut_{index:02}"),
                    session.plan.equality_query(index),
                    false,
                )?;
                if report["solver_result"] != "unsat" || report["status"] != "passed" {
                    return Ok(false);
                }
                handles.push(Handle {
                    owner: session.owner.clone(),
                    index,
                    equality: session.plan.cuts[index].clone(),
                });
            }
            let rewritten = session.rewrite(&handles)?;
            let positive = and(session.plan.antecedent(true), not(rewritten));
            let report = session.query(self, &format!("{name}_cut_positive"), positive, false)?;
            if report["solver_result"] != "unsat" || report["status"] != "passed" {
                return Ok(false);
            }
            let negative = and(
                session.plan.antecedent(false),
                not(session.plan.goal.clone()),
            );
            let report = session.query(self, &format!("{name}_cut_complement"), negative, false)?;
            Ok(report["solver_result"] == "unsat"
                && report["status"] == "passed"
                && session.within_shared_budget())
        })();
        let mut passed = matches!(result, Ok(true));
        let mut original_recheck = Value::Null;
        // Auxiliary SAT only rejects a cut. A parent SAT result requires a fresh
        // solve/replay of the exact ORIGINAL bad query and complete context.
        if !passed && session.children.iter().any(|r| r["solver_result"] == "sat") {
            if let Ok(report) = session.query(
                self,
                &format!("{name}_cut_original_recheck"),
                original.clone(),
                true,
            ) {
                passed = report["solver_result"] == "unsat"
                    && report["status"] == "passed"
                    && session.within_shared_budget();
                original_recheck = report;
            }
        }
        session.check()?;
        let sat = original_recheck["solver_result"] == "sat"
            && original_recheck["finite"]["original_formula_validated"] == true
            && session.within_shared_budget();
        let error = result.err();
        let mut ordered_names = (0..session.plan.cuts.len())
            .map(|i| format!("{name}_cut_{i:02}"))
            .collect::<Vec<_>>();
        ordered_names.extend([
            format!("{name}_cut_positive"),
            format!("{name}_cut_complement"),
        ]);
        let original_reproved =
            original_recheck["solver_result"] == "unsat" && original_recheck["status"] == "passed";
        self.reports.push(json!({"name":name,"status":if passed {"passed"} else if sat {"counterexample"} else {"unknown"},
            "solver_result":if passed {"unsat"} else if sat {"sat"} else {"unknown"},"backend":"checked_congruence",
            "logical_expectation":"unsat","z3_seconds":0.0,"seconds":session.start.elapsed().as_secs_f64(),
            "mode":mode.name(),"evidence":format!("{name}.smt2"),"children":session.children,"original_recheck":original_recheck,
            "original_source_validation":session.validation,"reason":error,
            "coverage":{"rule":"current-query-guarded-congruence-v1","branches":["guard","not_guard"],"complete":passed && !original_reproved,
                "ordered_names":ordered_names,"proof_route":if original_reproved {"original_recheck"} else {"independent_equalities_and_branches"},
                "same_full_precondition":true,"independent_equalities":true,"goal_only_substitution":true,"saved_reports_are_authority":false,
                "equalities":session.plan.cuts.len()},
            "cost":{"mode":mode.name(),"finite_work_and_plan_validation_work":session.work,"allocated_clauses":session.clauses,
                "per_query_work_limit":session.limits.max_work,"per_query_clause_limit":session.limits.max_clauses,"per_query_timeout_ms":session.limits.timeout_ms,
                "whole_bundle_is_one_query":mode==CutBudgetMode::SharedQuery,"kernel_enabled":mode==CutBudgetMode::IndependentLemmas,
                "kernel_cost_accounting":"independent mode preserves existing per-lemma kernel behavior; kernel seconds/rules in each child; strict shared mode disables kernel"},
            "trusted":"Rust typed exact branch coverage, independent fresh equality UNSAT, goal congruence and original-query replay; no serialized handles"}));
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, process::Command};
    fn v(n: &str) -> Term {
        var(n.into(), Sort::Bv(4))
    }
    fn add(a: Term, b: Term) -> Term {
        node(Sort::Bv(4), "bvadd", vec![a, b])
    }
    fn fixture() -> Term {
        let g = var("g".into(), Sort::Bool);
        let sum = add(v("z"), bv(4, 1));
        let p = and(
            eq(v("x"), v("z")),
            node(
                Sort::Bool,
                "=>",
                vec![not(g.clone()), eq(v("y"), sum.clone())],
            ),
        );
        and(p, not(eq(ite(g, add(v("x"), bv(4, 1)), v("y")), sum)))
    }
    fn session() -> Session {
        let (plan, work) = Plan::new(&fixture()).unwrap().unwrap();
        Session {
            frozen: plan.clone(),
            plan,
            context: Env::new(),
            frozen_context: Env::new(),
            owner: Rc::new(()),
            mode: CutBudgetMode::SharedQuery,
            start: Instant::now(),
            limits: finite::Limits::default(),
            work,
            clauses: 0,
            terms: 0,
            variables: 0,
            children: vec![],
            validation: Value::Null,
        }
    }
    fn handles(s: &Session) -> Vec<Handle> {
        s.plan
            .cuts
            .iter()
            .enumerate()
            .map(|(index, equality)| Handle {
                owner: s.owner.clone(),
                index,
                equality: equality.clone(),
            })
            .collect()
    }
    #[test]
    fn plan_preserves_exact_original_prefix_and_both_branches() {
        let s = session();
        assert_eq!(s.plan.original_query(), fixture());
        assert_eq!(s.plan.cuts, vec![(v("z"), v("x"))]);
        assert_eq!(
            s.plan.antecedent(false).0.args[1],
            not(s.plan.guard.clone())
        );
        assert_eq!(s.plan.equality_query(0).0.args[0], s.plan.antecedent(true));
        // A cut equal to the whole opposite operand is never proposed.
        assert!(Plan::new(&and(
            boolv(true),
            not(eq(ite(var("g".into(), Sort::Bool), v("x"), v("y")), v("z")))
        ))
        .unwrap()
        .is_none());
    }
    #[test]
    fn handles_reject_missing_duplicate_foreign_changed_and_stale() {
        let mut s = session();
        assert!(s.rewrite(&[]).is_err());
        let foreign = session();
        assert!(s.rewrite(&handles(&foreign)).is_err());
        let h = handles(&s);
        let extra = handles(&s);
        let mut duplicate = h;
        duplicate.extend(extra);
        assert!(s.rewrite(&duplicate).is_err());
        let mut bad = handles(&s);
        bad[0].equality.1 = v("invented");
        assert!(s.rewrite(&bad).is_err());
        let good = handles(&s);
        assert!(s.rewrite(&good).is_ok());
        s.plan.guard = boolv(false);
        assert!(s.rewrite(&good).is_err());
        let mut s = session();
        s.context.insert("new".into(), boolv(true));
        assert!(s.rewrite(&handles(&s)).is_err());
    }
    #[test]
    fn shared_accounting_rejects_aggregate_exhaustion() {
        let mut s = session();
        s.work = s.limits.max_work;
        assert!(s.remaining().is_err());
        s.work = 0;
        s.clauses = s.limits.max_clauses;
        assert!(s.remaining().is_err());
        s.clauses = 0;
        s.variables = s.limits.max_variables;
        assert!(s.remaining().is_err());
        s.variables = 0;
        s.terms = s.limits.max_terms;
        assert!(s.remaining().is_err());
        s.mode = CutBudgetMode::IndependentLemmas;
        assert_eq!(s.remaining().unwrap().max_work, 100_000_000);
    }
    #[test]
    fn actual_routes() {
        if std::env::var("HWVERIFY_TEST_CUT_CHILD").is_err() {
            let result = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "checked_congruence::tests::actual_routes",
                    "--nocapture",
                ])
                .env("HWVERIFY_TEST_CUT_CHILD", "1")
                .env("HWVERIFY_SOLVER", "finite")
                .env_remove("HWVERIFY_KERNEL")
                .env_remove("HWVERIFY_CONJUNCTIVE_LEMMAS")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{} {}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        let base =
            std::env::temp_dir().join(format!("hwverify-checked-cuts-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        let run = |name: &str, bad: Term, ctx: Env, mode: CutBudgetMode, limits: finite::Limits| {
            let out = base.join(name);
            fs::create_dir_all(&out).unwrap();
            let mut c = Check {
                z3: "forbidden".into(),
                out,
                reports: vec![],
            };
            assert!(c
                .query_checked_congruence_limited(name, bad, &ctx, mode, limits)
                .unwrap());
            c.reports.remove(0)
        };
        for mode in [CutBudgetMode::SharedQuery, CutBudgetMode::IndependentLemmas] {
            let r = run(
                mode.name(),
                fixture(),
                Env::new(),
                mode,
                finite::Limits::default(),
            );
            assert_eq!(r["status"], "passed");
            assert_eq!(r["coverage"]["complete"], true);
            assert_eq!(r["children"].as_array().unwrap().len(), 3);
            if mode == CutBudgetMode::SharedQuery {
                assert!(r["children"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|c| c["backend"] == "finite_bv"));
            }
        }
        // A false proposed equality must not become a parent counterexample.
        let g = var("g".into(), Sort::Bool);
        let sum = add(v("x"), v("y"));
        let valid = and(
            boolv(true),
            not(eq(ite(g.clone(), sum.clone(), sum), add(v("y"), v("x")))),
        );
        let r = run(
            "false_candidate",
            valid,
            Env::new(),
            CutBudgetMode::IndependentLemmas,
            finite::Limits::default(),
        );
        assert_eq!(r["status"], "passed");
        assert_eq!(r["coverage"]["proof_route"], "original_recheck");
        assert!(r["children"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["solver_result"] == "sat"));
        let invalid = and(
            boolv(true),
            not(eq(
                ite(g, add(v("x"), bv(4, 1)), v("x")),
                add(v("y"), bv(4, 1)),
            )),
        );
        let r = run(
            "actual_counterexample",
            invalid,
            Env::new(),
            CutBudgetMode::IndependentLemmas,
            finite::Limits::default(),
        );
        assert_eq!(r["status"], "counterexample");
        assert_eq!(
            r["original_recheck"]["finite"]["original_formula_validated"],
            true
        );
        for (name, ctx) in [
            (
                "unsupported_context",
                Env::from([("dead".into(), node(Sort::Bv(4), "unsupported", vec![]))]),
            ),
            (
                "inconsistent_context",
                Env::from([("bad".into(), var("x".into(), Sort::Bool))]),
            ),
        ] {
            let r = run(
                name,
                fixture(),
                ctx,
                CutBudgetMode::IndependentLemmas,
                finite::Limits::default(),
            );
            assert_eq!(r["status"], "unknown");
            assert!(r["children"].as_array().unwrap().is_empty());
        }
        for (name, limits) in [
            (
                "work_cap",
                finite::Limits {
                    max_work: 1,
                    ..Default::default()
                },
            ),
            (
                "clause_cap",
                finite::Limits {
                    max_clauses: 1,
                    ..Default::default()
                },
            ),
            (
                "time_cap",
                finite::Limits {
                    timeout_ms: 0,
                    ..Default::default()
                },
            ),
        ] {
            let r = run(
                name,
                fixture(),
                Env::new(),
                CutBudgetMode::SharedQuery,
                limits,
            );
            assert_eq!(r["status"], "unknown");
        }
    }
}
