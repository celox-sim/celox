use std::ops::Deref;
#[cfg(target_os = "linux")]
use std::{ops::Range, sync::Arc};

#[cfg(target_os = "linux")]
use super::executable_mapping::ExecutableMapping;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub(super) enum NativeCodeBytes {
    Owned(Vec<u8>),
    #[cfg(target_os = "linux")]
    Mapped {
        owner: Arc<ExecutableMapping>,
        range: Range<usize>,
    },
}

impl From<Vec<u8>> for NativeCodeBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Owned(bytes)
    }
}

impl Deref for NativeCodeBytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Owned(bytes) => bytes,
            #[cfg(target_os = "linux")]
            Self::Mapped { owner, range } => &owner.as_bytes()[range.clone()],
        }
    }
}

// Preserve the generic Vec<u8> Serde representation, including array-based
// formats. Only the private container decoder borrows Postcard byte spans.
impl Serialize for NativeCodeBytes {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.deref().serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for NativeCodeBytes {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<u8>::deserialize(deserializer).map(Self::Owned)
    }
}
