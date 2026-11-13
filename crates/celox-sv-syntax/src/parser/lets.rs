//! `let` declarations (IEEE 1800-2023 11.12).
//!
//! The parser records each declaration with its formals and body; its calls
//! stay ordinary call and name expressions, which the analyzer expands where
//! it resolves them.

use super::*;

impl Parser<'_> {
    /// Parse the `let` declaration whose tokens, from `start`, the body item
    /// grammar has just passed: `let name [(ports)] = expression;`.
    pub(super) fn record_let(&mut self, start: usize) -> Result<()> {
        let resume = self.at;
        self.at = start;
        // The item's range starts at its attributes, if any.
        self.attributes()?;
        self.expect("let")?;
        let name = self.name()?;
        let mut ports = Vec::new();
        if self.eat("(") && !self.eat(")") {
            loop {
                self.attributes()?;
                let data_type = if self.eat("untyped") {
                    None
                } else if self.starts_type() && !matches!(self.nth(1), "," | ")" | "=") {
                    Some(self.data_type()?)
                } else {
                    None
                };
                let name = self.name()?;
                if self.peek() == "[" {
                    return Err(self.unsupported_at(
                        self.range(start),
                        "`let` formal argument with unpacked dimensions",
                    ));
                }
                let default = if self.eat("=") {
                    Some(self.expression(0)?)
                } else {
                    None
                };
                ports.push(LetPort {
                    name,
                    data_type,
                    default,
                });
                if self.eat(")") {
                    break;
                }
                self.expect(",")?;
            }
        }
        self.expect("=")?;
        let body = self.expression(0)?;
        self.expect(";")?;
        self.lets.push(LetDeclaration {
            name,
            ports,
            body,
            tokens: self.range(start),
        });
        self.at = resume;
        Ok(())
    }
}
