//! Pure finite-word/array expression representation and memory laws. No I/O.
use std::{
    cmp::Ordering,
    collections::{hash_map::DefaultHasher, BTreeMap, HashSet},
    hash::{Hash, Hasher},
    rc::Rc,
};
pub type Res<T> = Result<T, String>;
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Sort {
    Bool,
    Bv(u32),
    Mem(u32, u32),
}
impl Sort {
    pub fn smt(&self) -> String {
        match self {
            Self::Bool => "Bool".into(),
            Self::Bv(w) => format!("(_ BitVec {w})"),
            Self::Mem(a, w) => format!("(Array (_ BitVec {a}) (_ BitVec {w}))"),
        }
    }
}
#[derive(Clone, Debug)]
pub struct Term(pub Rc<Node>);
// Hash equality is only a fast rejection. Structural equality remains mandatory,
// so collisions cannot establish expression equality or a proof.
impl PartialEq for Term {
    fn eq(&self, other: &Self) -> bool {
        let mut pending = vec![(self, other)];
        let mut seen = HashSet::new();
        while let Some((a, b)) = pending.pop() {
            if Rc::ptr_eq(&a.0, &b.0) {
                continue;
            }
            if a.0.structural_hash != b.0.structural_hash
                || a.0.sort != b.0.sort
                || a.0.op != b.0.op
                || a.0.args.len() != b.0.args.len()
            {
                return false;
            }
            // Identity pairs only avoid revisiting already checked DAG nodes.
            // Hash equality never establishes structural equality.
            if seen.insert((Rc::as_ptr(&a.0), Rc::as_ptr(&b.0))) {
                pending.extend(a.0.args.iter().zip(&b.0.args));
            }
        }
        true
    }
}
impl Eq for Term {}
impl PartialOrd for Term {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Term {
    fn cmp(&self, other: &Self) -> Ordering {
        enum Visit<'a> {
            Pair(&'a Term, &'a Term),
            Tail(&'a Term, &'a Term),
        }
        let mut pending = vec![Visit::Pair(self, other)];
        let mut equal = HashSet::new();
        while let Some(visit) = pending.pop() {
            let ordering = match visit {
                Visit::Pair(a, b) => {
                    if Rc::ptr_eq(&a.0, &b.0)
                        || equal.contains(&(Rc::as_ptr(&a.0), Rc::as_ptr(&b.0)))
                    {
                        continue;
                    }
                    let head = a.0.sort.cmp(&b.0.sort).then_with(|| a.0.op.cmp(&b.0.op));
                    if head != Ordering::Equal {
                        return head;
                    }
                    // Preserve derived Node ordering exactly: sort, op, the
                    // lexicographic argument vector, then cached metadata.
                    pending.push(Visit::Tail(a, b));
                    pending.extend(
                        a.0.args
                            .iter()
                            .zip(&b.0.args)
                            .rev()
                            .map(|(a, b)| Visit::Pair(a, b)),
                    );
                    continue;
                }
                Visit::Tail(a, b) => {
                    let tail =
                        a.0.args
                            .len()
                            .cmp(&b.0.args.len())
                            .then_with(|| a.0.structural_hash.cmp(&b.0.structural_hash))
                            .then_with(|| a.0.contains_memory.cmp(&b.0.contains_memory));
                    if tail == Ordering::Equal {
                        equal.insert((Rc::as_ptr(&a.0), Rc::as_ptr(&b.0)));
                    }
                    tail
                }
            };
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        Ordering::Equal
    }
}
impl Hash for Term {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.0.structural_hash);
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Node {
    pub sort: Sort,
    pub op: String,
    pub args: Vec<Term>,
    // Nodes are immutable after construction; only node() constructs them.
    structural_hash: u64,
    contains_memory: bool,
}
impl Term {
    /// Cached structural property; never used to establish equality.
    pub fn contains_memory(&self) -> bool {
        self.0.contains_memory
    }
}
pub fn node(sort: Sort, op: impl Into<String>, args: Vec<Term>) -> Term {
    let op = op.into();
    let mut hasher = DefaultHasher::new();
    sort.hash(&mut hasher);
    op.hash(&mut hasher);
    args.hash(&mut hasher);
    let contains_memory = matches!(sort, Sort::Mem(_, _)) || args.iter().any(Term::contains_memory);
    Term(Rc::new(Node {
        sort,
        op,
        args,
        structural_hash: hasher.finish(),
        contains_memory,
    }))
}
pub fn boolv(v: bool) -> Term {
    node(Sort::Bool, v.to_string(), vec![])
}
pub fn bv(w: u32, v: u64) -> Term {
    node(
        Sort::Bv(w),
        format!(
            "(_ bv{} {})",
            if w < 64 { v & ((1u64 << w) - 1) } else { v },
            w
        ),
        vec![],
    )
}
pub fn var(n: String, s: Sort) -> Term {
    node(s, format!("@{n}"), vec![])
}
pub fn eq(a: Term, b: Term) -> Term {
    node(Sort::Bool, "=", vec![a, b])
}
pub fn not(a: Term) -> Term {
    node(Sort::Bool, "not", vec![a])
}
pub fn and(a: Term, b: Term) -> Term {
    node(Sort::Bool, "and", vec![a, b])
}
pub fn ite(g: Term, a: Term, b: Term) -> Term {
    node(a.0.sort.clone(), "ite", vec![g, a, b])
}

pub fn memory_read(m: Term, a: Term, budget: u32, rules: &mut BTreeMap<String, u64>) -> Term {
    if m.0.op == "store" && budget > 0 {
        let v = &m.0.args;
        if v[1] == a {
            *rules.entry("read_after_same_write".into()).or_default() += 1;
            return v[2].clone();
        }
        *rules
            .entry("read_after_write_alias_split".into())
            .or_default() += 1;
        return ite(
            eq(a.clone(), v[1].clone()),
            v[2].clone(),
            memory_read(v[0].clone(), a, budget - 1, rules),
        );
    }
    if m.0.op.starts_with("(as const ") {
        *rules.entry("constant_memory_read".into()).or_default() += 1;
        return m.0.args[0].clone();
    }
    let w = match m.0.sort {
        Sort::Mem(_, w) => w,
        _ => unreachable!(),
    };
    node(Sort::Bv(w), "select", vec![m, a])
}

/// Identical-address overwrite elimination; never assumes non-aliasing.
pub fn memory_write(mut m: Term, a: Term, v: Term, rules: &mut BTreeMap<String, u64>) -> Term {
    if m.0.op == "store" && m.0.args[1] == a {
        *rules.entry("last_write_wins".into()).or_default() += 1;
        m = m.0.args[0].clone();
    }
    node(m.0.sort.clone(), "store", vec![m, a, v])
}

#[cfg(test)]
mod hash_collision_tests {
    use super::*;
    use std::collections::HashSet;
    #[test]
    fn equal_cached_hash_does_not_establish_term_equality() {
        // Deliberately force a collision. Equality still checks structure.
        let term = |op: &str| {
            Term(Rc::new(Node {
                sort: Sort::Bv(4),
                op: op.into(),
                args: vec![],
                structural_hash: 7,
                contains_memory: false,
            }))
        };
        let a = term("(_ bv0 4)");
        let b = term("(_ bv1 4)");
        assert_ne!(a, b);
        let mut set = HashSet::new();
        set.insert(a.clone());
        set.insert(b.clone());
        assert_eq!(set.len(), 2);
        assert_ne!(eq(a.clone(), a), eq(b.clone(), b));
    }
}

#[cfg(test)]
mod dag_comparison_tests {
    use super::*;

    fn shared(depth: usize, leaf: &str) -> Term {
        let mut t = var(leaf.into(), Sort::Bool);
        for _ in 0..depth {
            t = and(t.clone(), t);
        }
        t
    }
    fn old_cmp(a: &Term, b: &Term) -> Ordering {
        let mut order = a.0.sort.cmp(&b.0.sort).then_with(|| a.0.op.cmp(&b.0.op));
        for (x, y) in a.0.args.iter().zip(&b.0.args) {
            if order != Ordering::Equal {
                return order;
            }
            order = old_cmp(x, y);
        }
        order
            .then_with(|| a.0.args.len().cmp(&b.0.args.len()))
            .then_with(|| a.0.structural_hash.cmp(&b.0.structural_hash))
            .then_with(|| a.0.contains_memory.cmp(&b.0.contains_memory))
    }
    #[test]
    fn separately_allocated_shared_dags_compare_exactly() {
        let a = shared(96, "a");
        let b = shared(96, "a");
        let c = shared(96, "b");
        assert!(a == b);
        assert_eq!(a.cmp(&b), Ordering::Equal);
        assert!(a != c);
        assert_eq!(a.cmp(&c), Ordering::Less);
        assert_eq!(c.cmp(&a), Ordering::Greater);
    }
    #[test]
    fn ordering_matches_original_lexicographic_definition() {
        let mut terms = vec![
            boolv(false),
            boolv(true),
            bv(1, 0),
            bv(32, 7),
            shared(5, "a"),
            shared(5, "b"),
        ];
        for arity in 0..4 {
            terms.push(node(Sort::Bool, "synthetic", terms[..arity].to_vec()));
        }
        for a in &terms {
            for b in &terms {
                assert_eq!(a.cmp(b), old_cmp(a, b));
                assert_eq!(a.cmp(b), b.cmp(a).reverse());
                assert_eq!(a == b, a.cmp(b) == Ordering::Equal);
                for c in &terms {
                    if a <= b && b <= c {
                        assert!(a <= c);
                    }
                }
            }
        }
    }
    #[test]
    fn equal_hash_unequal_descendant_cannot_be_skipped() {
        let collision = |op: &str, args: Vec<Term>| {
            Term(Rc::new(Node {
                sort: Sort::Bool,
                op: op.into(),
                args,
                structural_hash: 0,
                contains_memory: false,
            }))
        };
        let a = collision("a", vec![]);
        let b = collision("b", vec![]);
        let x = collision("f", vec![a.clone(), a]);
        let y = collision("f", vec![b.clone(), b]);
        let lhs = collision("g", vec![x.clone(), x]);
        let rhs = collision("g", vec![y.clone(), y]);
        assert!(lhs != rhs);
        assert_eq!(lhs.cmp(&rhs), old_cmp(&lhs, &rhs));
        // Tail metadata remains in its historical position in Ord.
        let mut different_metadata = (*lhs.0).clone();
        different_metadata.contains_memory = true;
        let different_metadata = Term(Rc::new(different_metadata));
        assert_eq!(
            lhs.cmp(&different_metadata),
            old_cmp(&lhs, &different_metadata)
        );
    }
}
