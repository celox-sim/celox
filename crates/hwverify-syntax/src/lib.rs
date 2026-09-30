//! Parol grammar -> source-located surface syntax -> canonical IR document.
//! The generated grammar AST never crosses into the solver.
use serde_json::{json, Map, Value};
use std::{collections::BTreeMap, fmt};
include!(concat!(env!("OUT_DIR"), "/modules.rs"));
use hwv_grammar_trait as g;
mod json_input;
pub use json_input::parse as parse_json;
mod hwv_grammar {
    pub use super::HwvGrammar;
}
#[derive(Default)]
pub struct HwvGrammar<'t> {
    root: Option<g::Document<'t>>,
}
impl<'t> g::HwvGrammarTrait<'t> for HwvGrammar<'t> {
    fn document(&mut self, arg: &g::Document<'t>) -> parol_runtime::Result<()> {
        self.root = Some(arg.clone());
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub column: usize,
}
#[derive(Clone, Debug)]
pub struct SyntaxError {
    pub filename: String,
    pub message: String,
    pub span: Option<Span>,
}
impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(s) = &self.span {
            write!(
                f,
                "{}:{}:{}: {}",
                self.filename, s.line, s.column, self.message
            )
        } else {
            write!(f, "{}: {}", self.filename, self.message)
        }
    }
}
impl std::error::Error for SyntaxError {}
#[derive(Clone, Debug)]
pub struct ParsedDocument {
    pub canonical: Value,
    pub spans: BTreeMap<String, Span>,
    pub filename: String,
}
impl ParsedDocument {
    pub fn validate_specification(&self) -> Result<hwverify_ir::Specification, SyntaxError> {
        hwverify_ir::Specification::from_json(&self.canonical)
            .map_err(|e| self.diagnostic(&self.filename, &e))
    }
    pub fn validate(&self) -> Result<hwverify_ir::Design, SyntaxError> {
        hwverify_ir::Design::from_json(&self.canonical)
            .map_err(|e| self.diagnostic(&self.filename, &e))
    }
    pub fn span_for(&self, path: &str) -> Option<&Span> {
        let mut p = path;
        loop {
            if let Some(s) = self.spans.get(p) {
                return Some(s);
            };
            if let Some((head, _)) = p.rsplit_once('/') {
                p = head
            } else {
                return None;
            }
        }
    }
    pub fn diagnostic(&self, filename: &str, error: &hwverify_ir::ValidationError) -> SyntaxError {
        SyntaxError {
            filename: filename.into(),
            message: format!("{}: {}", error.path, error.message),
            span: self.span_for(&error.path).cloned(),
        }
    }
}
struct Lower<'s> {
    source: &'s str,
    filename: &'s str,
    spans: BTreeMap<String, Span>,
}
#[derive(Clone)]
struct E {
    value: Value,
    span: Span,
    /// Descendant expression locations indexed relative to this JSON expression.
    descendants: BTreeMap<String, Span>,
}
impl E {
    fn leaf(value: Value, span: Span) -> Self {
        Self {
            value,
            span,
            descendants: BTreeMap::new(),
        }
    }
    fn call(op: &str, args: Vec<E>, mut span: Span) -> Self {
        let mut values = vec![json!(op)];
        let mut descendants = BTreeMap::new();
        for (index, arg) in args.into_iter().enumerate() {
            span.end = span.end.max(arg.span.end);
            let prefix = format!("/{}", index + 1);
            descendants.insert(prefix.clone(), arg.span);
            descendants.extend(
                arg.descendants
                    .into_iter()
                    .map(|(p, s)| (format!("{prefix}{p}"), s)),
            );
            values.push(arg.value);
        }
        Self {
            value: Value::Array(values),
            span,
            descendants,
        }
    }
}
type Res<T> = Result<T, SyntaxError>;
fn id_token<'a, 't>(id: &'a g::Id<'t>) -> &'a parol_runtime::Token<'t> {
    match id {
        g::Id::Ident(x) => &x.ident.ident,
        g::Id::Bv(x) => &x.bv,
        g::Id::Mem(x) => &x.mem,
        g::Id::Bool(x) => &x.bool,
        g::Id::DocKind(x) => &x.doc_kind.doc_kind,
    }
}
impl Lower<'_> {
    fn span(&self, t: &parol_runtime::Token<'_>) -> Span {
        let start = t.location.start as usize;
        let end = t.location.end as usize;
        let prefix = &self.source[..start.min(self.source.len())];
        let line = prefix.bytes().filter(|c| *c == b'\n').count() + 1;
        let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        Span {
            start,
            end,
            line,
            column,
        }
    }
    fn error(&self, span: &Span, msg: impl Into<String>) -> SyntaxError {
        SyntaxError {
            filename: self.filename.into(),
            message: msg.into(),
            span: Some(span.clone()),
        }
    }
    fn uint(&self, t: &parol_runtime::Token<'_>) -> Res<u64> {
        t.text()
            .parse()
            .map_err(|_| self.error(&self.span(t), "integer must fit unsigned 64 bits"))
    }
    fn expr(&self, e: &g::Expr<'_>) -> Res<E> {
        self.or(&e.or)
    }
    fn record(&mut self, path: &str, expression: &E) {
        self.spans.insert(path.into(), expression.span.clone());
        self.spans.extend(
            expression
                .descendants
                .iter()
                .map(|(p, s)| (format!("{path}{p}"), s.clone())),
        );
    }
    fn binary(&self, op: &str, mut l: E, mut r: E) -> Res<E> {
        let span = Span {
            end: r.span.end,
            ..l.span.clone()
        };
        let mapped = match op {
            "||" => "or",
            "&&" => "and",
            "|" => "bor",
            "^" => "bxor",
            "&" => "band",
            "==" => "eq",
            "!=" => "ne",
            "<" => "ult",
            "<=" => "ule",
            ">" => {
                std::mem::swap(&mut l, &mut r);
                "ult"
            }
            ">=" => {
                std::mem::swap(&mut l, &mut r);
                "ule"
            }
            "<<" => "shl",
            ">>" => "lshr",
            "+" => "add",
            "-" => "sub",
            "*" => "mul",
            _ => return Err(self.error(&span, "unknown operator")),
        };
        Ok(E::call(mapped, vec![l, r], span))
    }
    fn compare(&self, e: &g::Compare<'_>) -> Res<E> {
        if e.compare_list.len() > 1 {
            return Err(self.error(
                &self.span(cmp_token(&e.compare_list[1].compare_op)),
                "comparison chaining is not supported; parenthesize explicitly",
            ));
        }
        let mut x = self.shift(&e.shift)?;
        for a in &e.compare_list {
            x = self.binary(cmp_token(&a.compare_op).text(), x, self.shift(&a.shift)?)?;
        }
        Ok(x)
    }
    fn unary(&self, e: &g::Unary<'_>) -> Res<E> {
        match e {
            g::Unary::Postfix(x) => self.postfix(&x.postfix),
            g::Unary::PrefixOpUnary(x) => {
                let e = self.unary(&x.unary)?;
                let span = Span {
                    end: e.span.end,
                    ..self.span(prefix_token(&x.prefix_op))
                };
                match prefix_token(&x.prefix_op).text() {
                    "!" => Ok(E::call("not", vec![e], span)),
                    "~" => Ok(E::call("bnot", vec![e], span)),
                    "-" => {
                        let Some(v) = e.value.as_u64() else {
                            return Err(self.error(&span, "unary minus requires an untyped integer literal; use sub for word arithmetic"));
                        };
                        let n = i64::try_from(-(v as i128)).map_err(|_| {
                            self.error(&span, "negative literal must fit signed 64 bits")
                        })?;
                        Ok(E::leaf(json!(n), span))
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
    fn postfix(&self, e: &g::Postfix<'_>) -> Res<E> {
        let mut x = self.primary(&e.primary)?;
        for a in &e.postfix_list {
            let index = self.expr(&a.expr)?;
            let span = Span {
                end: index.span.end,
                ..x.span.clone()
            };
            x = E::call("read", vec![x, index], span);
        }
        Ok(x)
    }
    fn primary(&self, e: &g::Primary<'_>) -> Res<E> {
        match e {
            g::Primary::Word(x) => {
                let t = &x.word.word;
                let (v, w) = t.text().split_once('u').unwrap();
                let value = v.parse::<u64>().map_err(|_| {
                    self.error(
                        &self.span(t),
                        "word literal value must fit unsigned 64 bits",
                    )
                })?;
                let width = w
                    .parse::<u64>()
                    .map_err(|_| self.error(&self.span(t), "invalid word width"))?;
                Ok(E::leaf(json!(["bv", width, value]), self.span(t)))
            }
            g::Primary::UInt(x) => Ok(E::leaf(
                json!(self.uint(&x.u_int.u_int)?),
                self.span(&x.u_int.u_int),
            )),
            g::Primary::Boolean(x) => Ok(E::leaf(
                json!(x.boolean.boolean.text() == "true"),
                self.span(&x.boolean.boolean),
            )),
            g::Primary::LParenExprRParen(x) => self.expr(&x.expr),
            g::Primary::IfExpr(x) => {
                let a = self.expr(&x.if_expr.expr)?;
                let b = self.expr(&x.if_expr.expr0)?;
                let c = self.expr(&x.if_expr.expr1)?;
                let span = Span {
                    end: c.span.end,
                    ..a.span.clone()
                };
                Ok(E::call("ite", vec![a, b, c], span))
            }
            g::Primary::Named(x) => {
                let n = &x.named;
                let t = id_token(&n.id);
                let span = self.span(t);
                if let Some(call) = &n.named_opt {
                    let mut args = vec![];
                    if let Some(arg) = &call.named_opt0 {
                        args.push(self.expr(&arg.args.expr)?);
                        for arg in &arg.args.args_list {
                            args.push(self.expr(&arg.expr)?)
                        }
                    }
                    let op = t.text();
                    if ![
                        "bv",
                        "const_mem",
                        "extract",
                        "zext",
                        "sext",
                        "not",
                        "bnot",
                        "and",
                        "or",
                        "xor",
                        "implies",
                        "eq",
                        "ne",
                        "ite",
                        "add",
                        "sub",
                        "mul",
                        "band",
                        "bor",
                        "bxor",
                        "shl",
                        "lshr",
                        "ult",
                        "ule",
                        "slt",
                        "sle",
                        "read",
                        "write",
                        "concat",
                        "range",
                        "compose",
                    ]
                    .contains(&op)
                    {
                        return Err(self.error(&span, format!("unknown builtin {op}")));
                    }
                    Ok(E::call(op, args, span))
                } else {
                    Ok(E::leaf(json!(t.text()), span))
                }
            }
        }
    }
    fn ty(&self, t: &g::Type<'_>) -> Res<Value> {
        Ok(match t {
            g::Type::Bool(_) => json!("bool"),
            g::Type::BvLtUIntGt(x) => json!({"bv":self.uint(&x.u_int.u_int)?}),
            g::Type::MemLtUIntCommaUIntGt(x) => {
                json!({"mem":[self.uint(&x.u_int.u_int)?,self.uint(&x.u_int0.u_int)?]})
            }
        })
    }
    fn entries<'a, 't>(
        &mut self,
        entries: Vec<&'a g::Entry<'t>>,
        context: &str,
        path: &str,
    ) -> Res<Value>
    where
        't: 'a,
    {
        if context == "trace" {
            let mut frames = vec![];
            for (index, entry) in entries.into_iter().enumerate() {
                let g::Entry::Block(x) = entry else {
                    return Err(SyntaxError {
                        filename: self.filename.into(),
                        message: "trace requires operation blocks".into(),
                        span: self.spans.get(path).cloned(),
                    });
                };
                let token = id_token(&x.block.id);
                let child = format!("{path}/{index}");
                self.spans.insert(child.clone(), self.span(token));
                self.spans
                    .insert(format!("{child}/operation"), self.span(token));
                let mut frame = self.entries(
                    x.block.block_list.iter().map(|e| &*e.entry).collect(),
                    "trace_frame",
                    &child,
                )?;
                frame["operation"] = json!(token.text());
                for key in ["inputs", "observe"] {
                    if frame.get(key).is_none() {
                        frame[key] = json!({});
                    }
                }
                frames.push(frame);
            }
            return Ok(Value::Array(frames));
        }
        let mut result = Map::new();
        for entry in entries {
            let (id, kind) = match entry {
                g::Entry::Block(x) => (&*x.block.id, "block"),
                g::Entry::Declaration(x) => (&*x.declaration.id, "declaration"),
                g::Entry::Assignment(x) => (&*x.assignment.id, "assignment"),
                g::Entry::Property(x) => (&*x.property.id, "property"),
            };
            let token = id_token(id);
            let raw = token.text();
            let span = self.span(token);
            let key = match (context, raw) {
                ("root", "contract") => "program_contract",
                ("contract", "pre") => "precondition",
                ("contract", "post") => "postcondition",
                ("contract", "partition") => "partitioning",
                _ => raw,
            };
            if result.contains_key(key) {
                return Err(self.error(&span, format!("duplicate {raw} in {context}")));
            }
            let child = format!("{path}/{key}");
            self.spans.insert(child.clone(), span.clone());
            let value = match entry {
                g::Entry::Block(x) => {
                    let subcontext = match (context, raw) {
                        ("root", "inputs") | ("specroot", "inputs" | "observations") => {
                            "declarations"
                        }
                        ("specroot", "operations") => "operations",
                        ("specroot", "implementation") => "rel_impl",
                        ("rel_impl", "state") => "declarations",
                        ("rel_impl", "reset" | "next" | "wires" | "operations") => "assignments",
                        ("rel_impl", "binding") => "rel_binding",
                        ("rel_binding", "states") => "state_maps",
                        ("state_maps", _) => "assignments",
                        ("rel_binding", "observations") => "assignments",
                        ("specroot", "components") => "component_group",
                        ("specroot", "compositions") => "composition_group",
                        ("operations", _) => "operation",
                        ("component_group", _) => "component",
                        ("composition_group", _) => "composition",
                        ("component", "state") => "declarations",
                        ("component", "steps") => "assignments",
                        ("component" | "composition", "examples") => "example_group",
                        ("example_group", _) => "example",
                        ("example", "initial") => "assignments",
                        ("example", "trace") => "trace",
                        ("trace_frame", "inputs" | "observe") => "assignments",
                        ("root", "spec" | "impl") => "machine",
                        ("root", "progress") => "progress",
                        ("root", "contract") => "contract",
                        ("machine", "state") => "declarations",
                        ("machine", "reset" | "next" | "wires" | "outputs") => "assignments",
                        ("contract", "parameters") => "declarations",
                        ("contract", "split") => "split",
                        ("contract", "cases") => "assignments",
                        _ => {
                            return Err(
                                self.error(&span, format!("unexpected block {raw} in {context}"))
                            )
                        }
                    };
                    self.entries(
                        x.block.block_list.iter().map(|e| &*e.entry).collect(),
                        subcontext,
                        &child,
                    )?
                }
                g::Entry::Declaration(x) => {
                    if context != "declarations" {
                        return Err(
                            self.error(&span, format!("unexpected declaration in {context}"))
                        );
                    }
                    if raw.contains('.') {
                        return Err(self.error(&span, "declaration names must not contain dots"));
                    }
                    self.ty(&x.declaration.r#type)?
                }
                g::Entry::Assignment(x) => {
                    let expr = self.expr(&x.assignment.expr)?;
                    self.record(&child, &expr);
                    if context == "split" {
                        let a=expr.value.as_array().filter(|a|a.len()==4&&a[0]=="range").ok_or_else(||self.error(&span,"split must be name = range(expression, inclusive_min, inclusive_max)"))?;
                        if !a[2].is_u64() || !a[3].is_u64() {
                            return Err(
                                self.error(&span, "split bounds must be unsigned integer literals")
                            );
                        }
                        for (index, field) in [(1, "expr"), (2, "min"), (3, "max")] {
                            let prefix = format!("/{index}");
                            for (path, span) in &expr.descendants {
                                if path == &prefix || path.starts_with(&format!("{prefix}/")) {
                                    self.spans.insert(
                                        format!("{child}/{field}{}", &path[prefix.len()..]),
                                        span.clone(),
                                    );
                                }
                            }
                        }
                        json!({"expr":a[1],"min":a[2],"max":a[3]})
                    } else if context == "assignments" {
                        expr.value
                    } else {
                        return Err(
                            self.error(&span, format!("unexpected assignment in {context}"))
                        );
                    }
                }
                g::Entry::Property(x) => {
                    let allowed = match context {
                        "root" => {
                            ["reset_input", "binding", "commit", "can_step", "hold_when"].as_slice()
                        }
                        "rel_impl" => ["composition", "reset_input"].as_slice(),
                        "component" => ["init", "invariant"].as_slice(),
                        "composition" => ["members"].as_slice(),
                        "example" => ["expect"].as_slice(),
                        "progress" => ["enabled", "rank"].as_slice(),
                        "contract" => [
                            "pre",
                            "precondition",
                            "invariant",
                            "terminal",
                            "post",
                            "postcondition",
                            "rank",
                            "partition",
                            "partitioning",
                        ]
                        .as_slice(),
                        _ => &[],
                    };
                    if !allowed.contains(&raw) {
                        return Err(
                            self.error(&span, format!("unexpected property {raw} in {context}"))
                        );
                    }
                    let expr = self.expr(&x.property.expr)?;
                    self.record(&child, &expr);
                    if context == "composition" && raw == "members" {
                        let a = expr
                            .value
                            .as_array()
                            .filter(|a| a.first() == Some(&json!("compose")))
                            .ok_or_else(|| {
                                self.error(
                                    &span,
                                    "members requires compose(Component, OtherComposition, ...)",
                                )
                            })?;
                        if a[1..].iter().any(|x| !x.is_string()) {
                            return Err(self.error(&span, "composition members must be names"));
                        }
                        for (index, _) in a[1..].iter().enumerate() {
                            if let Some(s) = expr.descendants.get(&format!("/{}", index + 1)) {
                                self.spans.insert(format!("{child}/{index}"), s.clone());
                            }
                        }
                        Value::Array(a[1..].to_vec())
                    } else {
                        expr.value
                    }
                }
            };
            let _ = kind;
            result.insert(key.into(), value);
        }
        Ok(Value::Object(result))
    }
}
macro_rules! chain {
    ($f:ident,$ty:ident,$first:ident,$list:ident,$op:ident,$rhs:ident,$next:ident) => {
        impl Lower<'_> {
            fn $f(&self, e: &g::$ty<'_>) -> Res<E> {
                let mut x = self.$next(&e.$first)?;
                for a in &e.$list {
                    x = self.binary(a.$op.$op.text(), x, self.$next(&a.$rhs)?)?;
                }
                Ok(x)
            }
        }
    };
}
chain!(or, Or, and, or_list, or_op, and, and);
chain!(and, And, bit_or, and_list, and_op, bit_or, bit_or);
chain!(
    bit_or,
    BitOr,
    bit_xor,
    bit_or_list,
    bit_or_op,
    bit_xor,
    bit_xor
);
chain!(
    bit_xor,
    BitXor,
    bit_and,
    bit_xor_list,
    bit_xor_op,
    bit_and,
    bit_and
);
chain!(
    bit_and,
    BitAnd,
    compare,
    bit_and_list,
    bit_and_op,
    compare,
    compare
);
chain!(shift, Shift, sum, shift_list, shift_op, sum, sum);
impl Lower<'_> {
    fn sum(&self, e: &g::Sum<'_>) -> Res<E> {
        let mut x = self.product(&e.product)?;
        for a in &e.sum_list {
            x = self.binary(sum_token(&a.sum_op).text(), x, self.product(&a.product)?)?;
        }
        Ok(x)
    }
}
chain!(
    product,
    Product,
    unary,
    product_list,
    product_op,
    unary,
    unary
);
pub fn parse_document(source: &str, filename: &str) -> Res<ParsedDocument> {
    let mut grammar = HwvGrammar::default();
    hwv_parser::parse(source, filename, &mut grammar)
        .map_err(|e| parser_error(source, filename, e))?;
    let root = grammar.root.ok_or_else(|| SyntaxError {
        filename: filename.into(),
        message: "parser did not produce a document".into(),
        span: None,
    })?;
    let mut lower = Lower {
        source,
        filename,
        spans: BTreeMap::new(),
    };
    lower
        .spans
        .insert("".into(), lower.span(&root.string.string));
    let mut canonical = lower.entries(
        root.document_list.iter().map(|e| &*e.entry).collect(),
        if root.doc_kind.doc_kind.text() == "specification" {
            "specroot"
        } else {
            "root"
        },
        "",
    )?;
    let relational = root.doc_kind.doc_kind.text() == "specification";
    canonical["version"] = json!(if relational { 3 } else { 2 });
    if relational {
        canonical["kind"] = json!("specification");
    }
    canonical["name"] = serde_json::from_str(root.string.string.text()).map_err(|e| {
        lower.error(
            &lower.span(&root.string.string),
            format!("invalid design string: {e}"),
        )
    })?;
    Ok(ParsedDocument {
        canonical,
        spans: lower.spans,
        filename: filename.into(),
    })
}

fn cmp_token<'a, 't>(op: &'a g::CompareOp<'t>) -> &'a parol_runtime::Token<'t> {
    match op {
        g::CompareOp::Equal(x) => &x.equal.equal,
        g::CompareOp::Unequal(x) => &x.unequal.unequal,
        g::CompareOp::Le(x) => &x.le.le,
        g::CompareOp::Ge(x) => &x.ge.ge,
        g::CompareOp::Lt(x) => &x.lt.lt,
        g::CompareOp::Gt(x) => &x.gt.gt,
    }
}
fn sum_token<'a, 't>(op: &'a g::SumOp<'t>) -> &'a parol_runtime::Token<'t> {
    match op {
        g::SumOp::Plus(x) => &x.plus.plus,
        g::SumOp::Minus(x) => &x.minus.minus,
    }
}
fn prefix_token<'a, 't>(op: &'a g::PrefixOp<'t>) -> &'a parol_runtime::Token<'t> {
    match op {
        g::PrefixOp::Bang(x) => &x.bang.bang,
        g::PrefixOp::Tilde(x) => &x.tilde.tilde,
        g::PrefixOp::Minus(x) => &x.minus.minus,
    }
}

fn parser_error(source: &str, filename: &str, error: parol_runtime::ParolError) -> SyntaxError {
    use parol_runtime::{ParolError, ParserError};
    let (message, location) = match &error {
        ParolError::ParserError(ParserError::SyntaxErrors { entries }) if !entries.is_empty() => {
            let e = &entries[0];
            (
                format!(
                    "syntax error; expected {}{}",
                    e.expected_tokens,
                    if entries.len() > 1 {
                        format!(" ({} parse errors)", entries.len())
                    } else {
                        String::new()
                    }
                ),
                Some(&*e.error_location),
            )
        }
        ParolError::ParserError(ParserError::UnprocessedInput { last_token, .. }) => {
            ("unexpected trailing input".into(), Some(&**last_token))
        }
        ParolError::ParserError(ParserError::Unsupported { error_location, .. }) => {
            (error.to_string(), Some(&**error_location))
        }
        _ => (error.to_string(), None),
    };
    let span = location.map(|l| {
        let mut start = if l.start_line == 0 {
            source.len()
        } else {
            (l.start as usize).min(source.len())
        };
        while !source.is_char_boundary(start) {
            start -= 1;
        }
        let prefix = &source[..start];
        Span {
            start,
            end: (l.end as usize).max(start).min(source.len()),
            line: prefix.bytes().filter(|b| *b == b'\n').count() + 1,
            column: prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1,
        }
    });
    SyntaxError {
        filename: filename.into(),
        message,
        span,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding(s: &str) -> Value {
        parse_document(&format!("design \"t\" {{ binding {s}; }}"), "test.hwv")
            .unwrap()
            .canonical["binding"]
            .clone()
    }
    #[test]
    fn surface_precedence() {
        assert_eq!(
            binding("1u8 + 2u8 * 3u8"),
            json!(["add", ["bv", 8, 1], ["mul", ["bv", 8, 2], ["bv", 8, 3]]])
        );
        assert_eq!(
            binding("!false && true || false"),
            json!(["or", ["and", ["not", false], true], false])
        );
    }
    #[test]
    fn comparison_and_negative() {
        assert_eq!(
            binding("1u8 < 2u8"),
            json!(["ult", ["bv", 8, 1], ["bv", 8, 2]])
        );
        assert_eq!(binding("bv(8,-1)"), json!(["bv", 8, -1]));
    }
    #[test]
    fn sugar() {
        assert_eq!(
            binding("if true { m[0u2] } else { 3u8 }"),
            json!(["ite", true, ["read", "m", ["bv", 2, 0]], ["bv", 8, 3]])
        );
    }
    #[test]
    fn duplicates_and_recovery_fail() {
        for s in [
            "design \"t\" { inputs { x: bool; x: bool; }}",
            "design \"t\" { binding true binding false; }",
            "design \"t\" { binding true; } garbage",
            "design \"t\" { binding 1u8 < 2u8 < 3u8; }",
            "design \"t\" { mystery true; }",
        ] {
            assert!(parse_document(s, "bad.hwv").is_err(), "{s}")
        }
    }
    #[test]
    fn json_rejects_duplicates() {
        assert!(parse_json(br#"{"x":1,"x":2}"#).is_err());
    }
    #[test]
    fn corpus_parses() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for (dsl, json) in [
            (
                "examples/auto_array_sum.hwv",
                "examples/auto_array_sum.json",
            ),
            ("examples/array_sum.hwv", "examples/array_sum.json"),
            (
                "examples/memory_increment.hwv",
                "audit/structural_solver/memory_increment_good.json",
            ),
        ] {
            let s = std::fs::read_to_string(root.join(dsl)).unwrap();
            let p = parse_document(&s, dsl).unwrap();
            let j = parse_json(&std::fs::read(root.join(json)).unwrap()).unwrap();
            assert_eq!(p.canonical, j, "{dsl}");
            p.validate().unwrap();
        }
    }
}
#[cfg(test)]
mod specification_surface_tests {
    use super::*;
    #[test]
    fn relational_components_and_ordered_repeated_operations_lower() {
        let source = r#"specification "views" {
          inputs { b: bool; } observations { value: bv<2>; }
          operations { tick {} }
          components {
            Counter {
              state { x: bv<2>; }
              init s.x == 0u2;
              invariant o.value == s.x;
              steps { tick = n.x == s.x + 1u2; }
              examples {
                rises {
                  expect positive; initial { value = 0u2; }
                  trace {
                    tick { inputs { b = true; } observe { value = 1u2; } }
                    tick { observe { value = 2u2; } }
                  }
                }
              }
            }
          }
          compositions { Wrapped { members compose(Counter); examples {} } }
        }"#;
        let p = parse_document(source, "views.hwv").unwrap();
        assert_eq!(p.canonical["version"], 3);
        assert_eq!(p.canonical["kind"], "specification");
        assert_eq!(
            p.canonical["compositions"]["Wrapped"]["members"],
            json!(["Counter"])
        );
        let trace = &p.canonical["components"]["Counter"]["examples"]["rises"]["trace"];
        assert_eq!(trace.as_array().unwrap().len(), 2);
        assert_eq!(trace[0]["inputs"]["b"], true);
        assert_eq!(trace[1]["inputs"], json!({}));
        assert_eq!(trace[0]["observe"]["value"], json!(["bv", 2, 1]));
        assert_eq!(trace[1]["observe"]["value"], json!(["bv", 2, 2]));
        let span = p
            .span_for("/components/Counter/examples/rises/trace/1/observe/value")
            .unwrap();
        assert_eq!(&source[span.start..span.end], "2u2");
    }
    #[test]
    fn invalid_member_expressions_and_malformed_traces_rejected() {
        for source in [
          "specification \"x\" { compositions { P {members compose(1u8);}}}",
          "specification \"x\" { components { C { examples { p {expect positive;trace {x=1u8;}}}}}}",
          "specification \"x\" { components { C {state {} state {}}}}",
        ] { assert!(parse_document(source,"bad.hwv").is_err()); }
    }
}
