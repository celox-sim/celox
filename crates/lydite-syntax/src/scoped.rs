//! Source-only lowering for scoped interfaces and named instance connections.
//! All port/operation semantics are validated by the IR elaborator.
use super::*;

impl Lower<'_> {
    pub(super) fn scoped_signature(
        &mut self,
        keyword: &parol_runtime::Token<'_>,
        name: &parol_runtime::Token<'_>,
        signature: &g::SignatureBody<'_>,
        context: &str,
        path: &str,
        result: &mut Map<String, Value>,
    ) -> Res<()> {
        let (group, body_context) = match (context, keyword.text()) {
            ("scoped_root", "spec") => ("specs", "scoped_spec"),
            ("scoped_root", "composition") => ("compositions", "scoped_composition"),
            _ => {
                return Err(self.error(
                    &self.span(keyword),
                    format!(
                        "unexpected interface declaration {} in {context}",
                        keyword.text()
                    ),
                ));
            }
        };
        let span = self.span(name);
        if name.text().contains('.') {
            return Err(self.error(&span, "declaration names must not contain dots"));
        }
        if result.get(group).and_then(|v| v.get(name.text())).is_some() {
            return Err(self.error(&span, format!("duplicate {} in {group}", name.text())));
        }
        let child = format!("{path}/{group}/{}", name.text());
        self.spans
            .entry(format!("{path}/{group}"))
            .or_insert_with(|| span.clone());
        self.spans.insert(child.clone(), span);
        let mut inputs = Map::new();
        let mut outputs = Map::new();
        if let Some(args) = &signature.signature_body_opt {
            for port in std::iter::once(&*args.ports.port)
                .chain(args.ports.ports_list.iter().map(|p| &*p.port))
            {
                let direction = id_token(&port.id);
                let name = id_token(&port.id0);
                let span = self.span(name);
                if name.text().contains('.') {
                    return Err(self.error(&span, "port names must not contain dots"));
                }
                if inputs.contains_key(name.text()) || outputs.contains_key(name.text()) {
                    return Err(
                        self.error(&span, format!("duplicate interface port {}", name.text()))
                    );
                }
                let (field, collection) = match direction.text() {
                    "input" => ("inputs", &mut inputs),
                    "output" => ("outputs", &mut outputs),
                    _ => {
                        return Err(self.error(
                            &self.span(direction),
                            "interface ports require input or output",
                        ));
                    }
                };
                self.spans
                    .entry(format!("{child}/{field}"))
                    .or_insert_with(|| span.clone());
                self.spans
                    .insert(format!("{child}/{field}/{}", name.text()), span);
                collection.insert(name.text().into(), self.ty(&port.r#type)?);
            }
        }
        let mut value = self.entries(
            signature
                .signature_body_list
                .iter()
                .map(|e| &*e.entry)
                .collect(),
            body_context,
            &child,
        )?;
        value["inputs"] = Value::Object(inputs);
        value["outputs"] = Value::Object(outputs);
        if body_context == "scoped_spec" {
            self.scoped_relations(&mut value, name.text(), &child)?;
        }
        result
            .entry(group)
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("signature collections are objects")
            .insert(name.text().into(), value);
        Ok(())
    }

    pub(super) fn scoped_use(
        &mut self,
        usage: &g::Use<'_>,
        context: &str,
        path: &str,
        result: &mut Map<String, Value>,
    ) -> Res<()> {
        if context != "scoped_composition" {
            return Err(self.error(
                &self.span(&usage.r#use),
                format!("use is only valid in a scoped composition, not {context}"),
            ));
        }
        let alias = id_token(&usage.id);
        let target = usage.use_opt.as_ref().map_or(alias, |v| id_token(&v.id));
        for name in [alias, target] {
            if name.text().contains('.') {
                return Err(self.error(
                    &self.span(name),
                    "instance aliases and target names must not contain dots",
                ));
            }
        }
        if result
            .get("instances")
            .and_then(|v| v.get(alias.text()))
            .is_some()
        {
            return Err(self.error(
                &self.span(alias),
                format!("duplicate instance {}", alias.text()),
            ));
        }
        let child = format!("{path}/instances/{}", alias.text());
        let alias_span = self.span(alias);
        self.spans
            .entry(format!("{path}/instances"))
            .or_insert(alias_span);
        self.spans.insert(child.clone(), self.span(alias));
        self.spans
            .insert(format!("{child}/target"), self.span(target));
        self.spans
            .insert(format!("{child}/connections"), self.span(alias));
        let mut connections = Map::new();
        if let Some(args) = &usage.use_opt0 {
            for connection in std::iter::once(&*args.connections.connection).chain(
                args.connections
                    .connections_list
                    .iter()
                    .map(|c| &*c.connection),
            ) {
                let port = id_token(&connection.id);
                let value = id_token(&connection.id0);
                if port.text().contains('.') || value.text().contains('.') {
                    return Err(self.error(
                        &self.span(port),
                        "connections require local unqualified port names",
                    ));
                }
                if connections.contains_key(port.text()) {
                    return Err(self.error(
                        &self.span(port),
                        format!("duplicate connection {}", port.text()),
                    ));
                }
                self.spans.insert(
                    format!("{child}/connections/{}", port.text()),
                    self.span(value),
                );
                connections.insert(port.text().into(), json!(value.text()));
            }
        }
        result
            .entry("instances")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("instance collection is an object")
            .insert(
                alias.text().into(),
                json!({"target":target.text(), "connections":connections}),
            );
        Ok(())
    }
}
