use super::*;

#[derive(Clone)]
struct Header {
    start: usize,
    label: Option<Name>,
    attributes: Option<TokenRange>,
    qualifier: Option<String>,
}

struct CaseState {
    header: Header,
    keyword: String,
    mode: Option<String>,
    expression: Option<ExprId>,
    arms: Vec<CaseArm>,
}
struct ActionState {
    header: Header,
    keyword: String,
    tokens: TokenRange,
    assertion: Option<AssertionHeader>,
    wait_order: bool,
    allow_else: bool,
}

enum Job {
    Parse,
    BlockNext {
        header: Header,
        label: Option<Name>,
        parallel: bool,
        braced: bool,
        items: Vec<StatementId>,
    },
    BlockItem {
        header: Header,
        label: Option<Name>,
        parallel: bool,
        braced: bool,
        items: Vec<StatementId>,
    },
    IfThen {
        header: Header,
        branches: Vec<ConditionalBranch>,
        predicate: Predicate,
    },
    IfNext {
        header: Header,
        branches: Vec<ConditionalBranch>,
    },
    IfElse {
        header: Header,
        branches: Vec<ConditionalBranch>,
    },
    CaseNext(CaseState),
    CaseArm {
        state: CaseState,
        start: usize,
        matches: Vec<CaseMatch>,
        default: bool,
    },
    Loop {
        header: Header,
        kind: LoopSyntax,
    },
    DoWhile(Header),
    Timing {
        header: Header,
        control: TimingControlId,
    },
    Wait {
        header: Header,
        condition: ExprId,
    },
    ActionNext(ActionState),
    ActionSuccess(ActionState),
    ActionFailure {
        state: ActionState,
        success: Option<StatementId>,
    },
    RandsequenceNext {
        header: Header,
        items: Vec<StatementId>,
    },
    RandsequenceBlock {
        header: Header,
        items: Vec<StatementId>,
    },
}

impl Parser<'_> {
    fn subroutine_call_expression(&self, id: ExprId) -> bool {
        let callable = |id: ExprId| match &self.expressions[id.0].kind {
            ExprKind::Name(name) => !self.reserved_word(&name.spelling),
            ExprKind::Member { .. } => true,
            _ => false,
        };
        match &self.expressions[id.0].kind {
            ExprKind::Call { expression, .. } => callable(*expression),
            _ => callable(id),
        }
    }

    fn call_statement_expression(&self, id: ExprId) -> bool {
        match &self.expressions[id.0].kind {
            ExprKind::Unary { operator, .. } if matches!(operator.as_str(), "++" | "--") => true,
            ExprKind::Cast { target, expression } => {
                matches!(&self.expressions[target.0].kind, ExprKind::Type(id) if matches!(&self.types[id.0].kind, DataTypeKind::Builtin(name) if name == "void"))
                    && self.subroutine_call_expression(*expression)
            }
            _ => self.subroutine_call_expression(id),
        }
    }

    pub(super) fn procedure(&mut self) -> Result<ProcedureId> {
        let start = self.at;
        let kind = match self.peek() {
            "initial" => ProcedureKind::Initial,
            "always" => ProcedureKind::Always,
            "always_comb" => ProcedureKind::AlwaysComb,
            "always_latch" => ProcedureKind::AlwaysLatch,
            "always_ff" => ProcedureKind::AlwaysFf,
            "final" => ProcedureKind::Final,
            _ => return Err(self.error("expected procedural body")),
        };
        self.at += 1;
        let body = self.statement_machine()?;
        let id = ProcedureId(self.procedures.len());
        let authored_statements = self.body_membership(&[body]);
        self.procedures.push(Procedure {
            kind,
            body,
            authored_statements,
            tokens: self.range(start),
        });
        Ok(id)
    }

    /// Capture syntax ownership before execution graph transformations. This
    /// flat membership also keeps deeply nested scopes independent of Rust's
    /// call stack; opaque statement children remain their own captured scope.
    pub(super) fn body_membership(&self, roots: &[StatementId]) -> Vec<StatementId> {
        let mut pending = roots.to_vec();
        let mut result = Vec::new();
        while let Some(id) = pending.pop() {
            result.push(id);
            let kind = &self.statements[id.0].kind;
            if !matches!(kind, StatementKind::Unconverted { .. }) {
                pending.extend(kind.children());
            }
        }
        result
    }

    fn statement_node(&mut self, header: Header, kind: StatementKind) -> Result<StatementId> {
        if self.statements.len() >= 1_000_000 {
            return Err(self.unsupported("native statement arena exceeds one million nodes"));
        }
        let id = StatementId(self.statements.len());
        self.statements.push(Statement {
            label: header.label,
            attributes: header.attributes,
            qualifier: header.qualifier,
            kind,
            tokens: self.range(header.start),
        });
        Ok(id)
    }

    fn statement_header(&mut self) -> Result<Header> {
        while self.directive()? {}
        let start = self.at;
        let label = if self.nth(1) == ":" && !matches!(self.peek(), "begin" | "fork") {
            let name = self.name()?;
            self.expect(":")?;
            Some(name)
        } else {
            None
        };
        let attr_start = self.at;
        self.attributes()?;
        let attributes = (self.at > attr_start).then(|| self.range(attr_start));
        let qualifier = if matches!(self.peek(), "unique" | "unique0" | "priority") {
            let text = self.peek().to_owned();
            self.at += 1;
            Some(text)
        } else {
            None
        };
        Ok(Header {
            start,
            label,
            attributes,
            qualifier,
        })
    }

    fn parentheses(&mut self) -> Result<TokenRange> {
        let start = self.at;
        self.expect("(")?;
        let mut stack = vec![")"];
        while let Some(end) = stack.last().copied() {
            if self.directive()? {
                continue;
            }
            match self.peek() {
                "" => return Err(self.error("unterminated parenthesized syntax")),
                "(" => stack.push(")"),
                "[" => stack.push("]"),
                "{" => stack.push("}"),
                word if word == end => {
                    stack.pop();
                }
                ")" | "]" | "}" => return Err(self.error("mismatched delimiter")),
                _ => {}
            }
            self.at += 1;
        }
        Ok(self.range(start))
    }

    fn semicolon_range(&mut self) -> Result<TokenRange> {
        let start = self.at;
        let mut stack = Vec::new();
        loop {
            if self.directive()? {
                continue;
            }
            match self.peek() {
                "" => return Err(self.error("unterminated statement syntax")),
                ";" if stack.is_empty() => {
                    self.at += 1;
                    return Ok(self.range(start));
                }
                "endmodule" | "endinterface" | "endprogram" | "endfunction" | "endtask"
                | "endcase" | "end"
                    if stack.is_empty() =>
                {
                    return Err(self.error("expected `;` before end of statement scope"));
                }
                "(" => stack.push(")"),
                "[" => stack.push("]"),
                "{" => stack.push("}"),
                word if stack.last().copied() == Some(word) => {
                    stack.pop();
                }
                ")" | "]" | "}" => return Err(self.error("mismatched statement delimiter")),
                _ => {}
            }
            self.at += 1;
        }
    }

    fn pattern_range(&mut self, end: &str) -> Result<TokenRange> {
        let start = self.at;
        let mut stack = Vec::new();
        let mut conditional = 0;
        loop {
            if self.directive()? {
                continue;
            }
            let word = self.peek();
            if stack.is_empty() && conditional == 0 && (word == end || word == "&&&") {
                break;
            }
            match word {
                "" => return Err(self.error("unterminated pattern")),
                "(" => stack.push(")"),
                "[" => stack.push("]"),
                "{" => stack.push("}"),
                word if stack.last().copied() == Some(word) => {
                    stack.pop();
                }
                "?" if stack.is_empty() => conditional += 1,
                ":" if stack.is_empty() && conditional > 0 => conditional -= 1,
                ")" | "]" | "}" => return Err(self.error("mismatched pattern delimiter")),
                _ => {}
            }
            self.at += 1;
        }
        if self.at == start {
            return Err(self.error("expected pattern"));
        }
        Ok(self.range(start))
    }

    fn predicate(&mut self) -> Result<Predicate> {
        let start = self.at;
        self.expect("(")?;
        let mut items = Vec::new();
        loop {
            let expression = self.expression(0)?;
            let pattern = if self.eat("matches") {
                Some(self.pattern_range(")")?)
            } else {
                None
            };
            items.push(PredicateItem {
                expression,
                pattern,
            });
            if self.eat(")") {
                break;
            }
            self.expect("&&&")?;
        }
        Ok(Predicate {
            items,
            tokens: self.range(start),
        })
    }

    fn parenthesized_expression(&mut self) -> Result<ExprId> {
        self.expect("(")?;
        let expression = self.expression(0)?;
        self.expect(")")?;
        Ok(expression)
    }

    fn for_header(&mut self) -> Result<ForHeader> {
        let start = self.at;
        self.expect("(")?;
        let initialization = if self.peek() == ";" {
            None
        } else if self.declaration_start() {
            let mut declarations = Vec::new();
            loop {
                let start = self.at;
                let var = self.eat("var");
                let data_type = self.data_type()?;
                let mut variables = Vec::new();
                loop {
                    let start = self.at;
                    let name = self.name()?;
                    self.expect("=")?;
                    let value = self.expression(0)?;
                    variables.push(ForVariable {
                        name,
                        value,
                        tokens: self.range(start),
                    });
                    if !self.eat(",") {
                        break;
                    }
                    if self.declaration_start() {
                        break;
                    }
                }
                declarations.push(ForVariableDeclaration {
                    var,
                    data_type,
                    variables,
                    tokens: self.range(start),
                });
                if self.peek() == ";" {
                    break;
                }
            }
            Some(ForInitialization::Declarations(declarations))
        } else {
            let mut assignments = Vec::new();
            loop {
                let start = self.at;
                let left = self.expression(14)?;
                if !self.variable_lvalue(left) {
                    return Err(self.error("expected for-initializer lvalue"));
                }
                self.expect("=")?;
                let right = self.expression(0)?;
                assignments.push(AssignmentSyntax {
                    left,
                    right,
                    tokens: self.range(start),
                });
                if !self.eat(",") {
                    break;
                }
            }
            Some(ForInitialization::Assignments(assignments))
        };
        self.expect(";")?;
        let condition = if self.peek() == ";" {
            None
        } else {
            Some(self.expression(0)?)
        };
        self.expect(";")?;
        let mut steps = Vec::new();
        if self.peek() != ")" {
            loop {
                let start = self.at;
                let left = self.expression(14)?;
                if matches!(
                    self.peek(),
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
                    if !self.variable_lvalue(left) {
                        return Err(self.error("expected for-step lvalue"));
                    }
                    let operator = self.peek().to_owned();
                    self.at += 1;
                    let right = self.expression(0)?;
                    steps.push(ForStep::Assignment {
                        left,
                        operator,
                        right,
                        tokens: self.range(start),
                    });
                } else {
                    steps.push(ForStep::Expression(left));
                }
                if !self.eat(",") {
                    break;
                }
            }
        }
        self.expect(")")?;
        Ok(ForHeader {
            initialization,
            condition,
            steps,
            tokens: self.range(start),
        })
    }

    pub(super) fn typed_timing_control(&mut self) -> Result<TimingControlId> {
        let start = self.at;
        let kind = match self.peek() {
            "#" => {
                self.at += 1;
                let parenthesized = self.peek() == "(";
                let value = self.expression(14)?;
                TimingControlKind::Delay {
                    value,
                    parenthesized,
                }
            }
            "@" | "@*" => {
                let star = self.eat("@*");
                if !star {
                    self.expect("@")?;
                }
                let event = if star || self.eat("*") {
                    EventControl::Wildcard
                } else if (self.peek() == "(*" && self.nth(1) == ")")
                    || (self.peek() == "(" && self.nth(1) == "*)")
                {
                    self.at += 2;
                    EventControl::Wildcard
                } else if self.peek() == "(" && self.nth(1) == "*" && self.nth(2) == ")" {
                    self.at += 3;
                    EventControl::Wildcard
                } else if self.eat("(") {
                    EventControl::Items(self.event_items()?)
                } else {
                    EventControl::Named(self.expression(14)?)
                };
                TimingControlKind::Event(event)
            }
            _ => return Err(self.error("expected delay or event control")),
        };
        if self.timing_controls.len() >= 1_000_000 {
            return Err(self.unsupported("native timing control arena exceeds one million nodes"));
        }
        let id = TimingControlId(self.timing_controls.len());
        self.timing_controls.push(TimingControl {
            kind,
            tokens: self.range(start),
        });
        Ok(id)
    }

    pub(super) fn foreach_header(&mut self) -> Result<ForeachHeader> {
        let start = self.at;
        self.expect("(")?;
        let array_start = self.at;
        let name = if matches!(self.peek(), "$root" | "$unit") {
            let token = self.token().unwrap();
            let name = Name {
                spelling: token.text.clone(),
                origin: token.origin,
            };
            self.at += 1;
            name
        } else {
            self.contextual_name(&["this", "super"])?
        };
        let mut array = self.node(array_start, ExprKind::Name(name))?;
        let mut indexed_receiver = false;
        loop {
            match self.peek() {
                "." | "::" => {
                    let separator = self.peek().to_owned();
                    self.at += 1;
                    let name = if separator == "."
                        && matches!(&self.expressions[array.0].kind, ExprKind::Name(name) if name.spelling == "this")
                    {
                        self.contextual_name(&["super"])?
                    } else {
                        self.name()?
                    };
                    array = self.node(
                        array_start,
                        ExprKind::Member {
                            expression: array,
                            separator,
                            name,
                        },
                    )?;
                    indexed_receiver = false;
                }
                "#" => {
                    self.at += 1;
                    let arguments = self
                        .expression_machine(super::expressions::Job::Arguments)?
                        .arguments();
                    array = self.node(
                        array_start,
                        ExprKind::Parameterized {
                            expression: array,
                            arguments,
                        },
                    )?;
                }
                "[" => {
                    let dimension_start = self.at;
                    self.at += 1;
                    let mut slots = Vec::new();
                    loop {
                        slots.push(if matches!(self.peek(), "," | "]") {
                            None
                        } else {
                            Some(self.expression(0)?)
                        });
                        if !self.eat(",") {
                            break;
                        }
                    }
                    self.expect("]")?;
                    if matches!(self.peek(), "." | "[") {
                        let [Some(index)] = slots.as_slice() else {
                            return Err(self.error("expected one hierarchical array index"));
                        };
                        array = self.node(
                            array_start,
                            ExprKind::Select {
                                expression: array,
                                dimension: Box::new(Dimension {
                                    kind: DimensionKind::Index(*index),
                                    tokens: self.range(dimension_start),
                                }),
                            },
                        )?;
                        indexed_receiver = true;
                    } else {
                        if indexed_receiver {
                            return Err(
                                self.error("expected member after hierarchical array indices")
                            );
                        }
                        let variables = slots
                            .into_iter()
                            .map(|slot| match slot {
                                Some(id) => match &self.expressions[id.0].kind {
                                    ExprKind::Name(name)
                                        if !name.spelling.starts_with('$')
                                            && !self.reserved_word(&name.spelling) =>
                                    {
                                        Ok(Some(name.clone()))
                                    }
                                    _ => Err(self.error("expected foreach index variable")),
                                },
                                None => Ok(None),
                            })
                            .collect::<Result<Vec<_>>>()?;
                        if matches!(&self.expressions[array.0].kind, ExprKind::Name(name) if self.reserved_word(&name.spelling))
                        {
                            return Err(self.error("foreach class handle requires a member array"));
                        }
                        self.expect(")")?;
                        return Ok(ForeachHeader {
                            array,
                            variables,
                            tokens: self.range(start),
                        });
                    }
                }
                _ => return Err(self.error("expected foreach array index slots")),
            }
        }
    }

    /// Groups use explicit frames and one output vector. Single ordinary
    /// expression groups resume Pratt parsing; event unions never concatenate
    /// nested output vectors or rescan their remaining tokens.
    fn event_items(&mut self) -> Result<Vec<EventItem>> {
        let mut frames = vec![(self.at - 1, 0usize, false)];
        let mut items: Vec<EventItem> = Vec::new();
        let mut parse = true;
        loop {
            if frames.len() > 1_000_000 {
                return Err(
                    self.unsupported("native event continuation stack exceeds one million entries")
                );
            }
            if parse {
                let start = self.at;
                if self.eat("(") {
                    frames.push((start, items.len(), true));
                    continue;
                }
                let edge = match self.peek() {
                    "posedge" => TimingEdge::Pos,
                    "negedge" => TimingEdge::Neg,
                    "edge" => TimingEdge::Both,
                    _ => TimingEdge::Any,
                };
                if edge != TimingEdge::Any {
                    self.at += 1;
                }
                let expression = self.expression(0)?;
                let iff = if self.eat("iff") {
                    Some(self.expression(0)?)
                } else {
                    None
                };
                items.push(EventItem {
                    edge,
                    expression,
                    iff,
                    tokens: self.range(start),
                });
                parse = false;
            }
            if self.eat("or") || self.eat(",") {
                parse = true;
                continue;
            }
            self.expect(")")?;
            let (start, before, group) = frames.pop().unwrap();
            if group
                && items.len() == before + 1
                && items[before].edge == TimingEdge::Any
                && items[before].iff.is_none()
            {
                let item = items.pop().unwrap();
                let expression = self.event_group_expression(start, item.expression)?;
                let iff = if self.eat("iff") {
                    Some(self.expression(0)?)
                } else {
                    None
                };
                items.push(EventItem {
                    edge: TimingEdge::Any,
                    expression,
                    iff,
                    tokens: self.range(start),
                });
            }
            if frames.is_empty() {
                return Ok(items);
            }
        }
    }

    fn timing_range(&mut self) -> Result<(TokenRange, Option<TimingControlId>)> {
        let start = self.at;
        let event = match self.peek() {
            "repeat" => {
                self.at += 1;
                self.parentheses()?;
                if !matches!(self.peek(), "@" | "@*") {
                    return Err(self.error("expected event control after repeat count"));
                }
                Some(self.typed_timing_control()?)
            }
            "@" | "@*" => Some(self.typed_timing_control()?),
            _ => {
                self.timing_atom()?;
                None
            }
        };
        Ok((self.range(start), event))
    }

    fn timing_atom(&mut self) -> Result<()> {
        let operator = self.peek().to_owned();
        if !matches!(operator.as_str(), "#" | "@" | "@*" | "##") {
            return Err(self.error("expected timing control"));
        }
        self.at += 1;
        if operator == "@*" || (operator == "@" && self.eat("*")) {
            return Ok(());
        }
        // The lexer retains attribute delimiters as longest-match tokens. In
        // an event control the same spelling denotes the wildcard event.
        if operator == "@"
            && ((self.peek() == "(*" && self.nth(1) == ")")
                || (self.peek() == "(" && self.nth(1) == "*)"))
        {
            self.at += 2;
            return Ok(());
        }
        if self.peek() == "(" {
            self.parentheses()?;
        } else {
            self.expression(14)?;
        }
        Ok(())
    }

    pub(super) fn declaration_start(&self) -> bool {
        if (super::expressions::builtin(self.peek()) && self.nth(1) != "'")
            || matches!(
                self.peek(),
                "const"
                    | "static"
                    | "automatic"
                    | "var"
                    | "signed"
                    | "unsigned"
                    | "struct"
                    | "union"
                    | "enum"
                    | "virtual"
                    | "interface"
                    | "event"
                    | "let"
                    | "nettype"
            )
            || (self.peek() == "type" && self.nth(1) == "(")
        {
            return true;
        }
        self.declaration_name_cursor().is_some()
    }

    pub(super) fn declaration_name_cursor(&self) -> Option<usize> {
        if !self.token().is_some_and(|token| {
            matches!(
                token.kind,
                TokenKind::Identifier | TokenKind::EscapedIdentifier
            )
        }) {
            return None;
        }
        let mut cursor = self.at + 1;
        let word = |at: usize| {
            self.indices
                .get(at)
                .map(|index| self.tokens[*index].text.as_str())
                .unwrap_or("")
        };
        loop {
            match word(cursor) {
                "::" | "." => {
                    cursor += 2;
                }
                "#" if word(cursor + 1) == "(" => {
                    cursor += 1;
                    let mut depth = 0;
                    loop {
                        match word(cursor) {
                            "" => return None,
                            "(" => depth += 1,
                            ")" => depth -= 1,
                            _ => {}
                        }
                        cursor += 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
                "[" => {
                    let mut depth = 0;
                    loop {
                        match word(cursor) {
                            "" => return None,
                            "[" => depth += 1,
                            "]" => depth -= 1,
                            _ => {}
                        }
                        cursor += 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        self.indices
            .get(cursor)
            .filter(|index| {
                matches!(
                    self.tokens[**index].kind,
                    TokenKind::Identifier | TokenKind::EscapedIdentifier
                )
            })
            .map(|_| cursor)
    }

    fn constructed_declaration(&self) -> bool {
        let mut stack = Vec::new();
        let mut previous = "";
        for index in &self.indices[self.at..] {
            let word = self.tokens[*index].text.as_str();
            if word == ";" && stack.is_empty() {
                break;
            }
            if word == "new" || (previous == "[" && word == "]") {
                return true;
            }
            match word {
                "(" => stack.push(")"),
                "[" => stack.push("]"),
                "{" => stack.push("}"),
                word if stack.last().copied() == Some(word) => {
                    stack.pop();
                }
                _ => {}
            }
            previous = word;
        }
        false
    }

    fn constructed_initializer(&mut self) -> Result<ExprId> {
        if self.peek() != "new" {
            return self.expression(0);
        }
        let start = self.at;
        let token = self.token().unwrap();
        let name = Name {
            spelling: token.text.clone(),
            origin: token.origin,
        };
        self.at += 1;
        if self.eat("[") {
            self.expression(0)?;
            self.expect("]")?;
            if self.eat("(") {
                self.expression(0)?;
                self.expect(")")?;
            }
        } else if self.peek() == "(" {
            // A class constructor call keeps its arguments (8.7).
            let callee = self.node(start, ExprKind::Name(name))?;
            let super::expressions::Value::Arguments(arguments) =
                self.expression_machine(super::expressions::Job::Arguments)?
            else {
                return Err(self.error("constructor arguments"));
            };
            return self.node(
                start,
                ExprKind::Call {
                    expression: callee,
                    arguments,
                },
            );
        } else if !matches!(self.peek(), ";" | "," | "") {
            self.expression(0)?;
        }
        // The declaration retains its explicit Unconverted semantic owner.
        // This range validates allocation grammar without introducing execution.
        self.node(start, ExprKind::Name(name))
    }

    pub(super) fn local_declaration(&mut self) -> Result<LocalDeclaration> {
        if matches!(self.peek(), "let" | "nettype") {
            return Ok(LocalDeclaration::Unconverted(self.semicolon_range()?));
        }
        let constructed = self.constructed_declaration();
        let start = self.at;
        let constant = self.eat("const");
        let var = self.eat("var");
        let lifetime = if matches!(self.peek(), "automatic" | "static") {
            let value = self.peek().to_owned();
            self.at += 1;
            Some(value)
        } else {
            None
        };
        let data_type = if self.starts_type() || !var {
            self.data_type()?
        } else {
            self.implicit_type()?
        };
        if !var && matches!(self.types[data_type.0].kind, DataTypeKind::Implicit) {
            return Err(self.error("implicit variable datatype requires `var`"));
        }
        let mut variables = Vec::new();
        loop {
            let start = self.at;
            let name = self.name()?;
            let dimensions = self.dimensions()?;
            let value = if self.eat("=") {
                Some(if constructed {
                    self.constructed_initializer()?
                } else {
                    self.expression(0)?
                })
            } else {
                None
            };
            variables.push(VariableDeclarator {
                name,
                dimensions,
                value,
                tokens: self.range(start),
            });
            if !self.eat(",") {
                break;
            }
        }
        self.expect(";")?;
        let declaration = VariableDeclaration {
            constant,
            var,
            lifetime,
            data_type,
            variables,
            tokens: self.range(start),
        };
        Ok(if constructed {
            LocalDeclaration::Constructed(declaration)
        } else {
            LocalDeclaration::Variables(declaration)
        })
    }

    fn action_node(
        &mut self,
        state: ActionState,
        success: Option<StatementId>,
        failure: Option<StatementId>,
    ) -> Result<StatementId> {
        let kind = if state.wait_order {
            StatementKind::WaitOrder {
                header: state.tokens,
                success,
                failure,
            }
        } else {
            StatementKind::Assertion {
                keyword: state.keyword,
                header: state.assertion.expect("assertion header continuation"),
                success,
                failure,
            }
        };
        self.statement_node(state.header, kind)
    }

    pub(super) fn statement_machine(&mut self) -> Result<StatementId> {
        let mut jobs = vec![Job::Parse];
        let mut values = Vec::new();
        while let Some(job) = jobs.pop() {
            if jobs.len() > 1_000_000 {
                return Err(
                    self.unsupported("native statement continuations exceed one million entries")
                );
            }
            match job {
                Job::Parse => {
                    let header = self.statement_header()?;
                    let word = self.peek().to_owned();
                    match word.as_str() {
                        ";" => {
                            self.at += 1;
                            values.push(self.statement_node(header, StatementKind::Empty)?);
                        }
                        "begin" | "fork" => {
                            self.at += 1;
                            let label = if self.eat(":") {
                                Some(self.name()?)
                            } else {
                                None
                            };
                            jobs.push(Job::BlockNext {
                                header,
                                label,
                                parallel: word == "fork",
                                braced: false,
                                items: Vec::new(),
                            });
                        }
                        "if" => {
                            self.at += 1;
                            let predicate = self.predicate()?;
                            jobs.push(Job::IfThen {
                                header,
                                branches: Vec::new(),
                                predicate,
                            });
                            jobs.push(Job::Parse);
                        }
                        "case" | "casex" | "casez" | "randcase" => {
                            self.at += 1;
                            let expression = if word == "randcase" {
                                None
                            } else {
                                Some(self.parenthesized_expression()?)
                            };
                            let mode = if matches!(self.peek(), "inside" | "matches") {
                                let mode = self.peek().to_owned();
                                self.at += 1;
                                Some(mode)
                            } else {
                                None
                            };
                            jobs.push(Job::CaseNext(CaseState {
                                header,
                                keyword: word,
                                mode,
                                expression,
                                arms: Vec::new(),
                            }));
                        }
                        "for" | "foreach" | "while" | "repeat" | "forever" => {
                            self.at += 1;
                            let kind = match word.as_str() {
                                "for" => LoopSyntax::For(self.for_header()?),
                                "foreach" => LoopSyntax::Foreach(self.foreach_header()?),
                                "while" => LoopSyntax::While(self.parenthesized_expression()?),
                                "repeat" => LoopSyntax::Repeat(self.parenthesized_expression()?),
                                _ => LoopSyntax::Forever,
                            };
                            jobs.push(Job::Loop { header, kind });
                            jobs.push(Job::Parse);
                        }
                        "do" => {
                            self.at += 1;
                            jobs.push(Job::DoWhile(header));
                            jobs.push(Job::Parse);
                        }
                        "#" | "@" | "@*" => {
                            let control = self.typed_timing_control()?;
                            jobs.push(Job::Timing { header, control });
                            jobs.push(Job::Parse);
                        }
                        "wait" => {
                            self.at += 1;
                            if self.eat("fork") {
                                self.expect(";")?;
                                values.push(self.statement_node(header, StatementKind::WaitFork)?);
                            } else {
                                let condition = self.parenthesized_expression()?;
                                jobs.push(Job::Wait { header, condition });
                                jobs.push(Job::Parse);
                            }
                        }
                        "assert" | "assume" | "cover" | "restrict" | "expect" | "wait_order" => {
                            let start = self.at;
                            self.at += 1;
                            let deferred = if self.eat("#") {
                                self.expect("0")?;
                                Some(AssertionDeferral::Zero)
                            } else if self.eat("final") {
                                Some(AssertionDeferral::Final)
                            } else {
                                None
                            };
                            let concurrent = matches!(self.peek(), "property" | "sequence");
                            if concurrent {
                                self.at += 1;
                            }
                            let condition = if !concurrent
                                && matches!(word.as_str(), "assert" | "assume" | "cover")
                            {
                                Some(self.parenthesized_expression()?)
                            } else {
                                self.parentheses()?;
                                None
                            };
                            let tokens = self.range(start);
                            let assertion = if word == "wait_order" {
                                None
                            } else {
                                Some(match condition {
                                    Some(condition) => AssertionHeader::Immediate {
                                        condition,
                                        deferred,
                                        tokens,
                                    },
                                    None => AssertionHeader::Unconverted(tokens),
                                })
                            };
                            if word == "restrict" {
                                self.expect(";")?;
                                values.push(self.action_node(
                                    ActionState {
                                        header,
                                        keyword: word,
                                        tokens,
                                        assertion,
                                        wait_order: false,
                                        allow_else: false,
                                    },
                                    None,
                                    None,
                                )?);
                            } else {
                                let allow_else = matches!(
                                    word.as_str(),
                                    "assert" | "assume" | "expect" | "wait_order"
                                );
                                let wait_order = word == "wait_order";
                                jobs.push(Job::ActionNext(ActionState {
                                    header,
                                    keyword: word,
                                    tokens,
                                    assertion,
                                    wait_order,
                                    allow_else,
                                }));
                            }
                        }
                        "return" | "break" | "continue" => {
                            self.at += 1;
                            let kind = match word.as_str() {
                                "return" => StatementKind::Return(if self.peek() == ";" {
                                    None
                                } else {
                                    Some(self.expression(0)?)
                                }),
                                "break" => StatementKind::Break,
                                _ => StatementKind::Continue,
                            };
                            self.expect(";")?;
                            values.push(self.statement_node(header, kind)?);
                        }
                        "parameter" | "localparam" => {
                            let id = self.parameter_declaration()?;
                            values.push(self.statement_node(
                                header,
                                StatementKind::Declaration(LocalDeclaration::Parameters(id)),
                            )?);
                        }
                        "typedef" => {
                            let id = self.type_alias()?;
                            values.push(self.statement_node(
                                header,
                                StatementKind::Declaration(LocalDeclaration::TypeAlias(id)),
                            )?);
                        }
                        "import" if self.package_binding_start() => {
                            let id = self.package_binding()?;
                            values.push(self.statement_node(
                                header,
                                StatementKind::Declaration(LocalDeclaration::PackageBinding(id)),
                            )?);
                        }
                        "timeunit" | "timeprecision" => {
                            let id = self.time_declaration()?;
                            values.push(self.statement_node(
                                header,
                                StatementKind::Declaration(LocalDeclaration::Time(id)),
                            )?);
                        }
                        "assign" => {
                            let id = self.assign_statement()?;
                            values.push(
                                self.statement_node(header, StatementKind::ProceduralAssign(id))?,
                            );
                        }
                        "disable" | "->" | "->>" | "force" | "release" | "deassign" | "##" => {
                            let tokens = self.semicolon_range()?;
                            let kind = match word.as_str() {
                                "disable" => StatementKind::Disable(tokens),
                                "->" | "->>" => StatementKind::EventTrigger(tokens),
                                "##" => StatementKind::Unconverted {
                                    category: "clocking drive".into(),
                                    tokens,
                                    items: Vec::new(),
                                },
                                _ => StatementKind::ProceduralRelease(tokens),
                            };
                            values.push(self.statement_node(header, kind)?);
                        }
                        "randsequence" => {
                            self.at += 1;
                            if self.peek() == "(" {
                                self.parentheses()?;
                            }
                            jobs.push(Job::RandsequenceNext {
                                header,
                                items: Vec::new(),
                            });
                        }
                        _ if self.declaration_start() => {
                            let declaration = self.local_declaration()?;
                            values.push(
                                self.statement_node(
                                    header,
                                    StatementKind::Declaration(declaration),
                                )?,
                            );
                        }
                        _ => {
                            let left = self.expression(14)?;
                            if matches!(
                                self.peek(),
                                "=" | "<="
                                    | "+="
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
                                if !self.variable_lvalue(left) {
                                    return Err(self.error("expected procedural assignment lvalue"));
                                }
                                let operator = self.peek().to_owned();
                                let nonblocking = operator == "<=";
                                self.at += 1;
                                let (timing, event_control) =
                                    if matches!(self.peek(), "#" | "@" | "@*" | "repeat" | "##") {
                                        let (tokens, event) = self.timing_range()?;
                                        (Some(tokens), event)
                                    } else {
                                        (None, None)
                                    };
                                let right = self.expression(0)?;
                                self.expect(";")?;
                                values.push(self.statement_node(
                                    header,
                                    StatementKind::Assignment {
                                        left,
                                        operator,
                                        right,
                                        nonblocking,
                                        timing,
                                        event_control,
                                    },
                                )?);
                            } else {
                                if !self.call_statement_expression(left) {
                                    return Err(self.error("expected a subroutine call or increment/decrement statement"));
                                }
                                self.expect(";")?;
                                values.push(
                                    self.statement_node(header, StatementKind::Expression(left))?,
                                );
                            }
                        }
                    }
                }
                Job::RandsequenceNext { header, items } => loop {
                    if self.directive()? {
                        continue;
                    }
                    match self.peek() {
                        "" => return Err(self.error("unterminated randsequence")),
                        "endsequence" => {
                            self.at += 1;
                            let tokens = self.range(header.start);
                            values.push(self.statement_node(
                                header,
                                StatementKind::Unconverted {
                                    category: "randsequence".into(),
                                    tokens,
                                    items,
                                },
                            )?);
                            break;
                        }
                        "(" => {
                            self.parentheses()?;
                        }
                        "{" => {
                            let block = Header {
                                start: self.at,
                                label: None,
                                attributes: None,
                                qualifier: None,
                            };
                            self.at += 1;
                            jobs.push(Job::RandsequenceBlock { header, items });
                            jobs.push(Job::BlockNext {
                                header: block,
                                label: None,
                                parallel: false,
                                braced: true,
                                items: Vec::new(),
                            });
                            break;
                        }
                        _ => self.at += 1,
                    }
                },
                Job::RandsequenceBlock { header, mut items } => {
                    items.push(values.pop().unwrap());
                    jobs.push(Job::RandsequenceNext { header, items });
                }
                Job::BlockNext {
                    header,
                    label,
                    parallel,
                    braced,
                    items,
                } => {
                    while self.directive()? {}
                    let end = if braced {
                        self.peek() == "}"
                    } else if parallel {
                        matches!(self.peek(), "join" | "join_any" | "join_none")
                    } else {
                        self.peek() == "end"
                    };
                    if end {
                        let join = self.peek().to_owned();
                        self.at += 1;
                        let end_label = if !braced && self.eat(":") {
                            Some(self.name()?)
                        } else {
                            None
                        };
                        let kind = if parallel {
                            StatementKind::Parallel {
                                label,
                                end_label,
                                join,
                                items,
                            }
                        } else {
                            StatementKind::Block {
                                label,
                                end_label,
                                items,
                            }
                        };
                        values.push(self.statement_node(header, kind)?);
                    } else {
                        jobs.push(Job::BlockItem {
                            header,
                            label,
                            parallel,
                            braced,
                            items,
                        });
                        jobs.push(Job::Parse);
                    }
                }
                Job::BlockItem {
                    header,
                    label,
                    parallel,
                    braced,
                    mut items,
                } => {
                    items.push(values.pop().unwrap());
                    jobs.push(Job::BlockNext {
                        header,
                        label,
                        parallel,
                        braced,
                        items,
                    });
                }
                Job::IfThen {
                    header,
                    mut branches,
                    predicate,
                } => {
                    let body = values.pop().unwrap();
                    branches.push(ConditionalBranch { predicate, body });
                    jobs.push(Job::IfNext { header, branches });
                }
                Job::IfNext { header, branches } => {
                    while self.directive()? {}
                    if self.eat("else") {
                        if self.eat("if") {
                            let predicate = self.predicate()?;
                            jobs.push(Job::IfThen {
                                header,
                                branches,
                                predicate,
                            });
                            jobs.push(Job::Parse);
                        } else {
                            jobs.push(Job::IfElse { header, branches });
                            jobs.push(Job::Parse);
                        }
                    } else {
                        values.push(self.statement_node(
                            header,
                            StatementKind::Conditional {
                                branches,
                                otherwise: None,
                            },
                        )?);
                    }
                }
                Job::IfElse { header, branches } => {
                    let otherwise = Some(values.pop().unwrap());
                    values.push(self.statement_node(
                        header,
                        StatementKind::Conditional {
                            branches,
                            otherwise,
                        },
                    )?);
                }
                Job::CaseNext(state) => {
                    while self.directive()? {}
                    if self.eat("endcase") {
                        values.push(self.statement_node(
                            state.header,
                            StatementKind::Case {
                                keyword: state.keyword,
                                mode: state.mode,
                                expression: state.expression,
                                arms: state.arms,
                            },
                        )?);
                    } else {
                        let start = self.at;
                        let default = self.eat("default");
                        let mut matches = Vec::new();
                        if default {
                            self.eat(":");
                        } else {
                            loop {
                                if state.mode.as_deref() == Some("matches") {
                                    let pattern_start = self.at;
                                    self.pattern_range(":")?;
                                    if self.eat("&&&") {
                                        self.expression(0)?;
                                    }
                                    matches.push(CaseMatch::Pattern(self.range(pattern_start)));
                                } else if state.mode.as_deref() == Some("inside")
                                    && self.peek() == "["
                                {
                                    let dimension = self.dimension()?;
                                    let DimensionKind::Range { left, right } = dimension.kind
                                    else {
                                        return Err(self.error("expected inside case range"));
                                    };
                                    matches.push(CaseMatch::Range { left, right });
                                } else {
                                    matches.push(CaseMatch::Expression(self.expression(0)?));
                                }
                                if self.eat(":") {
                                    break;
                                }
                                self.expect(",")?;
                            }
                        }
                        jobs.push(Job::CaseArm {
                            state,
                            start,
                            matches,
                            default,
                        });
                        jobs.push(Job::Parse);
                    }
                }
                Job::CaseArm {
                    mut state,
                    start,
                    matches,
                    default,
                } => {
                    let body = values.pop().unwrap();
                    state.arms.push(CaseArm {
                        matches,
                        default,
                        body,
                        tokens: self.range(start),
                    });
                    jobs.push(Job::CaseNext(state));
                }
                Job::Loop { header, kind } => {
                    let body = values.pop().unwrap();
                    values.push(self.statement_node(header, StatementKind::Loop { kind, body })?);
                }
                Job::DoWhile(header) => {
                    let body = values.pop().unwrap();
                    self.expect("while")?;
                    let expression = self.parenthesized_expression()?;
                    self.expect(";")?;
                    values.push(self.statement_node(
                        header,
                        StatementKind::Loop {
                            kind: LoopSyntax::DoWhile(expression),
                            body,
                        },
                    )?);
                }
                Job::Timing { header, control } => {
                    let body = values.pop().unwrap();
                    values.push(
                        self.statement_node(header, StatementKind::Timing { control, body })?,
                    );
                }
                Job::Wait { header, condition } => {
                    let body = values.pop().unwrap();
                    values.push(
                        self.statement_node(header, StatementKind::Wait { condition, body })?,
                    );
                }
                Job::ActionNext(state) => {
                    if state.allow_else && self.eat("else") {
                        jobs.push(Job::ActionFailure {
                            state,
                            success: None,
                        });
                        jobs.push(Job::Parse);
                    } else {
                        jobs.push(Job::ActionSuccess(state));
                        jobs.push(Job::Parse);
                    }
                }
                Job::ActionSuccess(state) => {
                    let success = Some(values.pop().unwrap());
                    while self.directive()? {}
                    if state.allow_else && self.eat("else") {
                        jobs.push(Job::ActionFailure { state, success });
                        jobs.push(Job::Parse);
                    } else {
                        values.push(self.action_node(state, success, None)?);
                    }
                }
                Job::ActionFailure { state, success } => {
                    let failure = Some(values.pop().unwrap());
                    values.push(self.action_node(state, success, failure)?);
                }
            }
        }
        debug_assert_eq!(values.len(), 1);
        Ok(values.pop().unwrap())
    }
}
