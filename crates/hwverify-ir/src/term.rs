//! Pure finite-word/array expression representation and memory laws. No I/O.
use std::{
    collections::{hash_map::DefaultHasher, BTreeMap},
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
#[derive(Clone, Debug, PartialOrd, Ord)]
pub struct Term(pub Rc<Node>);
// Hash equality is only a fast rejection. Structural equality remains mandatory,
// so collisions cannot establish expression equality or a proof.
impl PartialEq for Term {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
            || (self.0.structural_hash == other.0.structural_hash
                && self.0.sort == other.0.sort
                && self.0.op == other.0.op
                && self.0.args == other.0.args)
    }
}
impl Eq for Term {}
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
}
pub fn node(sort: Sort, op: impl Into<String>, args: Vec<Term>) -> Term {
    let op = op.into();
    let mut hasher = DefaultHasher::new();
    sort.hash(&mut hasher);
    op.hash(&mut hasher);
    args.hash(&mut hasher);
    Term(Rc::new(Node {
        sort,
        op,
        args,
        structural_hash: hasher.finish(),
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
