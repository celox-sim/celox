//! How values of a type are packed into, and unpacked from, a bit stream
//! (IEEE 1800-2023 6.24.3, 11.4.14): one recursive classification of the
//! type, which the stream lowering of operands and targets follows.
use super::*;

/// The side of a bit-stream conversion a type is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ast) enum StreamRole {
    /// The value is packed into the stream.
    Operand,
    /// The value is filled from the stream.
    Target,
}

/// How a value of a type takes part in a bit stream, as far as the stream
/// lowering supports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ast) enum StreamLayout {
    /// Fixed-size bits: an integral value of `width` bits, or a fixed-size
    /// unpacked array of `element`-bit integral elements, `width` in all.
    Bits { width: u128, element: Option<u128> },
    /// A string: its bytes, index 0 first (6.24.3).
    String,
    /// A queue, dynamic or associative array of integral or string elements:
    /// its elements in order.
    Container,
    /// An unpacked structure of integral members: its members in order. Only
    /// an operand has this layout.
    Struct { width: u128 },
}

impl TyTable {
    /// The layout of a value of type `id` in a bit stream on side `role`,
    /// or why the stream lowering cannot give it one.
    pub fn stream_layout(&self, id: TyId, role: StreamRole) -> Result<StreamLayout, String> {
        match self.data(id) {
            TyData::Packed(packed) => Ok(StreamLayout::Bits {
                width: packed.bits(),
                element: None,
            }),
            TyData::String => Ok(StreamLayout::String),
            TyData::Array {
                kind: ArrayKind::Fixed(range),
                element,
            } => match self.stream_layout(*element, role)? {
                StreamLayout::Bits { width, element } => Ok(StreamLayout::Bits {
                    width: width * range.size(),
                    element: Some(element.unwrap_or(width)),
                }),
                _ => Err(format!(
                    "a bit stream of a fixed-size array of {}",
                    self.spell(*element)
                )),
            },
            TyData::Array { element, .. } => match self.data(*element) {
                TyData::Packed(_) | TyData::String => Ok(StreamLayout::Container),
                _ => Err(format!(
                    "a bit stream of a dynamically sized array of {}",
                    self.spell(*element)
                )),
            },
            // An unpacked union is no bit-stream type (6.24.3); a streaming
            // concatenation takes its first member (11.4.14.1), which the
            // operand does not select here.
            TyData::UnpackedUnion(_) => Err("a bit stream of an unpacked union".into()),
            TyData::UnpackedStruct(Decl::Aggregate { members, .. }) => {
                if role == StreamRole::Target {
                    return Err("a bit stream into an unpacked structure".into());
                }
                let mut width = 0;
                for (_, member) in members.iter() {
                    match self.data(*member) {
                        TyData::Packed(packed) => width += packed.bits(),
                        _ => {
                            return Err(format!(
                                "a bit stream of an unpacked structure with a member of {}",
                                self.spell(*member)
                            ));
                        }
                    }
                }
                Ok(StreamLayout::Struct { width })
            }
            _ => Err(format!("a bit stream of {}", self.spell(id))),
        }
    }
}
