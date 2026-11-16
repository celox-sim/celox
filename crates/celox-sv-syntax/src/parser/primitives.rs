//! User-defined primitives (IEEE 1800-2023 29).
//!
//! A UDP declaration keeps its ports, output kind, initial value and state
//! table. Its instances share the grammar of n-input gate instances (29.8):
//! a UDP name, optional drive strength and delay, and instances whose
//! names are optional, so the parser reads them as [`LogicGate`]s whose
//! kind is the UDP's name. The names of the UDPs a source declares are
//! known before any item is parsed, since an instance may precede the
//! declaration it instantiates.
use super::*;

impl Parser<'_> {
    /// The names of the UDPs the source declares.
    pub(super) fn primitive_names(&self) -> Vec<String> {
        let word = |at: usize| {
            self.indices
                .get(at)
                .map_or("", |index| self.tokens[*index].text.as_str())
        };
        let mut names = Vec::new();
        for at in 0..self.indices.len() {
            if word(at) == "primitive" && !matches!(word(at + 1), "" | "(" | ";") {
                let name = word(at + 1);
                let name = name.strip_prefix('\\').unwrap_or(name).to_string();
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        names
    }

    /// Whether the cursor starts the instantiation of a UDP of the source:
    /// its name, then a drive strength, delay or terminal list, or the
    /// name of the instance.
    pub(super) fn primitive_instance_start(&self) -> bool {
        let Some(token) = self.token() else {
            return false;
        };
        let word = token.text.strip_prefix('\\').unwrap_or(&token.text);
        matches!(
            token.kind,
            TokenKind::Identifier | TokenKind::EscapedIdentifier
        ) && self.primitive_names.iter().any(|name| name == word)
            && (matches!(self.nth(1), "(" | "#") || self.nth(2) == "(")
    }

    /// The UDP declaration at the cursor (Syntax 29-1).
    pub(super) fn primitive(&mut self) -> Result<Primitive> {
        let start = self.at;
        self.expect("primitive")?;
        let name = self.name()?;
        self.expect("(")?;
        let mut ports = Vec::new();
        let mut output_reg = false;
        let mut initial = None;
        let mut directions: Vec<(String, &'static str)> = Vec::new();
        // An ANSI header declares the ports in its list (29.3.2).
        let ansi = matches!(self.peek(), "output" | "(*");
        loop {
            self.attributes()?;
            if ansi {
                let direction = match self.peek() {
                    "output" => "output",
                    "input" => "input",
                    _ => directions.last().map_or("", |(_, direction)| *direction),
                };
                if matches!(self.peek(), "output" | "input") {
                    self.at += 1;
                }
                if direction == "output" && self.eat("reg") {
                    output_reg = true;
                }
                let port = self.name()?;
                if direction == "output" && self.eat("=") {
                    initial = Some(self.expression(0)?);
                }
                directions.push((port.value().to_string(), direction));
                ports.push(port);
            } else {
                ports.push(self.name()?);
            }
            if self.eat(")") {
                break;
            }
            self.expect(",")?;
        }
        self.expect(";")?;
        // The port declarations of a non-ANSI header (29.3.2).
        while !ansi && matches!(self.peek(), "output" | "input" | "reg" | "(*") {
            self.attributes()?;
            let direction = match self.peek() {
                "output" => "output",
                "input" => "input",
                _ => "reg",
            };
            self.at += 1;
            if direction == "output" && self.eat("reg") {
                output_reg = true;
            }
            loop {
                let port = self.name()?;
                if direction == "reg" {
                    output_reg = true;
                } else if direction == "output" && self.eat("=") {
                    initial = Some(self.expression(0)?);
                }
                directions.push((port.value().to_string(), direction));
                if !self.eat(",") {
                    break;
                }
            }
            self.expect(";")?;
        }
        let Some(output) = ports.first() else {
            return Err(self.error("a UDP has an output port"));
        };
        if !directions
            .iter()
            .any(|(port, direction)| port == output.value() && *direction == "output")
            || ports.iter().skip(1).any(|port| {
                !directions
                    .iter()
                    .any(|(other, direction)| other == port.value() && *direction == "input")
            })
        {
            return Err(self.error(format!(
                "the first port of UDP `{}` is its output and the others are its inputs",
                name.value()
            )));
        }
        // The initial statement of a sequential UDP (29.5).
        if self.eat("initial") {
            let target = self.name()?;
            if target.value() != output.value() {
                return Err(self.error("a UDP initial statement assigns the output"));
            }
            self.expect("=")?;
            initial = Some(self.expression(0)?);
            self.expect(";")?;
        }
        self.expect("table")?;
        let mut rows = Vec::new();
        while !self.eat("endtable") {
            rows.push(self.primitive_row(ports.len() - 1, output_reg)?);
        }
        self.expect("endprimitive")?;
        let end_label = if self.eat(":") {
            let label = self.name()?;
            if label.value() != name.value() {
                return Err(self.error(format!(
                    "end label `{}` does not match UDP `{}`",
                    label.value(),
                    name.value()
                )));
            }
            Some(label)
        } else {
            None
        };
        Ok(Primitive {
            name,
            ports,
            sequential: output_reg,
            initial,
            rows,
            end_label,
            tokens: self.range(start),
        })
    }

    /// One entry of a UDP state table (Syntax 29-1): its input fields, then
    /// the current state of a sequential UDP, then the output. Symbols need
    /// no separating white space, so the fields are read from the spelling
    /// of their tokens.
    fn primitive_row(&mut self, inputs: usize, sequential: bool) -> Result<PrimitiveRow> {
        let start = self.at;
        let mut fields: Vec<Vec<String>> = vec![Vec::new()];
        let mut edge: Option<String> = None;
        loop {
            let word = self.peek().to_owned();
            match word.as_str() {
                "" | "endtable" | "endprimitive" => {
                    return Err(self.error("unterminated UDP table entry"));
                }
                ";" => {
                    self.at += 1;
                    break;
                }
                ":" => fields.push(Vec::new()),
                _ => {
                    for symbol in word.chars() {
                        let symbol = symbol.to_ascii_lowercase();
                        match (symbol, edge.as_mut()) {
                            ('(', None) => edge = Some(String::new()),
                            (')', Some(levels)) if levels.len() == 2 => {
                                let levels = std::mem::take(levels);
                                edge = None;
                                fields
                                    .last_mut()
                                    .expect("a row has a field")
                                    .push(format!("({levels})"));
                            }
                            ('0' | '1' | 'x' | '?' | 'b', Some(levels)) if levels.len() < 2 => {
                                levels.push(symbol);
                            }
                            (
                                '0' | '1' | 'x' | '?' | 'b' | 'r' | 'f' | 'p' | 'n' | '*' | '-',
                                None,
                            ) => {
                                fields
                                    .last_mut()
                                    .expect("a row has a field")
                                    .push(symbol.to_string());
                            }
                            _ => {
                                return Err(
                                    self.error(format!("`{symbol}` is not a UDP table symbol"))
                                );
                            }
                        }
                    }
                }
            }
            self.at += 1;
        }
        let (input, state, output) = match (fields.as_slice(), sequential) {
            ([input, output], false) => (input.clone(), None, output.clone()),
            ([input, state, output], true) => (input.clone(), Some(state.clone()), output.clone()),
            _ => {
                return Err(self.error(if sequential {
                    "a sequential UDP table entry has inputs, a current state and a next state"
                } else {
                    "a combinational UDP table entry has inputs and an output"
                }));
            }
        };
        let level = |symbol: &str| matches!(symbol, "0" | "1" | "x" | "?" | "b");
        let edges = input.iter().filter(|symbol| !level(symbol)).count();
        let valid_output =
            |symbol: &str| matches!(symbol, "0" | "1" | "x") || sequential && symbol == "-";
        if input.len() != inputs
            || edges > usize::from(sequential)
            || output.len() != 1
            || !valid_output(&output[0])
            || state
                .as_ref()
                .is_some_and(|state| state.len() != 1 || !level(&state[0]))
        {
            return Err(self.error_at(
                self.range(start),
                format!("malformed UDP table entry for {inputs} inputs"),
            ));
        }
        Ok(PrimitiveRow {
            inputs: input,
            state: state.map(|mut state| state.remove(0)),
            output: output[0].clone(),
            tokens: self.range(start),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_keep_ports_tables_and_instances() {
        let tree = Declarations::parse(
            "module Top(input a, b, output y, z, q);\n  udp #1 named(y, a, b);\n  udp (z, b, a);\n  \
             flop f(q, a, b);\nendmodule\n\
             primitive udp(o, i, j);\n output o;\n input i, j;\n table\n  01 : 1;\n  1? : 0;\n  \
             x b : x;\n endtable\nendprimitive : udp\n\
             primitive flop(output reg q = 1'b0, input d, c);\n table\n  ? (01) : ? : -;\n  \
             1 r : ? : 1;\n  0 (?1) : 0 : 0;\n endtable\nendprimitive\n",
            Path::new("udp.sv"),
        )
        .unwrap();
        let [udp, flop] = tree.primitives.as_slice() else {
            panic!("two primitives")
        };
        assert!(!udp.sequential);
        assert_eq!(udp.ports.len(), 3);
        let rows: Vec<_> = udp
            .rows
            .iter()
            .map(|row| (row.inputs.join(" "), row.output.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("0 1".into(), "1"),
                ("1 ?".into(), "0"),
                ("x b".into(), "x")
            ]
        );
        assert!(flop.sequential && flop.initial.is_some());
        assert_eq!(flop.rows[0].inputs, ["?", "(01)"]);
        assert_eq!(flop.rows[0].state.as_deref(), Some("?"));
        assert_eq!(flop.rows[0].output, "-");
        assert_eq!(flop.rows[2].inputs, ["0", "(?1)"]);
        // The instances are gates of the primitives' names.
        let gates: Vec<_> = tree.units[0]
            .items
            .iter()
            .filter_map(|&item| match &tree.body_item(item).kind {
                BodyItemKind::LogicGate(gate) => Some((
                    gate.kind.as_str(),
                    gate.delay.is_some(),
                    gate.instances[0].name.is_some(),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            gates,
            [
                ("udp", true, true),
                ("udp", false, false),
                ("flop", false, true)
            ]
        );
    }

    #[test]
    fn malformed_primitives_are_rejected() {
        for code in [
            // An entry of the wrong width.
            "primitive p(o, i); output o; input i; table 0 1 : 1; endtable endprimitive",
            // An edge in a combinational table.
            "primitive p(o, i); output o; input i; table r : 1; endtable endprimitive",
            // `-` is the next state of a sequential entry only.
            "primitive p(o, i); output o; input i; table 0 : -; endtable endprimitive",
            // Two edges in one sequential entry.
            "primitive p(o, i, j); output reg o; input i, j; table r f : ? : 1; endtable \
             endprimitive",
            // A mismatched end label.
            "primitive p(o, i); output o; input i; table 0 : 1; endtable endprimitive : q",
            // The first port is the output.
            "primitive p(i, o); output o; input i; table 0 : 1; endtable endprimitive",
        ] {
            assert!(
                Declarations::parse(code, Path::new("bad.sv")).is_err(),
                "{code}"
            );
        }
    }
}
