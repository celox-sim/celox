//! Ordered example binders and concise input/output trace assertions.
use super::*;

impl Lower<'_> {
    /// Source spelling only: ordinary lexical binder names keep the established
    /// q.<name> canonical representation and therefore the same typed formulas.
    pub(super) fn resolve_example_bindings(&self, document: &mut Value) -> Res<()> {
        let scoped = document["version"] == 4;
        let groups: &[&str] = if scoped {
            &["specs", "compositions"]
        } else if document["version"] == 3 {
            &["components", "compositions"]
        } else {
            return Ok(());
        };
        for group in groups {
            if let Some(targets) = document.get_mut(*group).and_then(Value::as_object_mut) {
                for (name, target) in targets {
                    // Only scoped ensures have unqualified output aliases. V3
                    // still spells an observable reference o.<name> explicitly.
                    let outputs: BTreeSet<String> = if scoped {
                        target["outputs"]
                            .as_object()
                            .map(|values| values.keys().cloned().collect())
                            .unwrap_or_default()
                    } else {
                        BTreeSet::new()
                    };
                    let path = format!("/{group}/{name}/examples");
                    if let Some(examples) =
                        target.get_mut("examples").and_then(Value::as_object_mut)
                    {
                        for (name, example) in examples {
                            self.resolve_example(example, &outputs, &format!("{path}/{name}"))?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn resolve_example(
        &self,
        example: &mut Value,
        outputs: &BTreeSet<String>,
        path: &str,
    ) -> Res<()> {
        let bound: BTreeSet<String> = example
            .get("quantifiers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|binder| binder["variables"].as_object())
            .flat_map(|variables| variables.keys().cloned())
            .collect();
        if bound.is_empty() {
            return Ok(());
        }
        if let Some(initial) = example.get_mut("initial").and_then(Value::as_object_mut) {
            for (name, expression) in initial {
                self.bound_expression(expression, &bound, None, &format!("{path}/initial/{name}"))?;
            }
        }
        if let Some(trace) = example.get_mut("trace").and_then(Value::as_array_mut) {
            for (index, frame) in trace.iter_mut().enumerate() {
                let path = format!("{path}/trace/{index}");
                for field in ["inputs", "observe"] {
                    if let Some(values) = frame.get_mut(field).and_then(Value::as_object_mut) {
                        for (name, expression) in values {
                            self.bound_expression(
                                expression,
                                &bound,
                                None,
                                &format!("{path}/{field}/{name}"),
                            )?;
                        }
                    }
                }
                if let Some(expression) = frame.get_mut("ensure") {
                    self.bound_expression(
                        expression,
                        &bound,
                        Some(outputs),
                        &format!("{path}/ensure"),
                    )?;
                }
            }
        }
        Ok(())
    }

    fn bound_expression(
        &self,
        value: &mut Value,
        bound: &BTreeSet<String>,
        outputs: Option<&BTreeSet<String>>,
        path: &str,
    ) -> Res<()> {
        if let Some(name) = value.as_str() {
            if bound.contains(name) {
                if outputs.is_some_and(|outputs| outputs.contains(name)) {
                    return Err(SyntaxError {
                        filename: self.filename.into(),
                        message: format!(
                            "{path}: ambiguous name {name}: both a quantified variable and an output; rename the quantified variable, or use o.{name} for the output"
                        ),
                        span: self.spans.get(path).cloned(),
                    });
                }
                *value = json!(format!("q.{name}"));
            } else if name
                .strip_suffix('\'')
                .is_some_and(|name| bound.contains(name))
            {
                return Err(SyntaxError {
                    filename: self.filename.into(),
                    message: format!(
                        "{path}: quantified variables have no next-state value; remove the prime from {name}"
                    ),
                    span: self.spans.get(path).cloned(),
                });
            }
            return Ok(());
        }
        if let Some(values) = value.as_array_mut() {
            // Never resolve the operator, literal payload, width or numeric
            // metadata as a variable. Match the IR's expression positions.
            let start = match values.first().and_then(Value::as_str) {
                Some("bv") => values.len(),
                Some("const_mem" | "zext" | "sext") => 2,
                Some("extract") => 3,
                _ => 1,
            };
            for (index, expression) in values.iter_mut().enumerate().skip(start) {
                self.bound_expression(expression, bound, outputs, &format!("{path}/{index}"))?;
            }
        }
        Ok(())
    }

    pub(super) fn invocation(&mut self, invocation: &g::Invocation<'_>, path: &str) -> Res<Value> {
        let operation = id_token(&invocation.id);
        let span = self.span(operation);
        if operation.text().contains('.') {
            return Err(self.error(
                &span,
                "trace invocations require an exported local operation name",
            ));
        }
        self.spans.insert(path.into(), span.clone());
        self.spans.insert(format!("{path}/operation"), span.clone());
        self.spans.insert(format!("{path}/inputs"), span);
        let mut inputs = Map::new();
        if let Some(arguments) = &invocation.invocation_opt {
            for argument in std::iter::once(&*arguments.invocation_inputs.invocation_input).chain(
                arguments
                    .invocation_inputs
                    .invocation_inputs_list
                    .iter()
                    .map(|a| &*a.invocation_input),
            ) {
                let name = id_token(&argument.id);
                let span = self.span(name);
                if name.text().contains('.') {
                    return Err(self.error(&span, "invocation input names must be unqualified"));
                }
                if inputs.contains_key(name.text()) {
                    return Err(
                        self.error(&span, format!("duplicate invocation input {}", name.text()))
                    );
                }
                let value = self.expr(&argument.expr)?;
                self.record(&format!("{path}/inputs/{}", name.text()), &value);
                inputs.insert(name.text().into(), value.value);
            }
        }
        let ensure = self.expr(&invocation.expr)?;
        self.record(&format!("{path}/ensure"), &ensure);
        Ok(
            json!({"operation": operation.text(), "inputs": inputs, "observe": {}, "ensure": ensure.value}),
        )
    }

    pub(super) fn quantifier(
        &mut self,
        entry: &g::Entry<'_>,
        context: &str,
        path: &str,
        result: &mut Map<String, Value>,
    ) -> Res<bool> {
        if !matches!(context, "example" | "scoped_example") {
            return Ok(false);
        }
        if let g::Entry::Block(x) = entry {
            let token = id_token(&x.block.id);
            if token.text() == "quantifiers" {
                if !x.block.block_list.is_empty() {
                    return Err(self.error(&self.span(token), "quantifiers {} is only the explicit empty prefix; use forall/exists declarations"));
                }
                if result.contains_key("expect") {
                    return Err(self.error(
                        &self.span(token),
                        "input quantifiers must precede the final execution/expect mode",
                    ));
                }
                if result.contains_key("quantifiers") {
                    return Err(self.error(&self.span(token), "duplicate quantifiers prefix"));
                }
                self.spans
                    .insert(format!("{path}/quantifiers"), self.span(token));
                result.insert("quantifiers".into(), json!([]));
                return Ok(true);
            }
        }
        let (token, variables) = match entry {
            g::Entry::NamedEntry(x) => {
                let token = id_token(&x.named_entry.id);
                if !matches!(token.text(), "forall" | "exists") {
                    return Ok(false);
                }
                let g::NamedBody::ColonTypeSemicolon(declaration) = &*x.named_entry.named_body
                else {
                    return Err(self.error(&self.span(token), "quantifier requires a typed variable declaration or typed declaration block"));
                };
                let name = id_token(&x.named_entry.id0);
                if name.text().contains('.') {
                    return Err(self.error(
                        &self.span(name),
                        "quantified variable names must be unqualified",
                    ));
                }
                let variable = self.ty(&declaration.r#type)?;
                (
                    token,
                    vec![(name.text().to_owned(), variable, self.span(name))],
                )
            }
            g::Entry::Block(x) => {
                let token = id_token(&x.block.id);
                if !matches!(token.text(), "forall" | "exists") {
                    return Ok(false);
                }
                let mut variables = Vec::new();
                for entry in &x.block.block_list {
                    let g::Entry::Declaration(declaration) = &*entry.entry else {
                        return Err(self.error(
                            &self.span(token),
                            "quantifier blocks contain only name: type declarations",
                        ));
                    };
                    let name = id_token(&declaration.declaration.id);
                    if name.text().contains('.') {
                        return Err(self.error(
                            &self.span(name),
                            "quantified variable names must be unqualified",
                        ));
                    }
                    variables.push((
                        name.text().to_owned(),
                        self.ty(&declaration.declaration.r#type)?,
                        self.span(name),
                    ));
                }
                if variables.is_empty() {
                    return Err(self.error(
                        &self.span(token),
                        "quantifier blocks must declare at least one variable",
                    ));
                }
                (token, variables)
            }
            _ => return Ok(false),
        };
        if result.contains_key("expect") {
            return Err(self.error(
                &self.span(token),
                "input quantifiers must precede the final execution/expect mode",
            ));
        }
        let quantifiers = result
            .entry("quantifiers")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap();
        let mut seen: BTreeSet<_> = quantifiers
            .iter()
            .flat_map(|q| q["variables"].as_object().unwrap().keys().cloned())
            .collect();
        let child = format!("{path}/quantifiers/{}", quantifiers.len());
        self.spans.insert(child.clone(), self.span(token));
        self.spans.insert(format!("{child}/kind"), self.span(token));
        let mut declarations = Map::new();
        for (name, sort, span) in variables {
            if !seen.insert(name.clone()) {
                return Err(self.error(
                    &span,
                    format!("duplicate quantified variable {name}; shadowing is not allowed"),
                ));
            }
            self.spans.insert(format!("{child}/variables/{name}"), span);
            declarations.insert(name, sort);
        }
        quantifiers.push(json!({"kind": token.text(), "variables": declarations}));
        Ok(true)
    }
}
