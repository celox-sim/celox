//! Differential check against independent concrete semantics, using actual ir.rs.
#![allow(dead_code)]
#[path = "../../crates/lydite-ir/src/term.rs"]
mod ir;
use ir::*;
use std::collections::BTreeMap;
#[derive(Clone, Debug, PartialEq, Eq)]
enum Concrete { Word(u64), Bit(bool), Memory([u64; 4]) }
fn word(x: Concrete) -> u64 { if let Concrete::Word(w) = x { w } else { panic!("word expected") } }
fn mem(x: Concrete) -> [u64; 4] { if let Concrete::Memory(m) = x { m } else { panic!("memory expected") } }
fn eval(t: &Term, base: [u64; 4], addresses: [u64; 3]) -> Concrete {
    let n = &t.0;
    let e = |i: usize| eval(&n.args[i], base, addresses);
    match n.op.as_str() {
        "@m" => Concrete::Memory(base),
        "@a" => Concrete::Word(addresses[0]),
        "@b" => Concrete::Word(addresses[1]),
        "@q" => Concrete::Word(addresses[2]),
        "=" => Concrete::Bit(e(0) == e(1)),
        "ite" => if e(0) == Concrete::Bit(true) { e(1) } else { e(2) },
        "store" => { let mut m = mem(e(0)); m[word(e(1)) as usize] = word(e(2)); Concrete::Memory(m) },
        "select" => Concrete::Word(mem(e(0))[word(e(1)) as usize]),
        op if op.starts_with("(as const ") => Concrete::Memory([word(e(0)); 4]),
        op if op.starts_with("(_ bv") => Concrete::Word(op[5..].split_whitespace().next().unwrap().parse().unwrap()),
        op => panic!("unsupported test expression: {op}"),
    }
}
fn main() {
    let m = var("m".into(), Sort::Mem(2, 2));
    let a = var("a".into(), Sort::Bv(2));
    let b = var("b".into(), Sort::Bv(2));
    let q = var("q".into(), Sort::Bv(2));
    let mut rules = BTreeMap::new();
    let mut cases = 0u64;
    for v in 0..4 { for w in 0..4 {
        let first = node(Sort::Mem(2, 2), "store", vec![m.clone(), a.clone(), bv(2, v)]);
        for target in [&a, &b] {
            let raw = node(Sort::Mem(2, 2), "store", vec![first.clone(), target.clone(), bv(2, w)]);
            let normalized = memory_write(first.clone(), target.clone(), bv(2, w), &mut rules);
            let constant = node(Sort::Mem(2, 2), "(as const (Array (_ BitVec 2) (_ BitVec 2)))", vec![bv(2, w)]);
            let mut reads = Vec::new();
            for query in [&a, &b, &q] { for budget in 0..4 {
                reads.push((memory_read(raw.clone(), query.clone(), budget, &mut rules), query.clone()));
                let cr = memory_read(constant.clone(), query.clone(), budget, &mut rules);
                assert_eq!(eval(&cr, [0; 4], [0; 3]), Concrete::Word(w));
            }}
            for bits in 0..256u64 {
                let base = std::array::from_fn(|i| (bits >> (2*i)) & 3);
                for av in 0..4 { for bv in 0..4 { for qv in 0..4 {
                    let env = [av, bv, qv];
                    assert_eq!(eval(&normalized, base, env), eval(&raw, base, env), "write mismatch");
                    for (read, query) in &reads {
                        let expected = Concrete::Word(mem(eval(&raw, base, env))[word(eval(query, base, env)) as usize]);
                        assert_eq!(eval(read, base, env), expected, "read mismatch");
                        cases += 1;
                    }
                }}}
            }
        }
    }}
    for rule in ["read_after_same_write", "read_after_write_alias_split", "constant_memory_read", "last_write_wins"] {
        assert!(rules.get(rule).copied().unwrap_or(0) > 0, "unexercised rule {rule}");
    }
    println!("PASS: {cases} read comparisons, {} write comparisons; all four rules exercised", cases / 12);
}
