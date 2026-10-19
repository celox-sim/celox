//! Body item classification. Types are recognized only at item boundaries,
//! never in the middle of a port/net declaration or instantiation.
use super::*;
impl Parser<'_> {
    pub(super) fn body_data_start(&self) -> bool {
        if matches!(
            self.peek(),
            "defparam"
                | "specparam"
                | "global"
                | "alias"
                | "generate"
                | "endgenerate"
                | "begin"
                | "end"
                | "if"
                | "else"
                | "for"
                | "case"
                | "casex"
                | "casez"
                | "endcase"
                | "class"
                | "endclass"
                | "extern"
                | "pure"
                | "modport"
                | "input"
                | "output"
                | "inout"
                | "ref"
                | "wire"
                | "tri"
                | "tri0"
                | "tri1"
                | "triand"
                | "trior"
                | "trireg"
                | "wand"
                | "wor"
                | "uwire"
                | "supply0"
                | "supply1"
                | "interconnect"
                | "export"
                | "import"
                | "specify"
                | "endspecify"
                | "clocking"
                | "endclocking"
                | "default"
                | "assert"
                | "assume"
                | "cover"
                | "restrict"
                | "property"
                | "sequence"
                | "checker"
                | "covergroup"
                | "bind"
                | "genvar"
                | "let"
                | "nettype"
        ) || (matches!(self.peek(), "virtual" | "interface") && self.nth(1) == "class")
        {
            return false;
        }
        if !self.declaration_start() {
            return false;
        }
        // A named type and an instance share a prefix. A parenthesized instance
        // connection list is an explicit remaining body-item grammar class.
        if let Some(name) = self.declaration_name_cursor() {
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
            if word(at) == "(" {
                return false;
            }
        }
        true
    }
    pub(super) fn body_data_declaration(&mut self) -> Result<DataDeclarationId> {
        let declaration = match self.local_declaration()? {
            LocalDeclaration::Variables(value) => DataDeclaration::Variables(value),
            LocalDeclaration::Constructed(value) => DataDeclaration::Constructed(value),
            LocalDeclaration::Unconverted(range) => DataDeclaration::Unconverted(range),
            _ => unreachable!("body data classification only recognizes variable/typed forms"),
        };
        if self.data_declarations.len() >= 1_000_000 {
            return Err(
                self.unsupported("native data-declaration arena exceeds one million entries")
            );
        }
        let id = DataDeclarationId(self.data_declarations.len());
        self.data_declarations.push(declaration);
        Ok(id)
    }
    /// Check terminal-list boundaries without lowering gate/switch semantics.
    /// IEEE 1800-2023 28.3, Syntax 28-1 has no empty gate terminals.
    pub(super) fn validate_gate_boundary(&self, category: &str, range: TokenRange) -> Result<()> {
        if !matches!(
            category,
            "and"
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
                | "cmos"
                | "rnmos"
                | "rpmos"
                | "rcmos"
                | "tran"
                | "rtran"
                | "tranif0"
                | "tranif1"
                | "rtranif0"
                | "rtranif1"
                | "pullup"
                | "pulldown"
        ) {
            return Ok(());
        }
        let mut delimiters = Vec::new();
        let mut previous = "";
        let mut attributes = false;
        for token in &self.tokens[range.start..range.end] {
            if token.is_trivia() {
                continue;
            }
            let word = token.text.as_str();
            if word == "(*" {
                attributes = true;
                continue;
            }
            if attributes {
                if word == "*)" {
                    attributes = false;
                }
                continue;
            }
            if delimiters.as_slice() == [")"]
                && matches!(word, "," | ")")
                && matches!(previous, "(" | ",")
            {
                return Err(ParseError {
                    kind: ParseErrorKind::MalformedHeader,
                    diagnostic: self
                        .sources
                        .error(token.origin, "expected gate or switch terminal"),
                });
            }
            match word {
                "(" => delimiters.push(")"),
                "[" => delimiters.push("]"),
                "{" => delimiters.push("}"),
                word if delimiters.last().copied() == Some(word) => {
                    delimiters.pop();
                }
                _ => {}
            }
            previous = word;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opaque_gate_terminal_lists_reject_gaps_at_native_boundaries() {
        for gate in [
            "and g(a,,a);",
            "and g(,a);",
            "and g(a,);",
            "and g();",
            "buf g(a,,b);",
            "tran g(a,);",
            "pullup g();",
        ] {
            let source = format!("module M;{gate}endmodule");
            let error = Declarations::parse(&source, Path::new("malformed-gate.sv")).unwrap_err();
            assert_eq!(error.kind, ParseErrorKind::MalformedHeader, "{gate}");
        }
    }
    #[test]
    fn valid_opaque_gates_retain_nested_expression_and_strength_boundaries() {
        for gate in [
            "and g(a,b,c);",
            "and (strong1,pull0) #(1,2) g(y,f(,a),{b,c});",
            "(* keep=f(,a) *) and g(a,b);",
            "pullup (strong1) g(a);",
            "tran g(a,b);",
        ] {
            let source = format!("module M;{gate}endmodule");
            let tree = Declarations::parse(&source, Path::new("opaque-gate.sv")).unwrap();
            assert!(
                matches!(&tree.body_item(tree.units[0].items[0]).kind,BodyItemKind::Unconverted{category,..} if matches!(category.as_str(),"and"|"pullup"|"tran"))
            );
        }
    }
}
