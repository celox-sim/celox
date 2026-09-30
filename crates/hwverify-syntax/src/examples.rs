//! Ordered example binders and concise input/output trace assertions.
use super::*;

impl Lower<'_> {
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
