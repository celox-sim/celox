//! Native proof authoring -> existing canonical candidate data, never proof authority.
use super::*;

impl Lower<'_> {
    fn proof_name(&self, token: &parol_runtime::Token<'_>) -> Res<String> {
        let s = token.text();
        if s.contains('.') || ["pre", "goal", "lhs", "rhs"].contains(&s) {
            return Err(self.error(
                &self.span(token),
                "proof names must be local and must not shadow pre/goal/lhs/rhs",
            ));
        }
        Ok(s.into())
    }
    fn proof_expr(
        &mut self,
        expr: &g::Expr<'_>,
        path: &str,
        variables: &Map<String, Value>,
    ) -> Res<Value> {
        let parser = Lower {
            source: self.source,
            filename: self.filename,
            spans: BTreeMap::new(),
            expectation_checks: vec![],
            proof_mode: true,
        };
        let e = parser.expr(expr)?;
        self.record(path, &e);
        fn resolve(v: &mut Value, variables: &Map<String, Value>) -> Result<(), String> {
            match v {
                Value::String(name) => {
                    let replacement = if let Some(old) = name.strip_suffix('\'') {
                        if let Some(field) = old.strip_prefix("impl.") {
                            format!("impl_next.{field}")
                        } else if let Some(field) = old.strip_prefix("spec.") {
                            format!("spec_next.{field}")
                        } else {
                            return Err("prime in a proof requires impl.state' or spec.state'; formals and inputs have no next value".into());
                        }
                    } else if ["pre", "goal", "lhs", "rhs"].contains(&name.as_str()) {
                        format!("${name}")
                    } else if variables.contains_key(name.as_str()) {
                        format!("formal.{name}")
                    } else if [
                        "impl.",
                        "spec.",
                        "i.",
                        "impl_next.",
                        "spec_next.",
                        "formal.",
                        "let.",
                    ]
                    .iter()
                    .any(|p| name.starts_with(p))
                    {
                        name.clone()
                    } else if let Some((handle, field)) = name.split_once('.') {
                        match field {
                            "claim" => format!("handle.{handle}.post"),
                            "pre" => format!("handle.{handle}.pre"),
                            _ => name.clone(),
                        }
                    } else if ["commit", "binding_before", "binding_after"].contains(&name.as_str())
                    {
                        name.clone()
                    } else {
                        format!("let.{name}")
                    };
                    *name = replacement;
                }
                Value::Array(a) => {
                    for x in a.iter_mut().skip(1) {
                        resolve(x, variables)?;
                    }
                }
                _ => (),
            }
            Ok(())
        }
        let mut value = e.value;
        resolve(&mut value, variables).map_err(|m| self.error(&e.span, m))?;
        Ok(value)
    }
    fn proof_source(&self, token: &parol_runtime::Token<'_>) -> String {
        let s = self.span(token);
        format!("{}:{}:{}", self.filename, s.line, s.column)
    }
    pub(super) fn proof_block(
        &mut self,
        entries: Vec<&g::Entry<'_>>,
        base: Option<&Value>,
    ) -> Res<Value> {
        if let Some(base) = base {
            if !base["variables"].is_object()
                || !base["lets"].is_array()
                || base["programs"]
                    .as_array()
                    .is_none_or(|ps| ps.iter().any(|p| !p.is_object() || !p["steps"].is_array()))
            {
                return Err(SyntaxError {
                    filename: self.filename.into(),
                    span: None,
                    message: "invalid base proof-program metadata shape".into(),
                });
            }
        }
        let mut metadata=base.cloned().unwrap_or_else(||json!({"version":1,"mode":"independent_lemmas","variables":{},"lets":[],"programs":[]}));
        let mut targets = BTreeSet::new();
        for entry in entries {
            match entry {
                g::Entry::Property(x) if id_token(&x.property.id).text() == "mode" => {
                    let e = self.expr(&x.property.expr)?;
                    if !matches!(
                        e.value.as_str(),
                        Some("independent_lemmas" | "shared_query")
                    ) {
                        return Err(self.error(
                            &e.span,
                            "proof mode must be independent_lemmas or shared_query",
                        ));
                    }
                    if base.is_some() && e.value != metadata["mode"] {
                        return Err(self.error(
                            &e.span,
                            "a lemma module cannot change the existing proof budget mode",
                        ));
                    }
                    metadata["mode"] = e.value;
                }
                g::Entry::NamedEntry(x) => {
                    let x = &x.named_entry;
                    let keyword = id_token(&x.id);
                    let token = id_token(&x.id0);
                    let name = self.proof_name(token)?;
                    match (&*x.named_body, keyword.text()) {
                        (g::NamedBody::ColonTypeSemicolon(b), "forall") => {
                            if metadata["variables"].get(&name).is_some()
                                || metadata["lets"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .any(|v| v["id"] == name)
                            {
                                return Err(self.error(&self.span(token), "duplicate proof formal"));
                            }
                            metadata["variables"][&name] = self.ty(&b.r#type)?;
                        }
                        (g::NamedBody::EquExprSemicolon(b), "let") => {
                            if metadata["variables"].get(&name).is_some()
                                || metadata["lets"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .any(|v| v["id"] == name)
                            {
                                return Err(self.error(&self.span(token), "duplicate proof let"));
                            }
                            let value = self.proof_expr(
                                &b.expr,
                                "/proof_programs/lets",
                                metadata["variables"].as_object().unwrap(),
                            )?;
                            metadata["lets"]
                                .as_array_mut()
                                .unwrap()
                                .push(json!({"id":name,"expr":value}));
                        }
                        (g::NamedBody::LBraceNamedBodyListRBrace(b), "target") => {
                            if !targets.insert(name.clone()) {
                                return Err(self.error(&self.span(token), "duplicate proof target"));
                            }
                            let index = metadata["programs"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .position(|p| p["id"] == name);
                            let old = index.map(|i| metadata["programs"][i].clone());
                            let program = self.proof_target(
                                &name,
                                b.named_body_list.iter().map(|e| &*e.entry).collect(),
                                old.as_ref(),
                                metadata["variables"].as_object().unwrap(),
                            )?;
                            if let Some(i) = index {
                                metadata["programs"][i] = program;
                            } else {
                                metadata["programs"].as_array_mut().unwrap().push(program);
                            }
                        }
                        _ => {
                            return Err(self.error(
                                &self.span(keyword),
                                "proof blocks accept forall, let, target and mode",
                            ))
                        }
                    }
                }
                _ => {
                    return Err(SyntaxError {
                        filename: self.filename.into(),
                        span: None,
                        message: "proof blocks accept forall, let, target and mode".into(),
                    })
                }
            }
        }
        Ok(metadata)
    }
    fn proof_target(
        &mut self,
        name: &str,
        entries: Vec<&g::Entry<'_>>,
        base: Option<&Value>,
        variables: &Map<String, Value>,
    ) -> Res<Value> {
        let mut program = base
            .cloned()
            .unwrap_or_else(|| json!({"id":name,"steps":[]}));
        let mut declared = BTreeSet::new();
        let mut properties = BTreeSet::new();
        for entry in entries {
            let (id, step, token) = match entry {
                g::Entry::Property(x) => {
                    let key = id_token(&x.property.id);
                    let field = match key.text() {
                        "rhs" => "match_rhs",
                        "result" => "result",
                        _ => {
                            return Err(
                                self.error(&self.span(key), "target properties are rhs and result")
                            )
                        }
                    };
                    if !properties.insert(field) {
                        return Err(self.error(&self.span(key), "duplicate target property"));
                    }
                    program[field] = if field == "result" {
                        let e = self.expr(&x.property.expr)?;
                        let s = e.value.as_str().ok_or_else(|| {
                            self.error(&e.span, "result must name a checked step")
                        })?;
                        json!(s)
                    } else {
                        self.proof_expr(&x.property.expr, "/proof_programs/target/rhs", variables)?
                    };
                    continue;
                }
                g::Entry::NamedEntry(x) => {
                    let x = &x.named_entry;
                    let key = id_token(&x.id);
                    let token = id_token(&x.id0);
                    let id = self.proof_name(token)?;
                    let step = match (&*x.named_body, key.text()) {
                        (g::NamedBody::EquExprSemicolon(b), "let") => {
                            json!({"op":"let","id":id,"expr":self.proof_expr(&b.expr,"/proof_programs/target/let",variables)?})
                        }
                        (g::NamedBody::LBraceNamedBodyListRBrace(b), "lemma") => {
                            let mut c = json!({"op":"candidate","id":id,"frame":"current_query","depends_on":[],"source":self.proof_source(token)});
                            let mut fields = BTreeSet::new();
                            for e in &b.named_body_list {
                                let g::Entry::Property(p) = &*e.entry else {
                                    return Err(self.error(&self.span(token),"lemma bodies require context, guard, claim and optional depends properties"));
                                };
                                let field = id_token(&p.property.id);
                                let key = field.text();
                                if !["context", "guard", "claim", "depends"].contains(&key)
                                    || !fields.insert(key.to_owned())
                                {
                                    return Err(self.error(
                                        &self.span(field),
                                        "unknown or duplicate lemma property",
                                    ));
                                }
                                if key == "depends" {
                                    let e = self.expr(&p.property.expr)?;
                                    let deps = if let Some(s) = e.value.as_str() {
                                        vec![json!(s)]
                                    } else if let Some(a) = e
                                        .value
                                        .as_array()
                                        .filter(|a| a.first() == Some(&json!("compose")))
                                    {
                                        a[1..].to_vec()
                                    } else {
                                        return Err(self.error(
                                            &e.span,
                                            "depends requires a name or compose(a, b, ...)",
                                        ));
                                    };
                                    if deps
                                        .iter()
                                        .any(|d| d.as_str().is_none_or(|s| s.contains('.')))
                                    {
                                        return Err(self.error(
                                            &e.span,
                                            "dependencies must be local step names",
                                        ));
                                    }
                                    c["depends_on"] = json!(deps);
                                } else {
                                    c[key] = self.proof_expr(
                                        &p.property.expr,
                                        &format!("/proof_programs/{name}/{id}/{key}"),
                                        variables,
                                    )?;
                                }
                            }
                            for field in ["context", "guard", "claim"] {
                                if c.get(field).is_none() {
                                    return Err(self.error(
                                        &self.span(token),
                                        format!("lemma {id} is missing {field}"),
                                    ));
                                }
                            }
                            c
                        }
                        _ => {
                            return Err(self.error(
                                &self.span(key),
                                "targets accept lemma, let, use, rhs and result",
                            ))
                        }
                    };
                    (id, step, token)
                }
                g::Entry::Use(x) => {
                    let u = &x.r#use;
                    let token = id_token(&u.id);
                    let id = self.proof_name(token)?;
                    let lemma = u.use_opt.as_ref().map_or(token, |v| id_token(&v.id));
                    let lemma = self.proof_name(lemma)?;
                    let mut context = None;
                    if let Some(args) = &u.use_opt0 {
                        for c in std::iter::once(&*args.connections.connection).chain(
                            args.connections
                                .connections_list
                                .iter()
                                .map(|c| &*c.connection),
                        ) {
                            let port = id_token(&c.id);
                            let value = id_token(&c.id0);
                            if port.text() != "context" || context.is_some() {
                                return Err(self.error(
                                    &self.span(port),
                                    "lemma use requires exactly one context connection",
                                ));
                            }
                            let text = value.text();
                            if text.contains('.') {
                                return Err(self.error(
                                    &self.span(value),
                                    "use context must be pre/goal/lhs/rhs or a local let name",
                                ));
                            }
                            context = Some(if ["pre", "goal", "lhs", "rhs"].contains(&text) {
                                json!(format!("${text}"))
                            } else {
                                json!(format!("let.{text}"))
                            });
                        }
                    }
                    let context = context.ok_or_else(|| {
                        self.error(
                            &self.span(token),
                            "lemma use requires context: pre (or a named let)",
                        )
                    })?;
                    (
                        id.clone(),
                        json!({"op":"use_candidate","id":id,"candidate":lemma,"context":context,"source":self.proof_source(token)}),
                        token,
                    )
                }
                _ => {
                    return Err(SyntaxError {
                        filename: self.filename.into(),
                        span: None,
                        message: "unsupported proof target entry".into(),
                    })
                }
            };
            if !declared.insert(id.clone()) {
                return Err(self.error(&self.span(token), "duplicate proof step declaration"));
            }
            let steps = program["steps"].as_array_mut().unwrap();
            if let Some(i) = steps.iter().position(|s| s["id"] == id) {
                steps[i] = step;
            } else {
                steps.push(step);
            }
        }
        for field in ["match_rhs", "result"] {
            if program.get(field).is_none() {
                return Err(SyntaxError {
                    filename: self.filename.into(),
                    span: None,
                    message: format!("new target {name} requires {field}"),
                });
            }
        }
        Ok(program)
    }
}

/// Attach source proposals to an existing metadata module. Only proof metadata is returned.
/// Existing named steps may be replaced; model fields and premises are never edited here.
pub fn merge_lemma_source(source: &str, filename: &str, base: Option<&Value>) -> Res<Value> {
    let mut grammar = HwvGrammar::default();
    hwv_parser::parse(source, filename, &mut grammar)
        .map_err(|e| parser_error(source, filename, e))?;
    let root = grammar.root.ok_or_else(|| SyntaxError {
        filename: filename.into(),
        span: None,
        message: "missing lemma module".into(),
    })?;
    let mut lower = Lower {
        source,
        filename,
        spans: BTreeMap::new(),
        expectation_checks: vec![],
        proof_mode: false,
    };
    if root.doc_kind.doc_kind.text() != "lemmas" {
        return Err(lower.error(
            &lower.span(&root.string.string),
            "--lemmas requires a lemmas module",
        ));
    }
    let entries = match &*root.document_body {
        g::DocumentBody::LBraceDocumentBodyListRBrace(x) => {
            x.document_body_list.iter().map(|e| &*e.entry).collect()
        }
        g::DocumentBody::DocumentBodyList0(x) => {
            x.document_body_list0.iter().map(|e| &*e.entry).collect()
        }
    };
    lower.proof_block(entries, base)
}
