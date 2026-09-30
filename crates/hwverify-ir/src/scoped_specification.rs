//! Version 4 specifications have lexical port scopes and fresh state per instance.
//! Elaboration is pure: independent exported actions become Boolean activation
//! inputs of one logical transition. Private state stutters on inactive leaves.
use crate::design::{at, child, declarations, fail};
use crate::{keys, named, text, Env, Sort, SpecImplementation, Specification, ValidationError};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

type Result<T> = std::result::Result<T, ValidationError>;
type Ports = BTreeMap<String, Sort>;
type Connections = BTreeMap<String, String>;
const MAX_COMPOSITION_DEPTH: usize = 64;
const MAX_TARGET_LEAVES: usize = 4096;
const MAX_DOCUMENT_LEAVES: usize = 16_384;

#[derive(Clone, Debug)]
pub struct ScopedSpecification {
    document: Value,
    targets: BTreeMap<String, ScopedTarget>,
}

#[derive(Clone, Debug)]
pub struct ScopedTarget {
    /// Either "spec" or "composition", matching the source declaration.
    pub kind: &'static str,
    /// Trace-only v3 elaboration with one composition and one hygienic tick.
    /// Implementation controls and the witness are validated separately.
    pub specification: Specification,
    /// Generated component identity to the full, dot-separated leaf instance path.
    /// Standalone specifications use the path "self".
    pub instance_paths: BTreeMap<String, String>,
    /// Exported action to its hygienic Boolean activation-input field.
    pub action_inputs: BTreeMap<String, String>,
    /// Leaf identity -> local operation -> exported actions activating it.
    /// Multiple exported actions reaching the same local operation are one activation.
    pub leaf_operations: BTreeMap<String, BTreeMap<String, BTreeSet<String>>>,
    /// Separately validated deterministic witness, absent from the trace specification.
    pub implementation: Option<SpecImplementation>,
    /// Actual witness inputs, including implementation-only controls, without synthetics.
    pub implementation_inputs: Env,
}

struct Definition<'a> {
    kind: &'static str,
    value: &'a Value,
    path: String,
    inputs: Ports,
    outputs: Ports,
}

#[derive(Clone)]
struct Summary {
    operations: BTreeSet<String>,
    leaves: usize,
    depth: usize,
}

impl ScopedSpecification {
    pub fn document(&self) -> &Value {
        &self.document
    }

    pub fn targets(&self) -> &BTreeMap<String, ScopedTarget> {
        &self.targets
    }

    pub fn from_json(document: &Value) -> Result<Self> {
        at(
            "",
            keys(
                document,
                &["version", "kind", "specs", "compositions"],
                &["name", "implementation"],
            ),
        )?;
        if document["version"] != 4 {
            return fail("/version", "scoped specification requires version 4");
        }
        if document["kind"] != "specification" {
            return fail("/kind", "expected specification");
        }
        if let Some(name) = document.get("name") {
            at("/name", text(name))?;
        }
        let mut definitions = BTreeMap::new();
        for (field, kind) in [("specs", "spec"), ("compositions", "composition")] {
            let path = child("", field);
            for (name, value) in at(&path, named(&document[field]))? {
                let path = child(&path, name);
                if definitions.contains_key(name) {
                    return fail(&path, "spec and composition names must be distinct");
                }
                let required: &[&str] = if kind == "spec" {
                    &[
                        "inputs",
                        "outputs",
                        "state",
                        "init",
                        "invariant",
                        "operations",
                        "examples",
                    ]
                } else {
                    &["inputs", "outputs", "instances", "examples"]
                };
                let optional: &[&str] = if kind == "composition" {
                    &["operations"]
                } else {
                    &[]
                };
                at(&path, keys(value, required, optional))?;
                let inputs = ports(&value["inputs"], &child(&path, "inputs"))?;
                let outputs = ports(&value["outputs"], &child(&path, "outputs"))?;
                if let Some(name) = inputs.keys().find(|name| outputs.contains_key(*name)) {
                    return fail(
                        &child(&child(&path, "outputs"), name),
                        "input and output names must be distinct within an interface",
                    );
                }
                if kind == "spec" {
                    let operations = at(&child(&path, "operations"), named(&value["operations"]))?;
                    if operations.is_empty() {
                        return fail(
                            &child(&path, "operations"),
                            "at least one operation is required",
                        );
                    }
                } else {
                    let instances = at(&child(&path, "instances"), named(&value["instances"]))?;
                    if instances.is_empty() {
                        return fail(
                            &child(&path, "instances"),
                            "composition must have at least one instance",
                        );
                    }
                    for (alias, instance) in instances {
                        let path = child(&child(&path, "instances"), alias);
                        at(&path, keys(instance, &["target", "connections"], &[]))?;
                        at(&child(&path, "target"), text(&instance["target"]))?;
                        at(
                            &child(&path, "connections"),
                            named(&instance["connections"]),
                        )?;
                    }
                }
                definitions.insert(
                    name.clone(),
                    Definition {
                        kind,
                        value,
                        path,
                        inputs,
                        outputs,
                    },
                );
            }
        }
        if definitions.is_empty() {
            return fail("/specs", "at least one spec or composition is required");
        }
        // Validate the whole graph before expansion, including unused templates.
        validate_connections(&definitions)?;
        let mut summaries = BTreeMap::new();
        for name in definitions.keys() {
            summarize(name, &definitions, &mut summaries, &mut Vec::new())?;
        }
        let mut total_leaves = 0;
        for (name, summary) in &summaries {
            total_leaves += summary.leaves;
            if total_leaves > MAX_DOCUMENT_LEAVES {
                return fail(
                    &definitions[name].path,
                    "document exceeds 16384 elaborated-leaf limit",
                );
            }
        }
        // Validate original Boolean relations before introducing guards. This also
        // diagnoses inactive or unexported operations at their exact source paths.
        for definition in definitions.values().filter(|d| d.kind == "spec") {
            validate_leaf(definition)?;
        }
        let implementation_target = implementation_target(document, &definitions)?;
        let mut reserved = BTreeSet::new();
        reserve_names(document, &mut reserved);
        let tick = fresh_name("__scoped_tick", &mut reserved);
        let mut targets = BTreeMap::new();
        let mut action_expansions = BTreeMap::new();
        for (name, definition) in &definitions {
            let mut elaboration = Elaboration {
                target: name,
                definitions: &definitions,
                components: Map::new(),
                instance_paths: BTreeMap::new(),
                source_paths: Vec::new(),
            };
            elaboration.expand(
                name,
                if definition.kind == "spec" {
                    "self"
                } else {
                    ""
                },
                &identity_connections(&definition.inputs),
                &identity_connections(&definition.outputs),
            )?;
            let members: Vec<_> = elaboration.components.keys().cloned().collect();
            let mut target_reserved = reserved.clone();
            let action_inputs: BTreeMap<_, _> = summaries[name]
                .operations
                .iter()
                .enumerate()
                .map(|(index, action)| {
                    (
                        action.clone(),
                        fresh_name(&format!("__scoped_action_{index}"), &mut target_reserved),
                    )
                })
                .collect();
            let by_path: BTreeMap<_, _> = elaboration
                .instance_paths
                .iter()
                .map(|(id, path)| (path.clone(), id.clone()))
                .collect();
            let mut leaf_operations: BTreeMap<_, BTreeMap<_, BTreeSet<String>>> = elaboration
                .components
                .iter()
                .map(|(id, component)| {
                    (
                        id.clone(),
                        component["steps"]
                            .as_object()
                            .unwrap()
                            .keys()
                            .map(|op| (op.clone(), BTreeSet::new()))
                            .collect(),
                    )
                })
                .collect();
            for action in action_inputs.keys() {
                let activations = expand_action(name, action, &definitions, &mut action_expansions);
                for (path, operation) in activations {
                    let path = if path.is_empty() { "self" } else { &path };
                    leaf_operations
                        .get_mut(&by_path[path])
                        .unwrap()
                        .get_mut(&operation)
                        .unwrap()
                        .insert(action.clone());
                }
            }
            for (id, component) in &mut elaboration.components {
                component["steps"] =
                    json!({&tick: guarded_step(component, &leaf_operations[id], &action_inputs)});
            }
            let examples = rewrite_examples(
                &definition.value["examples"],
                &child(&definition.path, "examples"),
                &definition.inputs,
                &action_inputs,
                &tick,
            )?;
            let mut trace_inputs = definition.value["inputs"].as_object().unwrap().clone();
            trace_inputs.extend(
                action_inputs
                    .values()
                    .map(|name| (name.clone(), json!("bool"))),
            );
            let mut legacy = json!({
                "version": 3, "kind": "specification",
                "inputs": trace_inputs,
                "observations": definition.value["outputs"],
                "operations": {&tick: {}},
                "components": elaboration.components,
                "compositions": {name: {"members": members, "examples": examples}}
            });
            if let Some(label) = document.get("name") {
                legacy["name"] = label.clone();
            }
            elaboration.source_paths.extend([
                ("/inputs".into(), child(&definition.path, "inputs")),
                ("/observations".into(), child(&definition.path, "outputs")),
                (
                    format!("/compositions/{name}/examples"),
                    child(&definition.path, "examples"),
                ),
            ]);
            let specification = Specification::from_json(&legacy)
                .map_err(|error| remap_error(error, &elaboration.source_paths))?;
            let mut implementation = None;
            let mut implementation_inputs = Env::new();
            if implementation_target == Some(name.as_str()) {
                // Dummy relations validate the implementation independently of
                // trace controls, retaining identical leaf/state/output identities.
                let mut witness = legacy.clone();
                witness["operations"] = Value::Object(
                    action_inputs
                        .keys()
                        .map(|op| (op.clone(), json!({})))
                        .collect(),
                );
                witness["compositions"][name]["examples"] = json!({});
                for component in witness["components"].as_object_mut().unwrap().values_mut() {
                    component["init"] = json!(true);
                    component["invariant"] = json!(true);
                    component["steps"] = Value::Object(
                        action_inputs
                            .keys()
                            .map(|op| (op.clone(), json!(true)))
                            .collect(),
                    );
                }

                let mut witness_paths = elaboration.source_paths.clone();
                witness_paths.retain(|(legacy, _)| legacy != "/inputs");
                witness_paths.push(("/inputs".into(), "/implementation/inputs".into()));
                attach_implementation(
                    document,
                    definition,
                    &elaboration.instance_paths,
                    &mut witness,
                    &mut witness_paths,
                )?;
                let validated = Specification::from_json(&witness)
                    .map_err(|error| remap_error(error, &witness_paths))?;
                implementation = validated.implementation().cloned();
                implementation_inputs = validated.inputs().clone();
            }
            targets.insert(
                name.clone(),
                ScopedTarget {
                    kind: definition.kind,
                    specification,
                    instance_paths: elaboration.instance_paths,
                    action_inputs,
                    leaf_operations,
                    implementation,
                    implementation_inputs,
                },
            );
        }
        Ok(Self {
            document: document.clone(),
            targets,
        })
    }
}

// Reusing the same relative expansion avoids exponential work through redundant
// nested export groups. Identity is the pair (complete leaf path, local operation).
type Activations = BTreeSet<(String, String)>;
fn expand_action(
    name: &str,
    action: &str,
    definitions: &BTreeMap<String, Definition<'_>>,
    cache: &mut BTreeMap<(String, String), Activations>,
) -> Activations {
    let key = (name.to_string(), action.to_string());
    if let Some(found) = cache.get(&key) {
        return found.clone();
    }
    let definition = &definitions[name];
    let result = if definition.kind == "spec" {
        BTreeSet::from([(String::new(), action.to_string())])
    } else {
        let references: BTreeSet<(String, String)> =
            if let Some(groups) = definition.value.get("operations") {
                groups[action]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|reference| {
                        let (alias, op) = reference.as_str().unwrap().split_once('.').unwrap();
                        (alias.into(), op.into())
                    })
                    .collect()
            } else {
                definition.value["instances"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(|alias| (alias.clone(), action.into()))
                    .collect()
            };
        let mut result = BTreeSet::new();
        for (alias, op) in references {
            let target = definition.value["instances"][&alias]["target"]
                .as_str()
                .unwrap();
            for (path, operation) in expand_action(target, &op, definitions, cache) {
                result.insert((
                    if path.is_empty() {
                        alias.clone()
                    } else {
                        format!("{alias}.{path}")
                    },
                    operation,
                ));
            }
        }
        result
    };
    cache.insert(key, result.clone());
    result
}

fn reserve_names(value: &Value, names: &mut BTreeSet<String>) {
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                names.insert(name.clone());
                reserve_names(value, names);
            }
        }
        Value::Array(values) => {
            for value in values {
                reserve_names(value, names);
            }
        }
        _ => {}
    }
}

fn fresh_name(base: &str, reserved: &mut BTreeSet<String>) -> String {
    let mut candidate = base.to_string();
    while !reserved.insert(candidate.clone()) {
        candidate.push('_');
    }
    candidate
}

// Balanced binary expressions keep generated AST depth logarithmic.
fn combine(operator: &str, values: &[Value], identity: bool) -> Value {
    match values.len() {
        0 => json!(identity),
        1 => values[0].clone(),
        len => json!([
            operator,
            combine(operator, &values[..len / 2], identity),
            combine(operator, &values[len / 2..], identity)
        ]),
    }
}

fn guarded_step(
    component: &Value,
    activators: &BTreeMap<String, BTreeSet<String>>,
    action_inputs: &BTreeMap<String, String>,
) -> Value {
    let guards: Vec<_> = activators
        .iter()
        .map(|(operation, actions)| {
            (
                operation,
                combine(
                    "or",
                    &actions
                        .iter()
                        .map(|action| json!(format!("i.{}", action_inputs[action])))
                        .collect::<Vec<_>>(),
                    false,
                ),
            )
        })
        .collect();
    let mut clauses = Vec::new();
    for (operation, guard) in &guards {
        clauses.push(json!(["implies", guard, component["steps"][*operation]]));
    }
    for (index, (_, first)) in guards.iter().enumerate() {
        for (_, second) in &guards[index + 1..] {
            clauses.push(json!(["not", ["and", first, second]]));
        }
    }
    let any_active = combine(
        "or",
        &guards
            .iter()
            .map(|(_, guard)| guard.clone())
            .collect::<Vec<_>>(),
        false,
    );
    let holds: Vec<_> = component["state"]
        .as_object()
        .unwrap()
        .keys()
        .map(|name| json!(["eq", format!("n.{name}"), format!("s.{name}")]))
        .collect();
    // Outputs remain relational and are constrained only by user relations and
    // current/next invariants. Inactivity holds PRIVATE state fields only.
    clauses.push(json!([
        "implies",
        ["not", any_active],
        combine("and", &holds, true)
    ]));
    combine("and", &clauses, true)
}

fn validate_leaf(definition: &Definition<'_>) -> Result<()> {
    let inputs = identity_connections(&definition.inputs);
    let outputs = identity_connections(&definition.outputs);
    let mut steps = Map::new();
    for (operation, relation) in definition.value["operations"].as_object().unwrap() {
        steps.insert(
            operation.clone(),
            rewrite_expression(
                relation,
                &inputs,
                &outputs,
                &child(&child(&definition.path, "operations"), operation),
            )?,
        );
    }
    let operations: Map<_, _> = steps.keys().map(|op| (op.clone(), json!({}))).collect();
    let legacy = json!({
        "version": 3, "kind": "specification", "inputs": definition.value["inputs"],
        "observations": definition.value["outputs"], "operations": operations,
        "components": {"leaf": {
            "state": definition.value["state"],
            "init": rewrite_expression(&definition.value["init"], &inputs, &outputs, &child(&definition.path, "init"))?,
            "invariant": rewrite_expression(&definition.value["invariant"], &inputs, &outputs, &child(&definition.path, "invariant"))?,
            "steps": steps, "examples": {}
        }}, "compositions": {}
    });
    let paths: Vec<_> = [
        ("state", "state"),
        ("init", "init"),
        ("invariant", "invariant"),
        ("steps", "operations"),
    ]
    .into_iter()
    .map(|(legacy, source)| {
        (
            format!("/components/leaf/{legacy}"),
            child(&definition.path, source),
        )
    })
    .collect();
    Specification::from_json(&legacy).map_err(|error| remap_error(error, &paths))?;
    Ok(())
}

fn rewrite_examples(
    value: &Value,
    path: &str,
    public_inputs: &Ports,
    action_inputs: &BTreeMap<String, String>,
    tick: &str,
) -> Result<Value> {
    let mut examples = Map::new();
    for (name, example) in at(path, named(value))? {
        let path = child(path, name);
        at(&path, keys(example, &["expect", "initial", "trace"], &[]))?;
        let trace_path = child(&path, "trace");
        let trace = example["trace"].as_array().ok_or_else(|| ValidationError {
            path: trace_path.clone(),
            message: "trace must be an array".into(),
        })?;
        if trace.len() > 4096 {
            return fail(&trace_path, "trace exceeds 4096-step example limit");
        }
        let mut rewritten = Vec::new();
        for (index, frame) in trace.iter().enumerate() {
            let path = child(&trace_path, &index.to_string());
            at(
                &path,
                keys(frame, &["inputs", "observe"], &["operation", "actions"]),
            )?;
            if frame.get("operation").is_some() == frame.get("actions").is_some() {
                return fail(
                    &path,
                    "trace frame must specify exactly one of operation or actions",
                );
            }
            let mut actions = BTreeSet::new();
            if let Some(operation) = frame.get("operation") {
                let path = child(&path, "operation");
                let operation = at(&path, text(operation))?;
                if !action_inputs.contains_key(operation) {
                    return fail(&path, format!("unknown operation {operation}"));
                }
                actions.insert(operation.to_string());
            } else {
                let action_path = child(&path, "actions");
                let values = frame["actions"].as_array().ok_or_else(|| ValidationError {
                    path: action_path.clone(),
                    message: "actions must be an array".into(),
                })?;
                for (index, action) in values.iter().enumerate() {
                    let path = child(&action_path, &index.to_string());
                    let action = at(&path, text(action))?;
                    if !action_inputs.contains_key(action) {
                        return fail(&path, format!("unknown action {action}"));
                    }
                    if !actions.insert(action.to_string()) {
                        return fail(&path, format!("duplicate action {action}"));
                    }
                }
            }
            let input_path = child(&path, "inputs");
            let mut inputs = at(&input_path, named(&frame["inputs"]))?.clone();
            for input in inputs.keys() {
                if !public_inputs.contains_key(input) {
                    return fail(
                        &child(&input_path, input),
                        format!("unknown observable/input {input}"),
                    );
                }
            }
            inputs.extend(
                action_inputs
                    .iter()
                    .map(|(action, input)| (input.clone(), json!(actions.contains(action)))),
            );
            rewritten
                .push(json!({"operation": tick, "inputs": inputs, "observe": frame["observe"]}));
        }
        let mut example = example.clone();
        example["trace"] = Value::Array(rewritten);
        examples.insert(name.clone(), example);
    }
    Ok(Value::Object(examples))
}

fn ports(value: &Value, path: &str) -> Result<Ports> {
    Ok(declarations(value, "port", path)?
        .into_iter()
        .map(|(name, term)| (name, term.0.sort.clone()))
        .collect())
}

fn identity_connections(ports: &Ports) -> Connections {
    ports
        .keys()
        .map(|name| (name.clone(), name.clone()))
        .collect()
}

fn validate_connections(definitions: &BTreeMap<String, Definition<'_>>) -> Result<()> {
    for definition in definitions
        .values()
        .filter(|definition| definition.kind == "composition")
    {
        for (alias, instance) in definition.value["instances"].as_object().unwrap() {
            let path = child(&child(&definition.path, "instances"), alias);
            let target_name = instance["target"].as_str().unwrap();
            let target = definitions
                .get(target_name)
                .ok_or_else(|| ValidationError {
                    path: child(&path, "target"),
                    message: format!("unknown target {target_name}"),
                })?;
            let path = child(&path, "connections");
            let connections = instance["connections"].as_object().unwrap();
            for port in target.inputs.keys().chain(target.outputs.keys()) {
                if !connections.contains_key(port) {
                    return fail(&path, format!("missing connection for port {port}"));
                }
            }
            for (port, connection) in connections {
                let path = child(&path, port);
                let (sort, enclosing, opposite, direction) =
                    if let Some(sort) = target.inputs.get(port) {
                        (sort, &definition.inputs, &definition.outputs, "input")
                    } else if let Some(sort) = target.outputs.get(port) {
                        (sort, &definition.outputs, &definition.inputs, "output")
                    } else {
                        return fail(&path, format!("unknown child port {port}"));
                    };
                let parent = at(&path, text(connection))?;
                let parent_sort = enclosing.get(parent).ok_or_else(|| ValidationError {
                    path: path.clone(),
                    message: if opposite.contains_key(parent) {
                        format!("connection direction mismatch: child {direction} {port} must map to enclosing {direction}")
                    } else {
                        format!("unknown enclosing {direction} port {parent}")
                    },
                })?;
                if sort != parent_sort {
                    return fail(&path, format!("connection type mismatch for {port}: expected {sort:?}, found {parent_sort:?}"));
                }
            }
        }
    }
    Ok(())
}

fn summarize(
    name: &str,
    definitions: &BTreeMap<String, Definition<'_>>,
    summaries: &mut BTreeMap<String, Summary>,
    active: &mut Vec<String>,
) -> Result<Summary> {
    if let Some(summary) = summaries.get(name) {
        return Ok(summary.clone());
    }
    let definition = &definitions[name];
    if active.iter().any(|current| current == name) {
        let mut cycle = active.clone();
        cycle.push(name.into());
        return fail(
            &definition.path,
            format!("composition cycle: {}", cycle.join(" -> ")),
        );
    }
    if active.len() > MAX_COMPOSITION_DEPTH {
        return fail(
            &definition.path,
            "composition exceeds 64-level nesting limit",
        );
    }
    let summary = if definition.kind == "spec" {
        Summary {
            operations: definition.value["operations"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect(),
            leaves: 1,
            depth: 0,
        }
    } else {
        active.push(name.into());
        let mut operations = None;
        let mut leaves = 0;
        let mut depth = 0;
        for (alias, instance) in definition.value["instances"].as_object().unwrap() {
            let summary = summarize(
                instance["target"].as_str().unwrap(),
                definitions,
                summaries,
                active,
            )?;
            let path = child(&child(&definition.path, "instances"), alias);
            if definition.value.get("operations").is_none()
                && operations
                    .as_ref()
                    .is_some_and(|previous| previous != &summary.operations)
            {
                return fail(
                    &path,
                    "all instances must expose identical nonempty operation sets",
                );
            }
            operations = Some(summary.operations);
            leaves += summary.leaves;
            depth = depth.max(summary.depth + 1);
            if leaves > MAX_TARGET_LEAVES {
                return fail(&definition.path, "target exceeds 4096-leaf expansion limit");
            }
            if depth > MAX_COMPOSITION_DEPTH {
                return fail(
                    &definition.path,
                    "composition exceeds 64-level nesting limit",
                );
            }
        }
        active.pop();
        let operations = if let Some(groups) = definition.value.get("operations") {
            let path = child(&definition.path, "operations");
            let groups = at(&path, named(groups))?;
            if groups.is_empty() {
                return fail(&path, "at least one operation is required");
            }
            for (action, group) in groups {
                let path = child(&path, action);
                let refs = group.as_array().ok_or_else(|| ValidationError {
                    path: path.clone(),
                    message: "operation group must be an array".into(),
                })?;
                if refs.is_empty() {
                    return fail(&path, "operation group must be nonempty; use an empty trace actions list for idle");
                }
                for (index, reference) in refs.iter().enumerate() {
                    let path = child(&path, &index.to_string());
                    let reference = at(&path, text(reference))?;
                    let parts: Vec<_> = reference.split('.').collect();
                    if parts.len() != 2 || parts.iter().any(|part| part.is_empty()) {
                        return fail(&path, "operation reference must be direct_child.operation");
                    }
                    let instance =
                        definition.value["instances"].get(parts[0]).ok_or_else(|| {
                            ValidationError {
                                path: path.clone(),
                                message: format!("unknown direct child {}", parts[0]),
                            }
                        })?;
                    let target = instance["target"].as_str().unwrap();
                    if !summaries[target].operations.contains(parts[1]) {
                        return fail(
                            &path,
                            format!(
                                "unknown exported operation {} on child {}",
                                parts[1], parts[0]
                            ),
                        );
                    }
                }
            }
            groups.keys().cloned().collect()
        } else {
            operations.unwrap()
        };
        Summary {
            operations,
            leaves,
            depth,
        }
    };
    summaries.insert(name.into(), summary.clone());
    Ok(summary)
}

struct Elaboration<'a, 'd> {
    target: &'a str,
    definitions: &'a BTreeMap<String, Definition<'d>>,
    components: Map<String, Value>,
    instance_paths: BTreeMap<String, String>,
    source_paths: Vec<(String, String)>,
}

impl Elaboration<'_, '_> {
    fn expand(
        &mut self,
        name: &str,
        instance_path: &str,
        inputs: &Connections,
        outputs: &Connections,
    ) -> Result<()> {
        let definition = &self.definitions[name];
        if definition.kind == "spec" {
            let mut id = format!("__scoped_leaf_{}", self.components.len());
            if id == self.target {
                id.push('_');
            }
            let mut steps = Map::new();
            for (operation, relation) in definition.value["operations"].as_object().unwrap() {
                steps.insert(
                    operation.clone(),
                    rewrite_expression(
                        relation,
                        inputs,
                        outputs,
                        &child(&child(&definition.path, "operations"), operation),
                    )?,
                );
            }
            let component = json!({
                "state": definition.value["state"],
                "init": rewrite_expression(&definition.value["init"], inputs, outputs, &child(&definition.path, "init"))?,
                "invariant": rewrite_expression(&definition.value["invariant"], inputs, outputs, &child(&definition.path, "invariant"))?,
                "steps": steps,
                "examples": {}
            });
            for (legacy, source) in [
                ("state", "state"),
                ("init", "init"),
                ("invariant", "invariant"),
                ("steps", "operations"),
            ] {
                self.source_paths.push((
                    format!("/components/{id}/{legacy}"),
                    child(&definition.path, source),
                ));
            }
            self.components.insert(id.clone(), component);
            self.instance_paths.insert(id, instance_path.into());
        } else {
            for (alias, instance) in definition.value["instances"].as_object().unwrap() {
                let name = instance["target"].as_str().unwrap();
                let target = &self.definitions[name];
                let connections = instance["connections"].as_object().unwrap();
                let connect = |ports: &Ports, parent: &Connections| -> Connections {
                    ports
                        .keys()
                        .map(|port| {
                            (
                                port.clone(),
                                parent[connections[port].as_str().unwrap()].clone(),
                            )
                        })
                        .collect()
                };
                let nested_inputs = connect(&target.inputs, inputs);
                let nested_outputs = connect(&target.outputs, outputs);
                let path = if instance_path.is_empty() {
                    alias.clone()
                } else {
                    format!("{instance_path}.{alias}")
                };
                self.expand(name, &path, &nested_inputs, &nested_outputs)?;
            }
        }
        Ok(())
    }
}

// Only expression positions are traversed: operator names, widths, and literal
// payloads retain their complete source ASTs and are checked by the v3 validator.
fn rewrite_expression(
    value: &Value,
    inputs: &Connections,
    outputs: &Connections,
    path: &str,
) -> Result<Value> {
    if let Some(reference) = value.as_str() {
        let replacement = if let Some(name) = inputs.get(reference) {
            format!("i.{name}")
        } else if let Some(name) = outputs.get(reference) {
            format!("o.{name}")
        } else if reference.starts_with("s.") || reference.starts_with("n.") {
            reference.into()
        } else if let Some((scope, name)) = reference.split_once('.') {
            let ports = match scope {
                "i" => inputs,
                "o" | "no" => outputs,
                _ => return fail(path, format!("unknown reference {reference}")),
            };
            let name = ports.get(name).ok_or_else(|| ValidationError {
                path: path.into(),
                message: format!("unknown local reference {reference}"),
            })?;
            format!("{scope}.{name}")
        } else {
            return fail(path, format!("unknown local reference {reference}"));
        };
        return Ok(Value::String(replacement));
    }
    if let Some(array) = value.as_array() {
        let start = match array.first().and_then(Value::as_str) {
            Some("bv") => array.len(),
            Some("const_mem" | "zext" | "sext") => 2,
            Some("extract") => 3,
            _ => 1,
        };
        return array
            .iter()
            .enumerate()
            .map(|(index, value)| {
                if index < start {
                    Ok(value.clone())
                } else {
                    rewrite_expression(value, inputs, outputs, &child(path, &index.to_string()))
                }
            })
            .collect::<Result<Vec<_>>>()
            .map(Value::Array);
    }
    Ok(value.clone())
}

fn implementation_target<'a>(
    document: &'a Value,
    definitions: &BTreeMap<String, Definition<'_>>,
) -> Result<Option<&'a str>> {
    let Some(implementation) = document.get("implementation") else {
        return Ok(None);
    };
    at(
        "/implementation",
        keys(
            implementation,
            &[
                "composition",
                "inputs",
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
    let name = at(
        "/implementation/composition",
        text(&implementation["composition"]),
    )?;
    if !definitions
        .get(name)
        .is_some_and(|definition| definition.kind == "composition")
    {
        return fail(
            "/implementation/composition",
            format!("unknown composition {name}"),
        );
    }
    at(
        "/implementation/binding",
        keys(&implementation["binding"], &["states", "outputs"], &[]),
    )?;
    Ok(Some(name))
}

fn attach_implementation(
    document: &Value,
    definition: &Definition<'_>,
    instance_paths: &BTreeMap<String, String>,
    legacy: &mut Value,
    source_paths: &mut Vec<(String, String)>,
) -> Result<()> {
    let implementation = &document["implementation"];
    let inputs = ports(&implementation["inputs"], "/implementation/inputs")?;
    for (name, sort) in &definition.inputs {
        match inputs.get(name) {
            None => {
                return fail(
                    "/implementation/inputs",
                    format!("missing target input {name}"),
                )
            }
            Some(found) if found != sort => {
                return fail(
                    &child("/implementation/inputs", name),
                    "implementation input type must match target input",
                )
            }
            _ => {}
        }
    }
    // Dotted leaf paths are deliberate here; legacy named-record restrictions
    // apply only after translating them to generated simple component names.
    let states = implementation["binding"]["states"]
        .as_object()
        .ok_or_else(|| ValidationError {
            path: "/implementation/binding/states".into(),
            message: "expected leaf-path record".into(),
        })?;
    let expected: BTreeSet<_> = instance_paths.values().collect();
    if states.keys().collect::<BTreeSet<_>>() != expected {
        return fail(
            "/implementation/binding/states",
            "binding must map exactly every instantiated leaf path",
        );
    }
    let mut translated_states = Map::new();
    for (id, path) in instance_paths {
        translated_states.insert(id.clone(), states[path].clone());
        source_paths.push((
            child("/implementation/binding/states", id),
            child("/implementation/binding/states", path),
        ));
    }
    let mut translated = implementation.as_object().unwrap().clone();
    translated.remove("inputs");
    translated.insert(
        "binding".into(),
        json!({"states": translated_states, "observations": implementation["binding"]["outputs"]}),
    );
    legacy["inputs"] = implementation["inputs"].clone();
    legacy["implementation"] = Value::Object(translated);
    source_paths.push((
        "/implementation/binding/observations".into(),
        "/implementation/binding/outputs".into(),
    ));
    Ok(())
}

fn remap_error(mut error: ValidationError, source_paths: &[(String, String)]) -> ValidationError {
    if let Some((legacy, source)) = source_paths
        .iter()
        .filter(|(legacy, _)| {
            error.path == *legacy
                || error
                    .path
                    .strip_prefix(legacy)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
        .max_by_key(|(legacy, _)| legacy.len())
    {
        error.path = format!("{source}{}", &error.path[legacy.len()..]);
    }
    error
}

#[cfg(test)]
mod tests;
