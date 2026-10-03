//! Strict lowering of untrusted, current-design proof-program hints.
//!
//! No saved reports or numeric query IDs enter this interface. Every successful
//! result is produced by the live opaque sequent kernel for the exact query.
use hwverify_ir::*;
use hwverify_solver::{Check, CutBudgetMode, ProofBundle, RewritePlan, SequentHandle};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashSet};

const MAX_PROGRAMS: usize = 16;
const MAX_STEPS: usize = 256;
const MAX_FORMALS: usize = 32;
const MAX_LETS: usize = 256;
const MAX_METADATA_NODES: usize = 1_000_000;

#[derive(Clone)]
struct Program {
    id: String,
    rhs: Term,
    steps: Vec<Value>,
    result: String,
}

pub struct ProofPrograms {
    mode: CutBudgetMode,
    context: Env,
    environment: Env,
    formals: BTreeSet<String>,
    programs: Vec<Program>,
}

fn identifier(v: &Value) -> Res<String> {
    let s = text(v)?;
    if s.is_empty() || s.len() > 80 || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err("invalid proof-program identifier".into());
    }
    Ok(s.to_owned())
}
fn array(v: &Value, max: usize) -> Res<&Vec<Value>> {
    let a = v.as_array().ok_or("expected proof-program array")?;
    if a.len() > max {
        return Err("proof-program size limit exceeded".into());
    }
    Ok(a)
}
fn bounded_metadata(v: &Value) -> Res<()> {
    let mut pending = vec![(v, 0usize)];
    let mut count = 0;
    while let Some((v, depth)) = pending.pop() {
        count += 1;
        if count > MAX_METADATA_NODES || depth > 256 {
            return Err("proof-program metadata limit exceeded".into());
        }
        match v {
            Value::Array(a) => pending.extend(a.iter().map(|x| (x, depth + 1))),
            Value::Object(o) => pending.extend(o.values().map(|x| (x, depth + 1))),
            _ => {}
        }
    }
    Ok(())
}
fn expr(v: &Value, env: &Env) -> Res<Term> {
    Lower::default().expr(v, env)
}
fn boolean(v: &Value, env: &Env) -> Res<Term> {
    require_bool(expr(v, env)?)
}
fn existing<'a, T>(map: &'a BTreeMap<String, T>, v: &Value) -> Res<&'a T> {
    map.get(text(v)?)
        .ok_or_else(|| "missing or forward proof-program dependency".into())
}
fn bind(env: &mut Env, key: String, term: Term) -> Res<()> {
    if env.insert(key, term).is_some() {
        return Err("duplicate proof-program binding".into());
    }
    Ok(())
}
fn source_variables(ctx: &Env) -> HashSet<String> {
    let mut vars = HashSet::new();
    let mut seen = HashSet::new();
    let mut pending: Vec<_> = ctx.values().cloned().collect();
    while let Some(t) = pending.pop() {
        if !seen.insert(t.clone()) {
            continue;
        }
        if t.0.op.starts_with('@') {
            vars.insert(t.0.op.clone());
        }
        pending.extend(t.0.args.iter().cloned());
    }
    vars
}
fn add_handle(env: &mut Env, id: &str, h: &SequentHandle) -> Res<()> {
    bind(env, format!("handle.{id}.pre"), h.pre().clone())?;
    bind(env, format!("handle.{id}.post"), h.post().clone())
}

impl ProofPrograms {
    pub fn is_independent(&self) -> bool {
        self.mode == CutBudgetMode::IndependentLemmas
    }
    pub fn from_json(metadata: &Value, context: &Env) -> Res<Self> {
        bounded_metadata(metadata)?;
        keys(
            metadata,
            &["version", "mode", "variables", "lets", "programs"],
            &[],
        )?;
        if metadata["version"] != 1 {
            return Err("unsupported proof-program version".into());
        }
        let mode = match text(&metadata["mode"])? {
            "independent_lemmas" => CutBudgetMode::IndependentLemmas,
            "shared_query" => CutBudgetMode::SharedQuery,
            _ => return Err("invalid proof-program budget mode".into()),
        };
        let variables = named(&metadata["variables"])?;
        if variables.len() > MAX_FORMALS {
            return Err("too many proof-program formal variables".into());
        }
        let mut full_context = context.clone();
        let mut environment = context.clone();
        let mut formals = BTreeSet::new();
        let existing_vars = source_variables(context);
        for (name, sort) in variables {
            let symbol = format!("proof_program_formal_{name}");
            if existing_vars.contains(&format!("@{symbol}")) {
                return Err("formal variable shadows original symbol".into());
            }
            let key = format!("formal.{name}");
            let t = var(symbol, ty(sort)?);
            bind(&mut full_context, key.clone(), t.clone())?;
            bind(&mut environment, key.clone(), t)?;
            formals.insert(key);
        }
        for row in array(&metadata["lets"], MAX_LETS)? {
            keys(row, &["id", "expr"], &[])?;
            let id = identifier(&row["id"])?;
            let value = expr(&row["expr"], &environment)?;
            bind(&mut environment, format!("let.{id}"), value)?;
        }
        let mut programs = vec![];
        let mut ids = BTreeSet::new();
        for row in array(&metadata["programs"], MAX_PROGRAMS)? {
            keys(row, &["id", "match_rhs", "steps", "result"], &[])?;
            let id = identifier(&row["id"])?;
            if !ids.insert(id.clone()) {
                return Err("duplicate proof-program id".into());
            }
            let rhs = expr(&row["match_rhs"], &environment)?;
            if programs.iter().any(|p: &Program| p.rhs == rhs) {
                return Err("ambiguous proof-program target".into());
            }
            let steps = array(&row["steps"], MAX_STEPS)?.clone();
            if steps.is_empty() {
                return Err("empty proof program".into());
            }
            let result = identifier(&row["result"])?;
            Self::validate_steps(&steps, &result, &environment, &formals, &rhs.0.sort)?;
            programs.push(Program {
                id,
                rhs,
                steps,
                result,
            });
        }
        if programs.is_empty() {
            return Err("no proof programs".into());
        }
        Ok(Self {
            mode,
            context: full_context,
            environment,
            formals,
            programs,
        })
    }

    fn validate_steps(
        steps: &[Value],
        result: &str,
        base: &Env,
        formals: &BTreeSet<String>,
        goal_sort: &Sort,
    ) -> Res<()> {
        let mut env = base.clone();
        for (name, sort) in [
            ("$pre", Sort::Bool),
            ("$goal", Sort::Bool),
            ("$lhs", goal_sort.clone()),
            ("$rhs", goal_sort.clone()),
        ] {
            bind(
                &mut env,
                name.into(),
                var(format!("proof_program_validation_{name}"), sort),
            )?;
        }
        let mut handles = BTreeMap::new();
        let mut plans = BTreeMap::new();
        let mut ids = BTreeSet::new();
        for step in steps {
            let op = text(&step["op"])?;
            let id = identifier(&step["id"])?;
            if !ids.insert(id.clone()) {
                return Err("duplicate proof-program step id".into());
            }
            match op {
                "let" => {
                    keys(step, &["op", "id", "expr"], &[])?;
                    let t = expr(&step["expr"], &env)?;
                    bind(&mut env, format!("let.{id}"), t)?;
                    continue;
                }
                "prove" => {
                    keys(step, &["op", "id", "pre", "post"], &[])?;
                    boolean(&step["pre"], &env)?;
                    boolean(&step["post"], &env)?;
                }
                "instantiate" => {
                    keys(step, &["op", "id", "source", "substitution"], &[])?;
                    existing(&handles, &step["source"])?;
                    let mut sources = BTreeSet::new();
                    for sub in array(&step["substitution"], MAX_FORMALS)? {
                        keys(sub, &["from", "to"], &[])?;
                        let from = text(&sub["from"])?;
                        if !formals.contains(from) || !sources.insert(from) {
                            return Err("invalid universal substitution variable".into());
                        }
                        if env[from].0.sort != expr(&sub["to"], &env)?.0.sort {
                            return Err("universal substitution sort mismatch".into());
                        }
                    }
                }
                "project" => {
                    keys(step, &["op", "id", "source", "path"], &[])?;
                    existing(&handles, &step["source"])?;
                    for n in array(&step["path"], 64)? {
                        if n.as_u64().is_none() {
                            return Err("invalid projection path".into());
                        }
                    }
                }
                "apply" => {
                    keys(step, &["op", "id", "lemma", "premise"], &[])?;
                    existing(&handles, &step["lemma"])?;
                    existing(&handles, &step["premise"])?;
                }
                "conditional_eq" => {
                    keys(step, &["op", "id", "pre", "guard", "source"], &[])?;
                    boolean(&step["pre"], &env)?;
                    boolean(&step["guard"], &env)?;
                    existing(&handles, &step["source"])?;
                }
                "prepare_rewrite" => {
                    keys(
                        step,
                        &[
                            "op",
                            "id",
                            "pre",
                            "goal",
                            "equalities",
                            "retain_rewritten_pre",
                        ],
                        &[],
                    )?;
                    boolean(&step["pre"], &env)?;
                    boolean(&step["goal"], &env)?;
                    if step["retain_rewritten_pre"].as_bool().is_none() {
                        return Err("invalid retain flag".into());
                    }
                    for h in array(&step["equalities"], 32)? {
                        existing(&handles, h)?;
                    }
                    plans.insert(id.clone(), ());
                    bind(&mut env, format!("plan.{id}.pre"), boolv(true))?;
                    bind(&mut env, format!("plan.{id}.post"), boolv(true))?;
                    continue;
                }
                "finish_rewrite" => {
                    keys(step, &["op", "id", "plan", "proof"], &[])?;
                    let plan = text(&step["plan"])?;
                    if plans.remove(plan).is_none() {
                        return Err("missing or consumed rewrite plan".into());
                    }
                    existing(&handles, &step["proof"])?;
                }
                "join" => {
                    keys(
                        step,
                        &["op", "id", "pre", "goal", "guard", "positive", "negative"],
                        &[],
                    )?;
                    for k in ["pre", "goal", "guard"] {
                        boolean(&step[k], &env)?;
                    }
                    existing(&handles, &step["positive"])?;
                    existing(&handles, &step["negative"])?;
                }
                _ => return Err("unsupported proof-program instruction".into()),
            }
            handles.insert(id.clone(), ());
            bind(&mut env, format!("handle.{id}.pre"), boolv(true))?;
            bind(&mut env, format!("handle.{id}.post"), boolv(true))?;
        }
        if !handles.contains_key(result) {
            return Err("proof-program result is not a prior handle".into());
        }
        Ok(())
    }

    pub fn try_query(
        &self,
        check: &mut Check,
        name: &str,
        original_bad: &Term,
        original_context: &Env,
    ) -> Res<bool> {
        // Context identity is checked before even matching a target. Formals only
        // extend the original environment with fresh universally free variables.
        if original_context
            .iter()
            .any(|(k, v)| self.context.get(k) != Some(v))
            || self.context.len() != original_context.len() + self.formals.len()
        {
            return Err("stale proof-program context".into());
        }
        let mut tail = original_bad;
        while tail.0.op == "and" && tail.0.args.len() == 2 {
            tail = &tail.0.args[1];
        }
        if tail.0.op != "not" || tail.0.args.len() != 1 {
            return Ok(false);
        }
        let goal = &tail.0.args[0];
        if goal.0.op != "=" || goal.0.args.len() != 2 {
            return Ok(false);
        }
        let Some(program) = self.programs.iter().find(|p| p.rhs == goal.0.args[1]) else {
            return Ok(false);
        };
        let mut bundle = ProofBundle::new(check, name, original_bad, &self.context, self.mode)?;
        let mut env = self.environment.clone();
        for (key, term) in [
            ("$pre", bundle.original_pre().clone()),
            ("$goal", bundle.original_goal().clone()),
            ("$lhs", goal.0.args[0].clone()),
            ("$rhs", goal.0.args[1].clone()),
        ] {
            bind(&mut env, key.into(), term)?;
        }
        let mut handles: BTreeMap<String, SequentHandle> = BTreeMap::new();
        let mut plans: BTreeMap<String, RewritePlan> = BTreeMap::new();
        for step in &program.steps {
            let id = text(&step["id"])?;
            let handle = match text(&step["op"])? {
                "let" => {
                    let t = expr(&step["expr"], &env)?;
                    bind(&mut env, format!("let.{id}"), t)?;
                    continue;
                }
                "prove" => bundle.prove(
                    &format!("{}_{}", program.id, id),
                    boolean(&step["pre"], &env)?,
                    boolean(&step["post"], &env)?,
                )?,
                "instantiate" => {
                    let substitutions = array(&step["substitution"], MAX_FORMALS)?
                        .iter()
                        .map(|x| Ok((env[text(&x["from"])?].clone(), expr(&x["to"], &env)?)))
                        .collect::<Res<Vec<_>>>()?;
                    bundle.instantiate(existing(&handles, &step["source"])?, &substitutions)?
                }
                "project" => {
                    let path = array(&step["path"], 64)?
                        .iter()
                        .map(|x| {
                            usize::try_from(x.as_u64().unwrap())
                                .map_err(|_| "projection index overflow".into())
                        })
                        .collect::<Res<Vec<_>>>()?;
                    bundle.project(existing(&handles, &step["source"])?, &path)?
                }
                "apply" => bundle.apply(
                    existing(&handles, &step["lemma"])?,
                    existing(&handles, &step["premise"])?,
                )?,
                "conditional_eq" => bundle.conditional_eq(
                    boolean(&step["pre"], &env)?,
                    boolean(&step["guard"], &env)?,
                    existing(&handles, &step["source"])?,
                )?,
                "prepare_rewrite" => {
                    let equalities = array(&step["equalities"], 32)?
                        .iter()
                        .map(|x| existing(&handles, x).cloned())
                        .collect::<Res<Vec<_>>>()?;
                    let plan = bundle.prepare_rewrite(
                        boolean(&step["pre"], &env)?,
                        boolean(&step["goal"], &env)?,
                        &equalities,
                        step["retain_rewritten_pre"].as_bool().unwrap(),
                    )?;
                    bind(&mut env, format!("plan.{id}.pre"), plan.pre().clone())?;
                    bind(&mut env, format!("plan.{id}.post"), plan.post().clone())?;
                    plans.insert(id.into(), plan);
                    continue;
                }
                "finish_rewrite" => {
                    let plan = plans
                        .remove(text(&step["plan"])?)
                        .ok_or("stale or consumed rewrite plan")?;
                    bundle.finish_rewrite(plan, existing(&handles, &step["proof"])?)?
                }
                "join" => bundle.join(
                    boolean(&step["pre"], &env)?,
                    boolean(&step["goal"], &env)?,
                    boolean(&step["guard"], &env)?,
                    existing(&handles, &step["positive"])?,
                    existing(&handles, &step["negative"])?,
                )?,
                _ => return Err("unsupported proof-program instruction".into()),
            };
            add_handle(&mut env, id, &handle)?;
            handles.insert(id.into(), handle);
        }
        bundle.finish(existing(&handles, &Value::String(program.result.clone()))?)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn context() -> Env {
        [("impl.x".into(), var("original_x".into(), Sort::Bv(8)))]
            .into_iter()
            .collect()
    }
    fn document() -> Value {
        json!({"version":1,"mode":"independent_lemmas","variables":{},"lets":[],"programs":[{
            "id":"simple","match_rhs":["bv",8,0],"steps":[{"op":"prove","id":"done","pre":"$pre","post":"$goal"}],"result":"done"
        }]})
    }
    #[test]
    fn simple_program_is_well_typed() {
        assert!(ProofPrograms::from_json(&document(), &context()).is_ok());
    }
    #[test]
    fn unknown_fields_and_versions_reject() {
        let mut d = document();
        d["saved_report"] = json!("proof.json");
        assert!(ProofPrograms::from_json(&d, &context()).is_err());
        let mut d = document();
        d["version"] = json!(2);
        assert!(ProofPrograms::from_json(&d, &context()).is_err());
    }
    #[test]
    fn undefined_names_and_wrong_sorts_reject() {
        let mut d = document();
        d["programs"][0]["steps"][0]["pre"] = json!("n.undefined");
        assert!(ProofPrograms::from_json(&d, &context()).is_err());
        let mut d = document();
        d["programs"][0]["steps"][0]["pre"] = json!("impl.x");
        assert!(ProofPrograms::from_json(&d, &context()).is_err());
    }
    #[test]
    fn forward_foreign_duplicate_and_ambiguous_handles_reject() {
        let mut d = document();
        d["programs"][0]["steps"] =
            json!([{"op":"project","id":"done","source":"later","path":[]}]);
        assert!(ProofPrograms::from_json(&d, &context()).is_err());
        let mut d = document();
        let step = d["programs"][0]["steps"][0].clone();
        d["programs"][0]["steps"].as_array_mut().unwrap().push(step);
        assert!(ProofPrograms::from_json(&d, &context()).is_err());
        let mut d = document();
        let mut duplicate = d["programs"][0].clone();
        duplicate["id"] = json!("other");
        d["programs"].as_array_mut().unwrap().push(duplicate);
        assert!(ProofPrograms::from_json(&d, &context()).is_err());
    }
    #[test]
    fn substitutions_require_fresh_variables_and_exact_sorts() {
        let mut d = document();
        d["variables"] = json!({"a":{"bv":8}});
        d["programs"][0]["steps"].as_array_mut().unwrap().push(json!({"op":"instantiate","id":"instance","source":"done","substitution":[{"from":"formal.a","to":"impl.x"}]}));
        d["programs"][0]["result"] = json!("instance");
        assert!(ProofPrograms::from_json(&d, &context()).is_ok());
        d["programs"][0]["steps"][1]["substitution"][0]["to"] = json!(true);
        assert!(ProofPrograms::from_json(&d, &context()).is_err());
        d["programs"][0]["steps"][1]["substitution"][0] = json!({"from":"impl.x","to":"impl.x"});
        assert!(ProofPrograms::from_json(&d, &context()).is_err());
    }
    #[test]
    fn formal_symbol_shadowing_and_duplicate_lets_reject() {
        let mut d = document();
        d["variables"] = json!({"a":{"bv":8}});
        let ctx = [(
            "original.alias".into(),
            var("proof_program_formal_a".into(), Sort::Bv(8)),
        )]
        .into_iter()
        .collect();
        assert!(ProofPrograms::from_json(&d, &ctx).is_err());
        let mut d = document();
        d["lets"] = json!([{"id":"a","expr":true},{"id":"a","expr":false}]);
        assert!(ProofPrograms::from_json(&d, &context()).is_err());
    }
    #[test]
    fn numeric_target_ids_and_stale_program_results_reject() {
        let mut d = document();
        d["programs"][0]["query_id"] = json!(360);
        assert!(ProofPrograms::from_json(&d, &context()).is_err());
        let mut d = document();
        d["programs"][0]["result"] = json!("missing");
        assert!(ProofPrograms::from_json(&d, &context()).is_err());
    }
    #[test]
    fn instructions_cannot_access_unproved_plan_getters() {
        let mut d = document();
        d["programs"][0]["steps"][0]["pre"] = json!("plan.future.pre");
        assert!(ProofPrograms::from_json(&d, &context()).is_err());
        let mut d = document();
        d["programs"][0]["steps"][0]["post"] = json!("handle.done.post");
        assert!(ProofPrograms::from_json(&d, &context()).is_err());
    }
    #[test]
    fn consumed_plans_reject_statically() {
        let mut d = document();
        d["programs"][0]["steps"] = json!([
            {"op":"prove","id":"e","pre":"$pre","post":["eq","impl.x",["bv",8,0]]},
            {"op":"prepare_rewrite","id":"r","pre":"$pre","goal":"$goal","equalities":["e"],"retain_rewritten_pre":false},
            {"op":"prove","id":"p","pre":"plan.r.pre","post":"plan.r.post"},
            {"op":"finish_rewrite","id":"first","plan":"r","proof":"p"},
            {"op":"finish_rewrite","id":"done","plan":"r","proof":"p"}
        ]);
        assert!(ProofPrograms::from_json(&d, &context()).is_err());
    }
    #[test]
    fn live_execution_boundaries() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "proof_program::tests::live_execution_worker",
                "--nocapture",
            ])
            .env("HWVERIFY_SOLVER", "finite")
            .env("PROOF_PROGRAM_EXECUTION_WORKER", "1")
            .status()
            .unwrap();
        assert!(status.success());
    }
    #[test]
    fn live_execution_worker() {
        if std::env::var("PROOF_PROGRAM_EXECUTION_WORKER").as_deref() != Ok("1") {
            return;
        }
        let root =
            std::env::temp_dir().join(format!("hwverify-proof-program-{}", std::process::id()));
        let ctx = context();
        let x = ctx["impl.x"].clone();
        let goal = eq(x.clone(), bv(8, 0));
        let original = and(goal.clone(), not(goal));
        let checker = |name: &str| {
            let out = root.join(name);
            std::fs::create_dir_all(&out).unwrap();
            Check {
                z3: "unused".into(),
                out,
                reports: vec![],
            }
        };
        let programs = ProofPrograms::from_json(&document(), &ctx).unwrap();
        assert!(programs
            .try_query(&mut checker("valid"), "valid", &original, &ctx)
            .unwrap());
        let stale = [("impl.x".into(), var("different_x".into(), Sort::Bv(8)))]
            .into_iter()
            .collect();
        assert!(programs
            .try_query(&mut checker("stale"), "stale", &original, &stale)
            .is_err());
        // Same RHS matches, but cannot prove a different false original goal.
        let false_query = and(boolv(true), not(eq(x, bv(8, 0))));
        assert!(programs
            .try_query(&mut checker("false_goal"), "false_goal", &false_query, &ctx)
            .is_err());
        let mut forged = document();
        forged["programs"][0]["steps"][0]["pre"] = json!(true);
        forged["programs"][0]["steps"][0]["post"] = json!(true);
        let forged = ProofPrograms::from_json(&forged, &ctx).unwrap();
        assert!(forged
            .try_query(
                &mut checker("wrong_result"),
                "wrong_result",
                &original,
                &ctx
            )
            .is_err());
        let mut inst = document();
        inst["variables"] = json!({"a":{"bv":8}});
        inst["programs"][0]["steps"] = json!([
            {"op":"prove","id":"u","pre":true,"post":["eq","formal.a","formal.a"]},
            {"op":"instantiate","id":"done","source":"u","substitution":[{"from":"formal.a","to":"impl.x"}]}
        ]);
        let mut inst = ProofPrograms::from_json(&inst, &ctx).unwrap();
        // Direct mutation is test-only; runtime must still reject type forgery.
        inst.programs[0].steps[1]["substitution"][0]["to"] = json!(true);
        assert!(inst
            .try_query(
                &mut checker("wrong_instantiation"),
                "wrong_instantiation",
                &original,
                &ctx
            )
            .is_err());
        let mut consumed = document();
        consumed["programs"][0]["steps"] = json!([
            {"op":"prove","id":"e","pre":"$pre","post":["eq","impl.x",["bv",8,0]]},
            {"op":"prepare_rewrite","id":"r","pre":"$pre","goal":"$goal","equalities":["e"],"retain_rewritten_pre":false},
            {"op":"prove","id":"p","pre":"plan.r.pre","post":"plan.r.post"},
            {"op":"finish_rewrite","id":"done","plan":"r","proof":"p"}
        ]);
        let mut consumed = ProofPrograms::from_json(&consumed, &ctx).unwrap();
        consumed.programs[0]
            .steps
            .push(json!({"op":"finish_rewrite","id":"again","plan":"r","proof":"p"}));
        consumed.programs[0].result = "again".into();
        assert!(consumed
            .try_query(&mut checker("consumed"), "consumed", &original, &ctx)
            .is_err());
    }
}
