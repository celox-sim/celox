//! Ordinary net declarations share expression/type/dimension grammar with the
//! other authored declarations. User net types require semantic name binding;
//! they remain the existing named declaration form.
use super::*;
impl Parser<'_> {
    pub(super) fn net_start(&self) -> bool {
        matches!(
            self.peek(),
            "wire"
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
        )
    }
    pub(super) fn net_declaration(&mut self) -> Result<NetDeclarationId> {
        let start = self.at;
        let net_type = self.peek().to_owned();
        self.at += 1;
        let strength = if self.peek() == "(" && matches!(self.nth(1), "small" | "medium" | "large")
        {
            let start = self.at;
            self.at += 1;
            let name = self.peek().to_owned();
            self.at += 1;
            self.expect(")")?;
            Some(NetStrength::Charge {
                name,
                tokens: self.range(start),
            })
        } else {
            self.drive_strength()?.map(NetStrength::Drive)
        };
        let vector = if matches!(self.peek(), "vectored" | "scalared") {
            let value = self.peek().to_owned();
            self.at += 1;
            Some(value)
        } else {
            None
        };
        let data_type = if self.starts_type() {
            self.data_type()?
        } else {
            self.implicit_type()?
        };
        let delay = self.assignment_delay()?;
        let mut variables = Vec::new();
        loop {
            let start = self.at;
            let name = self.name()?;
            let dimensions = self.dimensions()?;
            self.validate_unpacked_dimensions(&dimensions)?;
            let value = if self.eat("=") {
                Some(self.expression(0)?)
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
        if self.net_declarations.len() >= 1_000_000 {
            return Err(
                self.unsupported("native net-declaration arena exceeds one million entries")
            );
        }
        let id = NetDeclarationId(self.net_declarations.len());
        self.net_declarations.push(NetDeclaration {
            net_type,
            strength,
            vector,
            data_type,
            delay,
            variables,
            tokens: self.range(start),
        });
        Ok(id)
    }
}
