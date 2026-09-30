//! Relational specifications and finite observational examples. No solver I/O.
//! Components share observations/inputs, never private state. Every named
//! operation synchronizes all selected components; no implicit interleaving.
use crate::design::{at, boolean, child, declarations, expression, fail};
use crate::*;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

type Result<T> = std::result::Result<T, ValidationError>;
#[derive(Clone, Debug)]
pub struct SpecComponent {
    pub state: Env,
    pub next_state: Env,
    pub initial: Term,
    pub invariant: Term,
    pub steps: BTreeMap<String, Term>,
    pub examples: BTreeMap<String, TraceExample>,
}
#[derive(Clone, Debug)]
pub struct SpecComposition {
    /// Resolved component identities, including nested compositions. Each
    /// component occurs once; overlapping nested products are rejected.
    pub members: Vec<String>,
    pub examples: BTreeMap<String, TraceExample>,
}
#[derive(Clone, Debug)]
pub struct TraceStep {
    pub operation: String,
    pub inputs: Env,
    pub observe: Env,
}
#[derive(Clone, Debug)]
pub struct TraceExample {
    pub positive: bool,
    pub initial: Env,
    pub trace: Vec<TraceStep>,
}
#[derive(Clone, Debug)]
pub struct Specification {
    document: Value,
    inputs: Env,
    observations: Env,
    next_observations: Env,
    operations: BTreeSet<String>,
    components: BTreeMap<String, SpecComponent>,
    compositions: BTreeMap<String, SpecComposition>,
    implementation: Option<SpecImplementation>,
}
impl Specification {
    pub fn implementation(&self) -> Option<&SpecImplementation> {
        self.implementation.as_ref()
    }
    pub fn document(&self) -> &Value {
        &self.document
    }
    pub fn inputs(&self) -> &Env {
        &self.inputs
    }
    pub fn observations(&self) -> &Env {
        &self.observations
    }
    pub fn next_observations(&self) -> &Env {
        &self.next_observations
    }
    pub fn operations(&self) -> &BTreeSet<String> {
        &self.operations
    }
    pub fn components(&self) -> &BTreeMap<String, SpecComponent> {
        &self.components
    }
    pub fn compositions(&self) -> &BTreeMap<String, SpecComposition> {
        &self.compositions
    }
    pub fn from_json(doc: &Value) -> Result<Self> {
        at(
            "",
            keys(
                doc,
                &[
                    "version",
                    "kind",
                    "inputs",
                    "observations",
                    "operations",
                    "components",
                    "compositions",
                ],
                &["name", "implementation"],
            ),
        )?;
        if doc["version"] != 3 {
            return fail("/version", "specification requires version 3");
        }
        if doc["kind"] != "specification" {
            return fail("/kind", "expected specification");
        }
        if let Some(name) = doc.get("name") {
            at("/name", text(name))?;
        }
        let inputs = declarations(&doc["inputs"], "input", "/inputs")?;
        let observations = declarations(&doc["observations"], "observation", "/observations")?;
        let next_observations =
            declarations(&doc["observations"], "next_observation", "/observations")?;
        let mut operations = BTreeSet::new();
        for (name, op) in at("/operations", named(&doc["operations"]))? {
            at(&child("/operations", name), keys(op, &[], &[]))?;
            operations.insert(name.clone());
        }
        if operations.is_empty() {
            return fail("/operations", "at least one operation is required");
        }
        let definitions = at("/components", named(&doc["components"]))?;
        if definitions.is_empty() {
            return fail("/components", "at least one component is required");
        }
        let mut components = BTreeMap::new();
        let mut lower = Lower::default();
        for (index, (name, c)) in definitions.iter().enumerate() {
            let path = child("/components", name);
            at(
                &path,
                keys(c, &["state", "init", "invariant", "steps", "examples"], &[]),
            )?;
            let state = declarations(
                &c["state"],
                &format!("component{index}_state"),
                &child(&path, "state"),
            )?;
            let next_state = declarations(
                &c["state"],
                &format!("component{index}_next"),
                &child(&path, "state"),
            )?;
            let mut env = Env::new();
            extend_scope(&mut env, "s", &state);
            extend_scope(&mut env, "o", &observations);
            let initial = boolean(&c["init"], &env, &child(&path, "init"), &mut lower)?;
            let invariant = boolean(
                &c["invariant"],
                &env,
                &child(&path, "invariant"),
                &mut lower,
            )?;
            extend_scope(&mut env, "n", &next_state);
            extend_scope(&mut env, "no", &next_observations);
            extend_scope(&mut env, "i", &inputs);
            let defs = at(&child(&path, "steps"), named(&c["steps"]))?;
            if defs.keys().cloned().collect::<BTreeSet<_>>() != operations {
                return fail(&child(&path,"steps"), "steps must define every operation exactly; use an explicit relation for stuttering");
            }
            let steps = defs
                .iter()
                .map(|(op, expr)| {
                    Ok((
                        op.clone(),
                        boolean(expr, &env, &child(&child(&path, "steps"), op), &mut lower)?,
                    ))
                })
                .collect::<Result<_>>()?;
            let examples = examples(
                &c["examples"],
                &child(&path, "examples"),
                &inputs,
                &observations,
                &operations,
            )?;
            components.insert(
                name.clone(),
                SpecComponent {
                    state,
                    next_state,
                    initial,
                    invariant,
                    steps,
                    examples,
                },
            );
        }
        let products = at("/compositions", named(&doc["compositions"]))?;
        let mut direct = BTreeMap::new();
        let mut compositions = BTreeMap::new();
        for (name, p) in products {
            let path = child("/compositions", name);
            if components.contains_key(name) {
                return fail(&path, "component and composition names must be distinct");
            }
            at(&path, keys(p, &["members", "examples"], &[]))?;
            let values = p["members"].as_array().ok_or_else(|| ValidationError {
                path: child(&path, "members"),
                message: "members must be an array".into(),
            })?;
            if values.is_empty() {
                return fail(
                    &child(&path, "members"),
                    "composition must have at least one member",
                );
            }
            let members = values
                .iter()
                .enumerate()
                .map(|(index, v)| {
                    at(&format!("{path}/members/{index}"), text(v)).map(str::to_string)
                })
                .collect::<Result<Vec<_>>>()?;
            direct.insert(name.clone(), members);
            let examples = examples(
                &p["examples"],
                &child(&path, "examples"),
                &inputs,
                &observations,
                &operations,
            )?;
            compositions.insert(
                name.clone(),
                SpecComposition {
                    members: vec![],
                    examples,
                },
            );
        }
        fn resolve(
            name: &str,
            components: &BTreeMap<String, SpecComponent>,
            direct: &BTreeMap<String, Vec<String>>,
            active: &mut Vec<String>,
            found: &mut BTreeSet<String>,
            path: &str,
        ) -> Result<()> {
            if components.contains_key(name) {
                if !found.insert(name.into()) {
                    return fail(
                        path,
                        format!("duplicate component identity {name} in composition"),
                    );
                }
                return Ok(());
            }
            if active.iter().any(|x| x == name) {
                return fail(path, format!("composition cycle at {name}"));
            }
            let members = direct.get(name).ok_or_else(|| ValidationError {
                path: path.into(),
                message: format!("unknown member {name}"),
            })?;
            active.push(name.into());
            for m in members {
                resolve(m, components, direct, active, found, path)?;
            }
            active.pop();
            Ok(())
        }
        for (name, composition) in &mut compositions {
            let mut members = BTreeSet::new();
            resolve(
                name,
                &components,
                &direct,
                &mut vec![],
                &mut members,
                &format!("/compositions/{name}/members"),
            )?;
            composition.members = members.into_iter().collect();
        }
        let mut spec = Self {
            document: doc.clone(),
            inputs,
            observations,
            next_observations,
            operations,
            components,
            compositions,
            implementation: None,
        };
        spec.implementation = doc
            .get("implementation")
            .map(|value| implementation(value, &spec))
            .transpose()?;
        Ok(spec)
    }
}
fn extend_scope(env: &mut Env, prefix: &str, vars: &Env) {
    env.extend(
        vars.iter()
            .map(|(name, term)| (format!("{prefix}.{name}"), term.clone())),
    );
}
fn observations(value: &Value, path: &str, schema: &Env) -> Result<Env> {
    at(path, named(value))?
        .iter()
        .map(|(name, expr)| {
            let p = child(path, name);
            let declaration = schema.get(name).ok_or_else(|| ValidationError {
                path: p.clone(),
                message: format!("unknown observable/input {name}"),
            })?;
            // Closed expressions only: traces can never bind or refer to hidden state.
            let term = expression(expr, &Env::new(), &p, &mut Lower::default())?;
            if term.0.sort != declaration.0.sort {
                return fail(&p, "example assignment type mismatch");
            }
            Ok((name.clone(), term))
        })
        .collect()
}
fn examples(
    value: &Value,
    path: &str,
    inputs: &Env,
    obs: &Env,
    operations: &BTreeSet<String>,
) -> Result<BTreeMap<String, TraceExample>> {
    at(path, named(value))?
        .iter()
        .map(|(name, example)| {
            let path = child(path, name);
            at(&path, keys(example, &["expect", "initial", "trace"], &[]))?;
            let positive = match at(&child(&path, "expect"), text(&example["expect"]))? {
                "positive" => true,
                "negative" => false,
                _ => {
                    return fail(
                        &child(&path, "expect"),
                        "expect must be positive or negative",
                    )
                }
            };
            let initial = observations(&example["initial"], &child(&path, "initial"), obs)?;
            let steps = example["trace"].as_array().ok_or_else(|| ValidationError {
                path: child(&path, "trace"),
                message: "trace must be an array".into(),
            })?;
            if steps.len() > 4096 {
                return fail(
                    &child(&path, "trace"),
                    "trace exceeds 4096-step example limit",
                );
            }
            let trace = steps
                .iter()
                .enumerate()
                .map(|(index, step)| {
                    let path = format!("{path}/trace/{index}");
                    at(&path, keys(step, &["operation", "inputs", "observe"], &[]))?;
                    let operation = at(&child(&path, "operation"), text(&step["operation"]))?;
                    if !operations.contains(operation) {
                        return fail(
                            &child(&path, "operation"),
                            format!("unknown operation {operation}"),
                        );
                    }
                    let values = observations(&step["inputs"], &child(&path, "inputs"), inputs)?;
                    let observe = observations(&step["observe"], &child(&path, "observe"), obs)?;
                    Ok(TraceStep {
                        operation: operation.into(),
                        inputs: values,
                        observe,
                    })
                })
                .collect::<Result<_>>()?;
            Ok((
                name.clone(),
                TraceExample {
                    positive,
                    initial,
                    trace,
                },
            ))
        })
        .collect()
}

/// Capture-free substitution for typed, quantifier-free IR. Keys are complete
/// variable Terms, so spellings from unrelated namespaces cannot alias.
pub fn substitute(term: &Term, replacements: &BTreeMap<Term, Term>) -> Term {
    fn walk(
        term: &Term,
        replacements: &BTreeMap<Term, Term>,
        cache: &mut BTreeMap<Term, Term>,
    ) -> Term {
        if let Some(value) = replacements.get(term) {
            return value.clone();
        }
        if let Some(value) = cache.get(term) {
            return value.clone();
        }
        if term.0.args.is_empty() {
            return term.clone();
        }
        let result = node(
            term.0.sort.clone(),
            term.0.op.clone(),
            term.0
                .args
                .iter()
                .map(|t| walk(t, replacements, cache))
                .collect(),
        );
        cache.insert(term.clone(), result.clone());
        result
    }
    walk(term, replacements, &mut BTreeMap::new())
}

/// A state-only deterministic witness mapping into a relational product.
#[derive(Clone, Debug)]
pub struct SpecImplementation {
    pub composition: String,
    pub reset_input: String,
    pub machine: Machine,
    pub operations: BTreeMap<String, Term>,
    pub states: BTreeMap<String, Env>,
    pub observations: Env,
}
fn implementation(doc: &Value, spec: &Specification) -> Result<SpecImplementation> {
    let path = "/implementation";
    at(
        path,
        keys(
            doc,
            &[
                "composition",
                "reset_input",
                "state",
                "reset",
                "next",
                "operations",
                "binding",
            ],
            &["wires"],
        ),
    )?;
    let composition = at("/implementation/composition", text(&doc["composition"]))?;
    let target = spec
        .compositions()
        .get(composition)
        .ok_or_else(|| ValidationError {
            path: "/implementation/composition".into(),
            message: format!("unknown composition {composition}"),
        })?;
    let reset_input = at("/implementation/reset_input", text(&doc["reset_input"]))?;
    let reset = spec
        .inputs()
        .get(reset_input)
        .ok_or_else(|| ValidationError {
            path: "/implementation/reset_input".into(),
            message: "missing reset input".into(),
        })?;
    at("/implementation/reset_input", require_bool(reset.clone()))?;
    let state = declarations(&doc["state"], "implementation", "/implementation/state")?;
    let mut lower = Lower::default();
    let reset = crate::design::assignment(
        doc,
        "implementation",
        "reset",
        &state,
        spec.inputs(),
        &mut lower,
    )?;
    let next = crate::design::assignment(
        doc,
        "implementation",
        "next",
        &state,
        spec.inputs(),
        &mut lower,
    )?;
    let env = crate::design::model_wires(doc, "implementation", &state, spec.inputs(), &mut lower)?;
    let defs = at("/implementation/operations", named(&doc["operations"]))?;
    if defs.keys().cloned().collect::<BTreeSet<_>>() != *spec.operations() {
        return fail(
            "/implementation/operations",
            "selectors must define every operation exactly",
        );
    }
    let operations = defs
        .iter()
        .map(|(name, expr)| {
            Ok((
                name.clone(),
                boolean(
                    expr,
                    &env,
                    &child("/implementation/operations", name),
                    &mut lower,
                )?,
            ))
        })
        .collect::<Result<_>>()?;
    at(
        "/implementation/binding",
        keys(&doc["binding"], &["states", "observations"], &[]),
    )?;
    let bindings = at(
        "/implementation/binding/states",
        named(&doc["binding"]["states"]),
    )?;
    if bindings.keys().cloned().collect::<BTreeSet<_>>()
        != target.members.iter().cloned().collect::<BTreeSet<_>>()
    {
        return fail(
            "/implementation/binding/states",
            "binding must map exactly every selected component",
        );
    }
    let env = scope(&state, &Env::new());
    fn mapping(
        value: &Value,
        path: &str,
        schema: &Env,
        env: &Env,
        lower: &mut Lower,
    ) -> Result<Env> {
        let fields = at(path, named(value))?;
        if fields.keys().collect::<Vec<_>>() != schema.keys().collect::<Vec<_>>() {
            return fail(path, "binding must map every field exactly");
        }
        fields
            .iter()
            .map(|(name, expr)| {
                let p = child(path, name);
                let term = expression(expr, env, &p, lower)?;
                if term.0.sort != schema[name].0.sort {
                    return fail(&p, "binding type mismatch");
                }
                Ok((name.clone(), term))
            })
            .collect()
    }
    let states = bindings
        .iter()
        .map(|(name, value)| {
            Ok((
                name.clone(),
                mapping(
                    value,
                    &child("/implementation/binding/states", name),
                    &spec.components()[name].state,
                    &env,
                    &mut lower,
                )?,
            ))
        })
        .collect::<Result<_>>()?;
    let observations = mapping(
        &doc["binding"]["observations"],
        "/implementation/binding/observations",
        spec.observations(),
        &env,
        &mut lower,
    )?;
    Ok(SpecImplementation {
        composition: composition.into(),
        reset_input: reset_input.into(),
        machine: Machine {
            state,
            reset,
            next,
            outputs: Env::new(),
        },
        operations,
        states,
        observations,
    })
}
