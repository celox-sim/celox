//! How diagnostics spell semantic types.
use super::*;

impl TyTable {
    /// Type `id` as a diagnostic writes it: its element type, then each
    /// unpacked dimension, slowest varying first, as an element count or
    /// the kind of a variable-size dimension (`logic signed [7:0] [2][$]`).
    pub fn spell(&self, id: TyId) -> String {
        let mut dims = String::new();
        let mut id = id;
        while let TyData::Array { kind, element } = self.data(id) {
            match kind {
                ArrayKind::Fixed(range) => {
                    dims.push_str(&format!("[{}]", range.size()));
                }
                ArrayKind::Dynamic => dims.push_str("[]"),
                ArrayKind::Queue { bound: None } => dims.push_str("[$]"),
                ArrayKind::Queue { bound: Some(bound) } => {
                    dims.push_str(&format!("[$:{bound}]"));
                }
                ArrayKind::Associative { key } => {
                    dims.push_str(&format!("[{}]", self.spell(*key)));
                }
            }
            id = *element;
        }
        let element = self.spell_element(id);
        if dims.is_empty() {
            element
        } else {
            format!("{element} {dims}")
        }
    }

    fn spell_element(&self, id: TyId) -> String {
        match self.data(id) {
            TyData::Packed(packed) => match &packed.element {
                PackedElement::Enum { decl, array } => {
                    let name = format!("enum {}", spell_decl(decl));
                    match array {
                        false => name,
                        true => format!("{name} [{}:0]", packed.bits() - 1),
                    }
                }
                // A packed aggregate is spelled by its bits.
                PackedElement::Bits | PackedElement::Aggregate(_) => {
                    let mut name = if packed.four_state { "logic" } else { "bit" }.to_owned();
                    if packed.signed {
                        name.push_str(" signed");
                    }
                    let bits = packed.bits();
                    if bits > 1 {
                        name.push_str(&format!(" [{}:0]", bits - 1));
                    }
                    name
                }
            },
            TyData::Real => "real".into(),
            TyData::Shortreal => "shortreal".into(),
            TyData::String => "string".into(),
            TyData::Null => "null".into(),
            TyData::Class(decl) => format!("class {}", spell_decl(decl)),
            TyData::VirtualInterface { interface, modport } => match modport {
                Some(modport) => format!("virtual {}.{modport}", spell_decl(interface)),
                None => format!("virtual {}", spell_decl(interface)),
            },
            TyData::UnpackedStruct(decl) => format!("struct {}", spell_decl(decl)),
            TyData::Array { .. } => self.spell(id),
        }
    }
}

fn spell_decl(decl: &Decl) -> String {
    match decl {
        Decl::Named(name)
        | Decl::Aggregate { name, .. }
        | Decl::Class(name)
        | Decl::Interface(name) => name.to_string(),
        Decl::AnonymousEnum(domain) => format!(
            "{{{}}}",
            domain
                .members
                .iter()
                .map(|member| format!("{}={}", member.name, domain.value_of(member)))
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}
