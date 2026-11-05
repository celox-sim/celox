//! Property specifications and named sequence and property declarations in
//! the Boolean subset (IEEE 1800-2023 16.8, 16.12), and the substitution of
//! their instances in the concurrent assertions of a design element.
use super::*;
use fxhash::FxHashMap as HashMap;

/// Sequence and property operators, and the starts of assertion variable
/// declarations, outside the Boolean subset. A declaration using one keeps
/// its captured grammar boundary.
const TEMPORAL: &[&str] = &[
    "##",
    "[*",
    "[=",
    "[->",
    "|=>",
    "#-#",
    "#=#",
    "@",
    "and",
    "or",
    "not",
    "iff",
    "implies",
    "throughout",
    "within",
    "intersect",
    "until",
    "s_until",
    "until_with",
    "s_until_with",
    "strong",
    "weak",
    "nexttime",
    "s_nexttime",
    "always",
    "s_always",
    "eventually",
    "s_eventually",
    "accept_on",
    "reject_on",
    "sync_accept_on",
    "sync_reject_on",
    "first_match",
    "if",
    "case",
    "var",
];

impl Parser<'_> {
    /// The offset after the clocking event `@(...)` or `@name` that starts at
    /// `offset`, whose event expression may use `or`, `,` and `iff` (IEEE
    /// 1800-2023 9.4.2); `offset` itself when no clocking event starts there.
    pub(super) fn past_clocking_event(&self, offset: usize) -> usize {
        if self.nth(offset) != "@" {
            return offset;
        }
        if self.nth(offset + 1) != "(" {
            return offset + 2;
        }
        let mut depth = 0usize;
        let mut at = offset + 1;
        loop {
            match self.nth(at) {
                "" => return at,
                "(" => depth += 1,
                ")" => {
                    depth -= 1;
                    if depth == 0 {
                        return at + 1;
                    }
                }
                _ => {}
            }
            at += 1;
        }
    }

    /// Whether the word at `offset` of a property, after its leading clock,
    /// is outside the Boolean subset; `iff` of `disable iff` is not.
    pub(super) fn temporal(&self, word: &str, offset: usize) -> bool {
        TEMPORAL.contains(&word) && !(word == "iff" && self.nth(offset - 1) == "disable")
    }

    /// `[clocking_event] [disable iff (expression)] expression [|-> expression]`.
    /// A sequence has neither `disable iff` nor an implication.
    pub(super) fn property_spec(&mut self, sequence: bool) -> Result<PropertySpec> {
        let clock = if self.peek() == "@" {
            Some(self.typed_timing_control()?)
        } else {
            None
        };
        let disable = if !sequence && self.eat("disable") {
            self.expect("iff")?;
            Some(self.parenthesized_expression()?)
        } else {
            None
        };
        let first = self.expression(0)?;
        let (antecedent, condition) = if !sequence && self.eat("|->") {
            (Some(first), self.expression(0)?)
        } else {
            (None, first)
        };
        Ok(PropertySpec {
            clock,
            disable,
            antecedent,
            condition,
        })
    }

    /// A named sequence or property declaration without formal arguments or
    /// assertion variables whose body is a Boolean property specification.
    /// Any other declaration leaves the cursor and arenas unchanged and is
    /// captured as before.
    pub(super) fn assertion_declaration(&mut self) -> Result<Option<AssertionDeclarationId>> {
        let start = self.at;
        let end = if self.peek() == "sequence" {
            "endsequence"
        } else {
            "endproperty"
        };
        // `sequence name ;`, then the body.
        let mut offset = self.past_clocking_event(3);
        loop {
            match self.nth(offset) {
                "" | "endproperty" | "endsequence" if self.nth(offset) != end => return Ok(None),
                word if word == end => break,
                word if self.temporal(word, offset) => return Ok(None),
                _ => offset += 1,
            }
        }
        if self.assertion_declarations.len() >= 1_000_000 {
            return Err(
                self.unsupported("native assertion-declaration arena exceeds one million entries")
            );
        }
        let lengths = (
            self.expressions.len(),
            self.types.len(),
            self.timing_controls.len(),
        );
        match self.typed_assertion_declaration(start, end) {
            Ok(Some(declaration)) => {
                let id = AssertionDeclarationId(self.assertion_declarations.len());
                self.assertion_declarations.push(declaration);
                Ok(Some(id))
            }
            Ok(None) | Err(_) => {
                self.at = start;
                self.expressions.truncate(lengths.0);
                self.types.truncate(lengths.1);
                self.timing_controls.truncate(lengths.2);
                Ok(None)
            }
        }
    }

    fn typed_assertion_declaration(
        &mut self,
        start: usize,
        end: &str,
    ) -> Result<Option<AssertionDeclaration>> {
        let kind = if self.eat("sequence") {
            AssertionDeclarationKind::Sequence
        } else {
            self.expect("property")?;
            AssertionDeclarationKind::Property
        };
        let name = self.name()?;
        if !self.eat(";") {
            return Ok(None);
        }
        let body = self.property_spec(kind == AssertionDeclarationKind::Sequence)?;
        self.eat(";");
        if !self.eat(end) {
            return Ok(None);
        }
        let end_label = if self.eat(":") {
            Some(self.name()?)
        } else {
            None
        };
        if end_label
            .as_ref()
            .is_some_and(|label| label.value() != name.value())
        {
            return Ok(None);
        }
        Ok(Some(AssertionDeclaration {
            kind,
            name,
            body,
            end_label,
            tokens: self.range(start),
        }))
    }

    /// Substitute the instances of `declarations` in the property of each
    /// concurrent assertion in `assertions`, all items of one design element
    /// outside generate blocks, so that names in a declaration resolve where
    /// they would in the declaration (IEEE 1800-2023 16.8, 16.12). The
    /// assertion then waits for its leading clock.
    pub(super) fn substitute_assertion_instances(
        &mut self,
        declarations: &[AssertionDeclarationId],
        assertions: &[ProcedureId],
    ) -> Result<()> {
        let mut names = HashMap::default();
        for &id in declarations {
            let name = &self.assertion_declarations[id.0].name;
            if names.insert(name.value().to_owned(), id).is_some() {
                return Err(self.error_at(
                    self.assertion_declarations[id.0].tokens,
                    format!("`{}` is declared twice", name.value()),
                ));
            }
        }
        let mut resolved = HashMap::default();
        for &procedure in assertions {
            let statement = self.procedures[procedure.0].body;
            let StatementKind::Assertion {
                header: AssertionHeader::Concurrent { property, .. },
                ..
            } = self.statements[statement.0].kind
            else {
                continue;
            };
            self.resolve_declarations(property, &names, &mut resolved)?;
            let substituted = self.substitute_instances(property, &names, &resolved)?;
            if let StatementKind::Assertion {
                header: AssertionHeader::Concurrent { property, .. },
                ..
            } = &mut self.statements[statement.0].kind
            {
                *property = substituted;
            }
            self.wrap_module_assertion(procedure);
        }
        Ok(())
    }

    /// The declaration a bare (possibly parenthesized) name instantiates.
    fn instance(
        &self,
        id: ExprId,
        names: &HashMap<String, AssertionDeclarationId>,
    ) -> Option<AssertionDeclarationId> {
        match &self.expressions[id.0].kind {
            ExprKind::Group(inner) => self.instance(*inner, names),
            ExprKind::Name(name) => names.get(name.value()).copied(),
            _ => None,
        }
    }

    /// The instances of declarations that `spec` writes as its antecedent or
    /// its condition.
    fn instances(
        &self,
        spec: PropertySpec,
        names: &HashMap<String, AssertionDeclarationId>,
    ) -> impl Iterator<Item = (ExprId, AssertionDeclarationId)> {
        [spec.antecedent, Some(spec.condition)]
            .into_iter()
            .flatten()
            .filter_map(move |expr| self.instance(expr, names).map(|id| (expr, id)))
    }

    /// Substitute the body of each declaration that `spec` instantiates,
    /// directly or through other declarations, into `resolved`. Each
    /// declaration resolves once, after those it instantiates, on an explicit
    /// stack, so a long chain of declarations does not grow the call stack.
    fn resolve_declarations(
        &self,
        spec: PropertySpec,
        names: &HashMap<String, AssertionDeclarationId>,
        resolved: &mut HashMap<AssertionDeclarationId, PropertySpec>,
    ) -> Result<()> {
        let mut pending: Vec<_> = self.instances(spec, names).map(|(_, id)| id).collect();
        // Declarations whose instances are being resolved: the path from an
        // instance in `spec` to the top of `pending`.
        let mut active = fxhash::FxHashSet::default();
        while let Some(&id) = pending.last() {
            if resolved.contains_key(&id) {
                pending.pop();
                continue;
            }
            let body = self.assertion_declarations[id.0].body;
            if active.insert(id) {
                for (instance, callee) in self.instances(body, names) {
                    if active.contains(&callee) {
                        // Without a temporal operator such a cycle has no
                        // meaning (16.8, 16.12.17).
                        return Err(self.error_at(
                            self.expressions[instance.0].tokens,
                            format!(
                                "`{}` instantiates itself",
                                self.assertion_declarations[callee.0].name.value()
                            ),
                        ));
                    }
                    pending.push(callee);
                }
                continue;
            }
            active.remove(&id);
            let body = self.substitute_instances(body, names, resolved)?;
            resolved.insert(id, body);
            pending.pop();
        }
        Ok(())
    }

    /// `spec` with the resolved bodies of the declarations it instantiates.
    fn substitute_instances(
        &self,
        spec: PropertySpec,
        names: &HashMap<String, AssertionDeclarationId>,
        resolved: &HashMap<AssertionDeclarationId, PropertySpec>,
    ) -> Result<PropertySpec> {
        let mut result = spec;
        if let Some(antecedent) = spec.antecedent
            && let Some(id) = self.instance(antecedent, names)
        {
            let declaration = &self.assertion_declarations[id.0];
            if declaration.kind == AssertionDeclarationKind::Property {
                return Err(self.error_at(
                    self.expressions[antecedent.0].tokens,
                    format!(
                        "property `{}` is the antecedent of an implication, which is a sequence (IEEE 1800-2023 16.12.7)",
                        declaration.name.value()
                    ),
                ));
            }
            let inner = resolved[&id];
            result.antecedent = Some(inner.condition);
            // The leading clock of `m |-> p` is that of `m` (16.16.1).
            result.clock = inner.clock.or(result.clock);
        }
        let Some(id) = self.instance(spec.condition, names) else {
            return Ok(result);
        };
        let inner = resolved[&id];
        let name = self.assertion_declarations[id.0].name.value();
        let tokens = self.expressions[spec.condition.0].tokens;
        // An instance with `disable iff` may stand where a property_spec may,
        // after the assertion's clocking event, but is never a property_expr
        // operand under another `disable iff` or an implication (16.12,
        // 16.12.1).
        if inner.disable.is_some() && (spec.disable.is_some() || spec.antecedent.is_some()) {
            return Err(self.error_at(
                tokens,
                format!(
                    "property `{name}` has `disable iff` and is instantiated in a property expression (IEEE 1800-2023 16.12.1)"
                ),
            ));
        }
        if spec.antecedent.is_none() {
            result.disable = inner.disable.or(spec.disable);
            result.antecedent = inner.antecedent;
            result.condition = inner.condition;
            // An explicit leading clock of the instance overrides the
            // incoming one (16.16.1).
            result.clock = inner.clock.or(spec.clock);
            return Ok(result);
        }
        if inner.antecedent.is_some() {
            return Err(self.unsupported_at(
                tokens,
                format!("implication `{name}` as the consequent of an implication"),
            ));
        }
        if let Some(clock) = inner.clock
            && result
                .clock
                .is_none_or(|leading| !self.same_clock(leading, clock))
        {
            return Err(self.unsupported_at(
                tokens,
                format!("multiclocked property: consequent `{name}` has its own clocking event"),
            ));
        }
        result.condition = inner.condition;
        Ok(result)
    }

    /// Clocking events written alike, `@(posedge clk)` twice.
    fn same_clock(&self, left: TimingControlId, right: TimingControlId) -> bool {
        let words = |id: TimingControlId| {
            self.tokens[self.timing_controls[id.0].tokens.range()]
                .iter()
                .filter(|token| !token.is_trivia())
                .map(|token| token.text.as_str())
        };
        words(left).eq(words(right))
    }

    /// A clocked concurrent assertion module item waits for its leading
    /// clock (IEEE 1800-2023 16.14); without one it is left as it is.
    pub(super) fn wrap_module_assertion(&mut self, procedure: ProcedureId) {
        let assertion = self.procedures[procedure.0].body;
        let StatementKind::Assertion {
            header:
                AssertionHeader::Concurrent {
                    property:
                        PropertySpec {
                            clock: Some(control),
                            ..
                        },
                    ..
                },
            ..
        } = self.statements[assertion.0].kind
        else {
            return;
        };
        let mut wrapper = self.statements[assertion.0].clone();
        wrapper.kind = StatementKind::Timing {
            control,
            body: assertion,
        };
        wrapper.label = None;
        let body = StatementId(self.statements.len());
        self.statements.push(wrapper);
        self.procedures[procedure.0].body = body;
        self.procedures[procedure.0].authored_statements = self.body_membership(&[body]);
    }
}
