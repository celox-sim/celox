//! Proposed lemma data shared by hand-written proof programs and untrusted generators.
//! Construction and typing confer no proof authority; only a live bundle can check it.
use hwverify_ir::*;
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq)]
pub struct LemmaCandidate {
    pub context: Value,
    pub guard: Value,
    pub claim: Value,
    pub depends_on: Vec<String>,
    pub source: Option<String>,
}

/// Expressions resolved in the current query's typed environment, not a proof.
pub struct TypedLemmaCandidate {
    context: Term,
    guard: Term,
    claim: Term,
}
impl TypedLemmaCandidate {
    pub fn antecedent(&self) -> Term {
        if self.guard == boolv(true) {
            self.context.clone()
        } else if self.context == boolv(true) {
            self.guard.clone()
        } else {
            and(self.context.clone(), self.guard.clone())
        }
    }
    pub fn claim(&self) -> &Term {
        &self.claim
    }
}
impl LemmaCandidate {
    pub fn from_step(step: &Value) -> Res<Self> {
        keys(
            step,
            &[
                "op",
                "id",
                "context",
                "guard",
                "claim",
                "depends_on",
                "frame",
            ],
            &["source"],
        )?;
        if step["op"] != "candidate" || step["frame"] != "current_query" {
            return Err(
                "candidate requires current_query frame; history and induction are unsupported"
                    .into(),
            );
        }
        let deps = step["depends_on"]
            .as_array()
            .ok_or("candidate depends_on must be an array")?;
        if deps.len() > 256 {
            return Err("candidate dependency limit".into());
        }
        let depends_on = deps
            .iter()
            .map(|v| text(v).map(str::to_owned))
            .collect::<Res<Vec<_>>>()?;
        let source = step
            .get("source")
            .map(|v| text(v).map(str::to_owned))
            .transpose()?;
        if source.as_ref().is_some_and(|s| s.len() > 1024) {
            return Err("candidate source label limit".into());
        }
        Ok(Self {
            context: step["context"].clone(),
            guard: step["guard"].clone(),
            claim: step["claim"].clone(),
            depends_on,
            source,
        })
    }
    pub fn to_step(&self, id: &str) -> Value {
        let mut v = json!({"op":"candidate", "id":id, "frame":"current_query", "context":self.context,
            "guard":self.guard, "claim":self.claim, "depends_on":self.depends_on});
        if let Some(source) = &self.source {
            v["source"] = json!(source);
        }
        v
    }
    pub fn lower(&self, environment: &Env) -> Res<TypedLemmaCandidate> {
        let mut lower = Lower::default();
        Ok(TypedLemmaCandidate {
            context: require_bool(lower.expr(&self.context, environment)?)?,
            guard: require_bool(lower.expr(&self.guard, environment)?)?,
            claim: require_bool(lower.expr(&self.claim, environment)?)?,
        })
    }
}
