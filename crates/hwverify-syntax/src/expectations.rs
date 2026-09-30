//! Source-only expectation blocks and lexical current/next references.
//! Definitions are resolved before IR validation; no solver or action semantics change.
use super::*;

const MAX_EXPECTATION_DEPTH: usize = 64;
const MAX_EXPANDED_NODES: usize = 262_144;

impl Lower<'_> {
    pub(super) fn expectation_block<'a, 't>(
        &self,
        entries: Vec<&'a g::Entry<'t>>,
        operator: &str,
        path: &str,
    ) -> Res<E>
    where
        't: 'a,
    {
        let span = self
            .spans
            .get(path)
            .cloned()
            .unwrap_or_else(|| self.spans[""].clone());
        self.expectation_clauses(entries, operator, span, 0)
    }

    fn expectation_clauses<'a, 't>(
        &self,
        entries: Vec<&'a g::Entry<'t>>,
        operator: &str,
        span: Span,
        depth: usize,
    ) -> Res<E>
    where
        't: 'a,
    {
        if depth > MAX_EXPECTATION_DEPTH {
            return Err(self.error(&span, "expectation blocks exceed nesting limit 64"));
        }
        let mut clauses = Vec::new();
        for entry in entries {
            match entry {
                g::Entry::Property(x) if id_token(&x.property.id).text() == "expect" => {
                    clauses.push(self.expr(&x.property.expr)?);
                }
                g::Entry::Block(x) => {
                    let token = id_token(&x.block.id);
                    let op = match token.text() {
                        "all" => "and",
                        "any" => "or",
                        _ => return Err(self.error(&self.span(token), "expectation blocks allow expect clauses and nested all/any blocks only")),
                    };
                    clauses.push(self.expectation_clauses(
                        x.block.block_list.iter().map(|e| &*e.entry).collect(),
                        op,
                        self.span(token),
                        depth + 1,
                    )?);
                }
                _ => {
                    return Err(self.error(
                        &span,
                        "expectation blocks allow expect clauses and nested all/any blocks only",
                    ))
                }
            }
        }
        match clauses.len() {
            0 => Err(self.error(&span, "empty expectation/operation/all/any block; write expect true explicitly for an unconstrained relation")),
            1 => Ok(clauses.pop().unwrap()),
            _ => {
                let mut clauses = clauses.into_iter();
                let first = clauses.next().unwrap();
                Ok(clauses.fold(first, |left, right| E::call(operator, vec![left, right], span.clone())))
            },
        }
    }

    fn expression_at(&self, value: &Value, path: &str) -> E {
        E {
            value: value.clone(),
            span: self
                .spans
                .get(path)
                .cloned()
                .unwrap_or_else(|| self.spans[""].clone()),
            descendants: self
                .spans
                .iter()
                .filter_map(|(key, span)| {
                    key.strip_prefix(&format!("{path}/"))
                        .map(|suffix| (format!("/{suffix}"), span.clone()))
                })
                .collect(),
        }
    }

    pub(super) fn scoped_relations(&mut self, spec: &mut Value, name: &str, path: &str) -> Res<()> {
        let declarations = spec
            .as_object_mut()
            .unwrap()
            .remove("expectations")
            .unwrap_or_else(|| json!({}));
        let definitions: BTreeMap<_, _> = declarations
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, value)| {
                (
                    name.clone(),
                    self.expression_at(value, &format!("{path}/expectations/{name}")),
                )
            })
            .collect();
        let names = |field: &str| -> BTreeSet<String> {
            spec[field]
                .as_object()
                .into_iter()
                .flat_map(|v| v.keys().cloned())
                .collect()
        };
        let mut resolver = Resolver {
            lower: self,
            states: names("state"),
            inputs: names("inputs"),
            outputs: names("outputs"),
            definitions,
            expanded: BTreeMap::new(),
            nodes: 0,
        };
        for (name, expression) in &resolver.definitions {
            if resolver.states.contains(name)
                || resolver.inputs.contains(name)
                || resolver.outputs.contains(name)
            {
                return Err(self.error(&expression.span, format!("expectation {name} shadows a local state or port; expectation names must be distinct")));
            }
        }
        // Visit the complete dependency graph before expanding anything, including
        // unreachable declarations. A cached expansion never hides a cycle.
        let mut order = Vec::new();
        let mut visiting = BTreeSet::new();
        let mut done = BTreeMap::new();
        for name in resolver.definitions.keys() {
            resolver.visit(name, &mut visiting, &mut done, &mut order, 0)?;
        }
        for name in order {
            let definition = resolver.definitions[&name].clone();
            let expanded = resolver.resolve(&definition, false)?;
            resolver.expanded.insert(name, expanded);
        }
        let mut relations = Vec::new();
        for field in ["init", "invariant"] {
            if let Some(value) = spec.get(field) {
                let target = format!("{path}/{field}");
                let expr = resolver.resolve(&self.expression_at(value, &target), true)?;
                relations.push((target, field.to_owned(), None, expr));
            }
        }
        for (operation, relation) in spec["operations"].as_object().unwrap() {
            let target = format!("{path}/operations/{operation}");
            let expr = resolver.resolve(&self.expression_at(relation, &target), false)?;
            relations.push((target, "operations".into(), Some(operation.clone()), expr));
        }
        let checks = resolver.expanded;
        for (target, field, operation, expr) in relations {
            self.record(&target, &expr);
            if let Some(operation) = operation {
                spec[&field][operation] = expr.value;
            } else {
                spec[&field] = expr.value;
            }
        }
        // Validate every named relation as a Boolean, even when unused or used
        // where a bitvector would otherwise have been accepted. Keep these
        // checks separate from real operations, action groups and canonical JSON.
        if !checks.is_empty() {
            let mut operations = Map::new();
            let mut spans = self.spans.clone();
            for (expectation, expression) in checks {
                let target = format!("{path}/operations/{expectation}");
                spans.insert(target.clone(), expression.span);
                spans.extend(
                    expression
                        .descendants
                        .into_iter()
                        .map(|(p, s)| (format!("{target}{p}"), s)),
                );
                operations.insert(expectation, expression.value);
            }
            let check = json!({
                "version": 4, "kind": "specification", "compositions": {},
                "specs": { name: {
                    "inputs": spec["inputs"], "outputs": spec["outputs"], "state": spec["state"],
                    "init": true, "invariant": true, "operations": operations, "examples": {}
                }}
            });
            self.expectation_checks.push(ParsedDocument {
                canonical: check,
                spans,
                filename: self.filename.into(),
                expectation_checks: Vec::new(),
            });
        }
        Ok(())
    }
}

struct Resolver<'a, 's> {
    lower: &'a Lower<'s>,
    states: BTreeSet<String>,
    inputs: BTreeSet<String>,
    outputs: BTreeSet<String>,
    definitions: BTreeMap<String, E>,
    expanded: BTreeMap<String, E>,
    nodes: usize,
}

impl Resolver<'_, '_> {
    fn visit(
        &self,
        name: &str,
        visiting: &mut BTreeSet<String>,
        done: &mut BTreeMap<String, usize>,
        order: &mut Vec<String>,
        depth: usize,
    ) -> Res<()> {
        if done.contains_key(name) {
            return Ok(());
        }
        let expression = &self.definitions[name];
        if depth > MAX_EXPECTATION_DEPTH {
            return Err(self.lower.error(
                &expression.span,
                "expectation dependency depth exceeds limit 64",
            ));
        }
        if !visiting.insert(name.into()) {
            return Err(self.lower.error(
                &expression.span,
                format!("cyclic expectation dependency involving {name}"),
            ));
        }
        let mut height = 1;
        for reference in references(&expression.value) {
            if self.definitions.contains_key(reference) {
                self.visit(reference, visiting, done, order, depth + 1)?;
                height = height.max(done[reference] + 1);
            }
        }
        if height > MAX_EXPECTATION_DEPTH {
            return Err(self.lower.error(
                &expression.span,
                "expectation dependency depth exceeds limit 64",
            ));
        }
        visiting.remove(name);
        done.insert(name.into(), height);
        order.push(name.into());
        Ok(())
    }

    fn charge(&mut self, nodes: usize, span: &Span) -> Res<()> {
        self.nodes = self.nodes.saturating_add(nodes);
        if self.nodes > MAX_EXPANDED_NODES {
            return Err(self.lower.error(
                span,
                "expanded scoped relations exceed limit 262144 expression nodes per spec",
            ));
        }
        Ok(())
    }

    fn resolve(&mut self, expression: &E, current_only: bool) -> Res<E> {
        self.resolve_value(&expression.value, "", expression, current_only)
    }

    fn resolve_value(
        &mut self,
        value: &Value,
        path: &str,
        source: &E,
        current_only: bool,
    ) -> Res<E> {
        let span = source.descendants.get(path).unwrap_or(&source.span).clone();
        self.charge(1, &span)?;
        if let Some(reference) = value.as_str() {
            if let Some(expanded) = self.expanded.get(reference) {
                if current_only
                    && references(&expanded.value)
                        .into_iter()
                        .any(|r| self.transition_only(r))
                {
                    return Err(self.lower.error(&span, format!("expectation {reference} uses next-state/output or input references, which are not allowed in init/invariant")));
                }
                let count = node_count(&expanded.value);
                self.charge(count, &span)?;
                let mut expression = self.expanded[reference].clone();
                expression.span = span;
                return Ok(expression);
            }
            let (name, prime) = reference
                .strip_suffix('\'')
                .map_or((reference, false), |n| (n, true));
            let state = self.states.contains(name);
            let input = self.inputs.contains(name);
            let output = self.outputs.contains(name);
            if !name.contains('.') && (state as u8 + input as u8 + output as u8 > 1) {
                return Err(self.lower.error(&span, format!("ambiguous local reference {reference}; use explicit s./n. for state or i./o./no. for ports")));
            }
            let resolved = if prime {
                if input {
                    return Err(self.lower.error(&span, format!("input {name} has no next value; primes are only valid on state/output names")));
                }
                if state {
                    format!("n.{name}")
                } else if output {
                    format!("no.{name}")
                } else {
                    return Err(self
                        .lower
                        .error(&span, format!("unknown primed local state/output {name}")));
                }
            } else if state && !name.contains('.') {
                format!("s.{name}")
            } else {
                reference.into()
            };
            if current_only && self.transition_only(&resolved) {
                return Err(self.lower.error(
                    &span,
                    "next-state/output and input references are not allowed in init/invariant",
                ));
            }
            return Ok(E::leaf(json!(resolved), span));
        }
        if let Some(values) = value.as_array() {
            let mut args = Vec::new();
            for (index, arg) in values.iter().enumerate().skip(1) {
                args.push(self.resolve_value(
                    arg,
                    &format!("{path}/{index}"),
                    source,
                    current_only,
                )?);
            }
            return Ok(E::call(
                values[0].as_str().expect("source calls have operators"),
                args,
                span,
            ));
        }
        Ok(E::leaf(value.clone(), span))
    }

    fn transition_only(&self, reference: &str) -> bool {
        reference.starts_with("n.")
            || reference.starts_with("no.")
            || reference.starts_with("i.")
            || self.inputs.contains(reference)
    }
}

fn references(value: &Value) -> Vec<&str> {
    if let Some(reference) = value.as_str() {
        return vec![reference];
    }
    value
        .as_array()
        .into_iter()
        .flat_map(|values| values.iter().skip(1))
        .flat_map(references)
        .collect()
}

fn node_count(value: &Value) -> usize {
    1 + value
        .as_array()
        .into_iter()
        .flat_map(|values| values.iter().skip(1))
        .map(node_count)
        .sum::<usize>()
}
