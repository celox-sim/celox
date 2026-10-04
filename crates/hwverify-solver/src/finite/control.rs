//! Exhaustive Boolean cofactoring for guarded finite-word contracts.
//! A selected subset may be fixed; all remaining variables stay symbolic.
//! All cases share the original limits; SAT is replayed on the ORIGINAL query.
use super::*;
use hwverify_ir::{boolv, bv, node, not};

pub(super) fn empty(hint: SearchHint) -> Outcome {
    Outcome {
        search_hint: hint,
        search_strategy: SearchStrategy::NotStarted,
        verdict: Verdict::Unknown,
        reason: None,
        assignments: BTreeMap::new(),
        context_values: BTreeMap::new(),
        array_assignments: BTreeMap::new(),
        array_context_values: BTreeMap::new(),
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
    let mut word_mux = false;
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
        // Eligibility is only a search heuristic. Word muxes benefit even with
        // one or two Boolean inputs; every valuation is still solved below.
        if t.0.op == "ite" && matches!(t.0.sort, Sort::Bv(_)) {
            word_mux = true;
        }
        if t.0.op == "=>"
            && t.0.args.len() == 2
            && t.0.args[0].0.op.starts_with('@')
            && t.0.args[0].0.sort == Sort::Bool
            && word_equality(&t.0.args[1], b)?
        {
            guarded += 1;
        }
        todo.extend(t.0.args.iter().map(|x| (x, depth + 1)));
    }
    Ok(guarded >= 2 || word_mux)
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
// Expose mandatory counterexample guards without interpreting disjunctive
// branches as assumptions. This is an exact Boolean identity, not a projection.
fn asserted_polarity(t: &Term, negative: bool, b: &mut Budget, depth: usize) -> Res<Term> {
    fn visit(
        t: &Term,
        negative: bool,
        b: &mut Budget,
        depth: usize,
        memo: &mut HashMap<(Term, bool), Term>,
    ) -> Res<Term> {
        b.tick(1)?;
        if depth > b.limits.max_depth {
            return Err("finite polarity depth budget exhausted".into());
        }
        let key = (t.clone(), negative);
        if let Some(result) = memo.get(&key) {
            return Ok(result.clone());
        }
        if memo.len() >= b.limits.max_terms {
            return Err("finite polarity term budget exhausted".into());
        }
        let args = &t.0.args;
        let result = match operation(t)? {
            Op::Not => visit(&args[0], !negative, b, depth + 1, memo)?,
            Op::And if !negative => hwverify_ir::and(
                visit(&args[0], false, b, depth + 1, memo)?,
                visit(&args[1], false, b, depth + 1, memo)?,
            ),
            Op::Or if negative => hwverify_ir::and(
                visit(&args[0], true, b, depth + 1, memo)?,
                visit(&args[1], true, b, depth + 1, memo)?,
            ),
            Op::Implies if negative => hwverify_ir::and(
                visit(&args[0], false, b, depth + 1, memo)?,
                visit(&args[1], true, b, depth + 1, memo)?,
            ),
            _ => {
                if negative {
                    not(t.clone())
                } else {
                    t.clone()
                }
            }
        };
        // Children may have consumed the remaining node slots. Each unique
        // (source, polarity) allocates at most one resulting Boolean node.
        if memo.len() >= b.limits.max_terms {
            return Err("finite polarity term budget exhausted".into());
        }
        memo.insert(key, result.clone());
        Ok(result)
    }
    visit(t, negative, b, depth, &mut HashMap::new())
}

// Unit propagation is deliberately bounded. Stopping early only leaves more
// symbolic work to the solver. Every unit is entailed by the complete formula;
// contradictory units make that formula false. Caller validates ALL original
// formula/context nodes before this routine can discard any branch.
fn forced_units(original: &Term, b: &mut Budget) -> Res<(Term, BTreeMap<String, bool>)> {
    let mut formula = asserted_polarity(original, false, b, 0)?;
    let mut fixed = BTreeMap::new();
    for _ in 0..8 {
        let before = fixed.len();
        let mut todo = vec![formula.clone()];
        let mut seen = HashSet::new();
        while let Some(t) = todo.pop() {
            b.tick(1)?;
            if !seen.insert(t.clone()) {
                continue;
            }
            if seen.len() > b.limits.max_terms {
                return Err("finite forced-unit term budget exhausted".into());
            }
            if t.0.op == "and" {
                todo.extend(t.0.args.iter().cloned());
                continue;
            }
            let (atom, value) = if t.0.op == "not" {
                (&t.0.args[0], false)
            } else {
                (&t, true)
            };
            if atom.0.sort == Sort::Bool {
                if let Op::Variable(name) = operation(atom)? {
                    if fixed.insert(name, value).is_some_and(|old| old != value) {
                        return Ok((boolv(false), fixed));
                    }
                }
            }
        }
        if before == fixed.len() {
            break;
        }
        formula = fold(&formula, &fixed, &mut HashMap::new(), b, 0)?;
        formula = asserted_polarity(&formula, false, b, 0)?;
    }
    Ok((formula, fixed))
}

// Bound both selector preprocessing and downstream duplication independently of
// the available Boolean count. Existing one-to-six-control queries retain their
// original exhaustive order; large queries use at most eight cofactors.
const MAX_CONTROL_PROBES: usize = 16;
const MAX_SELECTED_CONTROLS: usize = 3;

fn word_muxes(t: &Term, b: &mut Budget) -> Res<usize> {
    let mut seen = HashSet::new();
    let mut todo = vec![t];
    let mut count = 0;
    while let Some(t) = todo.pop() {
        b.tick(1)?;
        if !seen.insert(t.clone()) {
            continue;
        }
        if seen.len() > b.limits.max_terms {
            return Err("finite solver term budget exhausted".into());
        }
        if t.0.op == "ite" && matches!(t.0.sort, Sort::Bv(_)) {
            count += 1;
        }
        todo.extend(&t.0.args);
    }
    Ok(count)
}

fn select_controls(
    original: &Term,
    controls: &mut BTreeMap<String, Term>,
    b: &mut Budget,
) -> Res<()> {
    let mut frequency = BTreeMap::<String, usize>::new();
    let mut seen = HashSet::new();
    let mut todo = vec![original];
    let mut original_muxes = 0usize;
    while let Some(t) = todo.pop() {
        b.tick(1)?;
        if !seen.insert(t.clone()) {
            continue;
        }
        if seen.len() > b.limits.max_terms {
            return Err("finite solver term budget exhausted".into());
        }
        if t.0.op == "ite" && matches!(t.0.sort, Sort::Bv(_)) {
            original_muxes += 1;
            let mut guard = &t.0.args[0];
            if guard.0.op == "not" {
                guard = &guard.0.args[0];
            }
            if let Some(name) = guard.0.op.strip_prefix('@') {
                if controls.contains_key(name) {
                    *frequency.entry(name.to_string()).or_default() += 1;
                }
            }
        }
        todo.extend(&t.0.args);
    }
    let mut candidates = frequency.into_iter().collect::<Vec<_>>();
    // Structural frequency bounds the probe list, with stable lexical ties.
    candidates.sort_by(|a, c| c.1.cmp(&a.1).then_with(|| a.0.cmp(&c.0)));
    candidates.truncate(MAX_CONTROL_PROBES);
    let mut influence = Vec::new();
    for (name, _) in candidates {
        let mut retained = 0usize;
        for value in [false, true] {
            let fixed = BTreeMap::from([(name.clone(), value)]);
            let folded = fold(original, &fixed, &mut HashMap::new(), b, 0)?;
            retained += word_muxes(&folded, b)?;
        }
        let removed = original_muxes.saturating_mul(2).saturating_sub(retained);
        if removed > 0 {
            influence.push((name, removed));
        }
    }
    influence.sort_by(|a, c| c.1.cmp(&a.1).then_with(|| a.0.cmp(&c.0)));
    influence.truncate(MAX_SELECTED_CONTROLS);
    let selected = influence
        .into_iter()
        .map(|(name, _)| name)
        .collect::<HashSet<_>>();
    controls.retain(|name, _| selected.contains(name));
    Ok(())
}

// Exact bounded DAG comparison. Pointer pairs identify previously checked
// pairs only: sorts, operators, arities and all children establish equality.
// This deliberately does not use structural hashes as proof evidence.
fn same_formula(left: &Term, right: &Term, b: &mut Budget) -> Res<bool> {
    let mut todo = vec![(left, right, 0usize)];
    let mut seen = HashSet::new();
    while let Some((left, right, depth)) = todo.pop() {
        b.tick(1)?;
        if depth > b.limits.max_depth {
            return Err("cofactor cache comparison depth budget exhausted".into());
        }
        let pair = (std::rc::Rc::as_ptr(&left.0), std::rc::Rc::as_ptr(&right.0));
        if !seen.insert(pair) {
            continue;
        }
        if seen.len() > b.limits.max_terms {
            return Err("cofactor cache comparison term budget exhausted".into());
        }
        if left.0.sort != right.0.sort
            || left.0.op != right.0.op
            || left.0.args.len() != right.0.args.len()
        {
            return Ok(false);
        }
        todo.extend(
            left.0
                .args
                .iter()
                .zip(&right.0.args)
                .map(|(a, c)| (a, c, depth + 1)),
        );
    }
    Ok(true)
}

fn merge_stats(out: &mut Stats, child: &Stats) {
    out.terms = out.terms.max(child.terms);
    out.variables = out.variables.max(child.variables);
    out.base_clauses += child.base_clauses;
    out.clauses += child.clauses;
    out.peak_live_clauses = out.peak_live_clauses.max(child.peak_live_clauses);
    out.base_cnf_reused |= child.base_cnf_reused;
    out.asserted_definitions += child.asserted_definitions;
    out.guarded_equalities += child.guarded_equalities;
    out.guarded_rewrites += child.guarded_rewrites;
    out.guarded_expansion_nodes += child.guarded_expansion_nodes;
    out.lookup_rewrites += child.lookup_rewrites;
    out.word_rewrites += child.word_rewrites;
    out.word_expansion_nodes += child.word_expansion_nodes;
    out.lookup_expansion_nodes += child.lookup_expansion_nodes;
    out.decisions += child.decisions;
    out.conflicts += child.conflicts;
    out.split_alternatives += child.split_alternatives;
    out.split_completed += child.split_completed;
    out.split_unsat += child.split_unsat;
    out.search_slices += child.search_slices;
    out.search_yields += child.search_yields;
}
pub(super) fn route(
    original: &Term,
    context: &Env,
    limits: Limits,
    hint: SearchHint,
    readonly: bool,
) -> Outcome {
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
        let mut normalized = original.clone();
        let mut forced = BTreeMap::new();
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
            eligible = !controls.is_empty() && seen.len() >= 128;
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
                if controls.len() > 6 {
                    // Only the large-control route needs this preprocessing.
                    // Keep the legacy small-control case order unchanged.
                    (normalized, forced) = forced_units(original, &mut budget)?;
                    controls.retain(|name, _| !forced.contains_key(name));
                    select_controls(&normalized, &mut controls, &mut budget)?;
                    // An empty selector set is one exact case, not a reason
                    // to lose already-propagated mandatory guards.
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
        // One invocation-local entry bounds retained formula memory. Trivial
        // false cases do not evict the last fully proved nontrivial cofactor.
        let mut last_unsat: Option<Term> = None;
        for case in 0..out.stats.control_cases {
            budget.tick(1)?;
            let before = budget.work;
            let mut fixed = controls
                .keys()
                .enumerate()
                .map(|(i, name)| (name.clone(), case & (1 << i) != 0))
                .collect::<BTreeMap<_, _>>();
            fixed.extend(forced.iter().map(|(name, value)| (name.clone(), *value)));
            let mut memo = HashMap::new();
            let formula = fold(&normalized, &fixed, &mut memo, &mut budget, 0)?;
            if formula == boolv(false) {
                out.stats.control_cases_closed += 1;
                out.stats.control_case_work.push(budget.work - before);
                continue;
            }
            let folded_context = context
                .iter()
                .map(|(name, t)| Ok((name.clone(), fold(t, &fixed, &mut memo, &mut budget, 0)?)))
                .collect::<Res<Env>>()?;
            let formula = if readonly {
                readonly::strengthen(&formula, &mut budget)?
            } else {
                formula
            };
            if let Some(closed) = &last_unsat {
                if same_formula(closed, &formula, &mut budget)? {
                    out.stats.control_cache_hits += 1;
                    out.stats.control_cases_closed += 1;
                    out.stats.control_case_work.push(budget.work - before);
                    continue;
                }
            }
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
                Verdict::Unsat => {
                    out.stats.control_cases_closed += 1;
                    last_unsat = Some(formula);
                }
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
    fn or(x: Term, y: Term) -> Term {
        node(Sort::Bool, "or", vec![x, y])
    }
    fn test_budget() -> Budget {
        Budget {
            limits: Limits::default(),
            start: Instant::now(),
            work: 0,
            time_check_in: 0,
        }
    }
    #[test]
    fn asserted_polarity_and_units_match_exhaustive_boolean_oracle() {
        let vars = (0..4)
            .map(|i| var(format!("p{i}"), Sort::Bool))
            .collect::<Vec<_>>();
        let imp = |x, y| node(Sort::Bool, "=>", vec![x, y]);
        let forms = vec![
            not(imp(vars[0].clone(), imp(vars[1].clone(), vars[2].clone()))),
            and(vars[0].clone(), not(imp(vars[0].clone(), vars[1].clone()))),
            and(
                vars[0].clone(),
                not(imp(not(vars[0].clone()), vars[1].clone())),
            ),
            not(or(vars[0].clone(), not(vars[1].clone()))),
            or(vars[0].clone(), not(imp(vars[1].clone(), vars[2].clone()))),
            imp(vars[0].clone(), and(vars[1].clone(), vars[2].clone())),
            and(
                vars[0].clone(),
                imp(vars[0].clone(), and(vars[1].clone(), not(vars[3].clone()))),
            ),
        ];
        for formula in forms {
            let normalized = asserted_polarity(&formula, false, &mut test_budget(), 0).unwrap();
            let (folded, units) = forced_units(&formula, &mut test_budget()).unwrap();
            for mask in 0..16 {
                let values = (0..4)
                    .map(|i| (format!("p{i}"), Scalar::Bool(mask & (1 << i) != 0)))
                    .collect::<BTreeMap<_, _>>();
                let ev = |t: &Term| {
                    evaluate(t, &values, &mut HashMap::new(), &mut test_budget(), 0).unwrap()
                };
                assert_eq!(ev(&formula), ev(&normalized));
                let satisfies_units = units
                    .iter()
                    .all(|(name, value)| values[name] == Scalar::Bool(*value));
                assert_eq!(
                    ev(&formula),
                    Scalar::Bool(satisfies_units && ev(&folded) == Scalar::Bool(true))
                );
            }
        }
    }
    #[test]
    fn asserted_polarity_shared_dag_and_limits_are_bounded() {
        let p = var("shared".into(), Sort::Bool);
        let mut dag = not(node(Sort::Bool, "=>", vec![p.clone(), p]));
        for _ in 0..24 {
            dag = and(dag.clone(), dag);
        }
        let mut budget = test_budget();
        let normalized = asserted_polarity(&dag, false, &mut budget, 0).unwrap();
        assert!(
            budget.work < 100,
            "shared DAG must not expand exponentially"
        );
        let mut seen = HashSet::new();
        let mut todo = vec![normalized];
        while let Some(t) = todo.pop() {
            if seen.insert(t.clone()) {
                todo.extend(t.0.args.iter().cloned());
            }
        }
        assert!(seen.len() < 32);
        for limits in [
            Limits {
                max_terms: 10,
                ..Limits::default()
            },
            Limits {
                max_work: 10,
                ..Limits::default()
            },
            Limits {
                max_depth: 10,
                ..Limits::default()
            },
        ] {
            let mut budget = Budget {
                limits,
                ..test_budget()
            };
            assert!(asserted_polarity(&dag, false, &mut budget, 0).is_err());
        }
    }
    #[test]
    fn forced_guard_route_keeps_original_replay_context_and_both_hints() {
        let (base, mut context) = many_controls(9, false);
        let p = var("mandatory".into(), Sort::Bool);
        let q = var("conclusion".into(), Sort::Bool);
        let tail = not(node(Sort::Bool, "=>", vec![p.clone(), q.clone()]));
        context.insert("mandatory".into(), p.clone());
        context.insert("conclusion".into(), q.clone());
        for conflict in [false, true] {
            let formula = and(
                base.clone(),
                if conflict {
                    and(not(p.clone()), tail.clone())
                } else {
                    tail.clone()
                },
            );
            for hint in [SearchHint::Sat, SearchHint::Unsat] {
                let out =
                    super::super::solve_with_hint(&formula, &context, Limits::default(), hint);
                assert_eq!(
                    out.verdict,
                    if conflict {
                        Verdict::Unsat
                    } else {
                        Verdict::Sat
                    }
                );
                if !conflict {
                    assert!(out.original_formula_validated);
                    assert_eq!(out.context_values["mandatory"], Scalar::Bool(true));
                    assert_eq!(out.context_values["conclusion"], Scalar::Bool(false));
                }
            }
        }
    }
    fn cache_fixture(varying: bool) -> (Term, Env) {
        let x = var("cache_word".into(), Sort::Bv(8));
        let c = var("cache_control".into(), Sort::Bool);
        let mut formula = boolv(true);
        for i in 0..48 {
            formula = and(
                formula,
                eq(var(format!("cache_padding{i}"), Sort::Bv(8)), bv(8, i)),
            );
        }
        let left = node(Sort::Bv(8), "bvadd", vec![x.clone(), bv(8, 1)]);
        let value = if varying {
            ite(c.clone(), bv(8, 2), bv(8, 1))
        } else {
            ite(c.clone(), bv(8, 1), bv(8, 1))
        };
        let right = node(Sort::Bv(8), "bvadd", vec![value, x]);
        formula = and(formula, not(eq(left, right)));
        (formula, Env::from([("selected case".into(), c)]))
    }
    #[test]
    fn exact_unsat_cofactor_reuse_and_distinct_case_sat_replay() {
        let (formula, context) = cache_fixture(false);
        let out =
            super::super::solve_with_hint(&formula, &context, Limits::default(), SearchHint::Unsat);
        assert_eq!(out.verdict, Verdict::Unsat);
        assert_eq!(out.stats.control_cases, 2);
        assert_eq!(out.stats.control_cases_closed, 2);
        assert_eq!(out.stats.control_cache_hits, 1);
        assert_eq!(out.diagnostics()["control_cache_hits"], 1);
        for work in [out.stats.work - 1, out.stats.work] {
            let limited = super::super::solve_with_hint(
                &formula,
                &context,
                Limits {
                    max_work: work,
                    ..Limits::default()
                },
                SearchHint::Unsat,
            );
            assert_eq!(
                limited.verdict,
                if work < out.stats.work {
                    Verdict::Unknown
                } else {
                    Verdict::Unsat
                }
            );
        }
        // An intervening trivially false cofactor must not evict the proof.
        let skipped = and(not(var("aaa_skip".into(), Sort::Bool)), formula.clone());
        let reused =
            super::super::solve_with_hint(&skipped, &context, Limits::default(), SearchHint::Unsat);
        assert_eq!(reused.verdict, Verdict::Unsat);
        assert_eq!(reused.stats.control_cases_closed, 4);
        assert_eq!(reused.stats.control_cache_hits, 1);
        // Different source/context and later calls never inherit cached UNSAT.
        let (formula, context) = cache_fixture(true);
        for hint in [SearchHint::Unsat, SearchHint::Sat] {
            let out = super::super::solve_with_hint(&formula, &context, Limits::default(), hint);
            assert_eq!(out.verdict, Verdict::Sat);
            assert!(out.original_formula_validated);
            assert_eq!(out.context_values["selected case"], Scalar::Bool(true));
            assert_eq!(out.stats.control_cache_hits, 0);
        }
        let (formula, mut context) = cache_fixture(false);
        context.insert("invalid".into(), node(Sort::Bool, "unsupported", vec![]));
        let out =
            super::super::solve_with_hint(&formula, &context, Limits::default(), SearchHint::Unsat);
        assert_eq!(out.verdict, Verdict::Unknown);
        assert_eq!(out.stats.control_cache_hits, 0);
    }
    #[test]
    fn cofactor_comparison_is_structural_bounded_and_shared_dag_safe() {
        let mut a = var("atom".into(), Sort::Bool);
        let mut b = var("atom".into(), Sort::Bool);
        for _ in 0..24 {
            a = and(a.clone(), a);
            b = and(b.clone(), b);
        }
        let mut budget = test_budget();
        assert!(same_formula(&a, &b, &mut budget).unwrap());
        assert!(budget.work < 100);
        assert!(!same_formula(&a, &not(b.clone()), &mut test_budget()).unwrap());
        assert!(!same_formula(
            &var("atom".into(), Sort::Bool),
            &var("atom".into(), Sort::Bv(1)),
            &mut test_budget()
        )
        .unwrap());
        for limits in [
            Limits {
                max_terms: 10,
                ..Limits::default()
            },
            Limits {
                max_depth: 10,
                ..Limits::default()
            },
            Limits {
                max_work: budget.work - 1,
                ..Limits::default()
            },
        ] {
            assert!(same_formula(
                &a,
                &b,
                &mut Budget {
                    limits,
                    ..test_budget()
                }
            )
            .is_err());
        }
    }
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
    fn one_and_two_control_word_muxes_replay_and_share_limits() {
        for count in 1..=2 {
            for sat in [false, true] {
                let controls = (0..count)
                    .map(|i| var(format!("select{i}"), Sort::Bool))
                    .collect::<Vec<_>>();
                let mut formula = boolv(true);
                for i in 0..48 {
                    formula = and(formula, eq(var(format!("word{i}"), Sort::Bv(8)), bv(8, i)));
                }
                let mut mux = bv(8, 7);
                for control in &controls {
                    mux = ite(control.clone(), mux, bv(8, 3));
                }
                formula = and(formula, eq(mux.clone(), bv(8, if sat { 7 } else { 9 })));
                let context = Env::from([("mux".into(), mux)]);
                let out = super::super::solve_with_hint(
                    &formula,
                    &context,
                    Limits::default(),
                    SearchHint::Unsat,
                );
                assert_eq!(out.search_strategy, SearchStrategy::ControlCofactors);
                assert_eq!(out.stats.control_cases, 1 << count);
                assert_eq!(
                    out.stats.control_cases_closed,
                    (1 << count) - usize::from(sat)
                );
                assert_eq!(out.verdict, if sat { Verdict::Sat } else { Verdict::Unsat });
                assert_eq!(out.original_formula_validated, sat);
                if sat {
                    assert_eq!(out.context_values["mux"], Scalar::Bv { width: 8, value: 7 });
                    for i in 0..count {
                        assert_eq!(out.assignments[&format!("select{i}")], Scalar::Bool(true));
                    }
                }
                let limited = super::super::solve_with_hint(
                    &formula,
                    &context,
                    Limits {
                        max_work: out.stats.work - 1,
                        ..Limits::default()
                    },
                    SearchHint::Unsat,
                );
                assert_eq!(limited.verdict, Verdict::Unknown);
                assert!(!limited.original_formula_validated);
                assert!(limited.assignments.is_empty());
            }
        }
    }
    #[test]
    fn mux_routing_handles_many_controls_and_validates_dead_context() {
        for count in [0, 1, 7] {
            let mut formula = boolv(true);
            for i in 0..48 {
                formula = and(formula, eq(var(format!("word{i}"), Sort::Bv(8)), bv(8, i)));
            }
            let mut mux = var("value".into(), Sort::Bv(8));
            for i in 0..count {
                mux = ite(var(format!("c{i}"), Sort::Bool), mux, bv(8, i));
            }
            formula = and(formula, eq(mux, bv(8, 42)));
            let result = super::super::solve_with_hint(
                &formula,
                &Env::new(),
                Limits::default(),
                SearchHint::Unsat,
            );
            assert_eq!(result.verdict, Verdict::Sat);
            assert!(result.original_formula_validated);
            assert_eq!(
                result.search_strategy == SearchStrategy::ControlCofactors,
                count > 0
            );
            let context = Env::from([(
                "dead".into(),
                ite(
                    boolv(false),
                    node(Sort::Bv(8), "unsupported", vec![]),
                    bv(8, 0),
                ),
            )]);
            let result = super::super::solve_with_hint(
                &formula,
                &context,
                Limits::default(),
                SearchHint::Unsat,
            );
            assert_eq!(result.verdict, Verdict::Unknown);
            assert!(!result.original_formula_validated);
        }
    }
    fn many_controls(count: usize, unsat: bool) -> (Term, Env) {
        let controls = (0..count)
            .map(|i| var(format!("control{i:02}"), Sort::Bool))
            .collect::<Vec<_>>();
        let mut formula = boolv(true);
        for i in 0..48 {
            formula = and(formula, eq(var(format!("word{i}"), Sort::Bv(8)), bv(8, i)));
        }
        for control in &controls[..count - 1] {
            formula = and(
                formula,
                eq(ite(control.clone(), bv(8, 7), bv(8, 3)), bv(8, 7)),
            );
        }
        formula = and(formula, controls[count - 1].clone());
        if unsat {
            // This variable is outside the lexical three-control selection.
            formula = and(formula, not(controls[count - 1].clone()));
        }
        // Both names must survive in the original-model reconstruction even
        // though exact folding removes their last occurrences.
        formula = and(
            formula,
            eq(
                ite(
                    boolv(false),
                    var("dead_formula".into(), Sort::Bv(8)),
                    bv(8, 0),
                ),
                bv(8, 0),
            ),
        );
        let context = Env::from([
            ("unselected".into(), controls[count - 1].clone()),
            (
                "dead_context".into(),
                ite(
                    controls[0].clone(),
                    bv(8, 9),
                    var("context_only".into(), Sort::Bv(8)),
                ),
            ),
        ]);
        (formula, context)
    }

    #[test]
    fn selected_subset_covers_all_cases_and_keeps_other_controls_symbolic() {
        for count in 7..=15 {
            for unsat in [false, true] {
                let (formula, context) = many_controls(count, unsat);
                let out = super::super::solve_with_hint(
                    &formula,
                    &context,
                    Limits::default(),
                    SearchHint::Unsat,
                );
                assert_eq!(out.search_strategy, SearchStrategy::ControlCofactors);
                if unsat {
                    assert!(out.stats.control_variables.is_empty());
                    assert_eq!(out.stats.control_cases, 1);
                    assert_eq!(out.stats.control_cases_closed, 1);
                } else {
                    assert_eq!(
                        out.stats.control_variables,
                        ["control00", "control01", "control02"]
                    );
                    assert_eq!(out.stats.control_cases, 8);
                    assert_eq!(out.stats.control_cases_closed, 7);
                }
                assert_eq!(
                    out.verdict,
                    if unsat { Verdict::Unsat } else { Verdict::Sat }
                );
                assert_eq!(out.original_formula_validated, !unsat);
                if !unsat {
                    assert_eq!(out.context_values["unselected"], Scalar::Bool(true));
                    assert_eq!(
                        out.context_values["dead_context"],
                        Scalar::Bv { width: 8, value: 9 }
                    );
                    assert!(out.assignments.contains_key("dead_formula"));
                    assert!(out.assignments.contains_key("context_only"));
                    for i in 0..count {
                        assert_eq!(
                            out.assignments[&format!("control{i:02}")],
                            Scalar::Bool(true)
                        );
                    }
                }
                for work in [out.stats.work - 1, out.stats.work] {
                    let limited = super::super::solve_with_hint(
                        &formula,
                        &context,
                        Limits {
                            max_work: work,
                            ..Limits::default()
                        },
                        SearchHint::Unsat,
                    );
                    assert_eq!(
                        limited.verdict,
                        if work < out.stats.work {
                            Verdict::Unknown
                        } else {
                            out.verdict
                        }
                    );
                    if work < out.stats.work {
                        assert!(limited.assignments.is_empty());
                        assert!(!limited.original_formula_validated);
                    }
                }
            }
        }
    }

    #[test]
    fn subset_probes_validate_unselected_sorts_and_dead_original_nodes() {
        let (formula, context) = many_controls(15, false);
        let mut bad_sort = context.clone();
        bad_sort.insert("bad_sort".into(), var("control14".into(), Sort::Bv(8)));
        let mut bad_context = context.clone();
        bad_context.insert(
            "bad_context".into(),
            ite(
                boolv(false),
                node(Sort::Bv(8), "unsupported", vec![]),
                bv(8, 0),
            ),
        );
        for context in [bad_sort, bad_context] {
            let out = super::super::solve_with_hint(
                &formula,
                &context,
                Limits::default(),
                SearchHint::Unsat,
            );
            assert_eq!(out.verdict, Verdict::Unknown);
            assert!(!out.original_formula_validated);
        }
        let malformed = and(
            formula,
            ite(
                boolv(false),
                node(Sort::Bool, "unsupported", vec![]),
                boolv(true),
            ),
        );
        let out = super::super::solve_with_hint(
            &malformed,
            &context,
            Limits::default(),
            SearchHint::Unsat,
        );
        assert_eq!(out.verdict, Verdict::Unknown);
        assert!(!out.original_formula_validated);
    }

    #[test]
    fn subset_scoring_is_deterministic_for_shared_and_negated_guards() {
        let (mut formula, context) = many_controls(7, false);
        let repeated = eq(
            ite(not(var("control06".into(), Sort::Bool)), bv(8, 3), bv(8, 7)),
            bv(8, 7),
        );
        for _ in 0..8 {
            formula = and(formula, repeated.clone());
        }
        formula = and(
            formula,
            eq(
                ite(
                    not(var("control06".into(), Sort::Bool)),
                    bv(8, 5),
                    bv(8, 11),
                ),
                bv(8, 11),
            ),
        );
        let first =
            super::super::solve_with_hint(&formula, &context, Limits::default(), SearchHint::Unsat);
        let second =
            super::super::solve_with_hint(&formula, &context, Limits::default(), SearchHint::Unsat);
        assert_eq!(first.verdict, Verdict::Sat);
        assert!(first.original_formula_validated);
        // The mandatory control is propagated, so cannot consume a split slot.
        assert!(!first
            .stats
            .control_variables
            .contains(&"control06".to_string()));
        assert_eq!(
            first.stats.control_variables,
            second.stats.control_variables
        );
        assert_eq!(first.stats.work, second.stats.work);
        assert_eq!(first.stats.control_cases_closed, 7);
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
