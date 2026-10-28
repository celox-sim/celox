//! Versioned container for attaching a native program image to a precompiled
//! runtime executable.

use std::{
    fmt,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use super::backend::NativeProgramImage;

const TRAILER_MAGIC: &[u8; 8] = b"CELOXNPI";
// Concurrent assertion kernels suspend into Observed and Reactive regions.
const CONTAINER_VERSION: u16 = 53;
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

struct AppendedNativePayload<'a> {
    runtime_len: usize,
    version: u16,
    architecture: u8,
    payload: &'a [u8],
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
        let runtime_len = discover_appended_payload(&runtime)?
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
        let Some(appended) = discover_appended_payload(bytes)? else {
            return Ok(None);
        };
        if appended.version != CONTAINER_VERSION {
            return Err(NativeImageContainerError::UnsupportedVersion(
                appended.version,
            ));
        }
        let found = NativeImageArchitecture::decode(appended.architecture)?;
        let expected = NativeImageArchitecture::current();
        if found != expected {
            return Err(NativeImageContainerError::ArchitectureMismatch { expected, found });
        }
        let image = NativeProgramImage::from_postcard_bytes(appended.payload)?;
        image
            .validate()
            .map_err(NativeImageContainerError::InvalidImage)?;
        Ok(Some(AppendedNativeImage {
            runtime_len: appended.runtime_len,
            image,
        }))
    }

    /// Read the running executable and discover an image attached at EOF.
    pub fn discover_in_current_executable()
    -> Result<Option<AppendedNativeImage>, NativeImageContainerError> {
        #[cfg(target_os = "linux")]
        if let Ok(mapping) = super::executable_mapping::ExecutableMapping::current() {
            // The decoder returns owned metadata/code, so the temporary input
            // mapping can be released immediately after decoding.
            return Self::discover_appended(mapping.as_bytes());
        }
        let executable = std::env::current_exe()?;
        let mut file = std::fs::File::open(executable)?;
        discover_reader(&mut file)
    }
}

// Read the trailer first so ordinary CLI commands do not copy the runtime
// executable and attached runs allocate only their serialized image.
fn discover_reader(
    reader: &mut (impl Read + Seek),
) -> Result<Option<AppendedNativeImage>, NativeImageContainerError> {
    let file_len = reader.seek(SeekFrom::End(0))?;
    if file_len < TRAILER_SIZE as u64 {
        return Ok(None);
    }
    reader.seek(SeekFrom::End(-(TRAILER_SIZE as i64)))?;
    let mut trailer = [0u8; TRAILER_SIZE];
    reader.read_exact(&mut trailer)?;
    if &trailer[..8] != TRAILER_MAGIC {
        return Ok(None);
    }
    let trailer_size = u32::from_le_bytes(trailer[12..16].try_into().unwrap());
    if trailer_size as usize != TRAILER_SIZE {
        return Err(NativeImageContainerError::UnsupportedTrailerSize(
            trailer_size,
        ));
    }
    let payload_len = u64::from_le_bytes(trailer[16..24].try_into().unwrap());
    let runtime_len = (file_len - TRAILER_SIZE as u64)
        .checked_sub(payload_len)
        .ok_or(NativeImageContainerError::TruncatedPayload)?;
    let runtime_len =
        usize::try_from(runtime_len).map_err(|_| NativeImageContainerError::SizeOverflow)?;
    let payload_size =
        usize::try_from(payload_len).map_err(|_| NativeImageContainerError::SizeOverflow)?;
    let buffer_size = payload_size
        .checked_add(TRAILER_SIZE)
        .ok_or(NativeImageContainerError::SizeOverflow)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(buffer_size)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::OutOfMemory, error))?;
    reader.seek(SeekFrom::Start(runtime_len as u64))?;
    reader.take(payload_len).read_to_end(&mut bytes)?;
    if bytes.len() != payload_size {
        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
    }
    bytes.extend_from_slice(&trailer);
    let mut appended = NativeProgramImage::discover_appended(&bytes)?;
    if let Some(appended) = &mut appended {
        appended.runtime_len = runtime_len;
    }
    Ok(appended)
}

fn discover_appended_payload(
    bytes: &[u8],
) -> Result<Option<AppendedNativePayload<'_>>, NativeImageContainerError> {
    let Some(trailer_start) = bytes.len().checked_sub(TRAILER_SIZE) else {
        return Ok(None);
    };
    let trailer = &bytes[trailer_start..];
    if &trailer[..8] != TRAILER_MAGIC {
        return Ok(None);
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

    Ok(Some(AppendedNativePayload {
        runtime_len,
        version: u16::from_le_bytes(trailer[8..10].try_into().unwrap()),
        architecture: trailer[10],
        payload,
    }))
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

#[cfg(all(
    test,
    target_pointer_width = "64",
    any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        target_arch = "aarch64"
    )
))]
mod tests {
    use super::*;

    // A virtual runtime prefix may be arbitrarily large but is never readable.
    // This checks I/O bounds without putting a large fixture or benchmark in CI.
    struct SuffixReader {
        prefix: u64,
        suffix: Vec<u8>,
        position: u64,
        bytes_read: usize,
    }
    impl Read for SuffixReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if buffer.is_empty() {
                return Ok(0);
            }
            let offset = self
                .position
                .checked_sub(self.prefix)
                .ok_or_else(|| std::io::Error::other("runtime prefix must not be read"))?;
            let offset = usize::try_from(offset).map_err(std::io::Error::other)?;
            let count = buffer.len().min(self.suffix.len().saturating_sub(offset));
            if count != 0 {
                buffer[..count].copy_from_slice(&self.suffix[offset..offset + count]);
            }
            self.position += count as u64;
            self.bytes_read += count;
            Ok(count)
        }
    }
    impl Seek for SuffixReader {
        fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
            let position = match from {
                SeekFrom::Start(n) => i128::from(n),
                SeekFrom::Current(n) => i128::from(self.position) + i128::from(n),
                SeekFrom::End(n) => {
                    i128::from(self.prefix) + self.suffix.len() as i128 + i128::from(n)
                }
            };
            self.position = u64::try_from(position).map_err(std::io::Error::other)?;
            Ok(self.position)
        }
    }
    fn image() -> NativeProgramImage {
        let source = "module Top(a: input logic<8>, b: input logic<8>, y: output logic<8>) { assign y = a + b; }";
        crate::Simulator::builder(source, "Top")
            .opt_level(crate::OptLevel::O1)
            .build()
            .unwrap()
            .shared_code()
            .program_image()
            .clone()
    }
    #[test]
    fn host_request_variants_preserve_existing_image_encoding() {
        use celox_design::HostRequestKind;

        // These bytes are the enum indices stored in pre-Callable images.
        // Round trips alone would miss a simultaneous encoder/decoder change.
        for (index, kind) in [
            HostRequestKind::Plusargs,
            HostRequestKind::FileIo,
            HostRequestKind::Vcd,
            HostRequestKind::Scan,
            HostRequestKind::Distribution,
            HostRequestKind::Process,
            HostRequestKind::Testbench,
        ]
        .into_iter()
        .enumerate()
        {
            let bytes = [index as u8];
            assert_eq!(
                postcard::from_bytes::<HostRequestKind>(&bytes).unwrap(),
                kind
            );
            assert_eq!(postcard::to_allocvec(&kind).unwrap(), bytes);
        }
    }

    #[test]
    fn reader_ignores_runtime_prefix_and_preserves_image_and_offset() {
        let image = image();
        let suffix = image.to_container_bytes().unwrap();
        let size = suffix.len();
        let mut reader = SuffixReader {
            prefix: 1u64 << 32,
            suffix,
            position: 0,
            bytes_read: 0,
        };
        let found = discover_reader(&mut reader).unwrap().unwrap();
        assert_eq!(found.runtime_len, 1usize << 32);
        assert_eq!(found.image.code_image(), image.code_image());
        assert_eq!(found.image.reflection(), image.reflection());
        assert_eq!(reader.bytes_read, size);
    }
    #[test]
    fn ordinary_executable_reads_only_its_trailer() {
        let mut reader = SuffixReader {
            prefix: 1u64 << 32,
            suffix: vec![0; TRAILER_SIZE],
            position: 0,
            bytes_read: 0,
        };
        assert!(discover_reader(&mut reader).unwrap().is_none());
        assert_eq!(reader.bytes_read, TRAILER_SIZE);
        let mut short = std::io::Cursor::new(vec![0u8; TRAILER_SIZE - 1]);
        assert!(discover_reader(&mut short).unwrap().is_none());
    }
    #[test]
    fn reader_preserves_container_corruption_checks() {
        let encoded = image().to_container_bytes().unwrap();
        let trailer = encoded.len() - TRAILER_SIZE;
        let mut bad_size = encoded.clone();
        bad_size[trailer + 12..trailer + 16].copy_from_slice(&0u32.to_le_bytes());
        assert!(matches!(
            discover_reader(&mut std::io::Cursor::new(bad_size)),
            Err(NativeImageContainerError::UnsupportedTrailerSize(0))
        ));
        let mut truncated = encoded.clone();
        truncated[trailer + 16..trailer + 24]
            .copy_from_slice(&(encoded.len() as u64).to_le_bytes());
        assert!(matches!(
            discover_reader(&mut std::io::Cursor::new(truncated)),
            Err(NativeImageContainerError::TruncatedPayload)
        ));
        let mut corrupt = encoded;
        corrupt[0] ^= 1;
        assert!(matches!(
            discover_reader(&mut std::io::Cursor::new(corrupt)),
            Err(NativeImageContainerError::ChecksumMismatch)
        ));
    }
}
