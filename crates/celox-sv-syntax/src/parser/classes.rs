//! Class header grammar; class execution and opaque body validation are separate.
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
        while matches!(
            self.peek(),
            "static" | "protected" | "local" | "rand" | "randc"
        ) {
            self.at += 1;
        }
        let qualified_const =
            self.peek() == "const" && matches!(self.nth(1), "protected" | "local" | "static");
        let mut special_const = false;
        if qualified_const {
            self.at += 1;
            while matches!(self.peek(), "static" | "protected" | "local") {
                special_const |= matches!(self.peek(), "protected" | "local");
                self.at += 1;
            }
        }
        let declaration = self.local_declaration()?;
        if special_const && let LocalDeclaration::Variables(value) = declaration {
            if value.variables.len() != 1 || !value.variables[0].dimensions.is_empty() {
                return Err(
                    self.error("qualified const class property requires one scalar declarator")
                );
            }
            if let Some(value) = value.variables[0].value {
                self.validate_constant_operators(value)?;
            }
        }
        Ok(())
    }

    pub(super) fn class_header(&mut self) -> Result<Vec<BodyItemId>> {
        let mut items = Vec::new();
        let interface = self.at > 0 && self.tokens[self.indices[self.at - 1]].text == "interface";
        self.expect("class")?;
        // Preserve the original parser's accepted pre-2023 class lifetime form.
        // IEEE 1800-2023 A.1.2 instead introduces a final specifier here.
        if !interface && matches!(self.peek(), "static" | "automatic") {
            self.at += 1;
        }
        self.name()?;
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
            self.class_base(interface)?;
            if interface {
                while self.eat(",") {
                    self.class_base(true)?;
                }
            } else if self.peek() == "(" {
                self.expression_machine(super::expressions::Job::Arguments)?;
            }
        }
        if !interface && self.eat("implements") {
            self.class_base(true)?;
            while self.eat(",") {
                self.class_base(true)?;
            }
        }
        self.expect(";")?;
        Ok(items)
    }

    fn class_base(&mut self, interface: bool) -> Result<()> {
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
        Ok(())
    }
}
