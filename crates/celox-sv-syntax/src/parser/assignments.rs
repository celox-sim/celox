use super::*;

fn strength(word: &str) -> bool {
    matches!(
        word,
        "supply0"
            | "supply1"
            | "strong0"
            | "strong1"
            | "pull0"
            | "pull1"
            | "weak0"
            | "weak1"
            | "highz0"
            | "highz1"
    )
}

impl Parser<'_> {
    pub(super) fn assign_statement(&mut self) -> Result<AssignId> {
        self.assign_statement_with("assign")
    }

    /// An assignment statement that starts with `keyword`: `assign`, or
    /// `force` (IEEE 1800-2023 A.6.2), whose binder rejects a strength, a
    /// delay and a list as it does for a procedural `assign`.
    pub(super) fn assign_statement_with(&mut self, keyword: &str) -> Result<AssignId> {
        let start = self.at;
        self.expect(keyword)?;
        let drive = self.drive_strength()?;
        let delay = self.assignment_delay()?;
        let mut assignments = Vec::new();
        loop {
            let start = self.at;
            let left = self.expression(0)?;
            if !self.variable_lvalue(left) {
                return Err(self.error("expected assignment lvalue"));
            }
            self.expect("=")?;
            let right = self.expression(0)?;
            assignments.push(AssignmentSyntax {
                left,
                right,
                tokens: self.range(start),
            });
            if self.eat(";") {
                break;
            }
            self.expect(",")?;
        }
        let id = AssignId(self.assign_statements.len());
        self.assign_statements.push(AssignSyntax {
            strength: drive,
            delay,
            assignments,
            tokens: self.range(start),
        });
        Ok(id)
    }
    /// `defparam a.b[1].P = value, ... ;` (IEEE 1800-2023 A.1.4, A.2.5).
    pub(super) fn defparam(&mut self) -> Result<Vec<DefparamAssignment>> {
        self.expect("defparam")?;
        let mut assignments = Vec::new();
        loop {
            let start = self.at;
            let rooted = self.peek() == "$root";
            if rooted {
                self.at += 1;
                self.expect(".")?;
            }
            let mut path = Vec::new();
            loop {
                let name = self.name()?;
                let index = if self.eat("[") {
                    let index = self.expression(0)?;
                    self.expect("]")?;
                    Some(index)
                } else {
                    None
                };
                path.push(DefparamSegment { name, index });
                if !self.eat(".") {
                    break;
                }
            }
            self.expect("=")?;
            let value = self.parameter_expression()?;
            assignments.push(DefparamAssignment {
                rooted,
                path,
                value,
                tokens: self.range(start),
            });
            if self.eat(";") {
                break;
            }
            self.expect(",")?;
        }
        Ok(assignments)
    }
    pub(super) fn drive_strength(&mut self) -> Result<Option<DriveStrength>> {
        Ok(if self.peek() == "(" && strength(self.nth(1)) {
            let start = self.at;
            self.at += 1;
            let first = self.peek().to_owned();
            self.at += 1;
            self.expect(",")?;
            if !strength(self.peek()) {
                return Err(self.error("expected drive strength"));
            }
            let second = self.peek().to_owned();
            self.at += 1;
            self.expect(")")?;
            Some(DriveStrength {
                first,
                second,
                tokens: self.range(start),
            })
        } else {
            None
        })
    }
    pub(super) fn assignment_delay(&mut self) -> Result<Option<AssignmentDelay>> {
        Ok(if self.peek() == "#" {
            let start = self.at;
            self.at += 1;
            let parenthesized = self.eat("(");
            let mut values = Vec::new();
            if parenthesized {
                loop {
                    values.push(self.parameter_expression()?);
                    if self.eat(")") {
                        break;
                    }
                    if values.len() == 3 {
                        return Err(self.error("assignment delay has more than three values"));
                    }
                    self.expect(",")?;
                }
            } else {
                values.push(self.expression(14)?);
            }
            Some(AssignmentDelay {
                values,
                parenthesized,
                tokens: self.range(start),
            })
        } else {
            None
        })
    }
}
