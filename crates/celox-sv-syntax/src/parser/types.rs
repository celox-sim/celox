use super::expressions::{Job, Value, builtin};
use super::*;

pub(super) struct Aggregate {
    start: usize,
    union: bool,
    soft: bool,
    tagged: bool,
    packed: bool,
    signed: Option<bool>,
    fields: Vec<Field>,
    random: Option<RandomQualifier>,
}
pub(super) struct Enumeration {
    start: usize,
    base: Option<TypeId>,
    items: Vec<EnumItem>,
}
pub(super) enum TypeJob {
    Start,
    AfterAttributes {
        start: usize,
    },
    FieldAfterAttributes(Aggregate),
    Finish {
        start: usize,
        kind: DataTypeKind,
        signed: Option<bool>,
    },
    Packed {
        start: usize,
        kind: DataTypeKind,
        signed: Option<bool>,
    },
    Named {
        start: usize,
        path: Vec<TypeSegment>,
    },
    NamedArguments {
        start: usize,
        path: Vec<TypeSegment>,
        name: Name,
    },
    NamedNext {
        start: usize,
        path: Vec<TypeSegment>,
    },
    VirtualArguments {
        start: usize,
        name: Name,
    },
    TypeOf {
        start: usize,
    },
    StructNext(Aggregate),
    StructType(Aggregate),
    FieldStart {
        state: Aggregate,
        data_type: TypeId,
    },
    FieldDimensions {
        state: Aggregate,
        data_type: TypeId,
        name: Name,
    },
    FieldValue {
        state: Aggregate,
        field: Field,
    },
    FieldNext {
        state: Aggregate,
        field: Field,
    },
    EnumBase {
        start: usize,
    },
    EnumItem(Enumeration),
    EnumDimensions {
        state: Enumeration,
        name: Name,
    },
    EnumValue {
        state: Enumeration,
        item: EnumItem,
    },
    EnumNext {
        state: Enumeration,
        item: EnumItem,
    },
}
impl Value {
    fn data_type(self) -> TypeId {
        let Self::DataType(value) = self else {
            unreachable!("type continuation")
        };
        value
    }
    fn dimensions(self) -> Vec<Dimension> {
        let Self::Dimensions(value) = self else {
            unreachable!("dimensions continuation")
        };
        value
    }
}

impl Parser<'_> {
    pub(super) fn data_type(&mut self) -> Result<TypeId> {
        Ok(self
            .expression_machine(Job::Type(TypeJob::Start))?
            .data_type())
    }
    pub(super) fn implicit_type(&mut self) -> Result<TypeId> {
        if self.types.len() >= 1_000_000 {
            return Err(self.unsupported("native type arena exceeds one million nodes"));
        }
        let id = TypeId(self.types.len());
        self.types.push(DataType {
            kind: DataTypeKind::Implicit,
            signed: None,
            packed_dimensions: Vec::new(),
            tokens: self.range(self.at),
        });
        Ok(id)
    }
    fn signing(&mut self) -> Option<bool> {
        if self.eat("signed") {
            Some(true)
        } else if self.eat("unsigned") {
            Some(false)
        } else {
            None
        }
    }
    fn type_name(&mut self) -> Result<Name> {
        if self.token().is_some_and(|token| {
            token.kind == TokenKind::SystemIdentifier
                && matches!(token.text.as_str(), "$unit" | "$root")
        }) {
            let token = self.token().unwrap();
            let name = Name {
                spelling: token.text.clone(),
                origin: token.origin,
            };
            self.at += 1;
            Ok(name)
        } else {
            self.name()
        }
    }
    pub(super) fn type_job(
        &mut self,
        job: TypeJob,
        jobs: &mut Vec<Job>,
        values: &mut Vec<Value>,
    ) -> Result<()> {
        match job {
            TypeJob::Start => {
                jobs.push(Job::Type(TypeJob::AfterAttributes { start: self.at }));
                jobs.push(Job::DropAttributes);
                jobs.push(Job::Attributes);
            }
            TypeJob::AfterAttributes { start } => {
                let word = self.peek();
                match word {
                    "struct" | "union" => {
                        let union = self.eat("union");
                        if !union {
                            self.expect("struct")?;
                        }
                        let soft = self.eat("soft");
                        if soft && !union {
                            return Err(self.error("only a union can be soft"));
                        }
                        let tagged = !soft && self.eat("tagged");
                        let packed = self.eat("packed");
                        let signed = self.signing();
                        if tagged && !union {
                            return Err(self.error("only a union can be tagged"));
                        }
                        if signed.is_some() && !packed {
                            return Err(self.error("aggregate signing requires `packed`"));
                        }
                        self.expect("{")?;
                        jobs.push(Job::Type(TypeJob::StructNext(Aggregate {
                            start,
                            union,
                            soft,
                            tagged,
                            packed,
                            signed,
                            fields: Vec::new(),
                            random: None,
                        })));
                    }
                    "enum" => {
                        self.at += 1;
                        if self.eat("{") {
                            jobs.push(Job::Type(TypeJob::EnumItem(Enumeration {
                                start,
                                base: None,
                                items: Vec::new(),
                            })));
                        } else {
                            jobs.push(Job::Type(TypeJob::EnumBase { start }));
                            jobs.push(Job::Type(TypeJob::Start));
                        }
                    }
                    "type" if self.nth(1) == "(" => {
                        self.at += 2;
                        jobs.push(Job::Type(TypeJob::TypeOf { start }));
                        jobs.push(Job::Expression(0));
                    }
                    "virtual" => {
                        self.at += 1;
                        self.eat("interface");
                        let name = self.name()?;
                        if self.eat("#") {
                            jobs.push(Job::Type(TypeJob::VirtualArguments { start, name }));
                            jobs.push(Job::Arguments);
                        } else {
                            let modport = if self.eat(".") {
                                Some(self.name()?)
                            } else {
                                None
                            };
                            jobs.push(Job::Type(TypeJob::Finish {
                                start,
                                kind: DataTypeKind::VirtualInterface {
                                    name,
                                    arguments: None,
                                    modport,
                                },
                                signed: None,
                            }));
                        }
                    }
                    "interface" => {
                        self.at += 1;
                        let modport = if self.eat(".") {
                            Some(self.name()?)
                        } else {
                            None
                        };
                        jobs.push(Job::Type(TypeJob::Finish {
                            start,
                            kind: DataTypeKind::Interface {
                                name: None,
                                modport,
                            },
                            signed: None,
                        }));
                    }
                    word if builtin(word) => {
                        let word = word.to_owned();
                        self.at += 1;
                        jobs.push(Job::Type(TypeJob::Finish {
                            start,
                            kind: DataTypeKind::Builtin(word),
                            signed: None,
                        }));
                    }
                    "signed" | "unsigned" | "[" => jobs.push(Job::Type(TypeJob::Finish {
                        start,
                        kind: DataTypeKind::Implicit,
                        signed: None,
                    })),
                    _ => jobs.push(Job::Type(TypeJob::Named {
                        start,
                        path: Vec::new(),
                    })),
                }
            }
            TypeJob::Finish {
                start,
                kind,
                signed,
            } => {
                let permits_signing = matches!(&kind, DataTypeKind::Implicit)
                    || matches!(&kind, DataTypeKind::Builtin(name) if matches!(name.as_str(), "bit" | "logic" | "reg" | "byte" | "shortint" | "int" | "longint" | "integer" | "time"));
                let signed = if permits_signing {
                    signed.or_else(|| self.signing())
                } else {
                    if matches!(self.peek(), "signed" | "unsigned") {
                        return Err(self.error("datatype does not permit a signing suffix"));
                    }
                    signed
                };
                jobs.push(Job::Type(TypeJob::Packed {
                    start,
                    kind,
                    signed,
                }));
                jobs.push(Job::Dimensions(Vec::new()));
            }
            TypeJob::Packed {
                start,
                kind,
                signed,
            } => {
                let packed_dimensions = values.pop().unwrap().dimensions();
                if packed_dimensions.iter().any(|dimension| {
                    !matches!(
                        dimension.kind,
                        DimensionKind::Range { .. } | DimensionKind::Unsized
                    )
                }) {
                    return Err(self.error("packed dimension must be a range or unsized dimension"));
                }
                if !packed_dimensions.is_empty()
                    && matches!(
                        &kind,
                        DataTypeKind::TypeOf(_)
                            | DataTypeKind::Interface { .. }
                            | DataTypeKind::VirtualInterface { .. }
                    )
                {
                    return Err(self.error("datatype does not permit packed dimensions"));
                }
                for dimension in &packed_dimensions {
                    if let DimensionKind::Range { left, right } = dimension.kind {
                        self.validate_constant_operators(left)?;
                        self.validate_constant_operators(right)?;
                    }
                }
                // A.2.2.1: only integer vectors permit packed dimensions;
                // integer atoms additionally permit signing, other builtins
                // permit neither. Check each completed type, including fields.
                if let DataTypeKind::Builtin(name) = &kind {
                    let vector = matches!(name.as_str(), "bit" | "logic" | "reg");
                    let atom = matches!(
                        name.as_str(),
                        "byte" | "shortint" | "int" | "longint" | "integer" | "time"
                    );
                    if !vector && !packed_dimensions.is_empty() {
                        return Err(
                            self.error("packed dimensions require an integer vector datatype")
                        );
                    }
                    if !vector && !atom && signed.is_some() {
                        return Err(self.error("signing requires an integer datatype"));
                    }
                }
                if self.types.len() >= 1_000_000 {
                    return Err(self.unsupported("native type arena exceeds one million nodes"));
                }
                let id = TypeId(self.types.len());
                self.types.push(DataType {
                    kind,
                    signed,
                    packed_dimensions,
                    tokens: self.range(start),
                });
                values.push(Value::DataType(id));
            }
            TypeJob::Named { start, mut path } => {
                let name = self.type_name()?;
                if self.eat("#") {
                    jobs.push(Job::Type(TypeJob::NamedArguments { start, path, name }));
                    jobs.push(Job::Arguments);
                } else {
                    path.push(TypeSegment {
                        name,
                        arguments: None,
                    });
                    jobs.push(Job::Type(TypeJob::NamedNext { start, path }));
                }
            }
            TypeJob::NamedArguments {
                start,
                mut path,
                name,
            } => {
                let arguments = Some(values.pop().unwrap().arguments());
                path.push(TypeSegment { name, arguments });
                jobs.push(Job::Type(TypeJob::NamedNext { start, path }));
            }
            TypeJob::NamedNext { start, mut path } => {
                if self.eat("::") {
                    jobs.push(Job::Type(TypeJob::Named { start, path }));
                } else {
                    let kind = if self.eat(".") {
                        if path.len() != 1 || path[0].arguments.is_some() {
                            return Err(self.unsupported("qualified interface port type"));
                        }
                        DataTypeKind::Interface {
                            name: Some(path.remove(0).name),
                            modport: Some(self.name()?),
                        }
                    } else {
                        DataTypeKind::Named(path)
                    };
                    jobs.push(Job::Type(TypeJob::Finish {
                        start,
                        kind,
                        signed: None,
                    }));
                }
            }
            TypeJob::VirtualArguments { start, name } => {
                let arguments = Some(values.pop().unwrap().arguments());
                let modport = if self.eat(".") {
                    Some(self.name()?)
                } else {
                    None
                };
                jobs.push(Job::Type(TypeJob::Finish {
                    start,
                    kind: DataTypeKind::VirtualInterface {
                        name,
                        arguments,
                        modport,
                    },
                    signed: None,
                }));
            }
            TypeJob::TypeOf { start } => {
                let expression = values.pop().unwrap().expression();
                self.expect(")")?;
                jobs.push(Job::Type(TypeJob::Finish {
                    start,
                    kind: DataTypeKind::TypeOf(expression),
                    signed: None,
                }));
            }
            TypeJob::StructNext(state) => {
                if self.eat("}") {
                    if state.fields.is_empty() {
                        return Err(self.error("aggregate requires at least one member"));
                    }
                    let Aggregate {
                        start,
                        union,
                        soft,
                        tagged,
                        packed,
                        signed,
                        fields,
                        ..
                    } = state;
                    jobs.push(Job::Type(TypeJob::Finish {
                        start,
                        kind: DataTypeKind::Struct {
                            union,
                            soft,
                            tagged,
                            packed,
                            fields,
                        },
                        signed,
                    }));
                } else {
                    jobs.push(Job::Type(TypeJob::FieldAfterAttributes(state)));
                    jobs.push(Job::DropAttributes);
                    jobs.push(Job::Attributes);
                }
            }
            TypeJob::FieldAfterAttributes(mut state) => {
                state.random = if self.eat("rand") {
                    Some(RandomQualifier::Rand)
                } else if self.eat("randc") {
                    Some(RandomQualifier::Randc)
                } else {
                    None
                };
                jobs.push(Job::Type(TypeJob::StructType(state)));
                jobs.push(Job::Type(TypeJob::Start));
            }
            TypeJob::StructType(state) => {
                let data_type = values.pop().unwrap().data_type();
                jobs.push(Job::Type(TypeJob::FieldStart { state, data_type }));
            }
            TypeJob::FieldStart { state, data_type } => {
                let name = self.name()?;
                jobs.push(Job::Type(TypeJob::FieldDimensions {
                    state,
                    data_type,
                    name,
                }));
                jobs.push(Job::Dimensions(Vec::new()));
            }
            TypeJob::FieldDimensions {
                state,
                data_type,
                name,
            } => {
                let dimensions = values.pop().unwrap().dimensions();
                self.validate_variable_dimensions(&dimensions)?;
                let field = Field {
                    name,
                    data_type,
                    random: state.random.clone(),
                    dimensions,
                    default: None,
                };
                if self.eat("=") {
                    jobs.push(Job::Type(TypeJob::FieldValue { state, field }));
                    jobs.push(Job::Expression(0));
                } else {
                    jobs.push(Job::Type(TypeJob::FieldNext { state, field }));
                }
            }
            TypeJob::FieldValue { state, mut field } => {
                field.default = Some(values.pop().unwrap().expression());
                jobs.push(Job::Type(TypeJob::FieldNext { state, field }));
            }
            TypeJob::FieldNext { mut state, field } => {
                let data_type = field.data_type;
                state.fields.push(field);
                if self.eat(",") {
                    jobs.push(Job::Type(TypeJob::FieldStart { state, data_type }));
                } else {
                    self.expect(";")?;
                    jobs.push(Job::Type(TypeJob::StructNext(state)));
                }
            }
            TypeJob::EnumBase { start } => {
                let base = values.pop().unwrap().data_type();
                let datatype = &self.types[base.0];
                let builtin = matches!(&datatype.kind, DataTypeKind::Builtin(name) if matches!(name.as_str(), "bit" | "logic" | "reg" | "byte" | "shortint" | "int" | "longint" | "integer" | "time"));
                let named = matches!(&datatype.kind, DataTypeKind::Named(path) if path.len() == 1 && path[0].arguments.is_none());
                if (!builtin && !named) || datatype.packed_dimensions.len() > 1 {
                    return Err(self.error("enum base must be an integer type or type identifier with at most one packed dimension"));
                }
                let base = Some(base);
                self.expect("{")?;
                jobs.push(Job::Type(TypeJob::EnumItem(Enumeration {
                    start,
                    base,
                    items: Vec::new(),
                })));
            }
            TypeJob::EnumItem(state) => {
                let name = self.name()?;
                jobs.push(Job::Type(TypeJob::EnumDimensions { state, name }));
                jobs.push(Job::Dimensions(Vec::new()));
            }
            TypeJob::EnumDimensions { state, name } => {
                let dimensions = values.pop().unwrap().dimensions();
                if dimensions.len() > 1 || dimensions.iter().any(|dimension| {
                    let tokens = self.tokens[dimension.tokens.start..dimension.tokens.end].iter().filter(|token| !token.is_trivia()).collect::<Vec<_>>();
                    !matches!(tokens.as_slice(), [open, number, close] if open.text == "[" && close.text == "]" && enum_number(number))
                        && !matches!(tokens.as_slice(), [open, left, colon, right, close] if open.text == "[" && colon.text == ":" && close.text == "]" && enum_number(left) && enum_number(right))
                }) {
                    return Err(self.error("enum member range requires one literal integral-number size or range"));
                }
                let item = EnumItem {
                    name,
                    dimensions,
                    value: None,
                };
                if self.eat("=") {
                    jobs.push(Job::Type(TypeJob::EnumValue { state, item }));
                    jobs.push(Job::Expression(0));
                } else {
                    jobs.push(Job::Type(TypeJob::EnumNext { state, item }));
                }
            }
            TypeJob::EnumValue { state, mut item } => {
                let value = values.pop().unwrap().expression();
                self.validate_constant_operators(value)?;
                item.value = Some(value);
                jobs.push(Job::Type(TypeJob::EnumNext { state, item }));
            }
            TypeJob::EnumNext { mut state, item } => {
                state.items.push(item);
                if self.eat("}") {
                    jobs.push(Job::Type(TypeJob::Finish {
                        start: state.start,
                        kind: DataTypeKind::Enum {
                            base: state.base,
                            items: state.items,
                        },
                        signed: None,
                    }));
                } else {
                    self.expect(",")?;
                    jobs.push(Job::Type(TypeJob::EnumItem(state)));
                }
            }
        }
        Ok(())
    }
    pub(super) fn starts_type(&self) -> bool {
        if !builtin(self.peek())
            && self.nth(1) == "["
            && self.token().is_some_and(|token| {
                matches!(
                    token.kind,
                    TokenKind::Identifier | TokenKind::EscapedIdentifier
                )
            })
        {
            let mut at = self.at + 1;
            while self
                .indices
                .get(at)
                .is_some_and(|index| self.tokens[*index].text == "[")
            {
                let mut nesting = 1;
                at += 1;
                while nesting > 0 {
                    let Some(index) = self.indices.get(at) else {
                        return false;
                    };
                    match self.tokens[*index].text.as_str() {
                        "[" => nesting += 1,
                        "]" => nesting -= 1,
                        _ => {}
                    }
                    at += 1;
                }
            }
            return self.indices.get(at).is_some_and(|index| {
                matches!(
                    self.tokens[*index].kind,
                    TokenKind::Identifier | TokenKind::EscapedIdentifier
                ) || self.tokens[*index].text == "#"
            });
        }
        builtin(self.peek())
            || matches!(
                self.peek(),
                "signed" | "unsigned" | "[" | "struct" | "union" | "enum" | "interface" | "virtual"
            )
            || (self.peek() == "type" && self.nth(1) == "(")
            || (self.token().is_some_and(|token| {
                matches!(
                    token.kind,
                    TokenKind::Identifier
                        | TokenKind::EscapedIdentifier
                        | TokenKind::SystemIdentifier
                )
            }) && self.indices.get(self.at + 1).is_some_and(|index| {
                matches!(
                    self.tokens[*index].kind,
                    TokenKind::Identifier | TokenKind::EscapedIdentifier
                ) || matches!(self.tokens[*index].text.as_str(), "::" | "." | "#")
            }))
    }
}

fn enum_number(token: &Token) -> bool {
    token.kind == TokenKind::Number && super::numbers::integral(&token.text)
}
