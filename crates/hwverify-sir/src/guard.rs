//! Bounded exact multi-terminal decision diagrams for lifted guards/word muxes.
//! Predicate atoms retain their original IR meaning; no relation between atoms
//! is assumed. Exhausting the optimization budget returns the original term.
use hwverify_ir::{self as ir, Sort, Term};
use std::collections::HashMap;
type Id = usize;
#[derive(Clone)]
enum Node {
    Leaf(Term),
    Branch { atom: usize, low: Id, high: Id },
}
pub(super) struct Normalizer {
    nodes: Vec<Node>,
    leaves: HashMap<Term, Id>,
    atoms: Vec<Term>,
    atom_ids: HashMap<Term, usize>,
    unique: HashMap<(usize, Id, Id), Id>,
    ites: HashMap<(Id, Id, Id), Id>,
    values: HashMap<Term, Id>,
    emitted: HashMap<Id, Term>,
    pub work: usize,
    pub aborted: bool,
}
impl Normalizer {
    pub fn new() -> Self {
        let f = ir::boolv(false);
        let t = ir::boolv(true);
        Self {
            nodes: vec![Node::Leaf(f.clone()), Node::Leaf(t.clone())],
            leaves: HashMap::from([(f, 0), (t, 1)]),
            atoms: vec![],
            atom_ids: HashMap::new(),
            unique: HashMap::new(),
            ites: HashMap::new(),
            values: HashMap::new(),
            emitted: HashMap::new(),
            work: 0,
            aborted: false,
        }
    }
    pub fn count(&self) -> usize {
        self.nodes.len()
    }
    fn tick(&mut self) -> Result<(), ()> {
        self.work += 1;
        if self.work > 1_000_000 || self.nodes.len() >= 50_000 || self.atoms.len() > 512 {
            Err(())
        } else {
            Ok(())
        }
    }
    fn branch(&mut self, atom: usize, low: Id, high: Id) -> Result<Id, ()> {
        self.tick()?;
        if low == high {
            return Ok(low);
        }
        if let Some(id) = self.unique.get(&(atom, low, high)) {
            return Ok(*id);
        }
        let id = self.nodes.len();
        self.nodes.push(Node::Branch { atom, low, high });
        self.unique.insert((atom, low, high), id);
        Ok(id)
    }
    fn top(&self, id: Id) -> usize {
        match self.nodes[id] {
            Node::Leaf(_) => usize::MAX,
            Node::Branch { atom, .. } => atom,
        }
    }
    fn cofactor(&self, id: Id, at: usize) -> (Id, Id) {
        match self.nodes[id] {
            Node::Branch { atom, low, high } if atom == at => (low, high),
            _ => (id, id),
        }
    }
    fn select(&mut self, g: Id, yes: Id, no: Id) -> Result<Id, ()> {
        self.tick()?;
        if g == 1 || yes == no {
            return Ok(yes);
        }
        if g == 0 {
            return Ok(no);
        }
        if let Some(id) = self.ites.get(&(g, yes, no)) {
            return Ok(*id);
        }
        let at = self.top(g).min(self.top(yes)).min(self.top(no));
        let (gl, gh) = self.cofactor(g, at);
        let (yl, yh) = self.cofactor(yes, at);
        let (nl, nh) = self.cofactor(no, at);
        let low = self.select(gl, yl, nl)?;
        let high = self.select(gh, yh, nh)?;
        let result = self.branch(at, low, high)?;
        self.ites.insert((g, yes, no), result);
        Ok(result)
    }
    fn atom(&mut self, term: &Term) -> Result<Id, ()> {
        let at = if let Some(id) = self.atom_ids.get(term) {
            *id
        } else {
            let id = self.atoms.len();
            self.atoms.push(term.clone());
            self.atom_ids.insert(term.clone(), id);
            id
        };
        self.branch(at, 0, 1)
    }
    fn value(&mut self, t: &Term) -> Result<Id, ()> {
        self.tick()?;
        if let Some(id) = self.values.get(t) {
            return Ok(*id);
        }
        let args = &t.0.args;
        let result = if t.0.sort == Sort::Bool {
            match t.0.op.as_str() {
                "false" => 0,
                "true" => 1,
                "not" => {
                    let a = self.value(&args[0])?;
                    self.select(a, 0, 1)?
                }
                "and" | "or" | "xor" | "=>" => {
                    let a = self.value(&args[0])?;
                    let b = self.value(&args[1])?;
                    match t.0.op.as_str() {
                        "and" => self.select(a, b, 0)?,
                        "or" => self.select(a, 1, b)?,
                        "=>" => self.select(a, b, 1)?,
                        _ => {
                            let nb = self.select(b, 0, 1)?;
                            self.select(a, nb, b)?
                        }
                    }
                }
                "ite" => {
                    let g = self.value(&args[0])?;
                    let a = self.value(&args[1])?;
                    let b = self.value(&args[2])?;
                    self.select(g, a, b)?
                }
                "=" if args[0].0.sort == Sort::Bool => {
                    let a = self.value(&args[0])?;
                    let b = self.value(&args[1])?;
                    let nb = self.select(b, 0, 1)?;
                    self.select(a, b, nb)?
                }
                _ => self.atom(t)?,
            }
        } else if t.0.op == "ite" {
            let g = self.value(&args[0])?;
            let a = self.value(&args[1])?;
            let b = self.value(&args[2])?;
            self.select(g, a, b)?
        } else if let Some(id) = self.leaves.get(t) {
            *id
        } else {
            let id = self.nodes.len();
            self.nodes.push(Node::Leaf(t.clone()));
            self.leaves.insert(t.clone(), id);
            id
        };
        self.values.insert(t.clone(), result);
        Ok(result)
    }
    fn emit(&mut self, id: Id) -> Term {
        if let Some(t) = self.emitted.get(&id) {
            return t.clone();
        }
        let result = match self.nodes[id].clone() {
            Node::Leaf(t) => t,
            Node::Branch { atom, low, high } => {
                let g = self.atoms[atom].clone();
                let no = self.emit(low);
                let yes = self.emit(high);
                if low == 0 && high == 1 {
                    g
                } else if low == 1 && high == 0 {
                    ir::not(g)
                } else if low == 0 {
                    ir::and(g, yes)
                } else if high == 0 {
                    ir::and(ir::not(g), no)
                } else if high == 1 {
                    ir::node(Sort::Bool, "or", vec![g, no])
                } else if low == 1 {
                    ir::node(Sort::Bool, "or", vec![ir::not(g), yes])
                } else if yes.0.op == "ite" && yes.0.args[2] == no {
                    ir::ite(ir::and(g, yes.0.args[0].clone()), yes.0.args[1].clone(), no)
                } else {
                    ir::ite(g, yes, no)
                }
            }
        };
        self.emitted.insert(id, result.clone());
        result
    }
    pub fn normalize(&mut self, t: &Term) -> Term {
        if self.aborted {
            return t.clone();
        }
        match self.value(t) {
            Ok(id) => self.emit(id),
            Err(()) => {
                self.aborted = true;
                t.clone()
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use hwverify_solver::finite::{solve, Limits, Verdict};
    #[test]
    fn word_mux_normalization_preserves_all_predicate_assignments() {
        let a = ir::var("a".into(), Sort::Bool);
        let b = ir::var("b".into(), Sort::Bool);
        let c = ir::var("c".into(), Sort::Bool);
        let x = ir::var("x".into(), Sort::Bv(8));
        let y = ir::var("y".into(), Sort::Bv(8));
        let guards = [
            ir::and(a.clone(), b.clone()),
            ir::not(ir::and(ir::not(a.clone()), ir::not(b.clone()))),
            ir::node(Sort::Bool, "xor", vec![a.clone(), c.clone()]),
            ir::ite(a.clone(), b.clone(), c.clone()),
        ];
        for g in guards {
            let term = ir::ite(
                g.clone(),
                ir::ite(a.clone(), x.clone(), y.clone()),
                ir::ite(b.clone(), y.clone(), x.clone()),
            );
            let normalized = Normalizer::new().normalize(&term);
            assert_eq!(
                solve(
                    &ir::not(ir::eq(term, normalized)),
                    &Default::default(),
                    Limits::default()
                )
                .verdict,
                Verdict::Unsat
            );
        }
    }
    #[test]
    fn irrelevant_control_drops_out_of_word_values() {
        let a = ir::var("a".into(), Sort::Bool);
        let b = ir::var("b".into(), Sort::Bool);
        let x = ir::var("x".into(), Sort::Bv(32));
        let y = ir::var("y".into(), Sort::Bv(32));
        let term = ir::ite(
            ir::and(a.clone(), b.clone()),
            x.clone(),
            ir::ite(ir::and(ir::not(a), b.clone()), x.clone(), y.clone()),
        );
        assert_eq!(Normalizer::new().normalize(&term), ir::ite(b, x, y));
    }
}

#[cfg(test)]
mod additional_tests {
    use super::*;
    use hwverify_solver::finite::{solve, Limits, Verdict};
    #[test]
    fn budget_exhaustion_preserves_the_original_term() {
        let original = ir::ite(ir::var("g".into(), Sort::Bool), ir::bv(8, 1), ir::bv(8, 2));
        let mut n = Normalizer::new();
        n.work = 1_000_000;
        assert_eq!(n.normalize(&original), original);
        assert!(n.aborted);
        assert_eq!(n.normalize(&ir::bv(8, 3)), ir::bv(8, 3));
    }
    #[test]
    fn varied_mux_trees_and_correlated_predicates_are_exact() {
        let x = ir::var("x".into(), Sort::Bv(8));
        let y = ir::var("y".into(), Sort::Bv(8));
        let atoms = [
            ir::var("a".into(), Sort::Bool),
            ir::var("b".into(), Sort::Bool),
            ir::var("c".into(), Sort::Bool),
            ir::eq(x.clone(), y.clone()),
        ];
        let leaves = [x, y, ir::bv(8, 0), ir::bv(8, 255)];
        let mut seed = 0x18e79ab3u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..64 {
            let mut term = leaves[(next() % 4) as usize].clone();
            for _ in 0..6 {
                let a = atoms[(next() % 4) as usize].clone();
                let b = atoms[(next() % 4) as usize].clone();
                let guard = match next() % 4 {
                    0 => ir::and(a, b),
                    1 => ir::node(Sort::Bool, "or", vec![a, b]),
                    2 => ir::node(Sort::Bool, "xor", vec![a, b]),
                    _ => ir::not(a),
                };
                let leaf = leaves[(next() % 4) as usize].clone();
                term = if next() & 1 == 0 {
                    ir::ite(guard, term, leaf)
                } else {
                    ir::ite(guard, leaf, term)
                };
            }
            let normalized = Normalizer::new().normalize(&term);
            assert_eq!(
                solve(
                    &ir::not(ir::eq(term, normalized)),
                    &Default::default(),
                    Limits::default()
                )
                .verdict,
                Verdict::Unsat
            );
        }
    }
}
