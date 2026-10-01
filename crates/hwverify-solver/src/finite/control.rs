//! Complete Boolean cofactoring for guarded finite-word contracts.
//! All cases share the original limits; SAT is replayed on the ORIGINAL query.
use super::*;
use hwverify_ir::{boolv, bv, node, not};

fn empty(hint: SearchHint) -> Outcome {
    Outcome {
        search_hint: hint,
        search_strategy: SearchStrategy::NotStarted,
        verdict: Verdict::Unknown,
        reason: None,
        assignments: BTreeMap::new(),
        context_values: BTreeMap::new(),
        original_formula_validated: false,
        stats: Stats::default(),
    }
}
fn remaining(b: &Budget, clauses: usize) -> Res<Limits> {
    b.check_time()?;
    let mut limits = b.limits.clone();
    limits.max_work = limits.max_work.saturating_sub(b.work);
    limits.max_clauses = limits.max_clauses.saturating_sub(clauses);
    limits.timeout_ms = limits
        .timeout_ms
        .saturating_sub(b.start.elapsed().as_millis() as u64);
    Ok(limits)
}
fn word_equality(t: &Term, b: &mut Budget) -> Res<bool> {
    let mut todo = vec![t];
    let mut seen = HashSet::new();
    while let Some(t) = todo.pop() {
        b.tick(1)?;
        if !seen.insert(t.clone()) {
            continue;
        }
        if seen.len() > 64 {
            return Ok(false);
        }
        if t.0.op == "=" && t.0.args.len() == 2 && matches!(t.0.args[0].0.sort, Sort::Bv(_)) {
            return Ok(true);
        }
        todo.extend(&t.0.args);
    }
    Ok(false)
}
fn candidate(t: &Term, b: &mut Budget) -> Res<bool> {
    let mut todo = vec![(t, 0usize)];
    let mut seen = HashSet::new();
    let mut guarded = 0;
    while let Some((t, depth)) = todo.pop() {
        b.tick(1)?;
        if depth > b.limits.max_depth {
            return Err("finite solver term depth budget exhausted".into());
        }
        if !seen.insert(t.clone()) {
            continue;
        }
        if seen.len() > b.limits.max_terms {
            return Err("finite solver term budget exhausted".into());
        }
        if t.0.op == "and" && t.0.args.len() == 2 {
            todo.extend(t.0.args.iter().map(|x| (x, depth + 1)));
        } else if t.0.op == "=>"
            && t.0.args.len() == 2
            && t.0.args[0].0.op.starts_with('@')
            && t.0.args[0].0.sort == Sort::Bool
            && word_equality(&t.0.args[1], b)?
        {
            guarded += 1;
        }
    }
    Ok(guarded >= 2)
}
fn validate(
    t: &Term,
    formula: bool,
    vars: &mut BTreeMap<String, Sort>,
    controls: &mut BTreeMap<String, Term>,
    seen: &mut HashSet<Term>,
    b: &mut Budget,
) -> Res<()> {
    let mut todo = vec![(t, 0usize)];
    while let Some((t, depth)) = todo.pop() {
        b.tick(1)?;
        if depth > b.limits.max_depth {
            return Err("finite solver term depth budget exhausted".into());
        }
        if !seen.insert(t.clone()) {
            continue;
        }
        if seen.len() > b.limits.max_terms {
            return Err("finite solver term budget exhausted".into());
        }
        if let Op::Variable(name) = operation(t)? {
            if let Some(sort) = vars.insert(name.clone(), t.0.sort.clone()) {
                if sort != t.0.sort {
                    return Err(format!("finite variable {name} has inconsistent sorts"));
                }
            }
            if formula && t.0.sort == Sort::Bool {
                controls.insert(name, t.clone());
            }
        }
        todo.extend(t.0.args.iter().map(|x| (x, depth + 1)));
    }
    Ok(())
}
fn fold(
    t: &Term,
    fixed: &BTreeMap<String, bool>,
    memo: &mut HashMap<Term, Term>,
    b: &mut Budget,
    depth: usize,
) -> Res<Term> {
    b.tick(1)?;
    if depth > b.limits.max_depth {
        return Err("finite solver term depth budget exhausted".into());
    }
    if let Some(t) = memo.get(t) {
        return Ok(t.clone());
    }
    if memo.len() >= b.limits.max_terms {
        return Err("finite solver term budget exhausted".into());
    }
    let op = operation(t)?;
    if let Op::Variable(name) = &op {
        if let Some(value) = fixed.get(name) {
            return Ok(boolv(*value));
        }
        return Ok(t.clone());
    }
    let args =
        t.0.args
            .iter()
            .map(|x| fold(x, fixed, memo, b, depth + 1))
            .collect::<Res<Vec<_>>>()?;
    let boolean = |x: &Term| match x.0.op.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    };
    let mut result = node(t.0.sort.clone(), t.0.op.clone(), args.clone());
    if matches!(op, Op::Ite) {
        if let Some(g) = boolean(&args[0]) {
            result = args[if g { 1 } else { 2 }].clone()
        } else if args[1] == args[2] {
            result = args[1].clone()
        }
    } else if matches!(op, Op::Not) {
        if let Some(v) = boolean(&args[0]) {
            result = boolv(!v)
        } else if args[0].0.op == "not" {
            result = args[0].0.args[0].clone()
        }
    } else if matches!(op, Op::And | Op::Or) {
        let identity = matches!(op, Op::And);
        if args.iter().any(|x| boolean(x) == Some(!identity)) {
            result = boolv(!identity)
        } else if boolean(&args[0]) == Some(identity) {
            result = args[1].clone()
        } else if boolean(&args[1]) == Some(identity) || args[0] == args[1] {
            result = args[0].clone()
        } else if (args[0].0.op == "not" && args[0].0.args[0] == args[1])
            || (args[1].0.op == "not" && args[1].0.args[0] == args[0])
        {
            result = boolv(!identity)
        }
    } else if matches!(op, Op::Implies) {
        if boolean(&args[0]) == Some(false) || boolean(&args[1]) == Some(true) {
            result = boolv(true)
        } else if boolean(&args[0]) == Some(true) {
            result = args[1].clone()
        } else if boolean(&args[1]) == Some(false) {
            result = not(args[0].clone())
        }
    } else if matches!(op, Op::Eq) && args[0] == args[1] {
        result = boolv(true)
    } else if matches!(op, Op::Add)
        && args[0]
            == bv(
                match t.0.sort {
                    Sort::Bv(w) => w,
                    _ => unreachable!(),
                },
                0,
            )
    {
        result = args[1].clone()
    } else if matches!(op, Op::Add | Op::Sub)
        && args[1]
            == bv(
                match t.0.sort {
                    Sort::Bv(w) => w,
                    _ => unreachable!(),
                },
                0,
            )
    {
        result = args[0].clone()
    }
    if !args.is_empty()
        && args
            .iter()
            .all(|x| matches!(operation(x), Ok(Op::Bool(_) | Op::Word(_))))
    {
        result = match evaluate(&result, &BTreeMap::new(), &mut HashMap::new(), b, 0)? {
            Scalar::Bool(v) => boolv(v),
            Scalar::Bv { width, value } => bv(width, value),
        };
    }
    memo.insert(t.clone(), result.clone());
    Ok(result)
}
fn merge_stats(out: &mut Stats, child: &Stats) {
    out.terms = out.terms.max(child.terms);
    out.variables = out.variables.max(child.variables);
    out.base_clauses += child.base_clauses;
    out.clauses += child.clauses;
    out.peak_live_clauses = out.peak_live_clauses.max(child.peak_live_clauses);
    out.base_cnf_reused |= child.base_cnf_reused;
    out.asserted_definitions += child.asserted_definitions;
    out.lookup_rewrites += child.lookup_rewrites;
    out.lookup_expansion_nodes += child.lookup_expansion_nodes;
    out.decisions += child.decisions;
    out.conflicts += child.conflicts;
    out.split_alternatives += child.split_alternatives;
    out.split_completed += child.split_completed;
    out.split_unsat += child.split_unsat;
    out.search_slices += child.search_slices;
    out.search_yields += child.search_yields;
}
pub(super) fn route(original: &Term, context: &Env, limits: Limits, hint: SearchHint) -> Outcome {
    let mut budget = Budget {
        limits,
        start: Instant::now(),
        work: 0,
        time_check_in: 0,
    };
    let mut out = empty(hint);
    let mut leaf_work = 0u64;
    let result = (|| -> Res<()> {
        budget.tick(1)?;
        if budget.limits.max_variables < 1 || budget.limits.max_clauses < 1 {
            return Err("finite solver initial CNF budget exhausted".into());
        }
        let mut eligible = candidate(original, &mut budget)?;
        let mut vars = BTreeMap::new();
        let mut controls = BTreeMap::new();
        let mut seen = HashSet::new();
        if eligible {
            validate(
                original,
                true,
                &mut vars,
                &mut controls,
                &mut seen,
                &mut budget,
            )?;
            eligible = (3..=6).contains(&controls.len()) && seen.len() >= 128;
            if eligible {
                for value in context.values() {
                    validate(
                        value,
                        false,
                        &mut vars,
                        &mut controls,
                        &mut seen,
                        &mut budget,
                    )?;
                }
            }
        }
        if !eligible {
            let prefix = budget.work;
            let plain = solve_plain(original, context, remaining(&budget, 0)?, hint);
            leaf_work += plain.stats.work;
            let used = plain.stats.work;
            out = plain;
            budget.tick(used)?;
            out.stats.work = budget.work;
            out.stats.control_preprocess_work = prefix;
            return Ok(());
        }
        out.search_strategy = SearchStrategy::ControlCofactors;
        out.stats.control_variables = controls.keys().cloned().collect();
        out.stats.control_cases = 1usize << controls.len();
        for case in 0..out.stats.control_cases {
            budget.tick(1)?;
            let before = budget.work;
            let fixed = controls
                .keys()
                .enumerate()
                .map(|(i, name)| (name.clone(), case & (1 << i) != 0))
                .collect::<BTreeMap<_, _>>();
            let mut memo = HashMap::new();
            let formula = fold(original, &fixed, &mut memo, &mut budget, 0)?;
            if formula == boolv(false) {
                out.stats.control_cases_closed += 1;
                out.stats.control_case_work.push(budget.work - before);
                continue;
            }
            let folded_context = context
                .iter()
                .map(|(name, t)| Ok((name.clone(), fold(t, &fixed, &mut memo, &mut budget, 0)?)))
                .collect::<Res<Env>>()?;
            let child = solve_plain(
                &formula,
                &folded_context,
                remaining(&budget, out.stats.clauses)?,
                hint,
            );
            merge_stats(&mut out.stats, &child.stats);
            leaf_work += child.stats.work;
            budget.tick(child.stats.work)?;
            out.stats.control_case_work.push(budget.work - before);
            match child.verdict {
                Verdict::Unknown => {
                    return Err(child
                        .reason
                        .unwrap_or_else(|| "unresolved control case".into()));
                }
                Verdict::Unsat => out.stats.control_cases_closed += 1,
                Verdict::Sat => {
                    let mut assignments = child.assignments;
                    for (name, sort) in &vars {
                        assignments
                            .entry(name.clone())
                            .or_insert_with(|| match sort {
                                Sort::Bool => Scalar::Bool(false),
                                Sort::Bv(w) => Scalar::Bv {
                                    width: *w,
                                    value: 0,
                                },
                                _ => unreachable!(),
                            });
                    }
                    for (name, value) in fixed {
                        assignments.insert(name, Scalar::Bool(value));
                    }
                    let mut memo = HashMap::new();
                    if evaluate(original, &assignments, &mut memo, &mut budget, 0)?
                        != Scalar::Bool(true)
                    {
                        return Err("control-case SAT failed original-formula validation".into());
                    }
                    let values = context
                        .iter()
                        .map(|(name, t)| {
                            Ok((
                                name.clone(),
                                evaluate(t, &assignments, &mut memo, &mut budget, 0)?,
                            ))
                        })
                        .collect::<Res<BTreeMap<_, _>>>()?;
                    budget.check_time()?;
                    out.assignments = assignments;
                    out.context_values = values;
                    out.original_formula_validated = true;
                    out.verdict = Verdict::Sat;
                    return Ok(());
                }
            }
        }
        budget.check_time()?;
        out.verdict = Verdict::Unsat;
        Ok(())
    })();
    if let Err(reason) = result {
        out.verdict = Verdict::Unknown;
        out.reason = Some(reason);
        out.assignments.clear();
        out.context_values.clear();
        out.original_formula_validated = false;
    }
    out.stats.work = budget.work;
    out.stats.control_preprocess_work = budget.work.saturating_sub(leaf_work);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use hwverify_ir::{and, eq, ite, var};
    fn fixture(last_only: bool) -> (Term, Env) {
        let controls = (0..3)
            .map(|i| var(format!("c{i}"), Sort::Bool))
            .collect::<Vec<_>>();
        let mut relation = boolv(true);
        for i in 0..48 {
            relation = and(relation, eq(var(format!("x{i}"), Sort::Bv(8)), bv(8, i)));
        }
        for (i, c) in controls.iter().enumerate() {
            relation = and(
                relation,
                node(
                    Sort::Bool,
                    "=>",
                    vec![
                        c.clone(),
                        eq(var(format!("x{i}"), Sort::Bv(8)), bv(8, i as u64)),
                    ],
                ),
            );
        }
        let formula = if last_only {
            let all = controls.iter().cloned().fold(boolv(true), and);
            and(
                relation,
                and(
                    all,
                    eq(
                        ite(
                            controls[0].clone(),
                            bv(8, 7),
                            var("dead_word".into(), Sort::Bv(8)),
                        ),
                        bv(8, 7),
                    ),
                ),
            )
        } else {
            and(relation.clone(), not(relation))
        };
        (formula, Env::from([("flag".into(), controls[0].clone())]))
    }
    #[test]
    fn all_control_cases_close_or_original_sat_model_is_reconstructed() {
        for sat in [false, true] {
            let (formula, context) = fixture(sat);
            let result = super::super::solve_with_hint(
                &formula,
                &context,
                Limits::default(),
                SearchHint::Unsat,
            );
            assert_eq!(result.search_strategy, SearchStrategy::ControlCofactors);
            assert_eq!(result.stats.control_cases, 8);
            assert_eq!(result.stats.control_cases_closed, if sat { 7 } else { 8 });
            assert_eq!(
                result.verdict,
                if sat { Verdict::Sat } else { Verdict::Unsat }
            );
            assert_eq!(result.original_formula_validated, sat);
            assert!(result.stats.control_preprocess_work > 0);
            if sat {
                for i in 0..3 {
                    assert_eq!(result.assignments[&format!("c{i}")], Scalar::Bool(true));
                }
                assert_eq!(result.context_values["flag"], Scalar::Bool(true));
                assert!(result.assignments.contains_key("dead_word"));
            }
            let limited = super::super::solve_with_hint(
                &formula,
                &context,
                Limits {
                    max_work: result.stats.work - 1,
                    ..Limits::default()
                },
                SearchHint::Unsat,
            );
            assert_eq!(limited.verdict, Verdict::Unknown);
            assert!(!limited.original_formula_validated);
            assert_eq!(
                super::super::solve_with_hint(
                    &formula,
                    &context,
                    Limits {
                        max_work: result.stats.work,
                        ..Limits::default()
                    },
                    SearchHint::Unsat
                )
                .verdict,
                result.verdict
            );
        }
    }
    #[test]
    fn control_splitting_never_bypasses_unsupported_terms_or_zero_limits() {
        let (formula, context) = fixture(false);
        for limits in [
            Limits {
                max_clauses: 0,
                ..Limits::default()
            },
            Limits {
                max_variables: 0,
                ..Limits::default()
            },
            Limits {
                timeout_ms: 0,
                ..Limits::default()
            },
        ] {
            assert_eq!(
                super::super::solve_with_hint(&formula, &context, limits, SearchHint::Unsat)
                    .verdict,
                Verdict::Unknown
            );
        }
        let malformed = and(formula, node(Sort::Bool, "unsupported", vec![]));
        assert_eq!(
            super::super::solve_with_hint(
                &malformed,
                &context,
                Limits::default(),
                SearchHint::Unsat
            )
            .verdict,
            Verdict::Unknown
        );
    }
}
