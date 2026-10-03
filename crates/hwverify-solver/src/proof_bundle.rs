//! Fresh, acyclic sequent proofs. Serialized programs are only proposals;
//! private live handles are the sole authority for derived proof steps.
use crate::{finite, Check, CutBudgetMode, QueryOptions};
use hwverify_ir::*;
use serde_json::{json, Value};
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
    time::Instant,
};

const MAX_STEPS: usize = 512;
const MAX_REWRITE_WORK: u64 = 100_000;
#[derive(Clone)]
pub struct SequentHandle {
    owner: Rc<()>,
    id: usize,
    pre: Term,
    post: Term,
}
impl SequentHandle {
    pub fn pre(&self) -> &Term {
        &self.pre
    }
    pub fn post(&self) -> &Term {
        &self.post
    }
}
pub struct RewritePlan {
    owner: Rc<()>,
    original_pre: Term,
    original_goal: Term,
    pre: Term,
    post: Term,
    dependencies: Vec<usize>,
}
impl RewritePlan {
    pub fn pre(&self) -> &Term {
        &self.pre
    }
    pub fn post(&self) -> &Term {
        &self.post
    }
}

pub struct ProofBundle<'a> {
    check: &'a mut Check,
    name: String,
    original: Term,
    frozen_original: Term,
    frozen_pre: Term,
    frozen_goal: Term,
    pre: Term,
    goal: Term,
    context: Env,
    frozen_context: Env,
    owner: Rc<()>,
    mode: CutBudgetMode,
    start: Instant,
    limits: finite::Limits,
    work: u64,
    clauses: usize,
    charged_terms: usize,
    variables: usize,
    poisoned: Cell<bool>,
    steps: Vec<(Term, Term)>,
    graph: Vec<Value>,
    queries: Vec<Value>,
    finished: bool,
    validation: Value,
    replay: Option<Value>,
    reason: Option<String>,
}
fn substitute(term: &Term, rules: &HashMap<Term, Term>) -> Res<(Term, u64)> {
    let mut pending = vec![(term.clone(), false, 0usize)];
    let mut memo = HashMap::new();
    let mut work = 0;
    while let Some((t, exit, depth)) = pending.pop() {
        work += 1;
        if work > MAX_REWRITE_WORK || depth > 512 {
            return Err("proof term substitution budget exhausted".into());
        }
        if memo.contains_key(&t) {
            continue;
        }
        if let Some(value) = rules.get(&t) {
            if value.0.sort != t.0.sort {
                return Err("proof substitution sort mismatch".into());
            }
            memo.insert(t, value.clone());
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
    Ok((memo.remove(term).unwrap(), work))
}
fn contains(root: &Term, needle: &Term) -> Res<(bool, u64)> {
    let mut pending = vec![root];
    let mut seen = HashSet::new();
    let mut work = 0;
    while let Some(t) = pending.pop() {
        work += 1;
        if work > MAX_REWRITE_WORK {
            return Err("proof occurrence budget exhausted".into());
        }
        if t == needle {
            return Ok((true, work));
        }
        if seen.insert(t.clone()) {
            pending.extend(t.0.args.iter());
        }
    }
    Ok((false, work))
}
impl<'a> ProofBundle<'a> {
    pub fn new(
        check: &'a mut Check,
        name: &str,
        original_bad: &Term,
        context: &Env,
        mode: CutBudgetMode,
    ) -> Res<Self> {
        if !crate::finite_only() {
            return Err("proof bundles require finite-only mode".into());
        }
        if name.is_empty()
            || name.len() > 200
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err("invalid proof bundle name".into());
        }
        let start = Instant::now();
        let mut tail = original_bad;
        let mut prefixes = vec![];
        while tail.0.op == "and" && tail.0.args.len() == 2 {
            if prefixes.len() >= 128 {
                return Err("proof prefix budget exhausted".into());
            }
            prefixes.push(tail.0.args[0].clone());
            tail = &tail.0.args[1];
        }
        if tail.0.op != "not" || tail.0.args.len() != 1 {
            return Err("proof bundle expects an exact negated goal".into());
        }
        let prefix_work = prefixes.len() as u64;
        let pre = prefixes
            .into_iter()
            .reduce(and)
            .unwrap_or_else(|| boolv(true));
        let goal = tail.0.args[0].clone();
        let mut bundle = Self {
            check,
            name: name.into(),
            original: original_bad.clone(),
            frozen_original: original_bad.clone(),
            frozen_pre: pre.clone(),
            frozen_goal: goal.clone(),
            pre,
            goal,
            context: context.clone(),
            frozen_context: context.clone(),
            owner: Rc::new(()),
            mode,
            start,
            limits: finite::Limits::default(),
            work: prefix_work,
            clauses: 0,
            charged_terms: 0,
            variables: 0,
            poisoned: Cell::new(false),
            steps: vec![],
            graph: vec![],
            queries: vec![],
            finished: false,
            validation: Value::Null,
            replay: None,
            reason: None,
        };
        let validation_start = Instant::now();
        let (nodes, work) = finite::validate_original_context_with_limits(
            original_bad,
            context,
            bundle.remaining()?,
        )?;
        bundle.work += work;
        bundle.validation = json!({"complete":true,"includes_derived_context":true,"nodes":nodes,"work":work,"seconds":validation_start.elapsed().as_secs_f64()});
        bundle.charged_terms = nodes;
        bundle.write_statement(name, original_bad)?;
        Ok(bundle)
    }
    pub fn original_pre(&self) -> &Term {
        &self.pre
    }
    pub fn original_goal(&self) -> &Term {
        &self.goal
    }
    fn check_scope(&self) -> Res<()> {
        if !crate::finite_only() {
            self.poisoned.set(true);
            return Err("proof bundle solver mode changed".into());
        }
        if self.context != self.frozen_context
            || self.original != self.frozen_original
            || self.pre != self.frozen_pre
            || self.goal != self.frozen_goal
        {
            self.poisoned.set(true);
            return Err("proof source context changed".into());
        }
        Ok(())
    }
    fn check_handle(&self, h: &SequentHandle) -> Res<()> {
        self.check_scope()?;
        if !Rc::ptr_eq(&self.owner, &h.owner)
            || self.steps.get(h.id) != Some(&(h.pre.clone(), h.post.clone()))
        {
            return Err("foreign, stale or unproved sequent handle".into());
        }
        Ok(())
    }
    fn remaining(&self) -> Res<finite::Limits> {
        if self.poisoned.get() {
            return Err("proof bundle resource or scope failure".into());
        }
        if self.mode == CutBudgetMode::IndependentLemmas {
            return Ok(self.limits.clone());
        }
        let elapsed = self.start.elapsed().as_millis() as u64;
        if self.work >= self.limits.max_work
            || self.clauses >= self.limits.max_clauses
            || elapsed >= self.limits.timeout_ms
            || self.charged_terms >= self.limits.max_terms
            || self.variables >= self.limits.max_variables
        {
            return Err("proof bundle shared resource budget exhausted".into());
        }
        Ok(finite::Limits {
            max_work: self.limits.max_work - self.work,
            max_clauses: self.limits.max_clauses - self.clauses,
            timeout_ms: self.limits.timeout_ms - elapsed,
            max_terms: self.limits.max_terms - self.charged_terms,
            max_variables: self.limits.max_variables - self.variables,
            ..self.limits.clone()
        })
    }
    /// Account for bounded search performed before opening this live bundle.
    /// This can only debit resources; it cannot manufacture a sequent.
    pub(crate) fn charge_search(&mut self, started: Instant, work: u64) -> Res<()> {
        self.start = self.start.min(started);
        self.charge(work)
    }
    fn charge(&mut self, work: u64) -> Res<()> {
        self.work = self.work.saturating_add(work);
        if self.mode == CutBudgetMode::SharedQuery {
            self.remaining()?;
        }
        Ok(())
    }
    fn validate(&mut self, pre: &Term, post: &Term) -> Res<()> {
        if pre.0.sort != Sort::Bool || post.0.sort != Sort::Bool {
            return Err("sequent pre/post must be Boolean".into());
        }
        let (_, work) = match finite::validate_original_context_with_limits(
            &and(pre.clone(), not(post.clone())),
            &self.context,
            self.remaining()?,
        ) {
            Ok(value) => value,
            Err(error) => {
                self.poisoned.set(true);
                return Err(error);
            }
        };
        self.charge(work)
    }
    fn write_statement(&mut self, name: &str, bad: &Term) -> Res<()> {
        let (result, work) = crate::z3::write_original_query(
            &self.check.out,
            name,
            bad,
            &self.context,
            Some(self.remaining()?),
        );
        self.charge(work as u64)?;
        if result.is_err() {
            self.poisoned.set(true);
        }
        result
    }
    fn mint(
        &mut self,
        rule: &str,
        pre: Term,
        post: Term,
        dependencies: Vec<usize>,
    ) -> Res<SequentHandle> {
        self.check_scope()?;
        if self.steps.len() >= MAX_STEPS || dependencies.iter().any(|i| *i >= self.steps.len()) {
            return Err("proof graph is oversized or not acyclic".into());
        }
        self.validate(&pre, &post)?;
        let id = self.steps.len();
        let statement = format!("{}_statement_{id:04}", self.name);
        self.write_statement(&statement, &and(pre.clone(), not(post.clone())))?;
        self.steps.push((pre.clone(), post.clone()));
        self.graph.push(json!({"id":id,"rule":rule,"dependencies":dependencies,"statement":format!("{statement}.smt2")}));
        Ok(SequentHandle {
            owner: self.owner.clone(),
            id,
            pre,
            post,
        })
    }
    fn query(&mut self, label: &str, formula: Term, replay: bool) -> Res<Value> {
        self.check_scope()?;
        if self.queries.len() >= MAX_STEPS {
            return Err("proof query count budget exhausted".into());
        }
        let limits = self.remaining()?;
        let context = if replay {
            self.context.clone()
        } else {
            self.context
                .iter()
                .filter(|(_, t)| t.0.op.starts_with('@'))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };
        let name = format!("{}_query_{:04}", self.name, self.queries.len());
        self.check.query_limited(
            &name,
            formula,
            false,
            &context,
            QueryOptions {
                timeout_ms: limits.timeout_ms,
                finite_search_hint: Some(if replay {
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
        let mut report = self
            .check
            .reports
            .pop()
            .ok_or("missing fresh proof result")?;
        report["proof_label"] = json!(label);
        self.work = self
            .work
            .saturating_add(report["finite"]["work"].as_u64().unwrap_or(0))
            .saturating_add(report["emission_work"].as_u64().unwrap_or(0));
        self.clauses = self
            .clauses
            .saturating_add(report["finite"]["clauses"].as_u64().unwrap_or(0) as usize);
        self.charged_terms = self
            .charged_terms
            .saturating_add(report["finite"]["terms"].as_u64().unwrap_or(0) as usize);
        self.variables = self
            .variables
            .saturating_add(report["finite"]["variables"].as_u64().unwrap_or(0) as usize);
        self.queries.push(report.clone());
        self.check_scope()?;
        if self.mode == CutBudgetMode::SharedQuery {
            self.remaining()?;
        }
        Ok(report)
    }
    pub fn prove(&mut self, label: &str, pre: Term, post: Term) -> Res<SequentHandle> {
        self.validate(&pre, &post)?;
        let r = self.query(label, and(pre.clone(), not(post.clone())), false)?;
        if r["status"] != "passed" || r["solver_result"] != "unsat" {
            self.reason = Some(format!("unproved auxiliary sequent: {label}"));
            if r["solver_result"] == "sat" {
                if let Ok(replayed) = self.query(
                    "original counterexample replay",
                    self.original.clone(),
                    true,
                ) {
                    self.replay = Some(replayed);
                }
            }
            return Err(format!("auxiliary sequent is not proved: {label}"));
        }
        let query_index = self.queries.len() - 1;
        let h = self.mint("fresh-solver-unsat", pre, post, vec![])?;
        self.graph[h.id]["query_index"] = json!(query_index);
        Ok(h)
    }
    fn occurs(&mut self, root: &Term, needle: &Term) -> Res<bool> {
        let (found, work) = match contains(root, needle) {
            Ok(value) => value,
            Err(error) => {
                self.poisoned.set(true);
                return Err(error);
            }
        };
        self.charge(work)?;
        Ok(found)
    }
    fn subst(&mut self, term: &Term, rules: &HashMap<Term, Term>) -> Res<(Term, u64)> {
        match substitute(term, rules) {
            Ok(value) => Ok(value),
            Err(error) => {
                self.poisoned.set(true);
                Err(error)
            }
        }
    }
    pub fn instantiate(&mut self, h: &SequentHandle, map: &[(Term, Term)]) -> Res<SequentHandle> {
        self.check_handle(h)?;
        let mut rules = HashMap::new();
        for (a, b) in map {
            if !a.0.op.starts_with('@')
                || !a.0.args.is_empty()
                || a.0.sort != b.0.sort
                || rules.insert(a.clone(), b.clone()).is_some()
                || (!self.occurs(&h.pre, a)? && !self.occurs(&h.post, a)?)
            {
                return Err("invalid universal variable substitution".into());
            }
        }
        let (pre, w1) = self.subst(&h.pre, &rules)?;
        let (post, w2) = self.subst(&h.post, &rules)?;
        self.charged_terms = self.charged_terms.saturating_add((w1 + w2) as usize);
        self.charge(w1 + w2)?;
        self.mint(
            "universal-typed-simultaneous-instantiation",
            pre,
            post,
            vec![h.id],
        )
    }
    pub fn project(&mut self, h: &SequentHandle, path: &[usize]) -> Res<SequentHandle> {
        self.check_handle(h)?;
        if path.len() > 128 {
            return Err("proof projection depth limit".into());
        }
        let mut pre = h.pre.clone();
        let mut post = h.post.clone();
        for index in path {
            if post.0.op == "and" && post.0.args.len() == 2 && *index < 2 {
                post = post.0.args[*index].clone();
            } else if post.0.op == "=>" && post.0.args.len() == 2 && *index == 1 {
                pre = and(pre, post.0.args[0].clone());
                post = post.0.args[1].clone();
            } else {
                return Err(
                    "projection is not an exact conjunction/implication consequence".into(),
                );
            }
        }
        self.charge(path.len() as u64)?;
        self.mint("guard-preserving-projection", pre, post, vec![h.id])
    }
    pub fn apply(&mut self, lemma: &SequentHandle, premise: &SequentHandle) -> Res<SequentHandle> {
        self.check_handle(lemma)?;
        self.check_handle(premise)?;
        if premise.post != lemma.pre {
            return Err("lemma antecedent is not exactly discharged".into());
        }
        self.mint(
            "exact-antecedent-modus-ponens",
            premise.pre.clone(),
            lemma.post.clone(),
            vec![lemma.id, premise.id],
        )
    }
    pub fn conditional_eq(
        &mut self,
        pre: Term,
        guard: Term,
        h: &SequentHandle,
    ) -> Res<SequentHandle> {
        self.check_handle(h)?;
        if guard.0.sort != Sort::Bool
            || h.pre != and(pre.clone(), guard.clone())
            || h.post.0.op != "="
            || h.post.0.args.len() != 2
        {
            return Err("conditional equality lacks its exact guard proof".into());
        }
        let a = h.post.0.args[0].clone();
        let b = h.post.0.args[1].clone();
        self.mint(
            "guarded-equality-with-original-fallback",
            pre,
            eq(a.clone(), ite(guard, b, a)),
            vec![h.id],
        )
    }
    pub fn prepare_rewrite(
        &mut self,
        pre: Term,
        goal: Term,
        equalities: &[SequentHandle],
        retain_rewritten_pre: bool,
    ) -> Res<RewritePlan> {
        if equalities.is_empty() || equalities.len() > 32 {
            return Err("invalid congruence equality count".into());
        }
        let mut rules = HashMap::new();
        let mut dependencies = vec![];
        for h in equalities {
            self.check_handle(h)?;
            if h.pre != pre || h.post.0.op != "=" || h.post.0.args.len() != 2 {
                return Err(
                    "rewrite equality is not proved under the same complete antecedent".into(),
                );
            }
            let a = h.post.0.args[0].clone();
            let b = h.post.0.args[1].clone();
            if rules.insert(a.clone(), b).is_some()
                || (!self.occurs(&goal, &a)?
                    && (!retain_rewritten_pre || !self.occurs(&pre, &a)?))
            {
                return Err("duplicate or unused equality replacement".into());
            }
            dependencies.push(h.id);
        }
        let (post, w1) = self.subst(&goal, &rules)?;
        let (rewritten_pre, w2) = if retain_rewritten_pre {
            self.subst(&pre, &rules)?
        } else {
            (pre.clone(), 0)
        };
        self.charged_terms = self.charged_terms.saturating_add((w1 + w2) as usize);
        self.charge(w1 + w2)?;
        let strengthened = if retain_rewritten_pre {
            and(pre.clone(), rewritten_pre)
        } else {
            pre.clone()
        };
        self.validate(&strengthened, &post)?;
        Ok(RewritePlan {
            owner: self.owner.clone(),
            original_pre: pre,
            original_goal: goal,
            pre: strengthened,
            post,
            dependencies,
        })
    }
    pub fn finish_rewrite(
        &mut self,
        plan: RewritePlan,
        proved: &SequentHandle,
    ) -> Res<SequentHandle> {
        self.check_handle(proved)?;
        if !Rc::ptr_eq(&self.owner, &plan.owner)
            || proved.pre != plan.pre
            || proved.post != plan.post
        {
            return Err("congruence result does not match its exact prepared obligation".into());
        }
        let mut dependencies = plan.dependencies;
        dependencies.push(proved.id);
        self.mint(
            "exact-congruence-with-original-premise-retained",
            plan.original_pre,
            plan.original_goal,
            dependencies,
        )
    }
    pub fn join(
        &mut self,
        pre: Term,
        goal: Term,
        guard: Term,
        positive: &SequentHandle,
        negative: &SequentHandle,
    ) -> Res<SequentHandle> {
        self.check_handle(positive)?;
        self.check_handle(negative)?;
        if guard.0.sort != Sort::Bool
            || positive.pre != and(pre.clone(), guard.clone())
            || negative.pre != and(pre.clone(), not(guard))
            || positive.post != goal
            || negative.post != goal
        {
            return Err("branch proofs do not cover the exact guard and complement".into());
        }
        self.mint(
            "exhaustive-guard-complement",
            pre,
            goal,
            vec![positive.id, negative.id],
        )
    }
    pub fn finish(mut self, h: &SequentHandle) -> Res<()> {
        self.check_handle(h)?;
        if h.pre != self.pre || h.post != self.goal {
            return Err("final sequent differs from the exact original query".into());
        }
        if self.mode == CutBudgetMode::SharedQuery {
            self.remaining()?;
        }
        self.finished = true;
        self.emit(Some(h.id));
        Ok(())
    }
    fn emit(&mut self, root: Option<usize>) {
        let replay_unsat = self
            .replay
            .as_ref()
            .is_some_and(|r| r["status"] == "passed" && r["solver_result"] == "unsat");
        let replay_sat = self.replay.as_ref().is_some_and(|r| {
            r["solver_result"] == "sat" && r["finite"]["original_formula_validated"] == true
        });
        let budget_ok = !self.poisoned.get()
            && (self.mode == CutBudgetMode::IndependentLemmas || self.remaining().is_ok());
        let passed = budget_ok && (root.is_some() || replay_unsat);
        let sat = budget_ok && !passed && replay_sat;
        self.check.reports.push(json!({"name":self.name,"backend":"checked_proof_bundle","status":if passed{"passed"}else if sat{"counterexample"}else{"unknown"},
            "solver_result":if passed{"unsat"}else if sat{"sat"}else{"unknown"},"logical_expectation":"unsat","z3_seconds":0.0,"seconds":self.start.elapsed().as_secs_f64(),
            "evidence":format!("{}.smt2",self.name),"original_source_validation":self.validation,"reason":self.reason,"children":self.queries,"proof_graph":self.graph,"root":root,
            "original_recheck":self.replay,"coverage":{"rule":"fresh-acyclic-sequent-bundle-v1","complete":passed,"exact_original_sequent":root.is_some(),"fresh_handles_only":true,"saved_reports_are_authority":false},
            "cost":{"mode":if self.mode==CutBudgetMode::SharedQuery{"shared_query"}else{"independent_lemmas"},"work_including_validation_and_derived_steps":self.work,"allocated_clauses":self.clauses,"charged_terms":self.charged_terms,"allocated_variables":self.variables,
                "per_query_work_limit":self.limits.max_work,"per_query_clause_limit":self.limits.max_clauses,"per_query_timeout_ms":self.limits.timeout_ms,"whole_bundle_is_one_query":self.mode==CutBudgetMode::SharedQuery,
                "kernel_enabled":self.mode==CutBudgetMode::IndependentLemmas,"kernel_cost_accounting":"per-lemma kernel seconds and rule counts are reported separately; strict shared mode disables kernel"}}));
    }
}
impl Drop for ProofBundle<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.emit(None);
        }
    }
}

pub(crate) fn validate_report(report: &Value) -> Res<()> {
    if report["backend"] != "checked_proof_bundle" {
        return Err("not a checked proof bundle".into());
    }
    if report["status"] == "unknown" && report["solver_result"] == "unknown" {
        return Ok(());
    }
    if report["original_source_validation"]["complete"] != true
        || report["coverage"]["rule"] != "fresh-acyclic-sequent-bundle-v1"
        || report["coverage"]["fresh_handles_only"] != true
        || report["coverage"]["saved_reports_are_authority"] != false
    {
        return Err("incomplete proof bundle source validation".into());
    }
    let children = report["children"]
        .as_array()
        .ok_or("missing proof bundle queries")?;
    for (index, child) in children.iter().enumerate() {
        if child["name"]
            != format!(
                "{}_query_{index:04}",
                report["name"].as_str().ok_or("missing proof name")?
            )
            || !matches!(
                child["backend"].as_str(),
                Some("finite_bv" | "structural_kernel")
            )
            || child["z3_seconds"] != 0.0
        {
            return Err("unexpected proof query identity/backend".into());
        }
        if child["finite"]["work"].as_u64().unwrap_or(0) > 100_000_000
            || child["finite"]["clauses"].as_u64().unwrap_or(0) > 1_000_000
        {
            return Err("proof leaf exceeds unchanged limits".into());
        }
    }
    let mode = report["cost"]["mode"]
        .as_str()
        .ok_or("missing proof budget mode")?;
    if mode != "shared_query" && mode != "independent_lemmas" {
        return Err("unknown proof budget mode".into());
    }
    if mode == "shared_query"
        && (report["cost"]["work_including_validation_and_derived_steps"]
            .as_u64()
            .unwrap_or(u64::MAX)
            > 100_000_000
            || report["cost"]["allocated_clauses"]
                .as_u64()
                .unwrap_or(u64::MAX)
                > 1_000_000
            || report["seconds"].as_f64().unwrap_or(f64::INFINITY) >= 10.0
            || children.iter().any(|c| c["backend"] != "finite_bv"))
    {
        return Err("shared proof budget not respected".into());
    }
    if report["status"] == "counterexample" && report["solver_result"] == "sat" {
        if report["original_recheck"]["finite"]["original_formula_validated"] == true
            && children.contains(&report["original_recheck"])
        {
            return Ok(());
        }
        return Err("proof cut SAT was not replayed on original query".into());
    }
    if report["status"] != "passed"
        || report["solver_result"] != "unsat"
        || report["coverage"]["complete"] != true
    {
        return Err("inconsistent proof bundle verdict".into());
    }
    if report["root"].is_null() {
        if report["original_recheck"]["status"] == "passed"
            && report["original_recheck"]["solver_result"] == "unsat"
            && children.contains(&report["original_recheck"])
        {
            return Ok(());
        }
        return Err("missing exact proof root".into());
    }
    let graph = report["proof_graph"]
        .as_array()
        .ok_or("missing proof graph")?;
    if graph.is_empty()
        || graph.len() > MAX_STEPS
        || report["root"]
            .as_u64()
            .is_none_or(|i| i as usize >= graph.len())
        || report["coverage"]["exact_original_sequent"] != true
    {
        return Err("invalid exact proof root".into());
    }
    for (id, node) in graph.iter().enumerate() {
        if node["id"] != id
            || node["dependencies"].as_array().is_none_or(|deps| {
                deps.iter()
                    .any(|d| d.as_u64().is_none_or(|i| i as usize >= id))
            })
        {
            return Err("proof dependency graph is not acyclic".into());
        }
        match node["rule"].as_str() {
            Some("fresh-solver-unsat") => {
                let q = node["query_index"]
                    .as_u64()
                    .and_then(|i| children.get(i as usize))
                    .ok_or("missing live solver leaf")?;
                if q["status"] != "passed" || q["solver_result"] != "unsat" {
                    return Err("unproved leaf in final proof graph".into());
                }
            }
            Some(
                "universal-typed-simultaneous-instantiation"
                | "guard-preserving-projection"
                | "exact-antecedent-modus-ponens"
                | "guarded-equality-with-original-fallback"
                | "exact-congruence-with-original-premise-retained"
                | "exhaustive-guard-complement",
            ) => {}
            _ => return Err("unknown proof inference rule".into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, process::Command};
    fn v(s: &str) -> Term {
        var(s.into(), Sort::Bv(4))
    }
    fn add(a: Term, b: Term) -> Term {
        node(Sort::Bv(4), "bvadd", vec![a, b])
    }
    fn checker(name: &str) -> Check {
        let out =
            std::env::temp_dir().join(format!("hwverify-bundle-{}-{name}", std::process::id()));
        fs::create_dir_all(&out).unwrap();
        Check {
            z3: "forbidden".into(),
            out,
            reports: vec![],
        }
    }
    #[test]
    fn bounded_simultaneous_substitution_is_typed() {
        let x = v("x");
        let y = v("y");
        let term = add(x.clone(), y.clone());
        let map = HashMap::from([(x.clone(), y.clone()), (y.clone(), bv(4, 7))]);
        assert_eq!(substitute(&term, &map).unwrap().0, add(y, bv(4, 7)));
        assert!(substitute(&x, &HashMap::from([(x.clone(), boolv(true))])).is_err());
    }
    #[test]
    fn actual_opaque_sequent_rules() {
        if std::env::var("HWVERIFY_TEST_BUNDLE_CHILD").is_err() {
            let r = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "proof_bundle::tests::actual_opaque_sequent_rules",
                    "--nocapture",
                ])
                .env("HWVERIFY_TEST_BUNDLE_CHILD", "1")
                .env("HWVERIFY_SOLVER", "finite")
                .env_remove("HWVERIFY_CONJUNCTIVE_LEMMAS")
                .env_remove("HWVERIFY_KERNEL")
                .output()
                .unwrap();
            assert!(
                r.status.success(),
                "{} {}",
                String::from_utf8_lossy(&r.stdout),
                String::from_utf8_lossy(&r.stderr)
            );
            return;
        }
        let mut c = checker("universal");
        let goal = eq(add(v("x"), bv(4, 0)), v("x"));
        let bad = and(boolv(true), not(goal.clone()));
        let mut b = ProofBundle::new(
            &mut c,
            "universal",
            &bad,
            &Env::new(),
            CutBudgetMode::IndependentLemmas,
        )
        .unwrap();
        let universal = b
            .prove(
                "universal identity",
                boolv(true),
                eq(add(v("u"), bv(4, 0)), v("u")),
            )
            .unwrap();
        assert!(b.instantiate(&universal, &[(v("u"), boolv(true))]).is_err());
        assert!(b
            .instantiate(&universal, &[(v("missing"), v("x"))])
            .is_err());
        assert!(b
            .instantiate(&universal, &[(v("u"), v("x")), (v("u"), v("y"))])
            .is_err());
        let instance = b.instantiate(&universal, &[(v("u"), v("x"))]).unwrap();
        let premise = b.prove("premise", boolv(true), boolv(true)).unwrap();
        let final_h = b.apply(&instance, &premise).unwrap();
        b.finish(&final_h).unwrap();
        assert_eq!(c.reports[0]["status"], "passed");
        assert!(c.reports[0]["root"].is_number());

        let mut c = checker("rewrite");
        let p = eq(v("a"), v("b"));
        let g = var("g".into(), Sort::Bool);
        let goal = eq(add(v("a"), v("c")), add(v("b"), v("c")));
        let bad = and(p.clone(), not(goal.clone()));
        let mut b = ProofBundle::new(
            &mut c,
            "rewrite",
            &bad,
            &Env::new(),
            CutBudgetMode::IndependentLemmas,
        )
        .unwrap();
        let theorem = b
            .prove(
                "guarded conjunction",
                p.clone(),
                and(
                    p.clone(),
                    node(Sort::Bool, "=>", vec![g.clone(), p.clone()]),
                ),
            )
            .unwrap();
        let projected = b.project(&theorem, &[1, 1]).unwrap();
        assert_eq!(projected.pre(), &and(p.clone(), g.clone()));
        assert!(b.project(&theorem, &[1, 0]).is_err());
        assert!(b
            .conditional_eq(p.clone(), not(g.clone()), &projected)
            .is_err());
        let conditional = b.conditional_eq(p.clone(), g.clone(), &projected).unwrap();
        assert_eq!(
            conditional.post(),
            &eq(v("a"), ite(g.clone(), v("b"), v("a")))
        );
        let pos = and(p.clone(), g.clone());
        let plan = b
            .prepare_rewrite(
                pos.clone(),
                goal.clone(),
                std::slice::from_ref(&projected),
                true,
            )
            .unwrap();
        assert_eq!(plan.pre().0.args[0], pos);
        let rewritten = b
            .prove("rewritten", plan.pre().clone(), plan.post().clone())
            .unwrap();
        let positive = b.finish_rewrite(plan, &rewritten).unwrap();
        assert!(b
            .join(p.clone(), goal.clone(), g.clone(), &positive, &positive)
            .is_err());
        let negative = b
            .prove("complement", and(p.clone(), not(g.clone())), goal.clone())
            .unwrap();
        let joined = b.join(p.clone(), goal, g, &positive, &negative).unwrap();
        b.finish(&joined).unwrap();
        assert_eq!(c.reports[0]["status"], "passed");

        // A solved leaf is not sufficient for a different original goal.
        let mut c = checker("wrong_final");
        let bad = and(boolv(true), not(eq(v("a"), v("b"))));
        let mut b = ProofBundle::new(
            &mut c,
            "wrong_final",
            &bad,
            &Env::new(),
            CutBudgetMode::IndependentLemmas,
        )
        .unwrap();
        let h = b.prove("trivial", boolv(true), boolv(true)).unwrap();
        assert!(b.finish(&h).is_err());
        assert_eq!(c.reports[0]["status"], "unknown");

        let mut c1 = checker("foreign1");
        let mut c2 = checker("foreign2");
        let bad = and(boolv(true), not(boolv(true)));
        let mut a = ProofBundle::new(
            &mut c1,
            "foreign1",
            &bad,
            &Env::new(),
            CutBudgetMode::IndependentLemmas,
        )
        .unwrap();
        let h = a.prove("true", boolv(true), boolv(true)).unwrap();
        let mut b = ProofBundle::new(
            &mut c2,
            "foreign2",
            &bad,
            &Env::new(),
            CutBudgetMode::IndependentLemmas,
        )
        .unwrap();
        assert!(b.instantiate(&h, &[]).is_err());
        let mut forged = h.clone();
        forged.post = boolv(false);
        assert!(a.check_handle(&forged).is_err());
        a.goal = boolv(false);
        assert!(a.check_scope().is_err());
        drop(a);
        drop(b);

        let mut c = checker("unsupported");
        let ctx = Env::from([("dead".into(), node(Sort::Bool, "unsupported", vec![]))]);
        assert!(ProofBundle::new(
            &mut c,
            "unsupported",
            &bad,
            &ctx,
            CutBudgetMode::IndependentLemmas
        )
        .is_err());
        assert_eq!(c.reports[0]["status"], "unknown");
        let mut c = checker("resource");
        let mut b = ProofBundle::new(
            &mut c,
            "resource",
            &bad,
            &Env::new(),
            CutBudgetMode::SharedQuery,
        )
        .unwrap();
        b.limits.max_work = b.work;
        assert!(b.prove("overbudget", boolv(true), boolv(true)).is_err());
        drop(b);
        assert_eq!(c.reports[0]["status"], "unknown");
        let mut c = checker("mode_change");
        let mut b = ProofBundle::new(
            &mut c,
            "mode_change",
            &bad,
            &Env::new(),
            CutBudgetMode::IndependentLemmas,
        )
        .unwrap();
        std::env::set_var("HWVERIFY_SOLVER", "z3");
        assert!(b
            .prove("forbidden backend", boolv(true), boolv(true))
            .is_err());
        std::env::set_var("HWVERIFY_SOLVER", "finite");
        drop(b);
        assert_eq!(c.reports[0]["status"], "unknown");
        let mut c = checker("query_cap");
        let mut b = ProofBundle::new(
            &mut c,
            "query_cap",
            &bad,
            &Env::new(),
            CutBudgetMode::IndependentLemmas,
        )
        .unwrap();
        b.queries = vec![Value::Null; MAX_STEPS];
        assert!(b.prove("too many", boolv(true), boolv(true)).is_err());
        drop(b);
        let mut c = checker("emission_cap");
        c.query_limited(
            "emission_cap",
            and(eq(v("a"), v("b")), not(eq(v("c"), v("d")))),
            false,
            &Env::new(),
            QueryOptions::default(),
            Some(crate::z3::QueryBudget {
                limits: finite::Limits {
                    max_work: 2,
                    ..Default::default()
                },
                allow_kernel: false,
            }),
        )
        .unwrap();
        assert_eq!(c.reports[0]["status"], "unknown");
        assert_eq!(c.reports[0]["finite"]["work"], 0);
        assert!(c.reports[0]["emission_work"].as_u64().unwrap() <= 3);
        let mut c = checker("false_cut");
        let mut b = ProofBundle::new(
            &mut c,
            "false_cut",
            &bad,
            &Env::new(),
            CutBudgetMode::IndependentLemmas,
        )
        .unwrap();
        assert!(b
            .prove("false auxiliary", boolv(true), eq(v("a"), v("b")))
            .is_err());
        drop(b);
        assert_eq!(c.reports[0]["status"], "passed");
        assert_eq!(c.reports[0]["original_recheck"]["solver_result"], "unsat");
    }
}
