//! Exact finite-observation reduction for total arrays with reads and stores.
//!
//! TRUSTED MODEL-EXTENSION RULE: observations of each total array at finitely
//! many BV indices extend to a total array iff equal indices have equal values.
//! Array equality is enforced on the COMPLETE typed index pool, including one
//! witness for each false equality and EVERY store index. Stores reduce by exact
//! read-over-write; outside this pool no store changes any cell. A common zero
//! default then extends every positive equality and preserves every negative equality. This is not a
//! certificate; both reduction and independent original-array replay are trusted.
use super::*;
use crate::{and, boolv, eq, ite, node, not, var};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArrayModel {
    pub address_width: u32,
    pub value_width: u32,
    /// Nonzero entries only. Every other address has value zero.
    pub entries: BTreeMap<u64, u64>,
}
impl ArrayModel {
    pub fn smt(&self) -> String {
        let mut s = format!(
            "((as const {}) (_ bv0 {}))",
            Sort::Mem(self.address_width, self.value_width).smt(),
            self.value_width
        );
        for (a, v) in &self.entries {
            s = format!(
                "(store {s} (_ bv{a} {}) (_ bv{v} {}))",
                self.address_width, self.value_width
            );
        }
        s
    }
    pub fn json(&self) -> Value {
        json!({"sort":"Mem","address_width":self.address_width,"value_width":self.value_width,"default":0,"entries":self.entries})
    }
    fn read(&self, a: u64) -> u64 {
        self.entries.get(&a).copied().unwrap_or(0)
    }
}
fn array_equality(t: &Term) -> bool {
    t.0.op == "="
        && t.0
            .args
            .first()
            .is_some_and(|a| matches!(a.0.sort, Sort::Mem(_, _)))
}
pub(super) fn validate(t: &Term) -> Res<()> {
    let a = &t.0.args;
    if let Sort::Mem(aw, vw) = t.0.sort {
        if !(1..=64).contains(&aw) || !(1..=64).contains(&vw) {
            return Err("unsupported readonly array width".into());
        }
        if t.0.op.strip_prefix('@').is_some_and(|n| !n.is_empty()) && a.is_empty() {
            return Ok(());
        }
        if t.0.op == "ite"
            && a.len() == 3
            && a[0].0.sort == Sort::Bool
            && a[1].0.sort == t.0.sort
            && a[2].0.sort == t.0.sort
        {
            return Ok(());
        }
        if t.0.op == "store"
            && a.len() == 3
            && a[0].0.sort == t.0.sort
            && a[1].0.sort == Sort::Bv(aw)
            && a[2].0.sort == Sort::Bv(vw)
        {
            return Ok(());
        }
        return Err(
            "unsupported or malformed array operation (only variables, ite and store)".into(),
        );
    }
    if t.0.op == "select" {
        if a.len() == 2 {
            if let Sort::Mem(aw, vw) = a[0].0.sort {
                if a[1].0.sort == Sort::Bv(aw) && t.0.sort == Sort::Bv(vw) {
                    return Ok(());
                }
            }
        }
        return Err("malformed readonly select".into());
    }
    if array_equality(t) {
        if a.len() == 2 && a[0].0.sort == a[1].0.sort && t.0.sort == Sort::Bool {
            return Ok(());
        }
        return Err("malformed readonly array equality".into());
    }
    operation(t).map(|_| ())
}
struct Reduction {
    names: HashSet<String>,
    next: usize,
    created: usize,
    source: usize,
    scalars: HashMap<Term, Term>,
    reads: HashMap<(Term, Term), Term>,
    equality: HashMap<Term, (Term, Term)>,
    arrays: Vec<Term>,
    aliases: Aliases,
    canonical_arrays: HashMap<Term, Term>,
    indices: BTreeMap<Sort, Vec<Term>>,
    forced: Env,
}
impl Reduction {
    fn charge(&mut self, b: &mut Budget) -> Res<()> {
        b.tick(1)?;
        self.created += 1;
        if self.source.saturating_add(self.created) > b.limits.max_terms {
            return Err("readonly array expansion term budget exhausted".into());
        }
        Ok(())
    }
    fn fresh(&mut self, sort: Sort, b: &mut Budget) -> Res<Term> {
        loop {
            self.charge(b)?;
            let name = format!("__lydite_readonly_{}", self.next);
            self.next += 1;
            if self.names.insert(name.clone()) {
                let t = var(name.clone(), sort);
                self.forced.insert(name, t.clone());
                return Ok(t);
            }
        }
    }
    fn canonical_array(&mut self, t: &Term, b: &mut Budget, depth: usize) -> Res<Term> {
        b.tick(1)?;
        if depth > b.limits.max_depth {
            return Err("readonly canonicalization depth budget exhausted".into());
        }
        if let Some(v) = self.canonical_arrays.get(t) {
            return Ok(v.clone());
        }
        let v = if let Some(name) = t.0.op.strip_prefix('@') {
            self.charge(b)?;
            var(self.aliases.representative(name, b)?, t.0.sort.clone())
        } else if t.0.op == "store" {
            let base = self.canonical_array(&t.0.args[0], b, depth + 1)?;
            self.charge(b)?;
            node(
                t.0.sort.clone(),
                "store",
                vec![base, t.0.args[1].clone(), t.0.args[2].clone()],
            )
        } else {
            let x = self.canonical_array(&t.0.args[1], b, depth + 1)?;
            let y = self.canonical_array(&t.0.args[2], b, depth + 1)?;
            if x == y {
                x
            } else {
                self.charge(b)?;
                ite(t.0.args[0].clone(), x, y)
            }
        };
        self.canonical_arrays.insert(t.clone(), v.clone());
        Ok(v)
    }
    fn scalar(&mut self, t: &Term, b: &mut Budget, depth: usize) -> Res<Term> {
        b.tick(1)?;
        if depth > b.limits.max_depth {
            return Err("readonly reduction depth budget exhausted".into());
        }
        if let Some(x) = self.scalars.get(t) {
            return Ok(x.clone());
        }
        let out = if t.0.op == "select" {
            self.read(&t.0.args[0], &t.0.args[1], b, depth + 1)?
        } else if array_equality(t) {
            self.equality[t].0.clone()
        } else {
            let args =
                t.0.args
                    .iter()
                    .map(|x| self.scalar(x, b, depth + 1))
                    .collect::<Res<Vec<_>>>()?;
            self.charge(b)?;
            node(t.0.sort.clone(), t.0.op.clone(), args)
        };
        self.scalars.insert(t.clone(), out.clone());
        Ok(out)
    }
    fn read(&mut self, array: &Term, index: &Term, b: &mut Budget, depth: usize) -> Res<Term> {
        b.tick(1)?;
        if depth > b.limits.max_depth {
            return Err("readonly reduction depth budget exhausted".into());
        }
        let key = (array.clone(), index.clone());
        if let Some(t) = self.reads.get(&key) {
            return Ok(t.clone());
        }
        let Sort::Mem(_, vw) = array.0.sort else {
            return Err("readonly read expected array".into());
        };
        let canonical = self.canonical_array(array, b, depth + 1)?;
        let t = if canonical != *array {
            self.read(&canonical, index, b, depth + 1)?
        } else if array.0.op == "ite" {
            let c = self.scalar(&array.0.args[0], b, depth + 1)?;
            let x = self.read(&array.0.args[1], index, b, depth + 1)?;
            let y = self.read(&array.0.args[2], index, b, depth + 1)?;
            self.charge(b)?;
            ite(c, x, y)
        } else if array.0.op == "store" {
            let written = self.scalar(&array.0.args[1], b, depth + 1)?;
            let queried = self.scalar(index, b, depth + 1)?;
            let value = self.scalar(&array.0.args[2], b, depth + 1)?;
            let prior = self.read(&array.0.args[0], index, b, depth + 1)?;
            self.charge(b)?;
            self.charge(b)?;
            ite(eq(written, queried), value, prior)
        } else {
            self.fresh(Sort::Bv(vw), b)?
        };
        self.reads.insert(key, t.clone());
        Ok(t)
    }
    fn constraint(&mut self, conditions: &mut Vec<Term>, t: Term, b: &mut Budget) -> Res<()> {
        // Each constraint has at most four new operator nodes.
        for _ in 0..4 {
            self.charge(b)?;
        }
        conditions.push(t);
        Ok(())
    }
}
fn conjunction(mut terms: Vec<Term>, r: &mut Reduction, b: &mut Budget) -> Res<Term> {
    if terms.is_empty() {
        return Ok(boolv(true));
    }
    while terms.len() > 1 {
        let mut next = Vec::new();
        for pair in terms.chunks(2) {
            if pair.len() == 2 {
                r.charge(b)?;
                next.push(and(pair[0].clone(), pair[1].clone()));
            } else {
                next.push(pair[0].clone());
            }
        }
        terms = next;
    }
    Ok(terms.pop().unwrap())
}
#[derive(Clone)]
enum ValueModel {
    Scalar(Scalar),
    Array(ArrayModel),
}
fn replay(
    t: &Term,
    inputs: &BTreeMap<String, Scalar>,
    arrays: &BTreeMap<String, ArrayModel>,
    memo: &mut HashMap<Term, ValueModel>,
    scalar_memo: &mut HashMap<Term, Scalar>,
    b: &mut Budget,
    depth: usize,
) -> Res<ValueModel> {
    b.tick(1)?;
    if depth > b.limits.max_depth {
        return Err("readonly witness depth budget exhausted".into());
    }
    if let Some(v) = memo.get(t) {
        if let ValueModel::Array(a) = v {
            b.tick(a.entries.len() as u64)?;
        }
        return Ok(v.clone());
    }
    let args =
        t.0.args
            .iter()
            .map(|x| replay(x, inputs, arrays, memo, scalar_memo, b, depth + 1))
            .collect::<Res<Vec<_>>>()?;
    let v = if matches!(t.0.sort, Sort::Mem(_, _)) {
        if let Some(n) = t.0.op.strip_prefix('@') {
            {
                let a = arrays.get(n).ok_or("missing original array witness")?;
                b.tick(a.entries.len() as u64)?;
                ValueModel::Array(a.clone())
            }
        } else if t.0.op == "store" {
            let (
                ValueModel::Array(base),
                ValueModel::Scalar(Scalar::Bv { value: index, .. }),
                ValueModel::Scalar(Scalar::Bv { value, .. }),
            ) = (&args[0], &args[1], &args[2])
            else {
                return Err("invalid store witness".into());
            };
            b.tick(base.entries.len() as u64 + 1)?;
            let mut updated = base.clone();
            if *value == 0 {
                updated.entries.remove(index);
            } else {
                updated.entries.insert(*index, *value);
            }
            ValueModel::Array(updated)
        } else {
            let ValueModel::Scalar(Scalar::Bool(c)) = args[0] else {
                return Err("invalid array ite witness".into());
            };
            let selected = &args[if c { 1 } else { 2 }];
            if let ValueModel::Array(a) = selected {
                b.tick(a.entries.len() as u64)?;
            }
            selected.clone()
        }
    } else if t.0.op == "select" {
        let (ValueModel::Array(a), ValueModel::Scalar(Scalar::Bv { value, .. })) =
            (&args[0], &args[1])
        else {
            return Err("invalid read witness".into());
        };
        ValueModel::Scalar(Scalar::Bv {
            width: a.value_width,
            value: a.read(*value),
        })
    } else if array_equality(t) {
        let (ValueModel::Array(a), ValueModel::Array(c)) = (&args[0], &args[1]) else {
            return Err("invalid equality witness".into());
        };
        b.tick((a.entries.len() + c.entries.len()) as u64)?;
        ValueModel::Scalar(Scalar::Bool(a == c))
    } else {
        ValueModel::Scalar(evaluate(t, inputs, scalar_memo, b, depth)?)
    };
    if let ValueModel::Scalar(s) = &v {
        scalar_memo.insert(t.clone(), s.clone());
    }
    if let ValueModel::Array(a) = &v {
        b.tick(a.entries.len() as u64)?;
    }
    memo.insert(t.clone(), v.clone());
    Ok(v)
}
pub(super) fn solve(formula: &Term, context: &Env, limits: Limits, hint: SearchHint) -> Outcome {
    let mut out = control::empty(hint);
    let mut b = Budget {
        limits,
        start: Instant::now(),
        work: 0,
        time_check_in: 0,
    };
    let result = (|| -> Res<()> {
        if formula.0.sort != Sort::Bool {
            return Err("finite formula must have Bool sort".into());
        }
        let mut r = Reduction {
            names: HashSet::new(),
            next: 0,
            created: 0,
            source: 0,
            scalars: HashMap::new(),
            reads: HashMap::new(),
            equality: HashMap::new(),
            arrays: Vec::new(),
            aliases: Aliases::default(),
            canonical_arrays: HashMap::new(),
            indices: BTreeMap::new(),
            forced: Env::new(),
        };
        let mut todo = vec![(formula, 0)];
        todo.extend(context.values().map(|t| (t, 0)));
        let mut seen = HashSet::new();
        let mut vars = BTreeMap::new();
        let mut equalities = Vec::new();
        while let Some((t, depth)) = todo.pop() {
            b.tick(1)?;
            if depth > b.limits.max_depth {
                return Err("readonly source depth budget exhausted".into());
            }
            if !seen.insert(t.clone()) {
                continue;
            }
            if seen.len() > b.limits.max_terms {
                return Err("readonly source term budget exhausted".into());
            }
            validate(t)?;
            if let Some(name) = t.0.op.strip_prefix('@') {
                if vars
                    .insert(name.to_string(), t.0.sort.clone())
                    .is_some_and(|s| s != t.0.sort)
                {
                    return Err("readonly variable has inconsistent sorts".into());
                }
                r.names.insert(name.into());
                if matches!(t.0.sort, Sort::Mem(_, _)) {
                    r.arrays.push(t.clone());
                } else {
                    r.forced.insert(name.into(), t.clone());
                }
            }
            // Store indices are required even if no read observes the store:
            // positive extensional equalities must hold at all changed cells.
            if t.0.op == "select" || t.0.op == "store" {
                r.indices
                    .entry(t.0.args[0].0.sort.clone())
                    .or_default()
                    .push(t.0.args[1].clone());
            }
            if array_equality(t) {
                equalities.push(t.clone());
            }
            todo.extend(t.0.args.iter().map(|x| (x, depth + 1)));
        }
        r.source = seen.len();
        // Only mandatory positive top-level variable-array equalities justify
        // sharing an array. Never mine facts under not/or/implication/context.
        let mut asserted = vec![formula];
        let mut visited = HashSet::new();
        while let Some(t) = asserted.pop() {
            b.tick(1)?;
            if !visited.insert(t.clone()) {
                continue;
            }
            if t.0.op == "and" {
                asserted.extend(&t.0.args);
            } else if array_equality(t) {
                if let (Some(x), Some(y)) = (
                    t.0.args[0].0.op.strip_prefix('@'),
                    t.0.args[1].0.op.strip_prefix('@'),
                ) {
                    r.aliases.join(x.into(), y.into(), &mut b)?;
                }
            }
        }
        let mut active_equalities = Vec::new();
        for t in &equalities {
            let x = r.canonical_array(&t.0.args[0], &mut b, 0)?;
            let y = r.canonical_array(&t.0.args[1], &mut b, 0)?;
            if x == y {
                r.charge(&mut b)?;
                r.charge(&mut b)?;
                r.equality.insert(t.clone(), (boolv(true), crate::bv(1, 0)));
                continue;
            }
            active_equalities.push(t.clone());
            let Sort::Mem(aw, _) = t.0.args[0].0.sort else {
                unreachable!()
            };
            let e = r.fresh(Sort::Bool, &mut b)?;
            let k = r.fresh(Sort::Bv(aw), &mut b)?;
            r.indices
                .entry(t.0.args[0].0.sort.clone())
                .or_default()
                .push(k.clone());
            r.equality.insert(t.clone(), (e, k));
        }
        for indices in r.indices.values_mut() {
            b.tick(indices.len() as u64)?;
            let mut distinct = HashSet::new();
            indices.retain(|t| distinct.insert(t.clone()));
        }
        let mut conditions = vec![r.scalar(formula, &mut b, 0)?];
        for t in context.values() {
            if !matches!(t.0.sort, Sort::Mem(_, _)) {
                let q = r.scalar(t, &mut b, 0)?;
                let mut name = format!("context_{}", r.forced.len());
                while r.forced.contains_key(&name) {
                    name.push('_');
                }
                r.forced.insert(name, q);
            }
        }
        // Materialize EVERY base array at EVERY compatible index, including
        // witnesses belonging to other equalities (essential for transitivity).
        for array in r.arrays.clone() {
            let indices = r.indices.get(&array.0.sort).cloned().unwrap_or_default();
            for index in &indices {
                r.read(&array, index, &mut b, 0)?;
                r.scalar(index, &mut b, 0)?;
            }
            if r.canonical_array(&array, &mut b, 0)? != array {
                continue;
            }
            for i in 0..indices.len() {
                for j in 0..i {
                    let x = r.scalar(&indices[i], &mut b, 0)?;
                    let y = r.scalar(&indices[j], &mut b, 0)?;
                    let a = r.read(&array, &indices[i], &mut b, 0)?;
                    let c = r.read(&array, &indices[j], &mut b, 0)?;
                    r.constraint(
                        &mut conditions,
                        node(Sort::Bool, "=>", vec![eq(x, y), eq(a, c)]),
                        &mut b,
                    )?;
                }
            }
        }
        for t in &active_equalities {
            let (e, k) = r.equality[t].clone();
            let indices = r.indices[&t.0.args[0].0.sort].clone();
            for index in indices {
                let a = r.read(&t.0.args[0], &index, &mut b, 0)?;
                let c = r.read(&t.0.args[1], &index, &mut b, 0)?;
                r.constraint(
                    &mut conditions,
                    node(Sort::Bool, "=>", vec![e.clone(), eq(a, c)]),
                    &mut b,
                )?;
            }
            let a = r.read(&t.0.args[0], &k, &mut b, 0)?;
            let c = r.read(&t.0.args[1], &k, &mut b, 0)?;
            r.constraint(
                &mut conditions,
                node(Sort::Bool, "=>", vec![not(e), not(eq(a, c))]),
                &mut b,
            )?;
        }
        let reduced = conjunction(conditions, &mut r, &mut b)?;
        let reduced = strengthen(&reduced, &mut b)?;
        b.check_time()?;
        let mut remaining = b.limits.clone();
        remaining.max_work = remaining.max_work.saturating_sub(b.work);
        remaining.timeout_ms = remaining
            .timeout_ms
            .saturating_sub(b.start.elapsed().as_millis() as u64);
        out = if hint == SearchHint::Unsat {
            control::route(&reduced, &r.forced, remaining, hint, true)
        } else {
            solve_plain(&reduced, &r.forced, remaining, hint)
        };
        b.tick(out.stats.work)?;
        // The scalar solver checked the reduction only; never report this as
        // an original-array witness until independent model extension/replay.
        out.original_formula_validated = false;
        if out.verdict != Verdict::Sat {
            return Ok(());
        }
        let mut arrays = BTreeMap::new();
        let mut scalar_memo = HashMap::new();
        for array in &r.arrays {
            let Sort::Mem(aw, vw) = array.0.sort else {
                unreachable!()
            };
            let mut model = ArrayModel {
                address_width: aw,
                value_width: vw,
                entries: BTreeMap::new(),
            };
            let mut observed = BTreeMap::new();
            for index in r.indices.get(&array.0.sort).into_iter().flatten() {
                let address = evaluate(
                    &r.scalars[index],
                    &out.assignments,
                    &mut scalar_memo,
                    &mut b,
                    0,
                )?
                .word();
                let value = evaluate(
                    &r.reads[&(array.clone(), index.clone())],
                    &out.assignments,
                    &mut scalar_memo,
                    &mut b,
                    0,
                )?
                .word();
                if observed
                    .insert(address, value)
                    .is_some_and(|old| old != value)
                {
                    return Err("readonly observations have conflicting values".into());
                }
                if value != 0 {
                    model.entries.insert(address, value);
                }
            }
            arrays.insert(array.0.op[1..].to_string(), model);
        }
        let mut memo = HashMap::new();
        let mut original_scalar_memo = HashMap::new();
        if !matches!(
            replay(
                formula,
                &out.assignments,
                &arrays,
                &mut memo,
                &mut original_scalar_memo,
                &mut b,
                0
            )?,
            ValueModel::Scalar(Scalar::Bool(true))
        ) {
            return Err("finite SAT assignment failed original-array validation".into());
        }
        out.context_values.clear();
        for (name, t) in context {
            match replay(
                t,
                &out.assignments,
                &arrays,
                &mut memo,
                &mut original_scalar_memo,
                &mut b,
                0,
            )? {
                ValueModel::Scalar(v) => {
                    out.context_values.insert(name.clone(), v);
                }
                ValueModel::Array(v) => {
                    out.array_context_values.insert(name.clone(), v);
                }
            }
        }
        b.tick(out.assignments.len() as u64)?;
        out.assignments.retain(|name, _| {
            vars.get(name)
                .is_some_and(|s| !matches!(s, Sort::Mem(_, _)))
        });
        out.array_assignments = arrays;
        b.check_time()?;
        out.original_formula_validated = true;
        Ok(())
    })();
    if let Err(reason) = result {
        out.verdict = Verdict::Unknown;
        out.reason = Some(reason);
        out.original_formula_validated = false;
        out.assignments.clear();
        out.context_values.clear();
        out.array_assignments.clear();
        out.array_context_values.clear();
    }
    out.stats.work = b.work;
    out
}

/// Promote only implications whose antecedent follows by exact substitution of
/// mandatory scalar equalities. This exposes Ackermann congruence to the normal
/// alias/definition encoder after control cofactoring; it is not an assumption.
pub(super) fn strengthen(formula: &Term, b: &mut Budget) -> Res<Term> {
    fn canonical(
        t: &Term,
        aliases: &Aliases,
        memo: &mut HashMap<Term, Term>,
        active: &mut HashSet<String>,
        rewrite: &mut lookup::Rewrite,
        b: &mut Budget,
        depth: usize,
    ) -> Res<Term> {
        b.tick(1)?;
        if depth > b.limits.max_depth {
            return Err("readonly congruence normalization depth exhausted".into());
        }
        if let Some(v) = memo.get(t) {
            return Ok(v.clone());
        }
        if memo.len() > b.limits.max_terms {
            return Err("readonly congruence normalization terms exhausted".into());
        }
        let result = if let Op::Variable(name) = operation(t)? {
            let rep = aliases.representative(&name, b)?;
            let base = var(rep.clone(), t.0.sort.clone());
            if active.insert(rep.clone()) {
                let result = if let Some(rhs) = aliases.definitions.get(&rep) {
                    canonical(rhs, aliases, memo, active, rewrite, b, depth + 1)?
                } else {
                    base
                };
                active.remove(&rep);
                result
            } else {
                base
            }
        } else {
            let args =
                t.0.args
                    .iter()
                    .map(|x| canonical(x, aliases, memo, active, rewrite, b, depth + 1))
                    .collect::<Res<Vec<_>>>()?;
            let term = node(t.0.sort.clone(), t.0.op.clone(), args);
            rewrite.normalize(&term, b, depth)?
        };
        memo.insert(t.clone(), result.clone());
        Ok(result)
    }
    let mut out = formula.clone();
    let mut promoted = HashSet::new();
    // Bounded optimization only: unpromoted implications remain in the formula.
    for _ in 0..16 {
        let mut aliases = Aliases::default();
        aliases.collect(&out, b)?;
        let mut todo = vec![formula];
        let mut seen = HashSet::new();
        let mut added = Vec::new();
        let mut memo = HashMap::new();
        let mut rewrite = lookup::Rewrite::default();
        while let Some(t) = todo.pop() {
            b.tick(1)?;
            if !seen.insert(t.clone()) {
                continue;
            }
            if seen.len() > b.limits.max_terms {
                return Err("readonly congruence scan terms exhausted".into());
            }
            if t.0.op == "and" {
                todo.extend(&t.0.args);
                continue;
            }
            if t.0.op != "=>" || t.0.args[0].0.op != "=" || t.0.args[1].0.op != "=" {
                continue;
            }
            let premise = &t.0.args[0].0.args;
            let conclusion = &t.0.args[1];
            if !conclusion.0.args.iter().all(|x| x.0.op.starts_with('@'))
                || promoted.contains(conclusion)
            {
                continue;
            }
            let x = canonical(
                &premise[0],
                &aliases,
                &mut memo,
                &mut HashSet::new(),
                &mut rewrite,
                b,
                0,
            )?;
            let y = canonical(
                &premise[1],
                &aliases,
                &mut memo,
                &mut HashSet::new(),
                &mut rewrite,
                b,
                0,
            )?;
            if x == y {
                promoted.insert(conclusion.clone());
                added.push(conclusion.clone());
            }
        }
        if added.is_empty() {
            break;
        }
        for equality in added {
            b.tick(1)?;
            out = and(out, equality);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
