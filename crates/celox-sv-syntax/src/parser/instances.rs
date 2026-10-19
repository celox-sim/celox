//! Authored instance grammar; declaration lookahead only determines the item
//! boundary. Parameter and connection values reuse the flat expression parser.
use super::*;
impl Parser<'_> {
    pub(super) fn instantiation_start(&self) -> bool {
        if matches!(
            self.peek(),
            "assert"
                | "assume"
                | "cover"
                | "restrict"
                | "modport"
                | "bind"
                | "let"
                | "checker"
                | "covergroup"
                | "class"
                | "property"
                | "sequence"
                | "clocking"
                | "input"
                | "output"
                | "inout"
                | "ref"
                | "nettype"
                | "and"
                | "nand"
                | "or"
                | "nor"
                | "xor"
                | "xnor"
                | "buf"
                | "not"
                | "bufif0"
                | "bufif1"
                | "notif0"
                | "notif1"
                | "nmos"
                | "pmos"
                | "rnmos"
                | "rpmos"
                | "cmos"
                | "rcmos"
                | "tran"
                | "rtran"
                | "tranif0"
                | "tranif1"
                | "rtranif0"
                | "rtranif1"
                | "pullup"
                | "pulldown"
        ) || self.net_start()
            || super::expressions::builtin(self.peek())
        {
            return false;
        }
        let Some(name) = self.declaration_name_cursor() else {
            return false;
        };
        let word = |at: usize| {
            self.indices
                .get(at)
                .map_or("", |index| self.tokens[*index].text.as_str())
        };
        let mut at = name + 1;
        while word(at) == "[" {
            let mut depth = 1usize;
            at += 1;
            while depth != 0 {
                match word(at) {
                    "[" => depth += 1,
                    "]" => depth -= 1,
                    "" => return false,
                    _ => {}
                }
                at += 1;
            }
        }
        word(at) == "("
    }
    pub(super) fn instantiation(&mut self) -> Result<InstantiationId> {
        let start = self.at;
        let name = self.name()?;
        let parameters = if self.eat("#") {
            self.expect("(")?;
            let mut parameters = Vec::new();
            if !self.eat(")") {
                let named = self.peek() == ".";
                loop {
                    let name = if named {
                        self.expect(".")?;
                        Some(self.name()?)
                    } else {
                        None
                    };
                    let value = if named {
                        self.expect("(")?;
                        if self.eat(")") {
                            None
                        } else {
                            let value = self.parameter_expression()?;
                            self.expect(")")?;
                            Some(value)
                        }
                    } else {
                        Some(self.parameter_expression()?)
                    };
                    parameters.push(Argument { name, value });
                    if self.eat(")") {
                        break;
                    }
                    self.expect(",")?;
                }
            }
            Some(parameters)
        } else {
            None
        };
        let mut instances = Vec::new();
        loop {
            let start = self.at;
            let name = self.name()?;
            let dimensions = self.dimensions()?;
            self.validate_unpacked_dimensions(&dimensions)?;
            let connections = self.instance_connections()?;
            instances.push(InstanceDeclarator {
                name,
                dimensions,
                connections,
                tokens: self.range(start),
            });
            if !self.eat(",") {
                break;
            }
        }
        self.expect(";")?;
        if self.instantiations.len() >= 1_000_000 {
            return Err(self.unsupported("native instantiation arena exceeds one million entries"));
        }
        let id = InstantiationId(self.instantiations.len());
        self.instantiations.push(Instantiation {
            name,
            parameters,
            instances,
            tokens: self.range(start),
        });
        Ok(id)
    }
    fn instance_connections(&mut self) -> Result<Vec<InstanceConnection>> {
        self.expect("(")?;
        if self.eat(")") {
            return Ok(Vec::new());
        }
        let mut connections = Vec::new();
        let mut named = None;
        loop {
            let start = self.at;
            self.attributes()?;
            let is_named = matches!(self.peek(), "." | ".*");
            if named.is_some_and(|named| named != is_named) {
                return Err(self.error("mixed named and ordered port connections"));
            }
            named = Some(is_named);
            let kind = if self.eat(".*") {
                InstanceConnectionKind::Wildcard
            } else if self.eat(".") {
                let name = self.name()?;
                let value = if self.eat("(") {
                    if self.eat(")") {
                        ConnectionValue::Empty
                    } else {
                        let expression = self.expression(0)?;
                        self.expect(")")?;
                        ConnectionValue::Expression(expression)
                    }
                } else {
                    ConnectionValue::Implicit
                };
                InstanceConnectionKind::Named { name, value }
            } else {
                InstanceConnectionKind::Ordered(if matches!(self.peek(), "," | ")") {
                    None
                } else {
                    Some(self.expression(0)?)
                })
            };
            connections.push(InstanceConnection {
                kind,
                tokens: self.range(start),
            });
            if self.eat(")") {
                break;
            }
            self.expect(",")?;
        }
        Ok(connections)
    }
}
