//! Semantic validation boundary shared by all frontends.
//!
//! A Design cannot be constructed from a parser AST without validating every
//! declaration and expression, including unused wires and late program fields.
//! Validation is pure and completes before verification may start a solver.
use crate::*;
use serde_json::Value;
use std::{collections::BTreeMap, fmt};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError {
    /// JSON pointer into the canonical document; DSL frontends attach its span.
    pub path: String,
    pub message: String,
}
impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {}",
            if self.path.is_empty() {
                "/"
            } else {
                &self.path
            },
            self.message
        )
    }
}
impl std::error::Error for ValidationError {}
type VResult<T> = Result<T, ValidationError>;
pub(crate) fn at<T>(path: &str, result: Res<T>) -> VResult<T> {
    result.map_err(|message| ValidationError {
        path: path.into(),
        message,
    })
}
pub(crate) fn fail<T>(path: &str, message: impl Into<String>) -> VResult<T> {
    Err(ValidationError {
        path: path.into(),
        message: message.into(),
    })
}
pub(crate) fn child(path: &str, key: &str) -> String {
    format!("{}/{}", path, key.replace('~', "~0").replace('/', "~1"))
}

/// Typed current/reset/next state and observable outputs. Maps are immutable
/// through Design's public API, so parser-generated data cannot bypass checking.
#[derive(Clone, Debug)]
pub struct Machine {
    pub state: Env,
    pub reset: Env,
    pub next: Env,
    pub outputs: Env,
}
#[derive(Clone, Debug)]
pub struct ProgramContract {
    pub parameters: Env,
    pub precondition: Term,
    pub invariant: Term,
    pub terminal: Term,
    pub postcondition: Term,
    pub rank: Term,
}
#[derive(Clone, Debug)]
pub struct Design {
    document: Value,
    inputs: Env,
    spec: Machine,
    implementation: Machine,
    machine_normalization: BTreeMap<String, u64>,
    binding: Term,
    enabled: Term,
    rank: Term,
    contract: Option<ProgramContract>,
}
impl Design {
    pub fn document(&self) -> &Value {
        &self.document
    }
    pub fn inputs(&self) -> &Env {
        &self.inputs
    }
    pub fn spec(&self) -> &Machine {
        &self.spec
    }
    pub fn implementation(&self) -> &Machine {
        &self.implementation
    }
    pub fn machine_normalization(&self) -> &BTreeMap<String, u64> {
        &self.machine_normalization
    }
    pub fn binding(&self) -> &Term {
        &self.binding
    }
    pub fn progress_enabled(&self) -> &Term {
        &self.enabled
    }
    pub fn progress_rank(&self) -> &Term {
        &self.rank
    }
    pub fn contract(&self) -> Option<&ProgramContract> {
        self.contract.as_ref()
    }

    pub fn from_json(doc: &Value) -> VResult<Self> {
        at(
            "",
            keys(
                doc,
                &[
                    "version",
                    "inputs",
                    "reset_input",
                    "spec",
                    "impl",
                    "binding",
                    "commit",
                    "can_step",
                    "progress",
                ],
                &["name", "hold_when", "program_contract"],
            ),
        )?;
        if doc["version"] != 2 {
            return fail("/version", "expected version 2");
        }
        let inputs = declarations(&doc["inputs"], "input", "/inputs")?;
        let reset = at("/reset_input", text(&doc["reset_input"]))?;
        at(
            "/reset_input",
            require_bool(
                inputs
                    .get(reset)
                    .ok_or_else(|| ValidationError {
                        path: "/reset_input".into(),
                        message: "missing reset input".into(),
                    })?
                    .clone(),
            ),
        )?;
        for m in ["spec", "impl"] {
            at(
                &format!("/{m}"),
                keys(&doc[m], &["state", "reset", "next", "outputs"], &["wires"]),
            )?;
        }
        let s = declarations(&doc["spec"]["state"], "spec", "/spec/state")?;
        let t = declarations(&doc["impl"]["state"], "impl", "/impl/state")?;
        if s.is_empty() || t.is_empty() {
            return fail(
                if s.is_empty() {
                    "/spec/state"
                } else {
                    "/impl/state"
                },
                "empty state",
            );
        }
        let mut l = Lower::default();
        // Preserve legacy lowering order and normalization diagnostics exactly.
        let sr = assignment(&doc["spec"], "spec", "reset", &s, &inputs, &mut l)?;
        let tr = assignment(&doc["impl"], "impl", "reset", &t, &inputs, &mut l)?;
        let sn = assignment(&doc["spec"], "spec", "next", &s, &inputs, &mut l)?;
        let tn = assignment(&doc["impl"], "impl", "next", &t, &inputs, &mut l)?;
        let so = model_outputs(&doc["spec"], "spec", &s, &inputs, &mut l)?;
        let io = model_outputs(&doc["impl"], "impl", &t, &inputs, &mut l)?;
        let machine_normalization = l.rules.clone();
        for (field, outputs, missing) in [
            ("commit", &io, "missing commit output"),
            ("can_step", &so, "missing spec can_step output"),
        ] {
            let path = format!("/{field}");
            let name = at(&path, text(&doc[field]))?;
            let value = outputs.get(name).ok_or_else(|| ValidationError {
                path: path.clone(),
                message: missing.into(),
            })?;
            at(&path, require_bool(value.clone()))?;
        }
        let relation = relation_env(&s, &t, &Env::new());
        let binding = boolean(&doc["binding"], &relation, "/binding", &mut l)?;
        let relation_inputs = relation_env(&s, &t, &inputs);
        if let Some(hold) = doc.get("hold_when") {
            boolean(hold, &relation_inputs, "/hold_when", &mut l)?;
        }
        at(
            "/progress",
            keys(&doc["progress"], &["enabled", "rank"], &[]),
        )?;
        let enabled = boolean(
            &doc["progress"]["enabled"],
            &relation_inputs,
            "/progress/enabled",
            &mut l,
        )?;
        let rank = expression(
            &doc["progress"]["rank"],
            &relation,
            "/progress/rank",
            &mut l,
        )?;
        if !matches!(rank.0.sort, Sort::Bv(_)) {
            return fail("/progress/rank", "rank must be unsigned word");
        }
        let contract = doc
            .get("program_contract")
            .map(|c| contract(c, &s, &inputs, &mut l))
            .transpose()?;
        Ok(Self {
            document: doc.clone(),
            inputs,
            spec: Machine {
                state: s,
                reset: sr,
                next: sn,
                outputs: so,
            },
            implementation: Machine {
                state: t,
                reset: tr,
                next: tn,
                outputs: io,
            },
            machine_normalization,
            binding,
            enabled,
            rank,
            contract,
        })
    }
}
pub(crate) fn declarations(doc: &Value, prefix: &str, path: &str) -> VResult<Env> {
    at(path, named(doc))?
        .iter()
        .map(|(name, value)| {
            Ok((
                name.clone(),
                var(
                    format!("{prefix}_{name}"),
                    at(&child(path, name), ty(value))?,
                ),
            ))
        })
        .collect()
}

/// Validate in expression order before performing the production lowering.
/// A separate scratch lowerer only diagnoses the failing subtree; it contributes
/// no normalization counts and cannot create accepted semantic nodes.
pub(crate) fn expression(value: &Value, env: &Env, path: &str, lower: &mut Lower) -> VResult<Term> {
    match lower.expr(value, env) {
        Ok(term) => Ok(term),
        Err(message) => {
            if let Some(xs) = value.as_array() {
                let start = match xs.first().and_then(Value::as_str) {
                    Some("bv") => xs.len(),
                    Some("const_mem" | "zext" | "sext") => 2,
                    Some("extract") => 3,
                    _ => 1,
                };
                for (i, arg) in xs.iter().enumerate().skip(start) {
                    expression(
                        arg,
                        env,
                        &child(path, &i.to_string()),
                        &mut Lower::default(),
                    )?;
                }
            }
            fail(path, message)
        }
    }
}
pub(crate) fn boolean(value: &Value, env: &Env, path: &str, lower: &mut Lower) -> VResult<Term> {
    let term = expression(value, env, path, lower)?;
    at(path, require_bool(term))
}
pub(crate) fn model_wires(
    model: &Value,
    name: &str,
    state: &Env,
    inputs: &Env,
    lower: &mut Lower,
) -> VResult<Env> {
    fn visit(
        n: &str,
        defs: &serde_json::Map<String, Value>,
        env: &mut Env,
        lower: &mut Lower,
        active: &mut Vec<String>,
        path: &str,
    ) -> VResult<()> {
        let key = format!("w.{n}");
        if env.contains_key(&key) {
            return Ok(());
        }
        let this = child(path, n);
        if active.iter().any(|x| x == n) {
            let mut cycle = active.clone();
            cycle.push(n.into());
            return fail(&this, format!("wire cycle: {}", cycle.join(" -> ")));
        }
        let value = defs.get(n).ok_or_else(|| ValidationError {
            path: this.clone(),
            message: format!("unknown wire {n}"),
        })?;
        active.push(n.into());
        fn deps(value: &Value, out: &mut Vec<String>) {
            if let Some(s) = value.as_str().and_then(|s| s.strip_prefix("w.")) {
                out.push(s.into());
            }
            if let Some(a) = value.as_array() {
                for x in a.iter().skip(1) {
                    deps(x, out);
                }
            }
        }
        let mut refs = vec![];
        deps(value, &mut refs);
        for dependency in refs {
            if !defs.contains_key(&dependency) {
                return fail(&this, format!("unknown wire {dependency}"));
            }
            visit(&dependency, defs, env, lower, active, path)?;
        }
        let term = expression(value, env, &this, lower)?;
        env.insert(key, term);
        active.pop();
        Ok(())
    }
    let mut env = scope(state, inputs);
    if let Some(w) = model.get("wires") {
        let path = format!("/{name}/wires");
        let defs = at(&path, named(w))?;
        for n in defs.keys() {
            visit(n, defs, &mut env, lower, &mut vec![], &path)?;
        }
    }
    Ok(env)
}
pub(crate) fn assignment(
    model: &Value,
    name: &str,
    field: &str,
    state: &Env,
    inputs: &Env,
    lower: &mut Lower,
) -> VResult<Env> {
    let path = format!("/{name}/{field}");
    let env = if field == "reset" {
        scope(&Env::new(), inputs)
    } else {
        model_wires(model, name, state, inputs, lower)?
    };
    let values = at(&path, named(&model[field]))?;
    if values.len() != state.len() || values.keys().any(|key| !state.contains_key(key)) {
        return fail(&path, format!("{field} must assign every state"));
    }
    values
        .iter()
        .map(|(name, value)| {
            let path = child(&path, name);
            let term = expression(value, &env, &path, lower)?;
            if term.0.sort != state[name].0.sort {
                return fail(
                    &path,
                    format!(
                        "state type mismatch {name}: expected {:?}, found {:?}",
                        state[name].0.sort, term.0.sort
                    ),
                );
            }
            Ok((name.clone(), term))
        })
        .collect()
}
fn model_outputs(
    model: &Value,
    name: &str,
    state: &Env,
    inputs: &Env,
    lower: &mut Lower,
) -> VResult<Env> {
    let env = model_wires(model, name, state, inputs, lower)?;
    let path = format!("/{name}/outputs");
    at(&path, named(&model["outputs"]))?
        .iter()
        .map(|(n, value)| Ok((n.clone(), expression(value, &env, &child(&path, n), lower)?)))
        .collect()
}
fn contract(c: &Value, state: &Env, inputs: &Env, lower: &mut Lower) -> VResult<ProgramContract> {
    let base = "/program_contract";
    at(
        base,
        keys(
            c,
            &[
                "parameters",
                "precondition",
                "invariant",
                "terminal",
                "postcondition",
                "rank",
            ],
            &["cases", "split", "partitioning"],
        ),
    )?;
    let parameters = declarations(
        &c["parameters"],
        "parameter",
        "/program_contract/parameters",
    )?;
    let env = |s: &Env, i: &Env| {
        let mut e = scope(s, i);
        e.extend(
            parameters
                .iter()
                .map(|(name, t)| (format!("p.{name}"), t.clone())),
        );
        e
    };
    let e = env(state, &Env::new());
    let precondition = boolean(
        &c["precondition"],
        &env(&Env::new(), inputs),
        "/program_contract/precondition",
        lower,
    )?;
    let invariant = boolean(&c["invariant"], &e, "/program_contract/invariant", lower)?;
    let terminal = boolean(&c["terminal"], &e, "/program_contract/terminal", lower)?;
    let postcondition = boolean(
        &c["postcondition"],
        &e,
        "/program_contract/postcondition",
        lower,
    )?;
    let rank = expression(&c["rank"], &e, "/program_contract/rank", lower)?;
    if !matches!(rank.0.sort, Sort::Bv(_)) {
        return fail(
            "/program_contract/rank",
            "program rank must be unsigned word",
        );
    }
    if c.get("cases").is_some() && c.get("split").is_some() {
        return fail(base, "program cases and split are mutually exclusive");
    }
    if let Some(mode) = c.get("partitioning") {
        let mode = at("/program_contract/partitioning", text(mode))?;
        if !["auto", "none"].contains(&mode) {
            return fail(
                "/program_contract/partitioning",
                "partitioning must be auto or none",
            );
        }
        if c.get("split").is_some() || c.get("cases").is_some() {
            return fail(base, "partitioning cannot be combined with split or cases");
        }
    }
    if let Some(cases) = c.get("cases") {
        for (name, guard) in at("/program_contract/cases", named(cases))? {
            boolean(guard, &e, &child("/program_contract/cases", name), lower)?;
        }
    }
    if let Some(split) = c.get("split") {
        let mut count = 1u128;
        for (name, hint) in at("/program_contract/split", named(split))? {
            let path = child("/program_contract/split", name);
            at(&path, keys(hint, &["expr", "min", "max"], &[]))?;
            let term = expression(&hint["expr"], &e, &child(&path, "expr"), lower)?;
            let Sort::Bv(width) = term.0.sort else {
                return fail(&child(&path, "expr"), "split expression must be word");
            };
            let min = hint["min"].as_u64().ok_or_else(|| ValidationError {
                path: child(&path, "min"),
                message: "invalid split min".into(),
            })?;
            let max = hint["max"].as_u64().ok_or_else(|| ValidationError {
                path: child(&path, "max"),
                message: "invalid split max".into(),
            })?;
            if min > max
                || (width < 64 && max >= (1u64 << width))
                || (max as u128 - min as u128 + 2) * count > 256
            {
                return fail(&path, "split bounds invalid or partition count exceeds 256");
            }
            count *= max as u128 - min as u128 + 2;
        }
    }
    Ok(ProgramContract {
        parameters,
        precondition,
        invariant,
        terminal,
        postcondition,
        rank,
    })
}
