use super::*;

impl Parser<'_> {
    pub(super) fn dpi_start(&self) -> bool {
        matches!(self.peek(), "import" | "export")
            && self
                .indices
                .get(self.at + 1)
                .is_some_and(|index| self.tokens[*index].kind == TokenKind::String)
    }

    pub(super) fn dpi_declaration(&mut self) -> Result<DpiId> {
        let start = self.at;
        let import = self.eat("import");
        if !import {
            self.expect("export")?;
        }
        let Some(specifier) = self.token().filter(|token| token.kind == TokenKind::String) else {
            return Err(self.error("expected a DPI specifier string"));
        };
        if !matches!(specifier.text.as_str(), "\"DPI-C\"" | "\"DPI\"") {
            return Err(self.error("expected `\"DPI-C\"` or `\"DPI\"`"));
        }
        let specifier = specifier.text.clone();
        self.at += 1;
        let property = if self.eat("pure") {
            Some(DpiProperty::Pure)
        } else if self.eat("context") {
            Some(DpiProperty::Context)
        } else {
            None
        };
        if !import && property.is_some() {
            return Err(self.error("DPI export cannot have an import property"));
        }
        let c_name = if self.nth(1) == "=" {
            // C linkage identifiers exclude SV escaped identifiers and `$`.
            if !self.peek().bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_alphabetic() || byte == b'_' || (index > 0 && byte.is_ascii_digit())
            }) {
                return Err(self.error("expected a C linkage identifier"));
            }
            let name = self.name()?;
            self.expect("=")?;
            Some(name)
        } else {
            None
        };
        let kind = if self.eat("function") {
            DpiKind::Function
        } else {
            self.expect("task")?;
            DpiKind::Task
        };
        if kind == DpiKind::Task && property == Some(DpiProperty::Pure) {
            return Err(self.error("DPI task import cannot be pure"));
        }
        let return_type = if import && kind == DpiKind::Function {
            Some(self.dpi_data_type(true)?)
        } else {
            None
        };
        let name = self.name()?;
        let mut ports = Vec::new();
        if import && self.eat("(") && !self.eat(")") {
            loop {
                let start = self.at;
                self.attributes()?;
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
                    return Err(self.error("expected `ref` after `const`"));
                }
                let variable = self.eat("var");
                // A bare identifier in a prototype can denote an unnamed
                // named datatype; retain the existing grammar's choice. An
                // implicit datatype with signing/dimensions is unambiguous.
                let named_type = self.token().is_some_and(|token| {
                    matches!(
                        token.kind,
                        TokenKind::Identifier | TokenKind::EscapedIdentifier
                    )
                }) && matches!(self.nth(1), "," | ")");
                let data_type = if self.starts_type() || named_type {
                    self.dpi_data_type(false)?
                } else {
                    self.implicit_type()?
                };
                let name = if matches!(self.peek(), "," | ")" | "=" | "[") {
                    None
                } else {
                    Some(self.name()?)
                };
                if name.is_none() && matches!(self.peek(), "[" | "=") {
                    return Err(self.error("DPI port dimensions and defaults require a port name"));
                }
                let dimensions = self.dimensions()?;
                let default = if self.eat("=") {
                    Some(self.expression(0)?)
                } else {
                    None
                };
                ports.push(DpiPort {
                    name,
                    direction,
                    constant,
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
        }
        self.expect(";")?;
        if self.dpi_declarations.len() >= 1_000_000 {
            return Err(self.unsupported("native DPI arena exceeds one million declarations"));
        }
        let id = DpiId(self.dpi_declarations.len());
        self.dpi_declarations.push(DpiDeclaration {
            import,
            specifier,
            property,
            c_name,
            kind,
            return_type,
            name,
            ports,
            tokens: self.range(start),
        });
        Ok(id)
    }

    fn dpi_data_type(&mut self, result: bool) -> Result<TypeId> {
        let id = self.data_type()?;
        if !result
            && matches!(&self.types[id.0].kind, DataTypeKind::Builtin(name) if name == "void")
        {
            return Err(self.error("void is not a DPI port datatype"));
        }
        Ok(id)
    }
}
