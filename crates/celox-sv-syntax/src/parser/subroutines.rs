//! Function/task headers and flat authored statement bodies (IEEE13.3/13.4).
use super::*;

impl Parser<'_> {
    // A qualified identifier before '(' or ';' is the subroutine name, not a
    // return datatype. Inspect token boundaries without reparsing source text.
    pub(super) fn subroutine_return_type(&self) -> bool {
        if super::expressions::builtin(self.peek())
            || matches!(
                self.peek(),
                "signed" | "unsigned" | "[" | "struct" | "union" | "enum" | "virtual" | "type"
            )
        {
            return true;
        }
        let mut at = self.at;
        let word = |at: usize| {
            self.indices
                .get(at)
                .map(|index| self.tokens[*index].text.as_str())
                .unwrap_or("")
        };
        loop {
            if word(at).is_empty() {
                return false;
            }
            at += 1;
            if word(at) == "#" {
                at += 1;
                if word(at) != "(" {
                    return false;
                }
                let mut stack = vec![")"];
                at += 1;
                while !stack.is_empty() {
                    match word(at) {
                        "" => return false,
                        "(" => stack.push(")"),
                        "[" => stack.push("]"),
                        "{" => stack.push("}"),
                        value if stack.last().copied() == Some(value) => {
                            stack.pop();
                        }
                        _ => {}
                    }
                    at += 1;
                }
            }
            if matches!(word(at), "::" | ".") {
                at += 1;
                continue;
            }
            if word(at) == "[" {
                return true;
            }
            return self.indices.get(at).is_some_and(|index| {
                matches!(
                    self.tokens[*index].kind,
                    TokenKind::Identifier | TokenKind::EscapedIdentifier
                )
            });
        }
    }

    fn subroutine_name(&mut self) -> Result<(Vec<SubroutineScope>, Name)> {
        let mut scope = Vec::new();
        loop {
            let name = if matches!(self.peek(), "$unit" | "$root") {
                let token = self.token().unwrap();
                let name = Name {
                    spelling: token.text.clone(),
                    origin: token.origin,
                };
                self.at += 1;
                name
            } else {
                self.contextual_name(&["new"])?
            };
            let arguments = if self.eat("#") {
                Some(
                    self.expression_machine(super::expressions::Job::Arguments)?
                        .arguments(),
                )
            } else {
                None
            };
            if matches!(self.peek(), "::" | ".") {
                let separator = self.peek().to_owned();
                self.at += 1;
                scope.push(SubroutineScope {
                    name,
                    arguments,
                    separator,
                });
            } else if arguments.is_some() {
                return Err(self.error("expected scope separator after class arguments"));
            } else {
                return Ok((scope, name));
            }
        }
    }

    fn subroutine_direction(&mut self) -> Result<(Option<Direction>, bool, bool)> {
        let constant = self.eat("const");
        let direction = match self.peek() {
            "input" => Some(Direction::Input),
            "output" => Some(Direction::Output),
            "inout" => Some(Direction::Inout),
            "ref" => Some(Direction::Ref),
            _ => None,
        };
        if direction.is_some() {
            self.at += 1;
        }
        if constant && direction != Some(Direction::Ref) {
            return Err(self.error("expected ref after const"));
        }
        let static_reference = direction == Some(Direction::Ref) && self.eat("static");
        Ok((direction, constant, static_reference))
    }

    pub(super) fn subroutine_ports(&mut self, prototype: bool) -> Result<Vec<SubroutinePort>> {
        self.expect("(")?;
        let mut ports = Vec::new();
        if self.eat(")") {
            return Ok(ports);
        }
        loop {
            let start = self.at;
            self.attributes()?;
            let (direction, constant, static_reference) = self.subroutine_direction()?;
            let variable = self.eat("var");
            let unnamed_type = prototype
                && self.token().is_some_and(|token| {
                    matches!(
                        token.kind,
                        TokenKind::Identifier | TokenKind::EscapedIdentifier
                    )
                })
                && matches!(self.nth(1), "," | ")");
            let data_type = if self.starts_type() || unnamed_type {
                self.data_type()?
            } else {
                self.implicit_type()?
            };
            let name = if prototype && matches!(self.peek(), "," | ")" | "=" | "[") {
                None
            } else {
                Some(self.name()?)
            };
            let dimensions = self.dimensions()?;
            let default = if self.eat("=") {
                Some(self.expression(0)?)
            } else {
                None
            };
            ports.push(SubroutinePort {
                name,
                direction,
                constant,
                static_reference,
                variable,
                data_type,
                dimensions,
                default,
                tokens: self.range(start),
            });
            if self.eat(")") {
                break;
            }
            self.expect(",")?;
        }
        Ok(ports)
    }

    fn subroutine_port_declaration(&mut self) -> Result<SubroutinePortDeclaration> {
        let start = self.at;
        self.attributes()?;
        let (direction, constant, static_reference) = self.subroutine_direction()?;
        if direction.is_none() {
            return Err(self.error("expected subroutine port direction"));
        }
        let variable = self.eat("var");
        let data_type = if self.starts_type() {
            self.data_type()?
        } else {
            self.implicit_type()?
        };
        let mut ports = Vec::new();
        loop {
            let start = self.at;
            let name = Some(self.name()?);
            let dimensions = self.dimensions()?;
            let default = if self.eat("=") {
                Some(self.expression(0)?)
            } else {
                None
            };
            ports.push(SubroutinePort {
                name,
                direction,
                constant,
                static_reference,
                variable,
                data_type,
                dimensions,
                default,
                tokens: self.range(start),
            });
            if !self.eat(",") {
                break;
            }
        }
        self.expect(";")?;
        Ok(SubroutinePortDeclaration {
            ports,
            tokens: self.range(start),
        })
    }

    pub(super) fn subroutine(&mut self, prototype: bool) -> Result<SubroutineId> {
        let start = self.at;
        let task = self.eat("task");
        if !task {
            self.expect("function")?;
        }
        let lifetime = if matches!(self.peek(), "static" | "automatic") {
            let value = self.peek().to_owned();
            self.at += 1;
            Some(value)
        } else {
            None
        };
        let implicit_range = self.range(self.at);
        let mut return_type = if !task && self.subroutine_return_type() {
            Some(self.data_type()?)
        } else {
            None
        };
        let (scope, name) = self.subroutine_name()?;
        let kind = if task {
            SubroutineKind::Task
        } else if name.value() == "new" {
            SubroutineKind::Constructor
        } else {
            SubroutineKind::Function
        };
        if kind == SubroutineKind::Function && return_type.is_none() {
            let id = self.implicit_type()?;
            self.types[id.0].tokens = implicit_range;
            return_type = Some(id);
        }
        let ports = if self.peek() == "(" {
            Some(self.subroutine_ports(prototype)?)
        } else {
            None
        };
        self.expect(";")?;
        let body_start = self.at;
        let mut port_declarations = Vec::new();
        let mut items = Vec::new();
        let mut end_label = None;
        let body_tokens;
        if !prototype {
            let end = if task { "endtask" } else { "endfunction" };
            while self.peek() != end {
                if self.peek().is_empty() {
                    return Err(self.error(format!("expected {end}")));
                }
                if self.directive()? {
                    continue;
                }
                // Attributes belong to the following declaration; inspect past
                // them without moving the parser to decide the authored class.
                let mut cursor = self.at;
                while self
                    .indices
                    .get(cursor)
                    .is_some_and(|index| self.tokens[*index].text == "(*")
                {
                    cursor += 1;
                    while self
                        .indices
                        .get(cursor)
                        .is_some_and(|index| self.tokens[*index].text != "*)")
                    {
                        cursor += 1;
                    }
                    if cursor >= self.indices.len() {
                        return Err(self.error("unterminated attribute"));
                    }
                    cursor += 1;
                }
                let word = self
                    .indices
                    .get(cursor)
                    .map(|index| self.tokens[*index].text.as_str())
                    .unwrap_or("");
                let next = self
                    .indices
                    .get(cursor + 1)
                    .map(|index| self.tokens[*index].text.as_str())
                    .unwrap_or("");
                if ports.is_none()
                    && (matches!(word, "input" | "output" | "inout" | "ref")
                        || word == "const" && next == "ref")
                {
                    port_declarations.push(self.subroutine_port_declaration()?);
                } else {
                    items.push(self.statement_machine()?);
                }
            }
            body_tokens = self.range(body_start);
            self.at += 1;
            if self.eat(":") {
                end_label = Some(if kind == SubroutineKind::Constructor {
                    self.contextual_name(&["new"])?
                } else {
                    self.name()?
                });
            }
        } else {
            body_tokens = self.range(body_start);
        }
        let id = SubroutineId(self.subroutines.len());
        if self.subroutines.len() >= 1_000_000 {
            return Err(self.unsupported("native subroutine arena exceeds one million nodes"));
        }
        let authored_statements = self.body_membership(&items);
        self.subroutines.push(Subroutine {
            kind,
            name,
            scope,
            lifetime,
            return_type,
            ports,
            port_declarations,
            items,
            authored_statements,
            prototype,
            end_label,
            body_tokens,
            tokens: self.range(start),
        });
        Ok(id)
    }
}
