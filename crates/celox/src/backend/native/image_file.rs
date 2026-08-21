//! Versioned container for attaching a native program image to a precompiled
//! runtime executable.

use std::{fmt, path::Path};

use super::backend::NativeProgramImage;

const TRAILER_MAGIC: &[u8; 8] = b"CELOXNPI";
const CONTAINER_VERSION: u16 = 4;
const TRAILER_SIZE: usize = 32;
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Native ISA required by an appended program image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NativeImageArchitecture {
    X86_64 = 1,
    Aarch64 = 2,
}

impl NativeImageArchitecture {
    pub fn current() -> Self {
        #[cfg(any(
            feature = "x86_64-codegen",
            all(target_arch = "x86_64", not(feature = "arm64-codegen"))
        ))]
        {
            Self::X86_64
        }
        #[cfg(any(
            feature = "arm64-codegen",
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
        {
            Self::Aarch64
        }
    }

    fn decode(value: u8) -> Result<Self, NativeImageContainerError> {
        match value {
            1 => Ok(Self::X86_64),
            2 => Ok(Self::Aarch64),
            value => Err(NativeImageContainerError::UnknownArchitecture(value)),
        }
    }
}

impl fmt::Display for NativeImageArchitecture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::X86_64 => formatter.write_str("x86-64"),
            Self::Aarch64 => formatter.write_str("AArch64"),
        }
    }
}

/// Result of finding and decoding an image appended to another byte sequence.
pub struct AppendedNativeImage {
    /// Length of the original runtime prefix before the serialized image.
    pub runtime_len: usize,
    /// Decoded pointer-free program image.
    pub image: NativeProgramImage,
}

/// Failure while encoding or discovering a native image container.
#[derive(Debug, thiserror::Error)]
pub enum NativeImageContainerError {
    #[error("native image container I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to encode or decode native program metadata: {0}")]
    Postcard(#[from] postcard::Error),
    #[error("native image container size overflows the host address space")]
    SizeOverflow,
    #[error("native image trailer is missing")]
    MissingTrailer,
    #[error("native image trailer has unsupported size {0}")]
    UnsupportedTrailerSize(u32),
    #[error("native image container version {0} is unsupported")]
    UnsupportedVersion(u16),
    #[error("native image architecture tag {0} is unknown")]
    UnknownArchitecture(u8),
    #[error("native image targets {found}, but this runtime targets {expected}")]
    ArchitectureMismatch {
        expected: NativeImageArchitecture,
        found: NativeImageArchitecture,
    },
    #[error("native image payload length exceeds the containing file")]
    TruncatedPayload,
    #[error("native image payload checksum does not match")]
    ChecksumMismatch,
    #[error("decoded native program image is invalid: {0}")]
    InvalidImage(String),
    #[error("standalone native image container unexpectedly has a runtime prefix")]
    UnexpectedRuntimePrefix,
}

impl NativeProgramImage {
    /// Serialize this image as a standalone versioned container.
    pub fn to_container_bytes(&self) -> Result<Vec<u8>, NativeImageContainerError> {
        self.append_to_runtime(&[])
    }

    /// Write this image as a standalone versioned binary container.
    pub fn write_container(
        &self,
        output_path: impl AsRef<Path>,
    ) -> Result<(), NativeImageContainerError> {
        std::fs::write(output_path, self.to_container_bytes()?)?;
        Ok(())
    }

    /// Append this image to an existing precompiled runtime byte sequence.
    ///
    /// The runtime bytes are copied verbatim. A fixed-size trailer at EOF lets
    /// the runtime discover the payload without parsing its own executable
    /// format.
    pub fn append_to_runtime(&self, runtime: &[u8]) -> Result<Vec<u8>, NativeImageContainerError> {
        self.validate()
            .map_err(NativeImageContainerError::InvalidImage)?;
        let payload = postcard::to_allocvec(self)?;
        let total_len = runtime
            .len()
            .checked_add(payload.len())
            .and_then(|size| size.checked_add(TRAILER_SIZE))
            .ok_or(NativeImageContainerError::SizeOverflow)?;
        let payload_len =
            u64::try_from(payload.len()).map_err(|_| NativeImageContainerError::SizeOverflow)?;

        let mut output = Vec::with_capacity(total_len);
        output.extend_from_slice(runtime);
        output.extend_from_slice(&payload);
        output.extend_from_slice(TRAILER_MAGIC);
        output.extend_from_slice(&CONTAINER_VERSION.to_le_bytes());
        output.push(NativeImageArchitecture::current() as u8);
        output.push(0); // flags, reserved for future container revisions
        output.extend_from_slice(&(TRAILER_SIZE as u32).to_le_bytes());
        output.extend_from_slice(&payload_len.to_le_bytes());
        output.extend_from_slice(&checksum(&payload).to_le_bytes());
        debug_assert_eq!(output.len(), total_len);
        Ok(output)
    }

    /// Copy a precompiled runtime and attach this image at EOF.
    ///
    /// If the input runtime already has an image, the old payload is stripped
    /// before the new one is appended. File permissions are preserved, which
    /// keeps an executable runtime executable on Unix hosts.
    pub fn write_attached_runtime(
        &self,
        runtime_path: impl AsRef<Path>,
        output_path: impl AsRef<Path>,
    ) -> Result<(), NativeImageContainerError> {
        let runtime_path = runtime_path.as_ref();
        let output_path = output_path.as_ref();
        let metadata = std::fs::metadata(runtime_path)?;
        let runtime = std::fs::read(runtime_path)?;
        let runtime_len = Self::discover_appended(&runtime)?
            .map(|appended| appended.runtime_len)
            .unwrap_or(runtime.len());
        let output = self.append_to_runtime(&runtime[..runtime_len])?;
        std::fs::write(output_path, output)?;
        std::fs::set_permissions(output_path, metadata.permissions())?;
        Ok(())
    }

    /// Decode a standalone container with no runtime prefix.
    pub fn from_container_bytes(bytes: &[u8]) -> Result<Self, NativeImageContainerError> {
        let appended =
            Self::discover_appended(bytes)?.ok_or(NativeImageContainerError::MissingTrailer)?;
        if appended.runtime_len != 0 {
            return Err(NativeImageContainerError::UnexpectedRuntimePrefix);
        }
        Ok(appended.image)
    }

    /// Discover an image at EOF. A missing magic trailer is reported as
    /// `Ok(None)` so an ordinary runtime executable can distinguish "no design
    /// attached" from a corrupt attached design.
    pub fn discover_appended(
        bytes: &[u8],
    ) -> Result<Option<AppendedNativeImage>, NativeImageContainerError> {
        let Some(trailer_start) = bytes.len().checked_sub(TRAILER_SIZE) else {
            return Ok(None);
        };
        let trailer = &bytes[trailer_start..];
        if &trailer[..8] != TRAILER_MAGIC {
            return Ok(None);
        }

        let version = u16::from_le_bytes(trailer[8..10].try_into().unwrap());
        if version != CONTAINER_VERSION {
            return Err(NativeImageContainerError::UnsupportedVersion(version));
        }
        let found = NativeImageArchitecture::decode(trailer[10])?;
        let expected = NativeImageArchitecture::current();
        if found != expected {
            return Err(NativeImageContainerError::ArchitectureMismatch { expected, found });
        }
        let trailer_size = u32::from_le_bytes(trailer[12..16].try_into().unwrap());
        if trailer_size as usize != TRAILER_SIZE {
            return Err(NativeImageContainerError::UnsupportedTrailerSize(
                trailer_size,
            ));
        }
        let payload_len = u64::from_le_bytes(trailer[16..24].try_into().unwrap());
        let payload_len =
            usize::try_from(payload_len).map_err(|_| NativeImageContainerError::SizeOverflow)?;
        let runtime_len = trailer_start
            .checked_sub(payload_len)
            .ok_or(NativeImageContainerError::TruncatedPayload)?;
        let payload = &bytes[runtime_len..trailer_start];
        let expected_checksum = u64::from_le_bytes(trailer[24..32].try_into().unwrap());
        if checksum(payload) != expected_checksum {
            return Err(NativeImageContainerError::ChecksumMismatch);
        }
        let image: NativeProgramImage = postcard::from_bytes(payload)?;
        image
            .validate()
            .map_err(NativeImageContainerError::InvalidImage)?;
        Ok(Some(AppendedNativeImage { runtime_len, image }))
    }

    /// Read the running executable and discover an image attached at EOF.
    pub fn discover_in_current_executable()
    -> Result<Option<AppendedNativeImage>, NativeImageContainerError> {
        let executable = std::env::current_exe()?;
        let bytes = std::fs::read(executable)?;
        Self::discover_appended(&bytes)
    }
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}
