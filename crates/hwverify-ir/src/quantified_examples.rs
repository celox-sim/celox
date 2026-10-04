//! Ordered typed input binders with one innermost execution binder.
use crate::design::{at, child, declarations, fail};
use crate::{keys, text, Env, ValidationError};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputQuantifierKind {
    Forall,
    Exists,
}
#[derive(Clone, Debug)]
pub struct InputQuantifier {
    pub kind: InputQuantifierKind,
    /// Source names map to hygienic solver variables; references use q.<name>.
    pub variables: Env,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionQuantifier {
    Exists,
    NotExists,
    Forall,
}
impl ExecutionQuantifier {
    pub fn name(self) -> &'static str {
        match self {
            Self::Exists => "exists",
            Self::NotExists => "not_exists",
            Self::Forall => "forall",
        }
    }
}
#[derive(Clone, Debug)]
pub struct TraceQuantification {
    pub inputs: Vec<InputQuantifier>,
    pub execution: ExecutionQuantifier,
}

pub(crate) fn parse(
    example: &Value,
    path: &str,
) -> Result<(Option<TraceQuantification>, Env), ValidationError> {
    let execution = match example["expect"].as_str() {
        Some("exists") => ExecutionQuantifier::Exists,
        Some("not_exists") => ExecutionQuantifier::NotExists,
        Some("forall") => ExecutionQuantifier::Forall,
        _ => {
            if example.get("quantifiers").is_some() {
                return fail(&child(path, "quantifiers"), "input quantifiers require explicit exists, not_exists, or forall execution expectation");
            }
            return Ok((None, Env::new()));
        }
    };
    let mut inputs = Vec::new();
    let mut bound = Env::new();
    let mut seen = BTreeSet::new();
    if let Some(value) = example.get("quantifiers") {
        let path = child(path, "quantifiers");
        let values = value.as_array().ok_or_else(|| ValidationError {
            path: path.clone(),
            message: "quantifiers must be an ordered array of input binders; execution is innermost and specified by expect".into(),
        })?;
        if values.len() > 256 {
            return fail(&path, "quantifier prefix exceeds 256-binder limit");
        }
        for (index, value) in values.iter().enumerate() {
            let path = child(&path, &index.to_string());
            at(&path, keys(value, &["kind", "variables"], &[]))?;
            let kind = match at(&child(&path, "kind"), text(&value["kind"]))? {
                "forall" => InputQuantifierKind::Forall,
                "exists" => InputQuantifierKind::Exists,
                _ => {
                    return fail(
                        &child(&path, "kind"),
                        "input binder kind must be forall or exists; execution is innermost",
                    )
                }
            };
            let variables = declarations(
                &value["variables"],
                &format!("quantified_input_{index}"),
                &child(&path, "variables"),
            )?;
            if variables.is_empty() {
                return fail(
                    &child(&path, "variables"),
                    "input binder must declare at least one variable",
                );
            }
            for (name, term) in &variables {
                if !seen.insert(name.clone()) {
                    return fail(
                        &child(&child(&path, "variables"), name),
                        "duplicate bound name; quantifier shadowing is not supported",
                    );
                }
                bound.insert(format!("q.{name}"), term.clone());
            }
            inputs.push(InputQuantifier { kind, variables });
        }
    }
    Ok((Some(TraceQuantification { inputs, execution }), bound))
}
