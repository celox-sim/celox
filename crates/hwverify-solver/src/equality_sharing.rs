//! Untrusted, bounded discovery and live-handle reuse of asserted equalities.
//! The graph only proposes paths: every edge and every guard discharge is
//! freshly checked by ProofBundle before an exact rewrite consumes it.
use crate::{ProofBundle, SequentHandle};
use hwverify_ir::*;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;
const MAX_FACTS: usize = 64;
const MAX_SCAN: usize = 4096;
const MAX_PATH: usize = 16;
const MAX_PATH_WORK: usize = 8192;
#[derive(Clone)]
struct Fact {
    left: Term,
    right: Term,
    guards: Vec<Term>,
}
pub(crate) struct EqualityPool {
    pre: Term,
    facts: Vec<Fact>,
    adjacency: HashMap<Term, Vec<(usize, bool)>>,
    known: HashSet<Term>,
    proved: HashMap<usize, SequentHandle>,
    instances: HashMap<(usize, Term), SequentHandle>,
    premises: HashMap<(Term, Term), SequentHandle>,
    paths: HashMap<(Term, Term, Term), SequentHandle>,
    pub(crate) scan_work: usize,
    path_work: usize,
    reused: usize,
    discharged: usize,
    cofactor_visits: usize,
    cofactor_proofs: usize,
}
fn conjuncts(term: &Term, out: &mut HashSet<Term>, work: &mut usize) {
    let mut todo = vec![term];
    while let Some(t) = todo.pop() {
        if *work >= MAX_SCAN {
            break;
        }
        *work += 1;
        if t.0.op == "and" && t.0.args.len() == 2 {
            todo.extend(t.0.args.iter());
        } else {
            out.insert(t.clone());
        }
    }
}
impl EqualityPool {
    pub(crate) fn new(pre: Term) -> Self {
        let mut facts = vec![];
        let mut todo = vec![(pre.clone(), vec![], 0usize)];
        let mut seen = HashSet::new();
        let mut work = 0;
        let mut known = HashSet::new();
        conjuncts(&pre, &mut known, &mut work);
        while let Some((t, guards, depth)) = todo.pop() {
            if work >= MAX_SCAN || facts.len() >= MAX_FACTS {
                break;
            }
            work += 1;
            if depth > 128 || !seen.insert((t.clone(), guards.clone())) {
                continue;
            }
            if t.0.op == "and" && t.0.args.len() == 2 {
                todo.push((t.0.args[1].clone(), guards.clone(), depth + 1));
                todo.push((t.0.args[0].clone(), guards, depth + 1));
            } else if t.0.op == "=>" && t.0.args.len() == 2 && guards.len() < 8 {
                let mut gs = guards;
                gs.push(t.0.args[0].clone());
                todo.push((t.0.args[1].clone(), gs, depth + 1));
            } else if t.0.op == "="
                && t.0.args.len() == 2
                && t.0.args[0].0.sort == t.0.args[1].0.sort
                && t.0.args[0] != t.0.args[1]
            {
                facts.push(Fact {
                    left: t.0.args[0].clone(),
                    right: t.0.args[1].clone(),
                    guards,
                });
            }
        }
        let mut adjacency: HashMap<Term, Vec<(usize, bool)>> = HashMap::new();
        for (index, fact) in facts.iter().enumerate() {
            adjacency
                .entry(fact.left.clone())
                .or_default()
                .push((index, true));
            adjacency
                .entry(fact.right.clone())
                .or_default()
                .push((index, false));
            work += 2;
        }
        Self {
            pre,
            facts,
            adjacency,
            known,
            proved: HashMap::new(),
            instances: HashMap::new(),
            premises: HashMap::new(),
            paths: HashMap::new(),
            scan_work: work,
            path_work: 0,
            reused: 0,
            discharged: 0,
            cofactor_visits: 0,
            cofactor_proofs: 0,
        }
    }
    fn active(&self, fact: &Fact, guards: &[(Term, bool)], work: &mut usize) -> bool {
        let mut todo = fact.guards.iter().collect::<Vec<_>>();
        let mut seen = HashSet::new();
        while let Some(g) = todo.pop() {
            *work += 1;
            if *work > MAX_PATH_WORK {
                return false;
            }
            if !seen.insert(g.clone()) {
                continue;
            }
            if *g == boolv(true)
                || self.known.contains(g)
                || guards
                    .iter()
                    .any(|(t, v)| if *v { t == g } else { not(t.clone()) == *g })
            {
                continue;
            }
            if g.0.op == "and" && g.0.args.len() == 2 {
                todo.extend(g.0.args.iter());
            } else {
                return false;
            }
        }
        true
    }
    pub(crate) fn path(
        &self,
        from: &Term,
        to: &Term,
        guards: &[(Term, bool)],
    ) -> (Option<Vec<(usize, bool)>>, usize) {
        let mut work = 0;
        if from.0.sort != to.0.sort || from == to {
            return (None, work);
        }
        let mut todo = VecDeque::from([(from.clone(), vec![])]);
        let mut seen = HashSet::from([from.clone()]);
        while let Some((t, path)) = todo.pop_front() {
            if path.len() >= MAX_PATH {
                continue;
            }
            for &(index, forward) in self.adjacency.get(&t).into_iter().flatten() {
                work += 1;
                if work > MAX_PATH_WORK {
                    return (None, work);
                }
                let fact = &self.facts[index];
                if !self.active(fact, guards, &mut work) {
                    continue;
                }
                let next = if forward { &fact.right } else { &fact.left };
                let mut next_path = path.clone();
                next_path.push((index, forward));
                if next == to {
                    return (Some(next_path), work);
                }
                if seen.insert(next.clone()) {
                    todo.push_back((next.clone(), next_path));
                }
            }
        }
        (None, work)
    }
    /// Untrusted one-step expansion candidate. Guards remain exact source
    /// guards and are discharged again by the live proof path at execution.
    pub(crate) fn expansion(
        &self,
        from: &Term,
        target: &Term,
        guards: &[(Term, bool)],
    ) -> (Option<Term>, usize) {
        let mut work = 0;
        if !from.0.op.starts_with('@')
            || !matches!(from.0.sort, Sort::Bv(_))
            || target.0.args.is_empty()
        {
            return (None, work);
        }
        for &(index, forward) in self.adjacency.get(from).into_iter().flatten() {
            work += 1;
            let fact = &self.facts[index];
            let next = if forward { &fact.right } else { &fact.left };
            if next.0.op == target.0.op
                && next.0.sort == target.0.sort
                && next.0.args.len() == target.0.args.len()
                && self.active(fact, guards, &mut work)
            {
                return (Some(next.clone()), work);
            }
        }
        (None, work)
    }
    fn edge(
        &mut self,
        bundle: &mut ProofBundle<'_>,
        index: usize,
        pre: &Term,
    ) -> Res<SequentHandle> {
        let key = (index, pre.clone());
        if let Some(h) = self.instances.get(&key) {
            self.reused += 1;
            return Ok(h.clone());
        }
        let source = if let Some(h) = self.proved.get(&index) {
            self.reused += 1;
            h.clone()
        } else {
            let f = &self.facts[index];
            let antecedent = f.guards.iter().cloned().fold(self.pre.clone(), and);
            let h = bundle.prove(
                "shared asserted equality",
                antecedent,
                eq(f.left.clone(), f.right.clone()),
            )?;
            self.proved.insert(index, h.clone());
            h
        };
        let h = if source.pre() == pre {
            source
        } else {
            let premise_key = (pre.clone(), source.pre().clone());
            let premise = if let Some(h) = self.premises.get(&premise_key) {
                self.reused += 1;
                h.clone()
            } else {
                let h = bundle.prove(
                    "shared equality exact guard discharge",
                    pre.clone(),
                    source.pre().clone(),
                )?;
                self.discharged += 1;
                self.premises.insert(premise_key, h.clone());
                h
            };
            bundle.apply(&source, &premise)?
        };
        self.instances.insert(key, h.clone());
        Ok(h)
    }
    pub(crate) fn prove(
        &mut self,
        bundle: &mut ProofBundle<'_>,
        pre: Term,
        from: Term,
        to: Term,
        guards: &[(Term, bool)],
    ) -> Res<SequentHandle> {
        let key = (pre.clone(), from.clone(), to.clone());
        if let Some(h) = self.paths.get(&key) {
            self.reused += 1;
            return Ok(h.clone());
        }
        let (path, work) = self.path(&from, &to, guards);
        self.path_work += work;
        bundle.charge_search(Instant::now(), work as u64)?;
        let Some(path) = path else {
            return bundle.prove("automatic frontier equality", pre, eq(from, to));
        };
        let mut combined: Option<SequentHandle> = None;
        for (index, forward) in path {
            let mut h = self.edge(bundle, index, &pre)?;
            if !forward {
                let goal = eq(h.post().0.args[1].clone(), h.post().0.args[0].clone());
                let plan = bundle.prepare_rewrite(pre.clone(), goal, &[h], false)?;
                let reflexive = bundle.prove(
                    "shared equality symmetry residual",
                    plan.pre().clone(),
                    plan.post().clone(),
                )?;
                h = bundle.finish_rewrite(plan, &reflexive)?;
            }
            combined = Some(if let Some(old) = combined {
                let goal = eq(from.clone(), h.post().0.args[1].clone());
                let plan = bundle.prepare_rewrite(pre.clone(), goal, &[old], false)?;
                let proof = if h.pre() == plan.pre() && h.post() == plan.post() {
                    h
                } else {
                    bundle.prove(
                        "shared equality transitive residual",
                        plan.pre().clone(),
                        plan.post().clone(),
                    )?
                };
                bundle.finish_rewrite(plan, &proof)?
            } else {
                h
            });
        }
        let h = combined.ok_or("empty equality sharing path")?;
        self.paths.insert(key, h.clone());
        Ok(h)
    }
    pub(crate) fn record_cofactor(&mut self, visits: usize, changed: bool) {
        self.cofactor_visits += visits;
        self.cofactor_proofs += usize::from(changed);
    }
    pub(crate) fn stats(&self) -> Value {
        json!({"rule":"fresh-handle-equality-path-reuse-v1","trusted":false,"asserted_facts":self.facts.len(),"fresh_edges":self.proved.len(),"instantiated_edges":self.instances.len(),"proved_paths":self.paths.len(),"reused_handles":self.reused,"fresh_guard_discharges":self.discharged,"scan_visits":self.scan_work,"execution_path_visits":self.path_work,"execution_cofactor_visits":self.cofactor_visits,"checked_cofactor_equivalences":self.cofactor_proofs,"max_facts":MAX_FACTS,"max_scan_visits":MAX_SCAN+2*MAX_FACTS,"max_source_scan_visits":MAX_SCAN,"max_path_edges":MAX_PATH,"max_path_visits":MAX_PATH_WORK,"saved_proofs_used":false})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Check, CutBudgetMode};
    use std::{fs, process::Command};
    fn v(name: &str) -> Term {
        var(name.into(), Sort::Bv(8))
    }
    fn word(op: &str, a: Term, b: Term) -> Term {
        node(Sort::Bv(8), op, vec![a, b])
    }
    fn terms() -> (Term, Term, Term) {
        (
            word("bvadd", v("a"), v("b")),
            word("bvxor", v("c"), v("d")),
            word("bvsub", v("e"), v("f")),
        )
    }
    #[test]
    fn word_expansion_requires_matching_structure_and_exact_active_guards() {
        let g = var("gate".into(), Sort::Bool);
        let definition = word("bvmul", v("a"), v("b"));
        let target = word("bvmul", v("c"), v("d"));
        let pool = EqualityPool::new(node(
            Sort::Bool,
            "=>",
            vec![g.clone(), eq(v("value"), definition.clone())],
        ));
        assert!(pool.expansion(&v("value"), &target, &[]).0.is_none());
        assert!(pool
            .expansion(&v("value"), &target, &[(g.clone(), false)])
            .0
            .is_none());
        assert_eq!(
            pool.expansion(&v("value"), &target, &[(g.clone(), true)]).0,
            Some(definition)
        );
        assert!(pool
            .expansion(&v("value"), &word("bvadd", v("c"), v("d")), &[(g, true)])
            .0
            .is_none());
        let negative = EqualityPool::new(not(eq(v("value"), target.clone())));
        assert!(negative.expansion(&v("value"), &target, &[]).0.is_none());
    }
    #[test]
    fn paths_are_bidirectional_guarded_and_ignore_negative_facts() {
        let (a, b, c) = terms();
        let g = var("gate".into(), Sort::Bool);
        let pre = and(
            eq(a.clone(), b.clone()),
            node(Sort::Bool, "=>", vec![g.clone(), eq(c.clone(), b.clone())]),
        );
        let pool = EqualityPool::new(pre);
        assert!(pool.path(&a, &c, &[]).0.is_none());
        assert!(pool.path(&a, &c, &[(g.clone(), false)]).0.is_none());
        assert_eq!(
            pool.path(&a, &c, &[(g, true)]).0.unwrap(),
            vec![(0, true), (1, false)]
        );
        let pool = EqualityPool::new(not(eq(a.clone(), c.clone())));
        assert!(pool.path(&a, &c, &[]).0.is_none());
    }
    #[test]
    fn live_paths_reuse_roots_across_branches_and_reject_forged_edges() {
        if std::env::var("HWVERIFY_EQUALITY_POOL_TEST").is_err() {
            let status=Command::new(std::env::current_exe().unwrap())
                .args(["--exact","equality_sharing::tests::live_paths_reuse_roots_across_branches_and_reject_forged_edges","--nocapture"])
                .env("HWVERIFY_EQUALITY_POOL_TEST","1").env("HWVERIFY_SOLVER","finite")
                .env_remove("HWVERIFY_AUTOMATIC_PROOFS").status().unwrap();
            assert!(status.success());
            return;
        }
        for mode in [CutBudgetMode::IndependentLemmas, CutBudgetMode::SharedQuery] {
            let (a, b, c) = terms();
            let pre = and(eq(a.clone(), b.clone()), eq(c.clone(), b));
            let goal = eq(
                word("bvmul", a.clone(), v("scale")),
                word("bvmul", c.clone(), v("scale")),
            );
            let out = std::env::temp_dir()
                .join(format!("equality-sharing-{}-{mode:?}", std::process::id()));
            fs::create_dir_all(&out).unwrap();
            let mut check = Check {
                z3: "must-not-run".into(),
                out: out.clone(),
                reports: vec![],
            };
            let mut bundle = ProofBundle::new(
                &mut check,
                "reuse",
                &and(pre.clone(), not(goal.clone())),
                &Env::new(),
                mode,
            )
            .unwrap();
            let mut pool = EqualityPool::new(pre.clone());
            let gate = var("gate".into(), Sort::Bool);
            let mut branches = vec![];
            for value in [true, false] {
                let branch = and(
                    pre.clone(),
                    if value {
                        gate.clone()
                    } else {
                        not(gate.clone())
                    },
                );
                let equality = pool
                    .prove(
                        &mut bundle,
                        branch.clone(),
                        a.clone(),
                        c.clone(),
                        &[(gate.clone(), value)],
                    )
                    .unwrap();
                let plan = bundle
                    .prepare_rewrite(branch, goal.clone(), &[equality], false)
                    .unwrap();
                let h = bundle
                    .prove(
                        "rewritten reflexive goal",
                        plan.pre().clone(),
                        plan.post().clone(),
                    )
                    .unwrap();
                branches.push(bundle.finish_rewrite(plan, &h).unwrap());
            }
            assert_eq!(pool.proved.len(), 2);
            assert!(pool.reused >= 2);
            assert_eq!(pool.discharged, 2);
            let h = bundle
                .join(pre, goal, gate, &branches[0], &branches[1])
                .unwrap();
            bundle.finish(&h).unwrap();
            crate::proof_bundle::validate_report(&check.reports[0]).unwrap();
            assert_eq!(check.reports[0]["status"], "passed");
            fs::remove_dir_all(out).unwrap();
        }
        {
            let (a, b, _) = terms();
            let gate = var("guard".into(), Sort::Bool);
            let pre = node(
                Sort::Bool,
                "=>",
                vec![gate.clone(), eq(a.clone(), b.clone())],
            );
            let goal = eq(a.clone(), b.clone());
            let out =
                std::env::temp_dir().join(format!("equality-guard-cache-{}", std::process::id()));
            fs::create_dir_all(&out).unwrap();
            let mut check = Check {
                z3: "must-not-run".into(),
                out: out.clone(),
                reports: vec![],
            };
            {
                let mut bundle = ProofBundle::new(
                    &mut check,
                    "guard_cache",
                    &and(pre.clone(), not(goal)),
                    &Env::new(),
                    CutBudgetMode::IndependentLemmas,
                )
                .unwrap();
                let mut pool = EqualityPool::new(pre.clone());
                pool.prove(
                    &mut bundle,
                    and(pre.clone(), gate.clone()),
                    a.clone(),
                    b.clone(),
                    &[(gate.clone(), true)],
                )
                .unwrap();
                // A lying heuristic guard hint cannot reuse the cached edge
                // under an antecedent that does not establish that guard.
                assert!(pool.prove(&mut bundle, pre, a, b, &[(gate, true)]).is_err());
            }
            assert_ne!(check.reports[0]["status"], "passed");
            assert_eq!(
                check.reports[0]["original_recheck"]["finite"]["original_formula_validated"],
                true
            );
            fs::remove_dir_all(out).unwrap();
        }
        let (a, b, _) = terms();
        let pre = boolv(true);
        let goal = eq(a.clone(), b.clone());
        let out = std::env::temp_dir().join(format!("equality-forged-{}", std::process::id()));
        fs::create_dir_all(&out).unwrap();
        let mut check = Check {
            z3: "must-not-run".into(),
            out: out.clone(),
            reports: vec![],
        };
        {
            let mut bundle = ProofBundle::new(
                &mut check,
                "forged",
                &and(pre.clone(), not(goal.clone())),
                &Env::new(),
                CutBudgetMode::IndependentLemmas,
            )
            .unwrap();
            // A forged heuristic premise must never become an authorized handle.
            let mut pool = EqualityPool::new(goal);
            pool.pre = pre.clone();
            assert!(pool.prove(&mut bundle, pre, a, b, &[]).is_err());
        }
        assert_ne!(check.reports[0]["status"], "passed");
        assert_eq!(
            check.reports[0]["original_recheck"]["finite"]["original_formula_validated"],
            true
        );
        fs::remove_dir_all(out).unwrap();
    }
}
