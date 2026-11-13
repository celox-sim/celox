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

/// The most Boolean operands of one property specification.
const MAX_SEQUENCE_OPERANDS: usize = 1024;

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
        // Repetition `[*`, `[=`, `[->` and `[+]` (16.9.2) arrives as
        // separate tokens.
        if word == "[" {
            return match self.nth(offset + 1) {
                "*" | "=" | "->" => true,
                "+" => self.nth(offset + 2) == "]",
                _ => false,
            };
        }
        TEMPORAL.contains(&word)
            && !(word == "iff" && self.nth(offset - 1) == "disable")
            // A cycle delay `##n` or `##[m:n]` (16.7), and `|=>` (16.12.7).
            && !(word == "##"
                && (self.nth(offset + 1).starts_with(|c: char| c.is_ascii_digit())
                    || (self.nth(offset + 1) == "["
                        && self.nth(offset + 2).starts_with(|c: char| c.is_ascii_digit()))))
            && word != "|=>"
            // The sequence operators `or`, `and`, `intersect` and
            // `throughout` (16.9.5 to 16.9.7, 16.9.9), and the clocking
            // event of a subsequence (16.13).
            && !matches!(word, "or" | "and" | "intersect" | "throughout" | "@")
    }

    /// `[clocking_event] [disable iff (expression)] sequence [|-> sequence]`
    /// (16.7, 16.12). A sequence declaration has neither `disable iff` nor an
    /// implication. A Boolean property of one step, also behind `|->`, has no
    /// `sequence`.
    pub(super) fn property_spec(
        &mut self,
        sequence: bool,
        locals: Vec<SequenceLocal>,
    ) -> Result<PropertySpec> {
        self.sequence_operands = 0;
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
        let first = self.sequence_expr()?;
        let (antecedent, consequent) = if !sequence && matches!(self.peek(), "|->" | "|=>") {
            let next = self.peek() == "|=>";
            self.at += 1;
            let consequent = self.sequence_expr()?;
            // `|=>` is `|-> ##1` (16.12.7).
            let consequent = if next {
                SequenceExpr::Concat {
                    left: None,
                    delay: CycleDelay { min: 1, max: 1 },
                    right: Box::new(consequent),
                }
            } else {
                consequent
            };
            (Some(first), consequent)
        } else {
            (None, first)
        };
        // `p iff q` binds tighter than an implication (Table 16-3), so it is
        // the consequent's: both of its operands are Boolean here.
        let equivalent = if !sequence && self.peek() == "iff" {
            self.at += 1;
            let right = self.sequence_expr()?;
            match (consequent.boolean(), right.boolean()) {
                (Some(_), Some(right))
                    if locals.is_empty()
                        && antecedent
                            .as_ref()
                            .is_none_or(|antecedent| antecedent.boolean().is_some())
                        && self.peek() != "iff" =>
                {
                    Some(right)
                }
                _ => return Err(self.unsupported("iff of properties over cycles")),
            }
        } else {
            None
        };
        if locals.is_empty()
            && let Some(condition) = consequent.boolean()
            && antecedent
                .as_ref()
                .is_none_or(|antecedent| antecedent.boolean().is_some())
        {
            return Ok(PropertySpec {
                clock,
                disable,
                antecedent: antecedent.and_then(|antecedent| antecedent.boolean()),
                condition,
                equivalent,
                sequence: None,
            });
        }
        let condition = consequent
            .booleans()
            .last()
            .expect("a sequence has a Boolean expression")
            .expression;
        let id = SequenceId(self.sequences.len());
        self.sequences.push(Sequence {
            antecedent,
            consequent,
            locals,
        });
        Ok(PropertySpec {
            clock,
            disable,
            antecedent: None,
            condition,
            equivalent: None,
            sequence: Some(id),
        })
    }

    /// A sequence expression of the supported subset of Syntax 16-3: `or`
    /// (16.9.7) of `and` (16.9.5) of `intersect` (16.9.6) of
    /// concatenations, each binding tighter than the one before it, all
    /// left-associative (Table 16-1).
    fn sequence_expr(&mut self) -> Result<SequenceExpr> {
        let mut left = self.sequence_and()?;
        while self.eat("or") {
            let right = self.sequence_and()?;
            left = SequenceExpr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn sequence_and(&mut self) -> Result<SequenceExpr> {
        let mut left = self.sequence_intersect()?;
        while self.eat("and") {
            let right = self.sequence_intersect()?;
            left = SequenceExpr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn sequence_intersect(&mut self) -> Result<SequenceExpr> {
        let mut left = self.sequence_throughout()?;
        while self.eat("intersect") {
            let right = self.sequence_throughout()?;
            left = SequenceExpr::Intersect(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// `condition throughout sequence` (16.9.9), right-associative and
    /// binding looser than a cycle delay (Table 16-1), or a concatenation.
    fn sequence_throughout(&mut self) -> Result<SequenceExpr> {
        let left = self.sequence_concat()?;
        if !self.eat("throughout") {
            return Ok(left);
        }
        let condition = match left {
            SequenceExpr::Boolean(condition) if condition.items.is_empty() => condition,
            _ => {
                return Err(
                    self.unsupported("throughout whose left operand is not a Boolean expression")
                );
            }
        };
        let sequence = self.sequence_throughout()?;
        Ok(SequenceExpr::Throughout {
            condition,
            sequence: Box::new(sequence),
        })
    }

    /// `[##delay] operand {##delay operand}` (16.7), left to right.
    fn sequence_concat(&mut self) -> Result<SequenceExpr> {
        let mut left = if self.peek() == "##" {
            None
        } else {
            Some(self.sequence_operand()?)
        };
        while self.eat("##") {
            let delay = self.cycle_delay_range()?;
            let right = self.sequence_operand()?;
            left = Some(SequenceExpr::Concat {
                left: left.map(Box::new),
                delay,
                right: Box::new(right),
            });
        }
        left.ok_or_else(|| self.unsupported("empty sequence"))
    }

    /// A parenthesized sequence `(s)`, a Boolean expression, or a Boolean
    /// expression with match items `(b, item {, item})` (16.7, 16.10).
    fn sequence_operand(&mut self) -> Result<SequenceExpr> {
        // `@(event) s` clocks the rest of the sequence (16.13).
        if self.peek() == "@" {
            let clock = self.typed_timing_control()?;
            let sequence = self.sequence_expr()?;
            return Ok(SequenceExpr::Clocked {
                clock,
                sequence: Box::new(sequence),
            });
        }
        // The tree of a longer one would nest as deep.
        self.sequence_operands += 1;
        if self.sequence_operands > MAX_SEQUENCE_OPERANDS {
            return Err(self.unsupported(format!(
                "property with more than {MAX_SEQUENCE_OPERANDS} Boolean operands"
            )));
        }
        if self.peek() == "(" && self.group_holds_sequence() {
            self.at += 1;
            let group = self.sequence_expr()?;
            self.expect(")")?;
            return Ok(group);
        }
        let start = self.at;
        let (expression, items) = if self.peek() == "(" && self.match_items_follow() {
            self.at += 1;
            let expression = self.expression(0)?;
            let mut items = Vec::new();
            while self.eat(",") {
                items.push(self.match_item()?);
            }
            self.expect(")")?;
            (expression, items)
        } else {
            (self.expression(0)?, Vec::new())
        };
        Ok(SequenceExpr::Boolean(SequenceBoolean {
            expression,
            items,
            tokens: self.range(start),
        }))
    }

    /// The delay after `##`: `n` or `[min:max]`, decimal numbers (16.7).
    fn cycle_delay_range(&mut self) -> Result<CycleDelay> {
        let start = self.at;
        if !self.eat("[") {
            let ticks = self.cycle_delay()?;
            return Ok(CycleDelay {
                min: ticks,
                max: ticks,
            });
        }
        let min = self.cycle_delay()?;
        self.expect(":")?;
        if self.peek() == "$" {
            return Err(self.unsupported("cycle delay range without an upper bound `##[m:$]`"));
        }
        let max = self.cycle_delay()?;
        self.expect("]")?;
        if max < min {
            return Err(self.error_at(
                self.range(start),
                "cycle delay range whose upper bound is below its lower bound (IEEE 1800-2023 16.7)".to_string(),
            ));
        }
        Ok(CycleDelay { min, max })
    }

    /// The number of ticks of `##n`, a decimal number.
    fn cycle_delay(&mut self) -> Result<u32> {
        let value = self
            .peek()
            .replace('_', "")
            .parse::<u32>()
            .map_err(|_| self.unsupported("cycle delay other than a decimal number"))?;
        self.at += 1;
        Ok(value)
    }

    /// Whether the parenthesis at the cursor holds a cycle delay or a
    /// sequence operator at its own level, as a parenthesized sequence does.
    fn group_holds_sequence(&self) -> bool {
        let mut depth = 0usize;
        let mut offset = 0;
        loop {
            match self.nth(offset) {
                "" => return false,
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => {
                    depth -= 1;
                    if depth == 0 {
                        return false;
                    }
                }
                // At any depth: a Boolean expression has no sequence
                // operator, also in its own parentheses, as `((a or b))`.
                "##" | "or" | "and" | "intersect" | "throughout" if depth >= 1 => return true,
                _ => {}
            }
            offset += 1;
        }
    }

    /// Whether the parenthesis at the cursor holds a comma at its own level,
    /// which a step with match items has and an expression does not.
    fn match_items_follow(&self) -> bool {
        let mut depth = 0usize;
        let mut offset = 0;
        loop {
            match self.nth(offset) {
                "" => return false,
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => {
                    depth -= 1;
                    if depth == 0 {
                        return false;
                    }
                }
                "," if depth == 1 => return true,
                _ => {}
            }
            offset += 1;
        }
    }

    /// An operator assignment, an increment or decrement, or a subroutine
    /// call (16.10, 16.11).
    fn match_item(&mut self) -> Result<SequenceMatchItem> {
        let expression = self.expression(0)?;
        let operator = self.peek().to_owned();
        if matches!(
            operator.as_str(),
            "=" | "+="
                | "-="
                | "*="
                | "/="
                | "%="
                | "&="
                | "|="
                | "^="
                | "<<="
                | ">>="
                | "<<<="
                | ">>>="
        ) {
            self.at += 1;
            return Ok(SequenceMatchItem::Assign {
                target: expression,
                operator,
                value: Some(self.expression(0)?),
            });
        }
        // As a match item, a prefix and a postfix update have one effect.
        if let ExprKind::Unary {
            operator,
            expression: target,
            ..
        } = &self.expressions[expression.0].kind
            && matches!(operator.as_str(), "++" | "--")
        {
            return Ok(SequenceMatchItem::Assign {
                target: *target,
                operator: operator.clone(),
                value: None,
            });
        }
        Ok(SequenceMatchItem::Call(expression))
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
        // `sequence name ;`, local variables of builtin types, then the body.
        let mut offset = 3;
        while super::expressions::builtin(self.nth(offset)) {
            while !matches!(self.nth(offset), ";" | "") {
                offset += 1;
            }
            offset += 1;
        }
        let mut offset = self.past_clocking_event(offset);
        let mut depth = 0usize;
        loop {
            match self.nth(offset) {
                "" | "endproperty" | "endsequence" if self.nth(offset) != end => return Ok(None),
                word if word == end => break,
                "(" => {
                    depth += 1;
                    offset += 1;
                }
                ")" => {
                    depth = depth.saturating_sub(1);
                    offset += 1;
                }
                // A property's top-level `iff` between Booleans (16.12.8).
                "iff" if depth == 0 && end == "endproperty" => offset += 1,
                word if self.temporal(word, offset) => return Ok(None),
                _ => offset += 1,
            }
        }
        let lengths = (
            self.expressions.len(),
            self.types.len(),
            self.timing_controls.len(),
            self.sequences.len(),
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
                self.sequences.truncate(lengths.3);
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
        let mut locals = Vec::new();
        // A local declaration, not a leading cast such as `int'(a)`.
        while super::expressions::builtin(self.peek()) && !self.nth(1).starts_with('\'') {
            let data_type = self.data_type()?;
            let mut names = vec![self.name()?];
            while self.eat(",") {
                names.push(self.name()?);
            }
            self.expect(";")?;
            locals.push(SequenceLocal { data_type, names });
        }
        let body = self.property_spec(kind == AssertionDeclarationKind::Sequence, locals)?;
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
    /// its condition, or as an operand of its sequence.
    fn instances(
        &self,
        spec: PropertySpec,
        names: &HashMap<String, AssertionDeclarationId>,
    ) -> Vec<(ExprId, AssertionDeclarationId)> {
        let operands = match spec.sequence {
            None => [spec.antecedent, Some(spec.condition)]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>(),
            Some(sequence) => {
                let sequence = &self.sequences[sequence.0];
                sequence
                    .antecedent
                    .iter()
                    .chain([&sequence.consequent])
                    .flat_map(SequenceExpr::booleans)
                    .map(|boolean| boolean.expression)
                    .collect()
            }
        };
        operands
            .into_iter()
            .filter_map(|expr| self.instance(expr, names).map(|id| (expr, id)))
            .collect()
    }

    /// `tree` with each operand that instantiates a named sequence replaced
    /// by the sequence (16.8): the clock of one, unless the sequence is
    /// clocked alike, becomes `clock` where it leads the tree (16.16.1).
    /// Whether it replaced any.
    fn substitute_sequence_instances(
        &mut self,
        tree: &mut SequenceExpr,
        leading: bool,
        names: &HashMap<String, AssertionDeclarationId>,
        resolved: &HashMap<AssertionDeclarationId, PropertySpec>,
        clock: &mut Option<TimingControlId>,
    ) -> Result<bool> {
        let (boolean, condition_leading) = match tree {
            SequenceExpr::Boolean(boolean) => (boolean.clone(), leading),
            SequenceExpr::Concat { left, right, .. } => {
                let mut changed = false;
                if let Some(left) = left {
                    changed |=
                        self.substitute_sequence_instances(left, leading, names, resolved, clock)?;
                }
                let right_leading = leading && left.is_none();
                changed |= self.substitute_sequence_instances(
                    right,
                    right_leading,
                    names,
                    resolved,
                    clock,
                )?;
                return Ok(changed);
            }
            SequenceExpr::Or(left, right)
            | SequenceExpr::And(left, right)
            | SequenceExpr::Intersect(left, right) => {
                let left =
                    self.substitute_sequence_instances(left, leading, names, resolved, clock)?;
                let right =
                    self.substitute_sequence_instances(right, leading, names, resolved, clock)?;
                return Ok(left || right);
            }
            SequenceExpr::Throughout {
                condition,
                sequence,
            } => {
                if self.instance(condition.expression, names).is_some() {
                    return Err(self.unsupported_at(
                        condition.tokens,
                        "instance of a named sequence as the left operand of throughout",
                    ));
                }
                return self
                    .substitute_sequence_instances(sequence, leading, names, resolved, clock);
            }
            // The instances of a clocked subsequence are clocked as it is.
            SequenceExpr::Clocked {
                clock: inner,
                sequence,
            } => {
                let mut inner = Some(*inner);
                return self
                    .substitute_sequence_instances(sequence, true, names, resolved, &mut inner);
            }
        };
        let Some(id) = self.instance(boolean.expression, names) else {
            return Ok(false);
        };
        let declaration = &self.assertion_declarations[id.0];
        let name = declaration.name.value().to_owned();
        if declaration.kind == AssertionDeclarationKind::Property {
            return Err(self.error_at(
                boolean.tokens,
                format!("property `{name}` is an operand of a sequence (IEEE 1800-2023 16.12)"),
            ));
        }
        if !boolean.items.is_empty() {
            return Err(self.unsupported_at(
                boolean.tokens,
                "match items of an instance of a named sequence",
            ));
        }
        let inner = resolved[&id];
        if let Some(inner_clock) = inner.clock {
            match *clock {
                Some(outer) if self.same_clock(outer, inner_clock) => {}
                None if condition_leading => *clock = Some(inner_clock),
                _ => {
                    return Err(self.unsupported_at(
                        boolean.tokens,
                        format!(
                            "multiclocked sequence: instance `{name}` has its own clocking event"
                        ),
                    ));
                }
            }
        }
        *tree = match inner.sequence {
            Some(sequence) => {
                let body = self.sequences[sequence.0].clone();
                if !body.locals.is_empty() {
                    return Err(self.unsupported_at(
                        boolean.tokens,
                        format!("instance of named sequence `{name}` with local variables inside a sequence"),
                    ));
                }
                body.consequent
            }
            None => SequenceExpr::Boolean(SequenceBoolean {
                expression: inner.condition,
                items: Vec::new(),
                tokens: boolean.tokens,
            }),
        };
        Ok(true)
    }

    /// Substitute the body of each declaration that `spec` instantiates,
    /// directly or through other declarations, into `resolved`. Each
    /// declaration resolves once, after those it instantiates, on an explicit
    /// stack, so a long chain of declarations does not grow the call stack.
    fn resolve_declarations(
        &mut self,
        spec: PropertySpec,
        names: &HashMap<String, AssertionDeclarationId>,
        resolved: &mut HashMap<AssertionDeclarationId, PropertySpec>,
    ) -> Result<()> {
        let mut pending: Vec<_> = self
            .instances(spec, names)
            .into_iter()
            .map(|(_, id)| id)
            .collect();
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
        &mut self,
        spec: PropertySpec,
        names: &HashMap<String, AssertionDeclarationId>,
        resolved: &HashMap<AssertionDeclarationId, PropertySpec>,
    ) -> Result<PropertySpec> {
        if let Some(sequence) = spec.sequence {
            let mut body = self.sequences[sequence.0].clone();
            let mut clock = spec.clock;
            let mut changed = false;
            if let Some(antecedent) = &mut body.antecedent {
                changed |= self
                    .substitute_sequence_instances(antecedent, true, names, resolved, &mut clock)?;
            }
            let leading = body.antecedent.is_none();
            changed |= self.substitute_sequence_instances(
                &mut body.consequent,
                leading,
                names,
                resolved,
                &mut clock,
            )?;
            if !changed {
                return Ok(spec);
            }
            let mut result = spec;
            result.condition = body
                .consequent
                .booleans()
                .last()
                .expect("a sequence has a Boolean expression")
                .expression;
            result.clock = clock;
            result.sequence = Some(SequenceId(self.sequences.len()));
            self.sequences.push(body);
            return Ok(result);
        }
        let mut result = spec;
        if let Some(equivalent) = spec.equivalent
            && let Some(operand) = [spec.condition, equivalent]
                .into_iter()
                .find(|&operand| self.instance(operand, names).is_some())
        {
            return Err(self.unsupported_at(
                self.expressions[operand.0].tokens,
                "instance of a named sequence or property as an operand of iff",
            ));
        }
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
            if let Some(sequence) = inner.sequence {
                // The implication of the sequence's steps and the consequent,
                // which starts at the tick of the antecedent's match
                // (16.12.7): a sequence of one tick with match items, or one
                // over cycles.
                let body = self.sequences[sequence.0].clone();
                if body.antecedent.is_some() {
                    return Err(self.unsupported_at(
                        self.expressions[antecedent.0].tokens,
                        "implication as the antecedent of an implication",
                    ));
                }
                if self.instance(spec.condition, names).is_some() {
                    return Err(self.unsupported_at(
                        self.expressions[spec.condition.0].tokens,
                        "instance of a named sequence or property as the consequent of a named sequence",
                    ));
                }
                let id = SequenceId(self.sequences.len());
                self.sequences.push(Sequence {
                    antecedent: Some(body.consequent),
                    consequent: SequenceExpr::Boolean(SequenceBoolean {
                        expression: spec.condition,
                        items: Vec::new(),
                        tokens: self.expressions[spec.condition.0].tokens,
                    }),
                    locals: body.locals,
                });
                result.antecedent = None;
                result.sequence = Some(id);
                result.clock = inner.clock.or(result.clock);
                return Ok(result);
            }
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
            result.equivalent = inner.equivalent;
            result.sequence = inner.sequence;
            // An explicit leading clock of the instance overrides the
            // incoming one (16.16.1).
            result.clock = inner.clock.or(spec.clock);
            return Ok(result);
        }
        if inner.antecedent.is_some() || inner.sequence.is_some() {
            return Err(self.unsupported_at(
                tokens,
                format!("implication or sequence over cycles `{name}` as the consequent of an implication"),
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
        result.equivalent = inner.equivalent;
        Ok(result)
    }

    /// The clocking event a module assertion over `sequence` waits for: its
    /// leading one, or with subsequences clocked differently (16.13), one
    /// of the events of each of its clocks. A subsequence clocked as the
    /// sequence around it is that sequence (16.13.1).
    fn multiclock_control(
        &mut self,
        assertion: StatementId,
        sequence: SequenceId,
        leading: TimingControlId,
    ) -> TimingControlId {
        let mut body = self.sequences[sequence.0].clone();
        let mut clocks = Vec::new();
        let mut changed = false;
        if let Some(antecedent) = &mut body.antecedent {
            changed |= self.same_clocks(antecedent, leading, &mut clocks);
        }
        changed |= self.same_clocks(&mut body.consequent, leading, &mut clocks);
        if changed {
            let id = SequenceId(self.sequences.len());
            self.sequences.push(body);
            if let StatementKind::Assertion {
                header: AssertionHeader::Concurrent { property, .. },
                ..
            } = &mut self.statements[assertion.0].kind
            {
                property.sequence = Some(id);
            }
        }
        if clocks.is_empty() {
            return leading;
        }
        // Any change of each clock expression wakes the monitor, so that it
        // sees every value its edge detection compares against; the edges and
        // `iff` conditions are applied by the monitor. A clock of another
        // form is left to the analyzer to reject.
        let mut items = Vec::new();
        for clock in std::iter::once(leading).chain(clocks) {
            match &self.timing_controls[clock.0].kind {
                TimingControlKind::Event(EventControl::Items(each)) => {
                    items.extend(each.iter().map(|item| EventItem {
                        edge: TimingEdge::Any,
                        iff: None,
                        ..item.clone()
                    }))
                }
                _ => return leading,
            }
        }
        let id = TimingControlId(self.timing_controls.len());
        let tokens = self.timing_controls[leading.0].tokens;
        self.timing_controls.push(TimingControl {
            kind: TimingControlKind::Event(EventControl::Items(items)),
            tokens,
        });
        id
    }

    /// `tree` with each subsequence clocked as `clock`, the clock around it,
    /// replaced by its sequence; the other clocks into `clocks`, each once.
    /// Whether it replaced any.
    fn same_clocks(
        &self,
        tree: &mut SequenceExpr,
        clock: TimingControlId,
        clocks: &mut Vec<TimingControlId>,
    ) -> bool {
        match tree {
            SequenceExpr::Boolean(_) => false,
            SequenceExpr::Concat { left, right, .. } => {
                let left = left
                    .as_deref_mut()
                    .is_some_and(|left| self.same_clocks(left, clock, clocks));
                self.same_clocks(right, clock, clocks) || left
            }
            SequenceExpr::Or(left, right)
            | SequenceExpr::And(left, right)
            | SequenceExpr::Intersect(left, right) => {
                let left = self.same_clocks(left, clock, clocks);
                self.same_clocks(right, clock, clocks) || left
            }
            SequenceExpr::Throughout { sequence, .. } => self.same_clocks(sequence, clock, clocks),
            SequenceExpr::Clocked {
                clock: inner,
                sequence,
            } => {
                let inner = *inner;
                if self.same_clock(inner, clock) {
                    let mut sequence = std::mem::replace(
                        sequence.as_mut(),
                        SequenceExpr::Boolean(SequenceBoolean {
                            expression: ExprId(0),
                            items: Vec::new(),
                            tokens: TokenRange::default(),
                        }),
                    );
                    self.same_clocks(&mut sequence, clock, clocks);
                    *tree = sequence;
                    return true;
                }
                if !clocks.iter().any(|&known| self.same_clock(known, inner)) {
                    clocks.push(inner);
                }
                self.same_clocks(sequence, inner, clocks)
            }
        }
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
                            clock: Some(leading),
                            sequence,
                            ..
                        },
                    ..
                },
            ..
        } = self.statements[assertion.0].kind
        else {
            return;
        };
        let control = match sequence {
            Some(sequence) => self.multiclock_control(assertion, sequence, leading),
            None => leading,
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
