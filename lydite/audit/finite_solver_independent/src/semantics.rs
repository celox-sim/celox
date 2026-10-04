//! Audit-owned integer semantics. Deliberately does not use the finite backend evaluator.
use lydite_ir::{Sort, Term};
use std::collections::BTreeMap;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum V {
    B(bool),
    W(u64),
}
pub type Assignment = BTreeMap<String, V>;
pub fn word(v: V) -> u64 {
    if let V::W(x) = v {
        x
    } else {
        panic!("expected word")
    }
}
pub fn boolean(v: V) -> bool {
    if let V::B(x) = v {
        x
    } else {
        panic!("expected Boolean")
    }
}
pub fn mask(w: u32) -> u64 {
    assert!((1..=64).contains(&w));
    if w == 64 {
        u64::MAX
    } else {
        (1u64 << w) - 1
    }
}
pub fn signed(v: u64, w: u32) -> i128 {
    let v = v as i128;
    if v >= (1i128 << (w - 1)) {
        v - (1i128 << w)
    } else {
        v
    }
}
pub fn eval(t: &Term, a: &Assignment) -> V {
    let n = &t.0;
    if let Some(name) = n.op.strip_prefix('@') {
        return a
            .get(name)
            .unwrap_or_else(|| panic!("missing {name}"))
            .clone();
    }
    let es: Vec<_> = n.args.iter().map(|x| eval(x, a)).collect();
    let b = |i: usize| boolean(es[i].clone());
    let v = |i: usize| word(es[i].clone());
    let w = match n.sort {
        Sort::Bv(w) => w,
        _ => 0,
    };
    let input_w = || match n.args[0].0.sort {
        Sort::Bv(w) => w,
        _ => panic!("word input"),
    };
    match n.op.as_str() {
        "true" => V::B(true),
        "false" => V::B(false),
        "not" => V::B(!b(0)),
        "and" => V::B(es.iter().cloned().all(boolean)),
        "or" => V::B(es.iter().cloned().any(boolean)),
        "xor" => V::B(es.iter().cloned().map(boolean).fold(false, |x, y| x ^ y)),
        "=>" => V::B(!b(0) || b(1)),
        "=" => V::B(es.windows(2).all(|p| p[0] == p[1])),
        "distinct" => V::B((0..es.len()).all(|i| (i + 1..es.len()).all(|j| es[i] != es[j]))),
        "ite" => {
            if b(0) {
                es[1].clone()
            } else {
                es[2].clone()
            }
        }
        "bvnot" => V::W(!v(0) & mask(w)),
        "bvneg" => V::W(v(0).wrapping_neg() & mask(w)),
        "bvand" => V::W(v(0) & v(1)),
        "bvor" => V::W(v(0) | v(1)),
        "bvxor" => V::W(v(0) ^ v(1)),
        "bvnand" => V::W(!(v(0) & v(1)) & mask(w)),
        "bvnor" => V::W(!(v(0) | v(1)) & mask(w)),
        "bvxnor" => V::W(!(v(0) ^ v(1)) & mask(w)),
        "bvadd" => V::W(v(0).wrapping_add(v(1)) & mask(w)),
        "bvsub" => V::W(v(0).wrapping_sub(v(1)) & mask(w)),
        "bvmul" => V::W(v(0).wrapping_mul(v(1)) & mask(w)),
        "bvshl" => V::W(if v(1) >= w as u64 {
            0
        } else {
            (v(0) << v(1)) & mask(w)
        }),
        "bvlshr" => V::W(if v(1) >= w as u64 { 0 } else { v(0) >> v(1) }),
        "bvashr" => V::W(if v(1) >= w as u64 {
            if signed(v(0), w) < 0 {
                mask(w)
            } else {
                0
            }
        } else {
            ((signed(v(0), w) >> v(1)) as u64) & mask(w)
        }),
        "bvult" => V::B(v(0) < v(1)),
        "bvule" => V::B(v(0) <= v(1)),
        "bvugt" => V::B(v(0) > v(1)),
        "bvuge" => V::B(v(0) >= v(1)),
        "bvslt" => V::B(signed(v(0), input_w()) < signed(v(1), input_w())),
        "bvsle" => V::B(signed(v(0), input_w()) <= signed(v(1), input_w())),
        "bvsgt" => V::B(signed(v(0), input_w()) > signed(v(1), input_w())),
        "bvsge" => V::B(signed(v(0), input_w()) >= signed(v(1), input_w())),
        "concat" => {
            let Sort::Bv(lo) = n.args[1].0.sort else {
                panic!()
            };
            V::W(((v(0) as u128) << lo | v(1) as u128) as u64)
        }
        op if op.starts_with("(_ bv") => V::W(
            op[5..]
                .split_whitespace()
                .next()
                .unwrap()
                .parse::<u64>()
                .unwrap()
                & mask(w),
        ),
        op if op.starts_with("(_ extract ") => {
            let nums: Vec<u32> = op[11..]
                .trim_end_matches(')')
                .split_whitespace()
                .map(|x| x.parse().unwrap())
                .collect();
            V::W((v(0) >> nums[1]) & mask(nums[0] - nums[1] + 1))
        }
        op if op.starts_with("(_ zero_extend ") => V::W(v(0)),
        op if op.starts_with("(_ sign_extend ") => V::W((signed(v(0), input_w()) as u64) & mask(w)),
        op => panic!("audit evaluator unsupported {op}"),
    }
}
pub fn variables(t: &Term, out: &mut BTreeMap<String, Sort>) {
    if let Some(name) = t.0.op.strip_prefix('@') {
        if let Some(old) = out.insert(name.to_string(), t.0.sort.clone()) {
            assert_eq!(old, t.0.sort);
        }
    }
    for arg in &t.0.args {
        variables(arg, out)
    }
}
pub fn exhaustive(t: &Term) -> (bool, u64) {
    let mut vars = BTreeMap::new();
    variables(t, &mut vars);
    let mut assignments = vec![Assignment::new()];
    for (name, sort) in vars {
        let values: Vec<V> = match sort {
            Sort::Bool => vec![V::B(false), V::B(true)],
            Sort::Bv(w) if w <= 4 => (0..1u64 << w).map(V::W).collect(),
            _ => panic!("too wide to enumerate"),
        };
        assignments = assignments
            .into_iter()
            .flat_map(|a| {
                values
                    .iter()
                    .map(|v| {
                        let mut a = a.clone();
                        a.insert(name.clone(), v.clone());
                        a
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
    }
    let count = assignments.len() as u64;
    // Evaluate every input, including after finding a witness, to count full truth-table coverage.
    let truth = assignments.iter().filter(|a| boolean(eval(t, a))).count();
    (truth > 0, count)
}
