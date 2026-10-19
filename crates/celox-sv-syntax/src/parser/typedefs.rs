use super::*;

impl Parser<'_> {
    pub(super) fn type_alias(&mut self) -> Result<TypeAliasId> {
        let start = self.at;
        self.expect("typedef")?;
        let forward = match self.peek() {
            "enum" => Some((ForwardType::Enum, 1)),
            "struct" => Some((ForwardType::Struct, 1)),
            "union" => Some((ForwardType::Union, 1)),
            "class" => Some((ForwardType::Class, 1)),
            "interface" if self.nth(1) == "class" => Some((ForwardType::InterfaceClass, 2)),
            _ => None,
        };
        let (target, name, dimensions) = if let Some((kind, words)) = forward
            && self.nth(words + 1) == ";"
        {
            self.at += words;
            (
                TypeAliasTarget::Forward(Some(kind)),
                self.name()?,
                Vec::new(),
            )
        } else if self.nth(1) == ";"
            && forward.is_none()
            && !super::expressions::builtin(self.peek())
            && !matches!(
                self.peek(),
                "interface" | "virtual" | "signed" | "unsigned" | "type"
            )
        {
            (TypeAliasTarget::Forward(None), self.name()?, Vec::new())
        } else {
            let target = if self.interface_member_alias() {
                TypeAliasTarget::InterfaceMember(self.expression(0)?)
            } else {
                TypeAliasTarget::DataType(self.data_type()?)
            };
            let name = self.name()?;
            let dimensions = self.dimensions()?;
            (target, name, dimensions)
        };
        self.expect(";")?;
        let id = TypeAliasId(self.type_aliases.len());
        self.type_aliases.push(TypeAlias {
            name,
            target,
            dimensions,
            tokens: self.range(start),
        });
        Ok(id)
    }

    // An interface-instance typedef has a top-level member dot. Dots inside
    // type(expr), aggregate fields and generic arguments belong to that type.
    fn interface_member_alias(&self) -> bool {
        if matches!(self.peek(), "virtual" | "interface") {
            return false;
        }
        let mut delimiters = Vec::new();
        for index in &self.indices[self.at..] {
            match self.tokens[*index].text.as_str() {
                "." if delimiters.is_empty() => return true,
                ";" if delimiters.is_empty() => return false,
                "(" => delimiters.push(")"),
                "[" => delimiters.push("]"),
                "{" => delimiters.push("}"),
                word if delimiters.last().copied() == Some(word) => {
                    delimiters.pop();
                }
                _ => {}
            }
        }
        false
    }
}
