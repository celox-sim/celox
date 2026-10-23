//! Class grammar: the header, properties and method qualifiers are recorded
//! as a [`ClassDeclaration`]; the body also remains an unconverted item.
use super::*;

impl Parser<'_> {
    pub(super) fn class_property_start(&self) -> bool {
        let mut cursor = self.at;
        while self.indices.get(cursor).is_some_and(|index| {
            matches!(
                self.tokens[*index].text.as_str(),
                "static" | "protected" | "local" | "rand" | "randc"
            )
        }) {
            cursor += 1;
        }
        let Some(index) = self.indices.get(cursor) else {
            return false;
        };
        let token = &self.tokens[*index];
        let next = self
            .indices
            .get(cursor + 1)
            .map(|index| self.tokens[*index].text.as_str())
            .unwrap_or("");
        if token.text == "virtual" && matches!(next, "function" | "task" | "class") {
            return false;
        }
        super::expressions::builtin(&token.text)
            || matches!(
                token.text.as_str(),
                "const"
                    | "var"
                    | "automatic"
                    | "struct"
                    | "union"
                    | "enum"
                    | "virtual"
                    | "interface"
                    | "signed"
                    | "unsigned"
            )
            || token.text == "type" && next == "("
            || matches!(
                token.kind,
                TokenKind::Identifier | TokenKind::EscapedIdentifier
            ) && !self.reserved_identifier(token)
    }

    pub(super) fn class_property(&mut self) -> Result<()> {
        let mut qualifiers = ClassQualifiers::default();
        while self.class_qualifier(&mut qualifiers) {}
        let qualified_const =
            self.peek() == "const" && matches!(self.nth(1), "protected" | "local" | "static");
        let mut special_const = false;
        if qualified_const {
            self.at += 1;
            while matches!(self.peek(), "static" | "protected" | "local") {
                special_const |= matches!(self.peek(), "protected" | "local");
                self.class_qualifier(&mut qualifiers);
            }
        }
        let declaration = self.local_declaration()?;
        if special_const && let LocalDeclaration::Variables(value) = &declaration {
            if value.variables.len() != 1 || !value.variables[0].dimensions.is_empty() {
                return Err(
                    self.error("qualified const class property requires one scalar declarator")
                );
            }
            if let Some(value) = value.variables[0].value {
                self.validate_constant_operators(value)?;
            }
        }
        if let (Some(class), LocalDeclaration::Variables(mut declaration)) =
            (self.class_stack.last_mut(), declaration)
        {
            declaration.constant |= qualified_const;
            class.properties.push(ClassProperty {
                qualifiers,
                declaration,
            });
        }
        Ok(())
    }

    /// Consume one property or method qualifier, recording it.
    fn class_qualifier(&mut self, qualifiers: &mut ClassQualifiers) -> bool {
        let flag = match self.peek() {
            "static" => &mut qualifiers.is_static,
            "virtual" => &mut qualifiers.is_virtual,
            "pure" => &mut qualifiers.is_pure,
            "extern" => &mut qualifiers.is_extern,
            "local" => &mut qualifiers.local,
            "protected" => &mut qualifiers.protected,
            "rand" => &mut qualifiers.rand,
            "randc" => &mut qualifiers.randc,
            _ => return false,
        };
        *flag = true;
        self.at += 1;
        true
    }

    /// The qualifiers written before the method keyword at `keyword`.
    fn method_qualifiers(&self, keyword: usize) -> ClassQualifiers {
        let mut qualifiers = ClassQualifiers::default();
        let mut at = keyword;
        while at > 0 {
            at -= 1;
            let flag = match self.tokens[self.indices[at]].text.as_str() {
                "static" => &mut qualifiers.is_static,
                "virtual" => &mut qualifiers.is_virtual,
                "pure" => &mut qualifiers.is_pure,
                "extern" => &mut qualifiers.is_extern,
                "local" => &mut qualifiers.local,
                "protected" => &mut qualifiers.protected,
                _ => break,
            };
            *flag = true;
        }
        qualifiers
    }

    /// Record subroutine `id`, which starts at token `keyword`, as a method
    /// of the innermost open class.
    pub(super) fn class_method(&mut self, keyword: usize, id: SubroutineId) {
        let qualifiers = self.method_qualifiers(keyword);
        if let Some(class) = self.class_stack.last_mut() {
            class.methods.push(ClassMethod {
                qualifiers,
                subroutine: id,
            });
        }
    }

    /// Complete the innermost open class at its `endclass`.
    pub(super) fn finish_class(&mut self) {
        if let Some(mut class) = self.class_stack.pop() {
            class.tokens = TokenRange {
                start: class.tokens.start,
                end: self.indices[self.at - 1] + 1,
            };
            self.classes.push(class);
        }
    }

    pub(super) fn class_header(&mut self) -> Result<Vec<BodyItemId>> {
        let mut items = Vec::new();
        let interface = self.at > 0 && self.tokens[self.indices[self.at - 1]].text == "interface";
        let virtual_class = self.at > 0 && self.tokens[self.indices[self.at - 1]].text == "virtual";
        let start = self.indices[self.at];
        self.expect("class")?;
        // Preserve the original parser's accepted pre-2023 class lifetime form.
        // IEEE 1800-2023 A.1.2 instead introduces a final specifier here.
        if !interface && matches!(self.peek(), "static" | "automatic") {
            self.at += 1;
        }
        let name = self.name()?;
        let mut base = None;
        let mut interfaces = Vec::new();
        if self.eat("#") {
            let start = self.at;
            self.expect("(")?;
            let parameters = self.parameters(")")?;
            let id = ParameterDeclarationId(self.parameter_declarations.len());
            self.parameter_declarations.push(ParameterDeclaration {
                parameters,
                tokens: self.range(start),
            });
            items.push(super::generate::Builder::node(
                self,
                start,
                BodyItemKind::Parameter(id),
            )?);
        }
        if self.eat("extends") {
            let data_type = self.class_base(interface)?;
            if interface {
                interfaces.push(data_type);
                while self.eat(",") {
                    interfaces.push(self.class_base(true)?);
                }
            } else {
                let arguments = if self.peek() == "(" {
                    match self.expression_machine(super::expressions::Job::Arguments)? {
                        super::expressions::Value::Arguments(arguments) => Some(arguments),
                        _ => return Err(self.error("class base constructor arguments")),
                    }
                } else {
                    None
                };
                base = Some(ClassBase {
                    data_type,
                    arguments,
                });
            }
        }
        if !interface && self.eat("implements") {
            interfaces.push(self.class_base(true)?);
            while self.eat(",") {
                interfaces.push(self.class_base(true)?);
            }
        }
        self.expect(";")?;
        self.class_stack.push(ClassDeclaration {
            name,
            virtual_class,
            interface,
            base,
            interfaces,
            has_constraints: false,
            properties: Vec::new(),
            methods: Vec::new(),
            tokens: TokenRange { start, end: start },
        });
        Ok(items)
    }

    fn class_base(&mut self, interface: bool) -> Result<TypeId> {
        let id = self.data_type()?;
        let datatype = &self.types[id.0];
        let DataTypeKind::Named(path) = &datatype.kind else {
            return Err(self.error("class inheritance requires a class type"));
        };
        if !datatype.packed_dimensions.is_empty()
            || interface
                && path
                    .iter()
                    .take(path.len().saturating_sub(1))
                    .any(|segment| segment.arguments.is_some())
        {
            return Err(self.error("invalid class inheritance type"));
        }
        Ok(id)
    }
}
