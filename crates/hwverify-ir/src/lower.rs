//! Shared expression type checking and environment utilities.
//! Complete machine and contract validation is owned by Design.
use crate::*;
use serde_json::Value;
use std::collections::BTreeMap;
pub fn keys(v: &Value, req: &[&str], opt: &[&str]) -> Res<()> {
    let o = v.as_object().ok_or("expected object")?;
    for k in req {
        if !o.contains_key(*k) {
            return Err(format!("missing field {k}"));
        }
    }
    for k in o.keys() {
        if !req.contains(&k.as_str()) && !opt.contains(&k.as_str()) {
            return Err(format!("unsupported field {k}"));
        }
    }
    Ok(())
}
pub fn text(v: &Value) -> Res<&str> {
    v.as_str().ok_or("expected string".into())
}
pub fn width(v: &Value) -> Res<u32> {
    let w = v.as_u64().ok_or("expected positive width")?;
    if w == 0 || w > 64 {
        return Err("width must be 1..64".into());
    }
    Ok(w as u32)
}
pub fn ty(v: &Value) -> Res<Sort> {
    if v == "bool" {
        return Ok(Sort::Bool);
    }
    if let Some(w) = v.get("bv") {
        keys(v, &["bv"], &[])?;
        return Ok(Sort::Bv(width(w)?));
    }
    if let Some(m) = v.get("mem") {
        keys(v, &["mem"], &[])?;
        let a = m.as_array().ok_or("mem requires array")?;
        if a.len() != 2 {
            return Err("mem requires two widths".into());
        }
        return Ok(Sort::Mem(width(&a[0])?, width(&a[1])?));
    }
    Err("unsupported type".into())
}
pub fn named(v: &Value) -> Res<&serde_json::Map<String, Value>> {
    let o = v.as_object().ok_or("expected named record")?;
    for n in o.keys() {
        if n.is_empty() || !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("invalid field name {n}"));
        }
    }
    Ok(o)
}
pub type Env = BTreeMap<String, Term>;
#[derive(Default)]
pub struct Lower {
    pub rules: BTreeMap<String, u64>,
}
impl Lower {
    pub fn count(&mut self, n: &str) {
        *self.rules.entry(n.into()).or_default() += 1;
    }
    pub fn expr(&mut self, v: &Value, e: &Env) -> Res<Term> {
        if let Some(b) = v.as_bool() {
            return Ok(boolv(b));
        }
        if let Some(s) = v.as_str() {
            return e.get(s).cloned().ok_or(format!("unknown reference {s}"));
        }
        let xs = v
            .as_array()
            .ok_or("expression must be bool, reference or array")?;
        if xs.is_empty() {
            return Err("empty expression".into());
        }
        let op = text(&xs[0])?;
        let a = &xs[1..];
        if op == "bv" {
            if a.len() != 2 {
                return Err("bv requires width,value".into());
            }
            let w = width(&a[0])?;
            let n = if let Some(n) = a[1].as_u64() {
                n
            } else {
                a[1].as_i64().ok_or("literal must fit 64 bits")? as u64
            };
            self.count("fixed_width_literal");
            return Ok(bv(w, n));
        }
        if op == "const_mem" {
            if a.len() != 2 {
                return Err("const_mem requires address width,value".into());
            }
            let aw = width(&a[0])?;
            let val = self.expr(&a[1], e)?;
            if let Sort::Bv(w) = val.0.sort {
                let s = Sort::Mem(aw, w);
                return Ok(node(
                    s.clone(),
                    format!("(as const {})", s.smt()),
                    vec![val],
                ));
            }
            return Err("memory value must be word".into());
        }
        if op == "extract" {
            if a.len() != 3 {
                return Err("extract requires high,low,word".into());
            }
            let hi = a[0].as_u64().ok_or("invalid high bit")?;
            let lo = a[1].as_u64().ok_or("invalid low bit")?;
            let t = self.expr(&a[2], e)?;
            if let Sort::Bv(w) = t.0.sort {
                if lo <= hi && hi < (w as u64) {
                    return Ok(node(
                        Sort::Bv((hi - lo + 1) as u32),
                        format!("(_ extract {hi} {lo})"),
                        vec![t],
                    ));
                }
            }
            return Err("invalid extraction".into());
        }
        if op == "zext" || op == "sext" {
            if a.len() != 2 {
                return Err("extension requires amount,word".into());
            }
            let n = a[0].as_u64().ok_or("invalid extension")?;
            let t = self.expr(&a[1], e)?;
            if let Sort::Bv(w) = t.0.sort {
                if n.checked_add(w as u64).is_some_and(|total| total <= 64) {
                    return Ok(node(
                        Sort::Bv(w + n as u32),
                        format!(
                            "(_ {} {n})",
                            if op == "zext" {
                                "zero_extend"
                            } else {
                                "sign_extend"
                            }
                        ),
                        vec![t],
                    ));
                }
            }
            return Err("invalid extension width".into());
        }
        let n = match op {
            "not" | "bnot" => 1,
            "ite" | "write" => 3,
            "and" | "or" | "xor" | "implies" | "eq" | "ne" | "add" | "sub" | "mul" | "band"
            | "bor" | "bxor" | "shl" | "lshr" | "ult" | "ule" | "slt" | "sle" | "read"
            | "concat" => 2,
            _ => return Err(format!("unsupported operator {op}")),
        };
        if a.len() != n {
            return Err(format!("wrong arity {op}"));
        }
        let ts = a.iter().map(|x| self.expr(x, e)).collect::<Res<Vec<_>>>()?;
        let sorts = ts.iter().map(|t| t.0.sort.clone()).collect::<Vec<_>>();
        let same = sorts.iter().all(|s| s == &sorts[0]);
        let (out, smt) = match op {
            "not" | "and" | "or" | "xor" | "implies" => {
                if sorts.iter().any(|s| s != &Sort::Bool) {
                    return Err(format!(
                        "bool operator type error for {op}: expected Bool operands, found {sorts:?}"
                    ));
                }
                (Sort::Bool, if op == "implies" { "=>" } else { op })
            }
            "eq" | "ne" => {
                if !same {
                    return Err(format!("equality type mismatch: found {sorts:?}"));
                }
                if op == "ne" {
                    return Ok(not(eq(ts[0].clone(), ts[1].clone())));
                }
                (Sort::Bool, "=")
            }
            "ite" => {
                if sorts[0] != Sort::Bool || sorts[1] != sorts[2] {
                    return Err(format!(
                        "ite type error: expected Bool condition and equal branch types, found {sorts:?}"
                    ));
                }
                (sorts[1].clone(), "ite")
            }
            "read" => {
                if let Sort::Mem(a, w) = sorts[0] {
                    if sorts[1] == Sort::Bv(a) {
                        let _ = w;
                        return Ok(memory_read(
                            ts[0].clone(),
                            ts[1].clone(),
                            32,
                            &mut self.rules,
                        ));
                    }
                }
                return Err(format!(
                    "memory read type error: expected memory and matching address width, found {sorts:?}"
                ));
            }
            "write" => {
                if let Sort::Mem(a, w) = sorts[0] {
                    if sorts[1] == Sort::Bv(a) && sorts[2] == Sort::Bv(w) {
                        return Ok(memory_write(
                            ts[0].clone(),
                            ts[1].clone(),
                            ts[2].clone(),
                            &mut self.rules,
                        ));
                    }
                }
                return Err(format!(
                    "memory write type error: expected memory, matching address and word widths, found {sorts:?}"
                ));
            }
            "concat" => {
                if let (Sort::Bv(a), Sort::Bv(b)) = (&sorts[0], &sorts[1]) {
                    if a + b <= 64 {
                        return Ok(node(Sort::Bv(a + b), "concat", ts));
                    }
                }
                return Err("concat width error".into());
            }
            _ => {
                if !same || !matches!(sorts[0], Sort::Bv(_)) {
                    return Err(format!(
                        "word operator type error for {op}: expected equal word widths, found {sorts:?}"
                    ));
                }
                let smt = match op {
                    "add" => "bvadd",
                    "sub" => "bvsub",
                    "mul" => "bvmul",
                    "band" => "bvand",
                    "bor" => "bvor",
                    "bxor" => "bvxor",
                    "bnot" => "bvnot",
                    "shl" => "bvshl",
                    "lshr" => "bvlshr",
                    "ult" => "bvult",
                    "ule" => "bvule",
                    "slt" => "bvslt",
                    "sle" => "bvsle",
                    _ => unreachable!(),
                };
                (
                    if ["ult", "ule", "slt", "sle"].contains(&op) {
                        Sort::Bool
                    } else {
                        sorts[0].clone()
                    },
                    smt,
                )
            }
        };
        Ok(node(out, smt, ts))
    }
}
pub fn require_bool(t: Term) -> Res<Term> {
    if t.0.sort != Sort::Bool {
        Err(format!("expected bool expression, found {:?}", t.0.sort))
    } else {
        Ok(t)
    }
}
pub fn symbols(types: &Value, prefix: &str) -> Res<Env> {
    named(types)?
        .iter()
        .map(|(n, t)| Ok((n.clone(), var(format!("{prefix}_{n}"), ty(t)?))))
        .collect()
}
pub fn scope(s: &Env, i: &Env) -> Env {
    s.iter()
        .map(|(n, t)| (format!("s.{n}"), t.clone()))
        .chain(i.iter().map(|(n, t)| (format!("i.{n}"), t.clone())))
        .collect()
}
pub fn relation_env(s: &Env, t: &Env, i: &Env) -> Env {
    s.iter()
        .map(|(n, t)| (format!("spec.{n}"), t.clone()))
        .chain(t.iter().map(|(n, t)| (format!("impl.{n}"), t.clone())))
        .chain(i.iter().map(|(n, t)| (format!("i.{n}"), t.clone())))
        .collect()
}
