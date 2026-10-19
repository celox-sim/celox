//! Covergroup headers and coverage-item prefixes. Bin/transition/cross bodies
//! remain explicitly opaque to the enclosing declaration scanner.
use super::*;

impl Parser<'_> {
    pub(super) fn coverage_header(&mut self) -> Result<()> {
        self.expect("covergroup")?;
        self.name()?;
        if self.peek() == "(" {
            self.subroutine_ports(true)?;
        }
        if self.eat("with") {
            self.expect("function")?;
            self.expect("sample")?;
            self.subroutine_ports(true)?;
        } else if self.eat("@@") {
            self.expect("(")?;
            loop {
                if !self.eat("begin") {
                    self.expect("end")?;
                }
                self.coverage_reference()?;
                if !self.eat("or") {
                    break;
                }
            }
            self.expect(")")?;
        } else if self.peek() == "@" {
            if self.nth(1) == "(" || self.nth(1) == "(*" {
                let id = self.typed_timing_control()?;
                if matches!(
                    self.timing_controls[id.0].kind,
                    TimingControlKind::Event(EventControl::Wildcard)
                ) {
                    return Err(self.error("coverage event cannot be a wildcard event"));
                }
            } else {
                self.at += 1;
                self.coverage_reference()?;
            }
        }
        self.expect(";")
    }

    // Returns true after consuming a body's opening brace. The original
    // scanner keeps ownership of its opaque bin/cross payload and nested items.
    pub(super) fn coverage_item_prefix(&mut self) -> Result<bool> {
        self.attributes()?;
        if matches!(self.peek(), "option" | "type_option") && self.nth(1) == "." {
            let constant = self.eat("type_option");
            if !constant {
                self.expect("option")?;
            }
            self.expect(".")?;
            self.name()?;
            self.expect("=")?;
            let value = self.expression(0)?;
            if constant {
                self.validate_constant_operators(value)?;
            }
            self.expect(";")?;
            return Ok(false);
        }
        let cross = self.peek() == "cross" || self.nth(1) == ":" && self.nth(2) == "cross";
        if cross {
            if self.peek() != "cross" {
                self.name()?;
                self.expect(":")?;
            }
            self.expect("cross")?;
            self.name()?;
            self.expect(",")?;
            self.name()?;
            while self.eat(",") {
                self.name()?;
            }
        } else {
            if self.peek() != "coverpoint" {
                if self.nth(1) != ":" {
                    self.data_type()?;
                }
                self.name()?;
                self.expect(":")?;
            }
            self.expect("coverpoint")?;
            self.expression(0)?;
        }
        if self.eat("iff") {
            self.expect("(")?;
            self.expression(0)?;
            self.expect(")")?;
        }
        if self.eat(";") {
            return Ok(false);
        }
        self.expect("{")?;
        Ok(true)
    }

    fn coverage_reference(&mut self) -> Result<()> {
        let mut id = self.expression(14)?;
        loop {
            match &self.expressions[id.0].kind {
                ExprKind::Name(name) if !self.reserved_word(&name.spelling) => return Ok(()),
                ExprKind::Member { expression, .. }
                | ExprKind::Select { expression, .. }
                | ExprKind::Parameterized { expression, .. } => id = *expression,
                _ => return Err(self.error("expected a coverage event identifier")),
            }
        }
    }
}
