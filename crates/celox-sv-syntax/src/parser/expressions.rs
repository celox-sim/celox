use super::*;

// Continuations own syntax handles, never native parser stack frames. The value
// stack has one result per completed job, including list and dimension jobs.
pub(super) enum Job {
    Type(super::types::TypeJob),
    Dimensions(Vec<Dimension>),
    DimensionsNext(Vec<Dimension>),
    TypeExpression {
        start: usize,
        minimum: u8,
    },
    Attributes,
    AttributeSpec,
    AttributeValue,
    AttributeNext,
    DropAttributes,
    AfterOperatorAttributes {
        precedence: u8,
        right: bool,
        inside: bool,
    },
    Expression(u8),
    ExpressionAfterAttributes {
        start: usize,
        minimum: u8,
    },
    Tail {
        start: usize,
        minimum: u8,
        left: ExprId,
    },
    Unary {
        start: usize,
        minimum: u8,
        operator: String,
    },
    Group {
        start: usize,
        minimum: u8,
    },
    GroupAssignment {
        start: usize,
        minimum: u8,
        left: ExprId,
        operator: String,
    },
    Tagged {
        start: usize,
        minimum: u8,
        member: Name,
    },
    GroupTypical {
        start: usize,
        minimum: u8,
        first: ExprId,
    },
    GroupMaximum {
        start: usize,
        minimum: u8,
        first: ExprId,
        typical: ExprId,
    },
    Binary {
        start: usize,
        minimum: u8,
        left: ExprId,
        operator: String,
    },
    Then {
        start: usize,
        minimum: u8,
        condition: ExprId,
    },
    Else {
        start: usize,
        minimum: u8,
        condition: ExprId,
        then_expression: ExprId,
    },
    Call {
        start: usize,
        minimum: u8,
        left: ExprId,
        parameterized: bool,
    },
    Select {
        start: usize,
        minimum: u8,
        left: ExprId,
    },
    Cast {
        start: usize,
        minimum: u8,
        target: ExprId,
    },
    Arguments,
    Argument {
        arguments: Vec<Argument>,
    },
    ArgumentValue {
        arguments: Vec<Argument>,
        name: Option<Name>,
    },
    ArgumentsNext {
        arguments: Vec<Argument>,
    },
    Dimension,
    DimensionLeft {
        start: usize,
    },
    DimensionRight {
        start: usize,
        left: ExprId,
        operator: String,
    },
    List {
        end: &'static str,
        stream: bool,
    },
    ListValue {
        end: &'static str,
        stream: bool,
        values: Vec<ExprId>,
    },
    ListNext {
        end: &'static str,
        stream: bool,
        values: Vec<ExprId>,
    },
    StreamRange {
        end: &'static str,
        expression: ExprId,
        values: Vec<ExprId>,
    },
    Concat {
        start: usize,
        minimum: u8,
    },
    ConcatFirst {
        start: usize,
        minimum: u8,
    },
    ConcatValue {
        start: usize,
        minimum: u8,
        values: Vec<ExprId>,
    },
    Replication {
        start: usize,
        minimum: u8,
        count: ExprId,
    },
    StreamSlice {
        start: usize,
        minimum: u8,
        operator: String,
    },
    Stream {
        start: usize,
        minimum: u8,
        operator: String,
        slice: Option<ExprId>,
    },
    Pattern {
        start: usize,
        minimum: u8,
        target: Option<ExprId>,
    },
    PatternFirst {
        start: usize,
        minimum: u8,
        target: Option<ExprId>,
        items: Vec<PatternItem>,
    },
    PatternValue {
        start: usize,
        minimum: u8,
        target: Option<ExprId>,
        items: Vec<PatternItem>,
        key: ExprId,
    },
    PatternReplication {
        start: usize,
        minimum: u8,
        target: Option<ExprId>,
        items: Vec<PatternItem>,
        count: ExprId,
    },
    PatternNext {
        start: usize,
        minimum: u8,
        target: Option<ExprId>,
        items: Vec<PatternItem>,
    },
    Set {
        start: usize,
    },
    SetNext {
        start: usize,
        values: Vec<Dimension>,
    },
    SetValue {
        start: usize,
        values: Vec<Dimension>,
        dimension: bool,
        value_start: usize,
    },
}

pub(super) enum Value {
    Attributes,
    DataType(TypeId),
    Dimensions(Vec<Dimension>),
    Expression(ExprId),
    Arguments(Vec<Argument>),
    Dimension(Dimension),
    List(Vec<ExprId>),
}
impl Value {
    pub(super) fn expression(self) -> ExprId {
        let Self::Expression(value) = self else {
            unreachable!("expression continuation")
        };
        value
    }
    pub(super) fn arguments(self) -> Vec<Argument> {
        let Self::Arguments(value) = self else {
            unreachable!("argument continuation")
        };
        value
    }
    pub(super) fn dimension(self) -> Dimension {
        let Self::Dimension(value) = self else {
            unreachable!("dimension continuation")
        };
        value
    }
    pub(super) fn list(self) -> Vec<ExprId> {
        let Self::List(value) = self else {
            unreachable!("list continuation")
        };
        value
    }
}

impl Parser<'_> {
    pub(super) fn variable_lvalue(&self, root: ExprId) -> bool {
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            match &self.expressions[id.0].kind {
                ExprKind::Name(_) => {}
                ExprKind::Member { expression, .. }
                | ExprKind::Select { expression, .. }
                | ExprKind::StreamSelect { expression, .. } => pending.push(*expression),
                ExprKind::Concatenation(parts) if !parts.is_empty() => pending.extend(parts),
                ExprKind::Streaming { expressions, .. } if !expressions.is_empty() => {
                    pending.extend(expressions)
                }
                ExprKind::Pattern { items, .. } if !items.is_empty() => {
                    pending.extend(items.iter().map(|item| item.value))
                }
                _ => return false,
            }
        }
        true
    }

    pub(super) fn expression(&mut self, minimum: u8) -> Result<ExprId> {
        Ok(self
            .expression_machine(Job::Expression(minimum))?
            .expression())
    }
    /// Resume an ordinary expression after an event group proved to contain
    /// only an expression. This resolves parentheses without token rescanning.
    pub(super) fn event_group_expression(&mut self, start: usize, inner: ExprId) -> Result<ExprId> {
        let left = self.node(start, ExprKind::Group(inner))?;
        Ok(self
            .expression_machine(Job::Tail {
                start,
                minimum: 0,
                left,
            })?
            .expression())
    }
    pub(super) fn constant_parameter_expression(&mut self) -> Result<ExprId> {
        let value = self.parameter_expression()?;
        self.validate_constant_operators(value)?;
        Ok(value)
    }

    // Constant operands cannot use assignment/inc-dec expressions. Function
    // call arguments retain their ordinary expression grammar (A.8.2), and
    // type references likewise retain their own expression operand grammar.
    pub(super) fn validate_constant_operators(&self, root: ExprId) -> Result<()> {
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            match &self.expressions[id.0].kind {
                ExprKind::Assignment { .. } => {
                    return Err(self.error("assignment is not a constant-expression operator"));
                }
                ExprKind::Unary {
                    operator,
                    expression,
                    ..
                } => {
                    if matches!(operator.as_str(), "++" | "--") {
                        return Err(self.error(
                            "increment or decrement is not a constant-expression operator",
                        ));
                    }
                    pending.push(*expression);
                }
                ExprKind::Group(value)
                | ExprKind::Member {
                    expression: value, ..
                } => pending.push(*value),
                ExprKind::Binary { left, right, .. } => pending.extend([*left, *right]),
                ExprKind::Conditional {
                    condition,
                    then_expression,
                    else_expression,
                } => pending.extend([*condition, *then_expression, *else_expression]),
                ExprKind::MinTypMax {
                    minimum,
                    typical,
                    maximum,
                } => pending.extend([*minimum, *typical, *maximum]),
                ExprKind::Cast { target, expression } => pending.extend([*target, *expression]),
                ExprKind::Concatenation(values) => pending.extend(values),
                ExprKind::Replication { count, expressions } => {
                    pending.push(*count);
                    pending.extend(expressions);
                }
                ExprKind::Pattern { target, items } => {
                    pending.extend(target);
                    for item in items {
                        pending.extend(item.key);
                        pending.push(item.value);
                    }
                }
                ExprKind::Select {
                    expression,
                    dimension,
                }
                | ExprKind::StreamSelect {
                    expression,
                    dimension,
                } => {
                    pending.push(*expression);
                    constant_dimension_operands(dimension, &mut pending);
                }
                ExprKind::Set(dimensions) => {
                    for dimension in dimensions {
                        constant_dimension_operands(dimension, &mut pending);
                    }
                }
                ExprKind::Streaming {
                    slice, expressions, ..
                } => {
                    pending.extend(slice);
                    pending.extend(expressions);
                }
                ExprKind::Tagged { value, .. } => pending.extend(value),
                ExprKind::Call { .. }
                | ExprKind::Parameterized { .. }
                | ExprKind::Type(_)
                | ExprKind::Literal(_)
                | ExprKind::Name(_) => {}
            }
        }
        Ok(())
    }

    pub(super) fn parameter_expression(&mut self) -> Result<ExprId> {
        let start = self.at;
        let minimum = self.expression(0)?;
        if !self.eat(":") {
            return Ok(minimum);
        }
        let typical = self.expression(0)?;
        self.expect(":")?;
        let maximum = self.expression(0)?;
        self.node(
            start,
            ExprKind::MinTypMax {
                minimum,
                typical,
                maximum,
            },
        )
    }
    pub(super) fn dimension(&mut self) -> Result<Dimension> {
        Ok(self.expression_machine(Job::Dimension)?.dimension())
    }
    pub(super) fn dimensions(&mut self) -> Result<Vec<Dimension>> {
        let mut dimensions = Vec::new();
        while self.peek() == "[" {
            dimensions.push(self.dimension()?);
        }
        self.validate_variable_dimensions(&dimensions)?;
        Ok(dimensions)
    }
    pub(super) fn validate_variable_dimensions(&self, dimensions: &[Dimension]) -> Result<()> {
        for dimension in dimensions {
            match dimension.kind {
                DimensionKind::Indexed { .. } => {
                    return Err(self.error("indexed part-select is not a declaration dimension"));
                }
                DimensionKind::Index(value) | DimensionKind::Queue(Some(value)) => {
                    self.validate_constant_operators(value)?
                }
                DimensionKind::Range { left, right } => {
                    self.validate_constant_operators(left)?;
                    self.validate_constant_operators(right)?;
                }
                DimensionKind::Unsized | DimensionKind::Wildcard | DimensionKind::Queue(None) => {}
            }
        }
        Ok(())
    }

    pub(super) fn validate_unpacked_dimensions(&self, dimensions: &[Dimension]) -> Result<()> {
        self.validate_variable_dimensions(dimensions)?;
        for dimension in dimensions {
            match dimension.kind {
                DimensionKind::Range { .. } => {}
                DimensionKind::Index(value)
                    if !matches!(self.expressions[value.0].kind, ExprKind::Type(_)) => {}
                _ => return Err(self.error("expected an unpacked constant size or range")),
            }
        }
        Ok(())
    }

    pub(super) fn node(&mut self, start: usize, kind: ExprKind) -> Result<ExprId> {
        // A million arena nodes is tens of MiB of structural data before token
        // strings/source storage, rather than an ordinary expression-depth limit.
        if self.expressions.len() >= 1_000_000 {
            return Err(self.unsupported("native expression arena exceeds one million nodes"));
        }
        let id = ExprId(self.expressions.len());
        self.expressions.push(Expr {
            kind,
            tokens: self.range(start),
        });
        Ok(id)
    }
    fn tail(
        &mut self,
        jobs: &mut Vec<Job>,
        start: usize,
        minimum: u8,
        kind: ExprKind,
    ) -> Result<()> {
        let left = self.node(start, kind)?;
        jobs.push(Job::Tail {
            start,
            minimum,
            left,
        });
        Ok(())
    }
    pub(super) fn expression_machine(&mut self, initial: Job) -> Result<Value> {
        let mut jobs = vec![initial];
        let mut values: Vec<Value> = Vec::new();
        let mut attribute_value = false;
        while let Some(job) = jobs.pop() {
            if jobs.len() > 1_000_000 {
                return Err(self
                    .unsupported("native syntax continuation stack exceeds one million entries"));
            }
            match job {
                Job::Type(job) => self.type_job(job, &mut jobs, &mut values)?,
                Job::TypeExpression { start, minimum } => {
                    let Value::DataType(data_type) = values.pop().unwrap() else {
                        unreachable!()
                    };
                    self.tail(&mut jobs, start, minimum, ExprKind::Type(data_type))?;
                }
                Job::Dimensions(mut dimensions) => {
                    if self.peek() == "[" {
                        jobs.push(Job::DimensionsNext(dimensions));
                        jobs.push(Job::Dimension);
                    } else {
                        values.push(Value::Dimensions(std::mem::take(&mut dimensions)));
                    }
                }
                Job::DimensionsNext(mut dimensions) => {
                    dimensions.push(values.pop().unwrap().dimension());
                    jobs.push(Job::Dimensions(dimensions));
                }
                Job::Attributes => {
                    if self.eat("(*") {
                        if attribute_value {
                            return Err(
                                self.error("attribute values cannot contain nested attributes")
                            );
                        }
                        jobs.push(Job::Attributes);
                        jobs.push(Job::AttributeSpec);
                    } else {
                        values.push(Value::Attributes);
                    }
                }
                Job::AttributeSpec => {
                    self.name()?;
                    jobs.push(Job::AttributeNext);
                    if self.eat("=") {
                        attribute_value = true;
                        jobs.push(Job::AttributeValue);
                        jobs.push(Job::Expression(0));
                    }
                }
                Job::AttributeValue => {
                    let expression = values.pop().unwrap().expression();
                    attribute_value = false;
                    self.validate_constant_operators(expression)?;
                }
                Job::AttributeNext => {
                    if self.eat(",") {
                        jobs.push(Job::AttributeSpec);
                    } else {
                        self.expect("*)")?;
                    }
                }
                Job::DropAttributes => {
                    assert!(matches!(values.pop(), Some(Value::Attributes)));
                }
                Job::AfterOperatorAttributes {
                    precedence,
                    right,
                    inside,
                } => {
                    jobs.push(if inside {
                        Job::Set { start: self.at }
                    } else {
                        Job::Expression(precedence + u8::from(!right))
                    });
                }
                Job::Expression(minimum) => {
                    jobs.push(Job::ExpressionAfterAttributes {
                        start: self.at,
                        minimum,
                    });
                    jobs.push(Job::DropAttributes);
                    jobs.push(Job::Attributes);
                }
                Job::ExpressionAfterAttributes { start, minimum } => {
                    let word = self.peek();
                    match word {
                        "+" | "-" | "!" | "~" | "&" | "~&" | "|" | "~|" | "^" | "~^" | "^~"
                        | "++" | "--" => {
                            let operator = self.peek().to_owned();
                            self.at += 1;
                            jobs.push(Job::Unary {
                                start,
                                minimum,
                                operator,
                            });
                            jobs.push(Job::Expression(14));
                        }
                        "tagged" => {
                            self.at += 1;
                            let member = self.name()?;
                            if matches!(self.peek(), "" | ";" | "," | ")" | "]" | "}" | ":" | "?") {
                                self.tail(
                                    &mut jobs,
                                    start,
                                    minimum,
                                    ExprKind::Tagged {
                                        member,
                                        value: None,
                                    },
                                )?;
                            } else {
                                jobs.push(Job::Tagged {
                                    start,
                                    minimum,
                                    member,
                                });
                                jobs.push(Job::Expression(0));
                            }
                        }
                        "(" => {
                            self.at += 1;
                            jobs.push(Job::Group { start, minimum });
                            jobs.push(Job::Expression(0));
                        }
                        "{" => jobs.push(Job::Concat { start, minimum }),
                        "'" if self.nth(1) == "{" => {
                            self.at += 1;
                            jobs.push(Job::Pattern {
                                start,
                                minimum,
                                target: None,
                            });
                        }
                        word if builtin(word)
                            || matches!(
                                word,
                                "struct" | "union" | "enum" | "virtual" | "interface"
                            )
                            || (word == "type" && self.nth(1) == "(") =>
                        {
                            jobs.push(Job::TypeExpression { start, minimum });
                            jobs.push(Job::Type(super::types::TypeJob::Start));
                        }
                        _ => {
                            let Some(token) = self.token() else {
                                return Err(self.error("expected an expression"));
                            };
                            if self.reserved_identifier(token)
                                && !matches!(
                                    token.text.as_str(),
                                    "null"
                                        | "default"
                                        | "this"
                                        | "super"
                                        | "new"
                                        | "local"
                                        | "const"
                                        | "signed"
                                        | "unsigned"
                                )
                            {
                                return Err(
                                    self.error("reserved keyword cannot begin an expression")
                                );
                            }
                            let numeric = token.kind == TokenKind::Number;
                            let mut kind = match token.kind {
                                TokenKind::Number | TokenKind::String => {
                                    ExprKind::Literal(token.text.clone())
                                }
                                TokenKind::Identifier
                                | TokenKind::EscapedIdentifier
                                | TokenKind::SystemIdentifier => {
                                    if matches!(token.text.as_str(), "null" | "default") {
                                        ExprKind::Literal(token.text.clone())
                                    } else {
                                        ExprKind::Name(Name {
                                            spelling: token.text.clone(),
                                            origin: token.origin,
                                        })
                                    }
                                }
                                _ => {
                                    return Err(self.unsupported(format!(
                                        "unsupported expression beginning with `{}`",
                                        token.text
                                    )));
                                }
                            };
                            self.at += 1;
                            if let ExprKind::Literal(value) = &kind {
                                if numeric
                                    && value.bytes().all(|b| b.is_ascii_digit() || b == b'_')
                                    && self.token().is_some_and(|next| {
                                        next.kind == TokenKind::Number
                                            && next.text.starts_with('\'')
                                            && next.text.as_bytes().get(1).is_some_and(|b| {
                                                matches!(
                                                    b.to_ascii_lowercase(),
                                                    b's' | b'b' | b'o' | b'd' | b'h'
                                                )
                                            })
                                    })
                                {
                                    kind = ExprKind::Literal(format!("{}{}", value, self.peek()));
                                    self.at += 1;
                                }
                            }
                            if numeric
                                && matches!(&kind, ExprKind::Literal(value) if !super::numbers::literal(value))
                            {
                                return Err(self.error("invalid numeric literal"));
                            }
                            // Numeric grammar permits whitespace between the base
                            // and digits. Preserve it in tokens, while expression
                            // values use the same compact spelling as other literals.
                            if numeric && let ExprKind::Literal(value) = &mut kind {
                                value.retain(|character| !character.is_ascii_whitespace());
                            }
                            self.tail(&mut jobs, start, minimum, kind)?;
                        }
                    }
                }
                Job::Tail {
                    start,
                    minimum,
                    left,
                } => match self.peek() {
                    "." | "::" => {
                        let separator = self.peek().to_owned();
                        self.at += 1;
                        let name = if separator == "."
                            && matches!(&self.expressions[left.0].kind, ExprKind::Name(name) if name.spelling == "this")
                        {
                            self.contextual_name(&["and", "or", "xor", "unique", "new", "super"])?
                        } else {
                            self.contextual_name(&["and", "or", "xor", "unique", "new"])?
                        };
                        self.tail(
                            &mut jobs,
                            start,
                            minimum,
                            ExprKind::Member {
                                expression: left,
                                separator,
                                name,
                            },
                        )?;
                    }
                    "(" | "#" => {
                        let parameterized = self.eat("#");
                        jobs.push(Job::Call {
                            start,
                            minimum,
                            left,
                            parameterized,
                        });
                        jobs.push(Job::Arguments);
                    }
                    "[" => {
                        jobs.push(Job::Select {
                            start,
                            minimum,
                            left,
                        });
                        jobs.push(Job::Dimension);
                    }
                    "'" => {
                        self.at += 1;
                        if self.peek() == "{" {
                            jobs.push(Job::Pattern {
                                start,
                                minimum,
                                target: Some(left),
                            });
                        } else {
                            self.expect("(")?;
                            jobs.push(Job::Cast {
                                start,
                                minimum,
                                target: left,
                            });
                            jobs.push(Job::Expression(0));
                        }
                    }
                    "++" | "--" => {
                        if !self.variable_lvalue(left) {
                            return Err(
                                self.error("increment or decrement requires a variable lvalue")
                            );
                        }
                        let operator = self.peek().to_owned();
                        self.at += 1;
                        self.tail(
                            &mut jobs,
                            start,
                            minimum,
                            ExprKind::Unary {
                                operator,
                                expression: left,
                                postfix: true,
                            },
                        )?;
                    }
                    "?" if minimum <= 2 => {
                        self.at += 1;
                        jobs.push(Job::Then {
                            start,
                            minimum,
                            condition: left,
                        });
                        jobs.push(Job::Expression(0));
                    }
                    operator => {
                        if let Some((precedence, right)) =
                            binding_power(operator).filter(|(power, _)| *power >= minimum)
                        {
                            let operator = operator.to_owned();
                            self.at += 1;
                            let inside = operator == "inside";
                            jobs.push(Job::Binary {
                                start,
                                minimum,
                                left,
                                operator,
                            });
                            jobs.push(Job::AfterOperatorAttributes {
                                precedence,
                                right,
                                inside,
                            });
                            jobs.push(Job::DropAttributes);
                            jobs.push(Job::Attributes);
                        } else {
                            values.push(Value::Expression(left));
                        }
                    }
                },
                Job::Unary {
                    start,
                    minimum,
                    operator,
                } => {
                    let expression = values.pop().unwrap().expression();
                    if matches!(operator.as_str(), "++" | "--") && !self.variable_lvalue(expression)
                    {
                        return Err(self.error("increment or decrement requires a variable lvalue"));
                    }
                    self.tail(
                        &mut jobs,
                        start,
                        minimum,
                        ExprKind::Unary {
                            operator,
                            expression,
                            postfix: false,
                        },
                    )?;
                }
                Job::Group { start, minimum } => {
                    let expression = values.pop().unwrap().expression();
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
                        if !self.variable_lvalue(expression) {
                            return Err(
                                self.error("expected variable lvalue before assignment operator")
                            );
                        }
                        let operator = self.peek().to_owned();
                        self.at += 1;
                        jobs.push(Job::GroupAssignment {
                            start,
                            minimum,
                            left: expression,
                            operator,
                        });
                        jobs.push(Job::Expression(0));
                    } else if self.eat(":") {
                        jobs.push(Job::GroupTypical {
                            start,
                            minimum,
                            first: expression,
                        });
                        jobs.push(Job::Expression(0));
                    } else {
                        self.expect(")")?;
                        self.tail(&mut jobs, start, minimum, ExprKind::Group(expression))?;
                    }
                }
                Job::GroupAssignment {
                    start,
                    minimum,
                    left,
                    operator,
                } => {
                    let right = values.pop().unwrap().expression();
                    self.expect(")")?;
                    self.tail(
                        &mut jobs,
                        start,
                        minimum,
                        ExprKind::Assignment {
                            left,
                            operator,
                            right,
                        },
                    )?;
                }
                Job::Tagged {
                    start,
                    minimum,
                    member,
                } => {
                    let value = Some(values.pop().unwrap().expression());
                    self.tail(
                        &mut jobs,
                        start,
                        minimum,
                        ExprKind::Tagged { member, value },
                    )?;
                }
                Job::GroupTypical {
                    start,
                    minimum,
                    first,
                } => {
                    let typical = values.pop().unwrap().expression();
                    self.expect(":")?;
                    jobs.push(Job::GroupMaximum {
                        start,
                        minimum,
                        first,
                        typical,
                    });
                    jobs.push(Job::Expression(0));
                }
                Job::GroupMaximum {
                    start,
                    minimum,
                    first,
                    typical,
                } => {
                    let maximum = values.pop().unwrap().expression();
                    self.expect(")")?;
                    let expression = self.node(
                        start,
                        ExprKind::MinTypMax {
                            minimum: first,
                            typical,
                            maximum,
                        },
                    )?;
                    self.tail(&mut jobs, start, minimum, ExprKind::Group(expression))?;
                }
                Job::Binary {
                    start,
                    minimum,
                    left,
                    operator,
                } => {
                    let right = values.pop().unwrap().expression();
                    self.tail(
                        &mut jobs,
                        start,
                        minimum,
                        ExprKind::Binary {
                            left,
                            operator,
                            right,
                        },
                    )?;
                }
                Job::Then {
                    start,
                    minimum,
                    condition,
                } => {
                    let then_expression = values.pop().unwrap().expression();
                    self.expect(":")?;
                    jobs.push(Job::Else {
                        start,
                        minimum,
                        condition,
                        then_expression,
                    });
                    jobs.push(Job::Expression(2));
                }
                Job::Else {
                    start,
                    minimum,
                    condition,
                    then_expression,
                } => {
                    let else_expression = values.pop().unwrap().expression();
                    self.tail(
                        &mut jobs,
                        start,
                        minimum,
                        ExprKind::Conditional {
                            condition,
                            then_expression,
                            else_expression,
                        },
                    )?;
                }
                Job::Call {
                    start,
                    minimum,
                    left,
                    parameterized,
                } => {
                    let arguments = values.pop().unwrap().arguments();
                    let kind = if parameterized {
                        ExprKind::Parameterized {
                            expression: left,
                            arguments,
                        }
                    } else {
                        ExprKind::Call {
                            expression: left,
                            arguments,
                        }
                    };
                    self.tail(&mut jobs, start, minimum, kind)?;
                }
                Job::Select {
                    start,
                    minimum,
                    left,
                } => {
                    let dimension = values.pop().unwrap().dimension();
                    self.tail(
                        &mut jobs,
                        start,
                        minimum,
                        ExprKind::Select {
                            expression: left,
                            dimension: Box::new(dimension),
                        },
                    )?;
                }
                Job::Cast {
                    start,
                    minimum,
                    target,
                } => {
                    let expression = values.pop().unwrap().expression();
                    self.expect(")")?;
                    self.tail(
                        &mut jobs,
                        start,
                        minimum,
                        ExprKind::Cast { target, expression },
                    )?;
                }
                Job::Arguments => {
                    self.expect("(")?;
                    if self.eat(")") {
                        values.push(Value::Arguments(Vec::new()));
                    } else {
                        jobs.push(Job::Argument {
                            arguments: Vec::new(),
                        });
                    }
                }
                Job::Argument { mut arguments } => {
                    if self.eat(".") {
                        let name = self.name()?;
                        self.expect("(")?;
                        if self.eat(")") {
                            arguments.push(Argument {
                                name: Some(name),
                                value: None,
                            });
                            jobs.push(Job::ArgumentsNext { arguments });
                        } else {
                            jobs.push(Job::ArgumentValue {
                                arguments,
                                name: Some(name),
                            });
                            jobs.push(Job::Expression(0));
                        }
                    } else if matches!(self.peek(), "," | ")") {
                        arguments.push(Argument {
                            name: None,
                            value: None,
                        });
                        jobs.push(Job::ArgumentsNext { arguments });
                    } else {
                        jobs.push(Job::ArgumentValue {
                            arguments,
                            name: None,
                        });
                        jobs.push(Job::Expression(0));
                    }
                }
                Job::ArgumentValue {
                    mut arguments,
                    name,
                } => {
                    let value = Some(values.pop().unwrap().expression());
                    if name.is_some() {
                        self.expect(")")?;
                    }
                    arguments.push(Argument { name, value });
                    jobs.push(Job::ArgumentsNext { arguments });
                }
                Job::ArgumentsNext { arguments } => {
                    if self.eat(")") {
                        values.push(Value::Arguments(arguments));
                    } else {
                        self.expect(",")?;
                        jobs.push(Job::Argument { arguments });
                    }
                }
                Job::Dimension => {
                    let start = self.at;
                    self.expect("[")?;
                    if self.eat("]") {
                        values.push(Value::Dimension(Dimension {
                            kind: DimensionKind::Unsized,
                            tokens: self.range(start),
                        }));
                    } else if self.eat("*") {
                        self.expect("]")?;
                        values.push(Value::Dimension(Dimension {
                            kind: DimensionKind::Wildcard,
                            tokens: self.range(start),
                        }));
                    } else {
                        jobs.push(Job::DimensionLeft { start });
                        jobs.push(Job::Expression(0));
                    }
                }
                Job::DimensionLeft { start } => {
                    let left = values.pop().unwrap().expression();
                    if matches!(self.peek(), ":" | "+:" | "-:") {
                        let operator = self.peek().to_owned();
                        self.at += 1;
                        jobs.push(Job::DimensionRight {
                            start,
                            left,
                            operator,
                        });
                        jobs.push(Job::Expression(0));
                    } else {
                        self.expect("]")?;
                        let kind = if self.unbounded(left) {
                            DimensionKind::Queue(None)
                        } else {
                            DimensionKind::Index(left)
                        };
                        values.push(Value::Dimension(Dimension {
                            kind,
                            tokens: self.range(start),
                        }));
                    }
                }
                Job::DimensionRight {
                    start,
                    left,
                    operator,
                } => {
                    let right = values.pop().unwrap().expression();
                    self.expect("]")?;
                    let kind = if operator == ":" {
                        if self.unbounded(left) {
                            DimensionKind::Queue(Some(right))
                        } else {
                            DimensionKind::Range { left, right }
                        }
                    } else {
                        DimensionKind::Indexed {
                            base: left,
                            operator,
                            width: right,
                        }
                    };
                    values.push(Value::Dimension(Dimension {
                        kind,
                        tokens: self.range(start),
                    }));
                }
                Job::List { end, stream } => {
                    if self.eat(end) {
                        values.push(Value::List(Vec::new()));
                    } else {
                        jobs.push(Job::ListValue {
                            end,
                            stream,
                            values: Vec::new(),
                        });
                        jobs.push(Job::Expression(0));
                    }
                }
                Job::ListValue {
                    end,
                    stream,
                    values: mut list,
                } => {
                    let expression = values.pop().unwrap().expression();
                    if stream && self.eat("with") {
                        jobs.push(Job::StreamRange {
                            end,
                            expression,
                            values: list,
                        });
                        jobs.push(Job::Dimension);
                    } else {
                        list.push(expression);
                        jobs.push(Job::ListNext {
                            end,
                            stream,
                            values: list,
                        });
                    }
                }
                Job::StreamRange {
                    end,
                    expression,
                    values: mut list,
                } => {
                    let dimension = Box::new(values.pop().unwrap().dimension());
                    let start = self
                        .indices
                        .binary_search(&self.expressions[expression.0].tokens.start)
                        .expect("expression begins at a significant token");
                    list.push(self.node(
                        start,
                        ExprKind::StreamSelect {
                            expression,
                            dimension,
                        },
                    )?);
                    jobs.push(Job::ListNext {
                        end,
                        stream: true,
                        values: list,
                    });
                }
                Job::ListNext {
                    end,
                    stream,
                    values: list,
                } => {
                    if self.eat(end) {
                        values.push(Value::List(list));
                    } else {
                        self.expect(",")?;
                        jobs.push(Job::ListValue {
                            end,
                            stream,
                            values: list,
                        });
                        jobs.push(Job::Expression(0));
                    }
                }
                Job::Concat { start, minimum } => {
                    self.expect("{")?;
                    if matches!(self.peek(), "<<" | ">>") {
                        let operator = self.peek().to_owned();
                        self.at += 1;
                        if self.eat("{") {
                            jobs.push(Job::Stream {
                                start,
                                minimum,
                                operator,
                                slice: None,
                            });
                            jobs.push(Job::List {
                                end: "}",
                                stream: true,
                            });
                        } else {
                            jobs.push(Job::StreamSlice {
                                start,
                                minimum,
                                operator,
                            });
                            jobs.push(Job::Expression(0));
                        }
                    } else {
                        jobs.push(Job::ConcatFirst { start, minimum });
                        jobs.push(Job::Expression(0));
                    }
                }
                Job::ConcatFirst { start, minimum } => {
                    let first = values.pop().unwrap().expression();
                    if self.eat("{") {
                        jobs.push(Job::Replication {
                            start,
                            minimum,
                            count: first,
                        });
                        jobs.push(Job::List {
                            end: "}",
                            stream: false,
                        });
                    } else if self.eat("}") {
                        self.tail(
                            &mut jobs,
                            start,
                            minimum,
                            ExprKind::Concatenation(vec![first]),
                        )?;
                    } else {
                        self.expect(",")?;
                        jobs.push(Job::ConcatValue {
                            start,
                            minimum,
                            values: vec![first],
                        });
                        jobs.push(Job::Expression(0));
                    }
                }
                Job::ConcatValue {
                    start,
                    minimum,
                    values: mut list,
                } => {
                    list.push(values.pop().unwrap().expression());
                    if self.eat("}") {
                        self.tail(&mut jobs, start, minimum, ExprKind::Concatenation(list))?;
                    } else {
                        self.expect(",")?;
                        jobs.push(Job::ConcatValue {
                            start,
                            minimum,
                            values: list,
                        });
                        jobs.push(Job::Expression(0));
                    }
                }
                Job::Replication {
                    start,
                    minimum,
                    count,
                } => {
                    let expressions = values.pop().unwrap().list();
                    self.expect("}")?;
                    self.tail(
                        &mut jobs,
                        start,
                        minimum,
                        ExprKind::Replication { count, expressions },
                    )?;
                }
                Job::StreamSlice {
                    start,
                    minimum,
                    operator,
                } => {
                    let slice = Some(values.pop().unwrap().expression());
                    self.expect("{")?;
                    jobs.push(Job::Stream {
                        start,
                        minimum,
                        operator,
                        slice,
                    });
                    jobs.push(Job::List {
                        end: "}",
                        stream: true,
                    });
                }
                Job::Stream {
                    start,
                    minimum,
                    operator,
                    slice,
                } => {
                    let expressions = values.pop().unwrap().list();
                    self.expect("}")?;
                    self.tail(
                        &mut jobs,
                        start,
                        minimum,
                        ExprKind::Streaming {
                            operator,
                            slice,
                            expressions,
                        },
                    )?;
                }
                Job::Pattern {
                    start,
                    minimum,
                    target,
                } => {
                    self.expect("{")?;
                    if self.eat("}") {
                        self.tail(
                            &mut jobs,
                            start,
                            minimum,
                            ExprKind::Pattern {
                                target,
                                items: Vec::new(),
                            },
                        )?;
                    } else {
                        jobs.push(Job::PatternFirst {
                            start,
                            minimum,
                            target,
                            items: Vec::new(),
                        });
                        jobs.push(Job::Expression(0));
                    }
                }
                Job::PatternFirst {
                    start,
                    minimum,
                    target,
                    mut items,
                } => {
                    let first = values.pop().unwrap().expression();
                    if self.eat(":") {
                        jobs.push(Job::PatternValue {
                            start,
                            minimum,
                            target,
                            items,
                            key: first,
                        });
                        jobs.push(Job::Expression(0));
                    } else if self.eat("{") {
                        jobs.push(Job::PatternReplication {
                            start,
                            minimum,
                            target,
                            items,
                            count: first,
                        });
                        jobs.push(Job::List {
                            end: "}",
                            stream: false,
                        });
                    } else {
                        items.push(PatternItem {
                            key: None,
                            value: first,
                        });
                        jobs.push(Job::PatternNext {
                            start,
                            minimum,
                            target,
                            items,
                        });
                    }
                }
                Job::PatternValue {
                    start,
                    minimum,
                    target,
                    mut items,
                    key,
                } => {
                    let value = values.pop().unwrap().expression();
                    items.push(PatternItem {
                        key: Some(key),
                        value,
                    });
                    jobs.push(Job::PatternNext {
                        start,
                        minimum,
                        target,
                        items,
                    });
                }
                Job::PatternReplication {
                    start,
                    minimum,
                    target,
                    mut items,
                    count,
                } => {
                    let expressions = values.pop().unwrap().list();
                    let value = self.node(start, ExprKind::Replication { count, expressions })?;
                    items.push(PatternItem { key: None, value });
                    jobs.push(Job::PatternNext {
                        start,
                        minimum,
                        target,
                        items,
                    });
                }
                Job::PatternNext {
                    start,
                    minimum,
                    target,
                    items,
                } => {
                    if self.eat("}") {
                        self.tail(
                            &mut jobs,
                            start,
                            minimum,
                            ExprKind::Pattern { target, items },
                        )?;
                    } else {
                        self.expect(",")?;
                        jobs.push(Job::PatternFirst {
                            start,
                            minimum,
                            target,
                            items,
                        });
                        jobs.push(Job::Expression(0));
                    }
                }
                Job::Set { start } => {
                    self.expect("{")?;
                    jobs.push(Job::SetNext {
                        start,
                        values: Vec::new(),
                    });
                }
                Job::SetNext {
                    start,
                    values: list,
                } => {
                    let dimension = self.peek() == "[";
                    jobs.push(Job::SetValue {
                        start,
                        values: list,
                        dimension,
                        value_start: self.at,
                    });
                    jobs.push(if dimension {
                        Job::Dimension
                    } else {
                        Job::Expression(0)
                    });
                }
                Job::SetValue {
                    start,
                    values: mut list,
                    dimension,
                    value_start,
                } => {
                    let value = values.pop().unwrap();
                    list.push(if dimension {
                        value.dimension()
                    } else {
                        Dimension {
                            kind: DimensionKind::Index(value.expression()),
                            tokens: self.range(value_start),
                        }
                    });
                    if self.eat("}") {
                        let expression = self.node(start, ExprKind::Set(list))?;
                        values.push(Value::Expression(expression));
                    } else {
                        self.expect(",")?;
                        jobs.push(Job::SetNext {
                            start,
                            values: list,
                        });
                    }
                }
            }
        }
        debug_assert_eq!(values.len(), 1);
        Ok(values.pop().unwrap())
    }
    fn unbounded(&self, id: ExprId) -> bool {
        matches!(&self.expressions[id.0].kind,ExprKind::Name(name) if name.spelling=="$")
    }
}

// IEEE 1800-2023 11.3.2, Table 11-2: power is left associative,
// below unary; conditional and implication are right associative.
fn binding_power(operator: &str) -> Option<(u8, bool)> {
    Some((
        match operator {
            "->" | "<->" => return Some((1, true)),
            "||" => 3,
            "&&" => 4,
            "|" => 5,
            "^" | "~^" | "^~" => 6,
            "&" => 7,
            "==" | "!=" | "===" | "!==" | "==?" | "!=?" => 8,
            "<" | "<=" | ">" | ">=" | "inside" => 9,
            "<<" | ">>" | "<<<" | ">>>" => 10,
            "+" | "-" => 11,
            "*" | "/" | "%" => 12,
            "**" => 13,
            _ => return None,
        },
        false,
    ))
}
pub(super) fn builtin(word: &str) -> bool {
    matches!(
        word,
        "bit"
            | "logic"
            | "reg"
            | "byte"
            | "shortint"
            | "int"
            | "longint"
            | "integer"
            | "time"
            | "real"
            | "shortreal"
            | "realtime"
            | "string"
            | "chandle"
            | "event"
            | "void"
    )
}

fn constant_dimension_operands(dimension: &Dimension, pending: &mut Vec<ExprId>) {
    match dimension.kind {
        DimensionKind::Index(value) => pending.push(value),
        DimensionKind::Range { left, right } => pending.extend([left, right]),
        DimensionKind::Indexed { base, width, .. } => pending.extend([base, width]),
        DimensionKind::Queue(Some(value)) => pending.push(value),
        DimensionKind::Unsized | DimensionKind::Queue(None) | DimensionKind::Wildcard => {}
    }
}
