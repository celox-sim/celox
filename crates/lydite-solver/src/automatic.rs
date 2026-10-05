//! Bounded, untrusted search for guarded congruence proofs.
//!
//! This module cannot mint a proof handle. All proposed equalities, rewritten
//! obligations and both sides of every split are checked by `ProofBundle`.
use crate::{Check, CutBudgetMode, ProofBundle, SequentHandle, equality_sharing::EqualityPool};
use lydite_ir::*;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

type Frontier = (Option<Vec<(Term, Term)>>, Option<Term>);

const MAX_PLAN_WORK: usize = 100_000;
const MAX_SEARCH_MS: u128 = 1_000;
const MAX_CUTS: usize = 8;
const MAX_SPLIT_DEPTH: usize = 3;
const MAX_LEAVES: usize = 8;

pub(crate) fn mode() -> Res<Option<CutBudgetMode>> {
    let mode = match std::env::var("LYDITE_AUTOMATIC_PROOFS") {
        Err(std::env::VarError::NotPresent) => None,
        Ok(s) if s == "0" => None,
        Ok(s) if s == "independent_lemmas" => Some(CutBudgetMode::IndependentLemmas),
        Ok(s) if s == "shared_query" => Some(CutBudgetMode::SharedQuery),
        _ => {
            return Err(
                "LYDITE_AUTOMATIC_PROOFS must be 0, independent_lemmas or shared_query".into(),
            );
        }
    };
    if mode.is_some() && !crate::finite_only() {
        return Err("automatic proofs require finite-only mode".into());
    }
    Ok(mode)
}

#[derive(Clone, Debug)]
enum Plan {
    Leaf(Vec<(Term, Term)>),
    Split(Term, Box<Plan>, Box<Plan>),
}
struct Planner {
    started: Instant,
    work: usize,
    leaves: usize,
    cuts: usize,
    word_cutpoint_frontiers: usize,
    guarded_word_expansions: usize,
}
impl Default for Planner {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            work: 0,
            leaves: 0,
            cuts: 0,
            word_cutpoint_frontiers: 0,
            guarded_word_expansions: 0,
        }
    }
}
impl Planner {
    fn stats(&self, prefix_work: u64, accepted: bool, reason: Option<String>) -> Value {
        json!({"version":1,"heuristic":"bounded-structural-frontier-and-mux-splits",
            "search_work":self.work as u64+prefix_work,"search_work_unit":"planner visits; structural comparisons are not constant-time",
            "search_seconds":self.started.elapsed().as_secs_f64(),"leaves":self.leaves,"equalities":self.cuts,"word_cutpoint_frontiers":self.word_cutpoint_frontiers,"guarded_word_expansions":self.guarded_word_expansions,
            "accepted":accepted,"reason":reason,"max_search_work":MAX_PLAN_WORK,"max_search_ms":MAX_SEARCH_MS,
            "max_cuts_per_leaf":MAX_CUTS,"max_split_depth":MAX_SPLIT_DEPTH,"max_leaves":MAX_LEAVES,
            "trusted":false,"source_names_used":false,"saved_hints_used":false})
    }
    fn tick(&mut self) -> Res<()> {
        self.work += 1;
        if self.work > MAX_PLAN_WORK || self.started.elapsed().as_millis() >= MAX_SEARCH_MS {
            Err("automatic proof planning budget exhausted".into())
        } else {
            Ok(())
        }
    }
    // This is only a search view. Cofactored terms are never accepted as facts;
    // the kernel always receives the unmodified original goal and antecedent.
    fn selected(&mut self, t: &Term, guards: &[(Term, bool)]) -> Res<Term> {
        let mut t = t.clone();
        loop {
            self.tick()?;
            if t.0.op != "ite" || t.0.args.len() != 3 {
                return Ok(t);
            }
            let guard = &t.0.args[0];
            let value = guards.iter().rev().find_map(|(g, v)| {
                if guard == g {
                    Some(*v)
                } else if guard.0.op == "not" && guard.0.args.as_slice() == [g.clone()] {
                    Some(!v)
                } else {
                    None
                }
            });
            match value {
                Some(v) => t = t.0.args[if v { 1 } else { 2 }].clone(),
                None => return Ok(t),
            }
        }
    }
    /// A bounded search view only. Execution freshly checks its equivalence
    /// under the exact branch antecedent before using any changed expression.
    fn view(&mut self, root: &Term, guards: &[(Term, bool)]) -> Res<Term> {
        let mut todo = vec![(root.clone(), false, 0usize)];
        let mut memo: HashMap<Term, Term> = HashMap::new();
        while let Some((term, exit, depth)) = todo.pop() {
            self.tick()?;
            if depth > 256 {
                return Err("automatic cofactor depth limit".into());
            }
            if memo.contains_key(&term) {
                continue;
            }
            let selected = self.selected(&term, guards)?;
            if exit {
                let args = selected
                    .0
                    .args
                    .iter()
                    .map(|t| memo[t].clone())
                    .collect::<Vec<_>>();
                let value = if args == selected.0.args {
                    selected.clone()
                } else {
                    node(selected.0.sort.clone(), selected.0.op.clone(), args)
                };
                memo.insert(term, value);
            } else {
                todo.push((term, true, depth));
                todo.extend(
                    selected
                        .0
                        .args
                        .iter()
                        .map(|t| (t.clone(), false, depth + 1)),
                );
            }
        }
        Ok(memo.remove(root).unwrap())
    }
    fn frontier(
        &mut self,
        lhs: &Term,
        rhs: &Term,
        guards: &[(Term, bool)],
        pool: &EqualityPool,
    ) -> Res<Frontier> {
        let mut todo = vec![(self.view(lhs, guards)?, self.view(rhs, guards)?, 0usize)];
        let mut seen = HashSet::new();
        let mut cuts: Vec<(Term, Term)> = vec![];
        let mut split = None;
        let mut matched = false;
        while let Some((a, b, depth)) = todo.pop() {
            self.tick()?;
            if depth > 256 {
                return Ok((None, split));
            }
            let a = self.selected(&a, guards)?;
            let b = self.selected(&b, guards)?;
            if a == b || !seen.insert((a.clone(), b.clone())) {
                continue;
            }
            if a.0.sort != b.0.sort {
                return Ok((None, split));
            }
            let (bridge, visits) = pool.path(&b, &a, guards);
            self.work += visits;
            self.tick()?;
            if bridge.is_some() {
                if cuts.iter().any(|(from, to)| *from == b && *to != a) {
                    return Ok((None, split));
                }
                if !cuts.iter().any(|(from, to)| *from == b && *to == a) {
                    cuts.push((b, a));
                }
                if cuts.len() > MAX_CUTS {
                    return Ok((None, split));
                }
                matched = true;
                continue;
            }
            // Open a named word only through an asserted, guarded equality.
            // This merely aligns operator structure; the equality must still
            // obtain a live handle before any substitution is consumed.
            let mut expanded = false;
            for (from, target, left) in [(&a, &b, true), (&b, &a, false)] {
                let (replacement, work) = pool.expansion(from, target, guards);
                self.work += work;
                self.tick()?;
                if let Some(to) = replacement {
                    if cuts.iter().any(|(f, _)| f == from) {
                        continue;
                    }
                    if cuts.len() >= MAX_CUTS {
                        return Ok((None, split));
                    }
                    cuts.push((from.clone(), to.clone()));
                    self.guarded_word_expansions += 1;
                    todo.push(if left {
                        (to, b.clone(), depth + 1)
                    } else {
                        (a.clone(), to, depth + 1)
                    });
                    matched = true;
                    expanded = true;
                    break;
                }
            }
            if expanded {
                continue;
            }
            // Split a mismatching mux before proposing an equality across its
            // branches. This avoids assuming that an inactive input is used.
            for t in [&a, &b] {
                if t.0.op == "ite" && t.0.args.len() == 3 && split.is_none() {
                    split = Some(t.0.args[0].clone());
                }
            }
            if a.0.op == b.0.op && a.0.args.len() == b.0.args.len() && !a.0.args.is_empty() {
                matched = true;
                todo.extend(
                    a.0.args
                        .iter()
                        .zip(&b.0.args)
                        .rev()
                        .map(|(a, b)| (a.clone(), b.clone(), depth + 1)),
                );
            } else {
                if depth == 0 || cuts.iter().any(|(from, to)| *from == b && *to != a) {
                    return Ok((None, split));
                }
                if !cuts.iter().any(|(from, to)| *from == b && *to == a) {
                    cuts.push((b, a));
                }
                if cuts.len() > MAX_CUTS {
                    return Ok((None, split));
                }
            }
        }
        // Ordinary frontier cuts require a shared operator. A whole-goal
        // bridge is allowed only as a proposal backed by asserted facts; its
        // live handle still requires independent fresh proof.
        Ok((
            if matched || cuts.is_empty() {
                Some(cuts)
            } else {
                None
            },
            split,
        ))
    }
    fn build(
        &mut self,
        lhs: &Term,
        rhs: &Term,
        guards: &mut Vec<(Term, bool)>,
        pool: &EqualityPool,
    ) -> Res<Plan> {
        let (cuts, split) = self.frontier(lhs, rhs, guards, pool)?;
        // Preserve a whole typed word relation when its frontier reaches an
        // input/state cutpoint. Splitting inside a read/mux cone can destroy the
        // word-level structure which the finite engine can prove cheaply.
        // This ranks two existing proposals; neither proposal authorizes a fact.
        let word_cutpoints = cuts.as_ref().is_some_and(|cuts| {
            !cuts.is_empty()
                && cuts.iter().all(|(a, b)| {
                    matches!(a.0.sort, Sort::Bv(_))
                        && (a.0.op.starts_with('@') || b.0.op.starts_with('@'))
                })
        });
        if word_cutpoints {
            self.word_cutpoint_frontiers += 1;
        }
        let prefer_split = split.is_some()
            && !word_cutpoints
            && cuts
                .as_ref()
                .is_some_and(|cuts| cuts.iter().any(|(a, b)| a.0.op == "ite" || b.0.op == "ite"))
            && guards.len() < MAX_SPLIT_DEPTH;
        if let Some(cuts) = cuts.filter(|_| !prefer_split) {
            self.leaves += 1;
            self.cuts += cuts.len();
            if self.leaves > MAX_LEAVES {
                return Err("automatic proof leaf limit".into());
            }
            return Ok(Plan::Leaf(cuts));
        }
        if guards.len() < MAX_SPLIT_DEPTH {
            if let Some(guard) = split {
                if !guards.iter().any(|(g, _)| *g == guard) {
                    guards.push((guard.clone(), true));
                    let positive = self.build(lhs, rhs, guards, pool)?;
                    guards.last_mut().unwrap().1 = false;
                    let negative = self.build(lhs, rhs, guards, pool)?;
                    guards.pop();
                    return Ok(Plan::Split(guard, Box::new(positive), Box::new(negative)));
                }
            }
        }
        self.leaves += 1;
        if self.leaves > MAX_LEAVES {
            return Err("automatic proof leaf limit".into());
        }
        Ok(Plan::Leaf(vec![]))
    }
}

/// Apply cuts in checked stages when an expansion introduces a later source.
/// Cuts already present remain simultaneous, preserving ordinary frontier rules.
/// Search only schedules handles: every stage still enforces exact antecedents
/// and the kernel's unchanged occurrence and final-sequent checks.
fn rewrite_frontier(
    bundle: &mut ProofBundle<'_>,
    pre: Term,
    goal: Term,
    mut pending: Vec<SequentHandle>,
    cofactor_work: usize,
) -> Res<SequentHandle> {
    if pending.is_empty() || pending.len() > MAX_CUTS {
        return Err("automatic frontier equality count limit".into());
    }
    let mut current = goal;
    let mut stages = vec![];
    let mut search = Planner {
        work: cofactor_work,
        ..Default::default()
    };
    while !pending.is_empty() {
        let before = search.work;
        let ready = (|| -> Res<Vec<SequentHandle>> {
            let mut terms = HashSet::new();
            let mut todo = vec![(current.clone(), 0usize)];
            while let Some((term, depth)) = todo.pop() {
                search.tick()?;
                if depth > 256 {
                    return Err("automatic rewrite scheduling depth limit".into());
                }
                if terms.insert(term.clone()) {
                    todo.extend(term.0.args.iter().map(|a| (a.clone(), depth + 1)));
                }
            }
            let mut ready = vec![];
            let mut later = vec![];
            for h in pending.drain(..) {
                search.tick()?;
                if h.post().0.op != "=" || h.post().0.args.len() != 2 {
                    return Err("automatic frontier handle is not an equality".into());
                }
                if terms.contains(&h.post().0.args[0]) {
                    ready.push(h);
                } else {
                    later.push(h);
                }
            }
            pending = later;
            if ready.is_empty() {
                return Err("automatic frontier has an unused replacement".into());
            }
            Ok(ready)
        })();
        bundle.charge_search(search.started, (search.work - before) as u64)?;
        let plan = bundle.prepare_rewrite(pre.clone(), current, &ready?, false)?;
        current = plan.post().clone();
        stages.push(plan);
    }
    let mut result = bundle.prove("automatic rewritten goal", pre, current)?;
    while let Some(plan) = stages.pop() {
        result = bundle.finish_rewrite(plan, &result)?;
    }
    Ok(result)
}

fn execute(
    bundle: &mut ProofBundle<'_>,
    plan: &Plan,
    pre: Term,
    goal: Term,
    guards: &mut Vec<(Term, bool)>,
    pool: &mut EqualityPool,
) -> Res<SequentHandle> {
    match plan {
        Plan::Leaf(cuts) => {
            let started = Instant::now();
            let mut cofactor = Planner {
                started,
                ..Default::default()
            };
            let viewed = cofactor.view(&goal, guards);
            pool.record_cofactor(cofactor.work, false);
            bundle.charge_search(started, cofactor.work as u64)?;
            let viewed = viewed?;
            let normalization = if viewed != goal {
                let equivalent = bundle.prove(
                    "automatic guarded cofactor equivalence",
                    pre.clone(),
                    eq(goal.clone(), viewed.clone()),
                )?;
                pool.record_cofactor(0, true);
                Some(bundle.prepare_rewrite(pre.clone(), goal, &[equivalent], false)?)
            } else {
                None
            };
            let result = if cuts.is_empty() {
                bundle.prove("automatic residual", pre, viewed)?
            } else {
                let mut handles = vec![];
                for (from, to) in cuts {
                    handles.push(pool.prove(
                        bundle,
                        pre.clone(),
                        from.clone(),
                        to.clone(),
                        guards,
                    )?);
                }
                rewrite_frontier(bundle, pre, viewed, handles, cofactor.work)?
            };
            if let Some(plan) = normalization {
                bundle.finish_rewrite(plan, &result)
            } else {
                Ok(result)
            }
        }
        Plan::Split(guard, positive, negative) => {
            guards.push((guard.clone(), true));
            let yes = execute(
                bundle,
                positive,
                and(pre.clone(), guard.clone()),
                goal.clone(),
                guards,
                pool,
            )?;
            guards.last_mut().unwrap().1 = false;
            let no = execute(
                bundle,
                negative,
                and(pre.clone(), not(guard.clone())),
                goal.clone(),
                guards,
                pool,
            )?;
            guards.pop();
            bundle.join(pre, goal, guard.clone(), &yes, &no)
        }
    }
}

impl Check {
    /// Discover a plan from this exact typed query, without source names,
    /// serialized hints, numeric term IDs or ISA-specific knowledge.
    pub fn query_automatic_proof(
        &mut self,
        name: &str,
        original: Term,
        context: &Env,
        mode: CutBudgetMode,
    ) -> Res<bool> {
        self.query_automatic_proof_detailed(name, original, context, mode)
            .map(|(handled, _)| handled)
    }
    pub(crate) fn query_automatic_proof_detailed(
        &mut self,
        name: &str,
        original: Term,
        context: &Env,
        mode: CutBudgetMode,
    ) -> Res<(bool, Value)> {
        if !crate::finite_only() {
            return Err("automatic proofs require finite-only mode".into());
        }
        let started = Instant::now();
        let mut tail = &original;
        let mut prefix_count = 0;
        let mut prefixes = vec![];
        while tail.0.op == "and" && tail.0.args.len() == 2 {
            prefix_count += 1;
            if prefix_count > 128 {
                return Ok((false, Value::Null));
            }
            prefixes.push(tail.0.args[0].clone());
            tail = &tail.0.args[1];
        }
        if tail.0.op != "not" || tail.0.args.len() != 1 {
            return Ok((false, Value::Null));
        }
        let goal = &tail.0.args[0];
        if goal.0.op != "=" || goal.0.args.len() != 2 {
            return Ok((false, Value::Null));
        }
        let mut planner = Planner {
            started,
            ..Default::default()
        };
        let pre = prefixes
            .into_iter()
            .reduce(and)
            .unwrap_or_else(|| boolv(true));
        let mut pool = EqualityPool::new(pre);
        planner.work += pool.scan_work;
        let plan = match planner.build(&goal.0.args[0], &goal.0.args[1], &mut vec![], &pool) {
            Ok(plan) if planner.cuts > 0 || planner.leaves > 1 => plan,
            Ok(_) => {
                return Ok((
                    false,
                    planner.stats(
                        prefix_count,
                        false,
                        Some("no useful frontier or split".into()),
                    ),
                ));
            }
            Err(error) => return Ok((false, planner.stats(prefix_count, false, Some(error)))),
        };
        let stats = planner.stats(prefix_count, true, None);
        let before = self.reports.len();
        let result = (|| -> Res<()> {
            let mut bundle = ProofBundle::new(self, name, &original, context, mode)?;
            bundle.charge_search(started, planner.work as u64 + prefix_count)?;
            let pre = bundle.original_pre().clone();
            let goal = bundle.original_goal().clone();
            let h = execute(&mut bundle, &plan, pre, goal, &mut vec![], &mut pool)?;
            bundle.finish(&h)
        })();
        if self.reports.len() != before + 1 {
            return result.map(|_| (false, stats));
        }
        let report = self.reports.last_mut().unwrap();
        report["automatic_plan"] = stats.clone();
        report["equality_sharing"] = pool.stats();
        if let Err(error) = result {
            report["automatic_plan_error"] = json!(error);
        }
        Ok((true, stats))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, process::Command};
    fn v(n: &str) -> Term {
        var(n.into(), Sort::Bv(8))
    }
    fn add(a: Term, b: Term) -> Term {
        node(Sort::Bv(8), "bvadd", vec![a, b])
    }
    fn sum(prefix: &str) -> Term {
        add(
            add(v(&format!("{prefix}0")), v(&format!("{prefix}1"))),
            v(&format!("{prefix}2")),
        )
    }
    fn fixture(mutate: bool) -> (Term, Env) {
        let g = var("route".into(), Sort::Bool);
        let p = (0..3)
            .map(|i| eq(v(&format!("a{i}")), v(&format!("b{i}"))))
            .reduce(and)
            .unwrap();
        let source = sum("a");
        let target = sum("b");
        let goal = eq(
            ite(
                g,
                source.clone(),
                if mutate {
                    add(source, bv(8, 1))
                } else {
                    source
                },
            ),
            target,
        );
        (and(p, not(goal)), Env::new())
    }
    #[test]
    fn discovers_more_than_two_frontier_cuts_and_exhaustive_split() {
        let (bad, _) = fixture(false);
        let goal = &bad.0.args[1].0.args[0];
        let mut planner = Planner::default();
        let plan = planner
            .build(
                &goal.0.args[0],
                &goal.0.args[1],
                &mut vec![],
                &EqualityPool::new(boolv(true)),
            )
            .unwrap();
        assert!(matches!(plan, Plan::Split(_, _, _)));
        assert_eq!(planner.leaves, 2);
        assert_eq!(planner.cuts, 6);
    }
    #[test]
    fn asserted_word_definition_exposes_a_nontrivial_frontier() {
        let left = add(v("a"), v("b"));
        let right = node(Sort::Bv(8), "bvxor", vec![v("c"), v("d")]);
        let product = |t| node(Sort::Bv(8), "bvmul", vec![t, v("factor")]);
        let pre = and(
            eq(v("encoded"), product(left.clone())),
            eq(left, right.clone()),
        );
        let mut planner = Planner::default();
        let plan = planner
            .build(
                &v("encoded"),
                &product(right),
                &mut vec![],
                &EqualityPool::new(pre),
            )
            .unwrap();
        assert!(matches!(plan, Plan::Leaf(cuts) if cuts.len() == 2));
        assert_eq!(planner.guarded_word_expansions, 1);
        assert_eq!(planner.leaves, 1);
        assert!(planner.work < MAX_PLAN_WORK);
    }
    #[test]
    fn guarded_word_expansion_proves_both_goal_orientations() {
        if std::env::var("LYDITE_EXPANSION_ORIENTATION_CHILD").is_err() {
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "automatic::tests::guarded_word_expansion_proves_both_goal_orientations",
                    "--nocapture",
                ])
                .env("LYDITE_EXPANSION_ORIENTATION_CHILD", "1")
                .env("LYDITE_SOLVER", "finite")
                .env_remove("LYDITE_AUTOMATIC_PROOFS")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        for reversed in [false, true] {
            let left = add(v("a"), v("b"));
            let right = node(Sort::Bv(8), "bvxor", vec![v("c"), v("d")]);
            let product = |t| node(Sort::Bv(8), "bvmul", vec![t, v("factor")]);
            let pre = and(
                eq(v("encoded"), product(left.clone())),
                eq(left, right.clone()),
            );
            let goal = if reversed {
                eq(product(right), v("encoded"))
            } else {
                eq(v("encoded"), product(right))
            };
            let out = std::env::temp_dir().join(format!(
                "lydite-expansion-orientation-{}-{reversed}",
                std::process::id()
            ));
            fs::create_dir_all(&out).unwrap();
            let mut checker = Check {
                out: out.clone(),
                z3: "must-not-run".into(),
                reports: vec![],
            };
            assert!(
                checker
                    .query_automatic_proof(
                        "orientation",
                        and(pre, not(goal)),
                        &Env::new(),
                        CutBudgetMode::IndependentLemmas
                    )
                    .unwrap()
            );
            let r = &checker.reports[0];
            assert_eq!(
                r["status"],
                "passed",
                "reversed={reversed}: {:?}",
                r.get("automatic_plan_error")
            );
            crate::proof_bundle::validate_report(r).unwrap();
            fs::remove_dir_all(out).unwrap();
        }
    }
    #[test]
    fn staged_frontier_checks_each_intermediate_goal_and_rejects_unused_handles() {
        if std::env::var("LYDITE_STAGED_FRONTIER_CHILD").is_err() {
            let status = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "automatic::tests::staged_frontier_checks_each_intermediate_goal_and_rejects_unused_handles", "--nocapture"])
                .env("LYDITE_STAGED_FRONTIER_CHILD", "1").env("LYDITE_SOLVER", "finite")
                .env_remove("LYDITE_AUTOMATIC_PROOFS").status().unwrap();
            assert!(status.success());
            return;
        }
        for unused in [false, true] {
            let pre = and(
                eq(v("encoded"), add(v("middle"), v("offset"))),
                and(
                    eq(v("middle"), add(v("a"), v("b"))),
                    eq(add(v("a"), v("b")), v("result")),
                ),
            );
            let goal = eq(v("encoded"), add(v("result"), v("offset")));
            let out = std::env::temp_dir().join(format!(
                "lydite-staged-frontier-{}-{unused}",
                std::process::id()
            ));
            fs::create_dir_all(&out).unwrap();
            let mut checker = Check {
                out: out.clone(),
                z3: "must-not-run".into(),
                reports: vec![],
            };
            let mut bundle = ProofBundle::new(
                &mut checker,
                "staged",
                &and(pre.clone(), not(goal.clone())),
                &Env::new(),
                CutBudgetMode::IndependentLemmas,
            )
            .unwrap();
            let mut handles = vec![];
            for (from, to) in [
                (v("encoded"), add(v("middle"), v("offset"))),
                (v("middle"), add(v("a"), v("b"))),
                (add(v("a"), v("b")), v("result")),
            ] {
                handles.push(
                    bundle
                        .prove("fresh definition", pre.clone(), eq(from, to))
                        .unwrap(),
                );
            }
            if unused {
                handles.push(
                    bundle
                        .prove(
                            "unused reflexivity",
                            pre.clone(),
                            eq(v("absent"), v("absent")),
                        )
                        .unwrap(),
                );
                let result = rewrite_frontier(&mut bundle, pre, goal, handles, 0);
                assert!(matches!(result, Err(error) if error.contains("unused replacement")));
                drop(bundle);
            } else {
                let proved = rewrite_frontier(&mut bundle, pre, goal, handles, 0).unwrap();
                bundle.finish(&proved).unwrap();
                crate::proof_bundle::validate_report(&checker.reports[0]).unwrap();
                assert_eq!(checker.reports[0]["status"], "passed");
                assert_eq!(
                    checker.reports[0]["proof_graph"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|step| step["rule"]
                            == "exact-congruence-with-original-premise-retained")
                        .count(),
                    3
                );
            }
            fs::remove_dir_all(out).unwrap();
        }
    }
    #[test]
    fn entire_goal_is_not_its_own_cut_and_search_is_bounded() {
        let mut planner = Planner::default();
        assert!(
            matches!(planner.build(&v("a"),&v("b"),&mut vec![], &EqualityPool::new(boolv(true))).unwrap(),Plan::Leaf(c) if c.is_empty())
        );
        let mut planner = Planner {
            work: MAX_PLAN_WORK,
            ..Default::default()
        };
        assert!(
            planner
                .build(
                    &sum("a"),
                    &sum("b"),
                    &mut vec![],
                    &EqualityPool::new(boolv(true))
                )
                .is_err()
        );
    }
    #[test]
    fn fresh_kernel_accepts_correct_plan_and_replays_complement_mutation() {
        if std::env::var("LYDITE_TEST_AUTO_CHILD").is_err() {
            let status=Command::new(std::env::current_exe().unwrap())
                .args(["--exact","automatic::tests::fresh_kernel_accepts_correct_plan_and_replays_complement_mutation","--nocapture"])
                .env("LYDITE_TEST_AUTO_CHILD","1").env("LYDITE_SOLVER","finite")
                .env_remove("LYDITE_AUTOMATIC_PROOFS").status().unwrap();
            assert!(status.success());
            return;
        }
        for (bad, mode) in [
            (false, CutBudgetMode::IndependentLemmas),
            (false, CutBudgetMode::SharedQuery),
            (true, CutBudgetMode::IndependentLemmas),
        ] {
            let (formula, context) = fixture(bad);
            let out = std::env::temp_dir()
                .join(format!("lydite-auto-{}-{bad}-{mode:?}", std::process::id()));
            fs::create_dir_all(&out).unwrap();
            let mut check = Check {
                out: out.clone(),
                z3: "must-not-run".into(),
                reports: vec![],
            };
            assert!(
                check
                    .query_automatic_proof("automatic", formula, &context, mode)
                    .unwrap()
            );
            let r = &check.reports[0];
            crate::proof_bundle::validate_report(r).unwrap();
            assert_eq!(r["solver_result"], if bad { "sat" } else { "unsat" });
            assert_eq!(r["automatic_plan"]["source_names_used"], false);
            if bad {
                assert_eq!(
                    r["original_recheck"]["finite"]["original_formula_validated"],
                    true
                );
            }
            fs::remove_dir_all(out).unwrap();
        }
    }
    #[test]
    fn invalid_modes_and_mixed_budgets_reject_before_queries() {
        if let Ok(case) = std::env::var("LYDITE_TEST_AUTO_CONFIG") {
            let out =
                std::env::temp_dir().join(format!("lydite-auto-config-{}", std::process::id()));
            fs::create_dir_all(&out).unwrap();
            let mut check = Check {
                out: out.clone(),
                z3: "external-solver-must-not-be-started".into(),
                reports: vec![],
            };
            let result = if case == "mixed_shared" || case == "mixed_independent" {
                let mut callback = |_: &mut Check, _: &str, _: &Term, _: &Env| Ok(false);
                if case == "mixed_shared" {
                    check.query_implication_with_callback(
                        "mixed",
                        boolv(true),
                        boolv(true),
                        &Env::new(),
                        &mut callback,
                    )
                } else {
                    check.query_implication_with_fallback(
                        "mixed",
                        boolv(true),
                        boolv(true),
                        &Env::new(),
                        &mut callback,
                    )
                }
            } else {
                check.query("first_nonvacuity", boolv(true), true, &Env::new())
            };
            let error = result.unwrap_err();
            assert!(
                error.starts_with(if case == "invalid" {
                    "LYDITE_AUTOMATIC_PROOFS"
                } else {
                    "automatic proof"
                }),
                "{error}"
            );
            assert!(check.reports.is_empty());
            fs::remove_dir_all(out).unwrap();
            return;
        }
        for (case, setting, finite) in [
            ("invalid", "invalid", true),
            ("nonfinite", "independent_lemmas", false),
            ("mixed_shared", "independent_lemmas", true),
            ("mixed_independent", "shared_query", true),
        ] {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "automatic::tests::invalid_modes_and_mixed_budgets_reject_before_queries",
                    "--nocapture",
                ])
                .env("LYDITE_TEST_AUTO_CONFIG", case)
                .env("LYDITE_AUTOMATIC_PROOFS", setting)
                .env_remove("LYDITE_CONJUNCTIVE_LEMMAS");
            if finite {
                command.env("LYDITE_SOLVER", "finite");
            } else {
                command.env_remove("LYDITE_SOLVER");
            }
            assert!(command.status().unwrap().success(), "{case}");
        }
    }
    #[test]
    fn shared_budget_charges_planner_before_proof() {
        if std::env::var("LYDITE_TEST_AUTO_BUDGET").is_err() {
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "automatic::tests::shared_budget_charges_planner_before_proof",
                    "--nocapture",
                ])
                .env("LYDITE_TEST_AUTO_BUDGET", "1")
                .env("LYDITE_SOLVER", "finite")
                .env_remove("LYDITE_AUTOMATIC_PROOFS")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let (formula, context) = fixture(false);
        let out = std::env::temp_dir().join(format!("lydite-auto-budget-{}", std::process::id()));
        fs::create_dir_all(&out).unwrap();
        let mut check = Check {
            out: out.clone(),
            z3: "must-not-run".into(),
            reports: vec![],
        };
        {
            let mut bundle = ProofBundle::new(
                &mut check,
                "budget",
                &formula,
                &context,
                CutBudgetMode::SharedQuery,
            )
            .unwrap();
            assert!(bundle.charge_search(Instant::now(), 100_000_000).is_err());
        }
        assert_eq!(check.reports[0]["solver_result"], "unknown");
        assert!(
            check.reports[0]["cost"]["work_including_validation_and_derived_steps"]
                .as_u64()
                .unwrap()
                >= 100_000_000
        );
        fs::remove_dir_all(out).unwrap();
    }
    #[test]
    fn preserves_whole_word_variable_cutpoints_without_name_matching() {
        for prefix in ["plain", "alpha_9413_unrelated"] {
            let guard = var(format!("{prefix}_choose"), Sort::Bool);
            let mux = ite(
                guard,
                v(&format!("{prefix}_left")),
                v(&format!("{prefix}_right")),
            );
            let lhs = add(mux.clone(), bv(8, 1));
            let rhs = add(v(&format!("{prefix}_latched")), bv(8, 1));
            let mut planner = Planner::default();
            let plan = planner
                .build(&lhs, &rhs, &mut vec![], &EqualityPool::new(boolv(true)))
                .unwrap();
            assert!(matches!(plan,Plan::Leaf(ref cuts) if cuts.len()==1 && cuts[0].1==mux));
            assert_eq!(planner.word_cutpoint_frontiers, 1);
            assert_eq!(planner.leaves, 1);
        }
    }
    #[test]
    fn compound_datapath_muxes_still_use_exhaustive_splits() {
        let guard = var("route".into(), Sort::Bool);
        let mul = |a, b| node(Sort::Bv(8), "bvmul", vec![a, b]);
        let lhs = add(
            mul(ite(guard.clone(), v("a"), v("b")), v("scale")),
            bv(8, 1),
        );
        let rhs = add(
            ite(guard, mul(v("a"), v("scale")), mul(v("b"), v("scale"))),
            bv(8, 1),
        );
        let mut planner = Planner::default();
        let plan = planner
            .build(&lhs, &rhs, &mut vec![], &EqualityPool::new(boolv(true)))
            .unwrap();
        assert!(matches!(plan, Plan::Split(_, _, _)));
        assert_eq!(planner.word_cutpoint_frontiers, 0);
        assert_eq!(planner.leaves, 2);
        assert_eq!(planner.cuts, 0);
    }
}
