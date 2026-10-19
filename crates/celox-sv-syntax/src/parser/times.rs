use super::*;

impl Parser<'_> {
    pub(super) fn time_value(&mut self) -> Result<TimeValue> {
        let start = self.at;
        let token = self
            .token()
            .filter(|token| token.kind == TokenKind::Number)
            .ok_or_else(|| self.error("expected a time literal"))?;
        let text = token.text.clone();
        self.at += 1;
        // A unit may be part of the numeric token or a separate token after
        // trivia/macro expansion. Preserve both token origins in the range.
        let (magnitude, unit) = if let Some(unit) = ["ms", "us", "ns", "ps", "fs", "s"]
            .into_iter()
            .find(|unit| text.ends_with(unit))
        {
            (text[..text.len() - unit.len()].to_owned(), unit.to_owned())
        } else {
            if !matches!(self.peek(), "s" | "ms" | "us" | "ns" | "ps" | "fs") {
                return Err(self.error("expected a time unit suffix"));
            }
            let unit = self.peek().to_owned();
            self.at += 1;
            (text, unit)
        };
        Ok(TimeValue {
            magnitude,
            unit,
            tokens: self.range(start),
        })
    }

    pub(super) fn time_declaration(&mut self) -> Result<TimeDeclarationId> {
        let start = self.at;
        let (unit, precision) = if self.eat("timeunit") {
            let unit = self.time_value()?;
            let precision = if self.eat("/") {
                Some(self.time_value()?)
            } else {
                None
            };
            (Some(unit), precision)
        } else {
            self.expect("timeprecision")?;
            (None, Some(self.time_value()?))
        };
        self.expect(";")?;
        let id = TimeDeclarationId(self.time_declarations.len());
        self.time_declarations.push(TimeDeclaration {
            unit,
            precision,
            tokens: self.range(start),
        });
        Ok(id)
    }
}
