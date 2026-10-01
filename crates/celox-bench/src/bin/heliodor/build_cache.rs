//! Local, opt-in cache of trusted native compiler artifacts.
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use celox::NativeProgramImage;

use super::{CeloxHeliodorError, Options};

const MAGIC: &[u8] = b"CELOX-BUILD-CACHE-1\n";

pub(super) struct BuildCache {
    path: PathBuf,
}

fn field(hash: &mut blake3::Hasher, bytes: &[u8]) {
    hash.update(&(bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

fn file_hash(path: &Path) -> io::Result<String> {
    let mut hash = blake3::Hasher::new();
    let mut file = fs::File::open(path)?;
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hash.finalize().to_hex().to_string())
}

fn dependency_hash(path: &Path) -> io::Result<Option<String>> {
    match file_hash(path) {
        Ok(hash) => Ok(Some(hash)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

impl BuildCache {
    pub(super) fn new(
        dir: &Path,
        opts: &Options,
        sources: &[(String, PathBuf)],
        metadata: &veryl_metadata::Metadata,
    ) -> Result<Self, CeloxHeliodorError> {
        let mut hash = blake3::Hasher::new();
        field(&mut hash, MAGIC);
        // The exact compiler binary covers revisions, dirty builds, Cargo
        // features, target architecture, and changes to image serialization.
        field(&mut hash, file_hash(&std::env::current_exe()?)?.as_bytes());
        field(
            &mut hash,
            std::env::current_dir()?.as_os_str().as_encoded_bytes(),
        );
        field(&mut hash, opts.project.as_os_str().as_encoded_bytes());
        field(&mut hash, opts.test.as_bytes());
        field(
            &mut hash,
            format!(
                "{}:{}:{}:{}:{:?}",
                opts.opt_level.as_str(),
                opts.four_state,
                opts.native_memory_width,
                opts.x86_slp,
                opts.pass_overrides
            )
            .as_bytes(),
        );
        field(
            &mut hash,
            format!("{:?}", celox::DiagnosticsOptions::from_env()).as_bytes(),
        );
        // Value uses ordered object keys, including metadata's HashMaps.
        let metadata = serde_json::to_value((metadata, &metadata.lockfile, &metadata.pubfile))
            .map_err(io::Error::other)?;
        field(
            &mut hash,
            &serde_json::to_vec(&metadata).map_err(io::Error::other)?,
        );
        for (source, path) in sources {
            field(&mut hash, path.as_os_str().as_encoded_bytes());
            field(&mut hash, source.as_bytes());
        }
        Ok(Self {
            path: dir.join(format!("{}.cache", hash.finalize().to_hex())),
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn load(&self) -> Option<NativeProgramImage> {
        match self.try_load() {
            Ok(image) => image,
            Err(error) => {
                eprintln!("build cache ignored {}: {error}", self.path.display());
                None
            }
        }
    }

    fn try_load(&self) -> Result<Option<NativeProgramImage>, CeloxHeliodorError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid build cache envelope");
        let rest = bytes.strip_prefix(MAGIC).ok_or_else(invalid)?;
        let length = u64::from_le_bytes(rest.get(..8).ok_or_else(invalid)?.try_into().unwrap());
        let length = usize::try_from(length).map_err(|_| invalid())?;
        let rest = &rest[8..];
        let manifest = rest.get(..length).ok_or_else(invalid)?;
        let dependencies: Vec<(PathBuf, Option<String>)> =
            serde_json::from_slice(manifest).map_err(io::Error::other)?;
        for (path, expected) in dependencies {
            if dependency_hash(&path)? != expected {
                return Ok(None);
            }
        }
        Ok(Some(NativeProgramImage::from_container_bytes(
            &rest[length..],
        )?))
    }

    pub(super) fn store(&self, image: &NativeProgramImage, dependencies: &[PathBuf]) {
        if let Err(error) = self.try_store(image, dependencies) {
            eprintln!("build cache write skipped {}: {error}", self.path.display());
        }
    }

    fn try_store(
        &self,
        image: &NativeProgramImage,
        dependencies: &[PathBuf],
    ) -> Result<(), CeloxHeliodorError> {
        let dependencies = dependencies
            .iter()
            .map(|path| Ok((path, dependency_hash(path)?)))
            .collect::<io::Result<Vec<_>>>()?;
        let manifest = serde_json::to_vec(&dependencies).map_err(io::Error::other)?;
        let dir = self.path.parent().expect("cache entry has a parent");
        fs::create_dir_all(dir)?;
        // A reader sees either a complete old entry or a complete new entry.
        // Concurrent writers use distinct files and may replace the same key.
        let mut temporary = tempfile::NamedTempFile::new_in(dir)?;
        temporary.write_all(MAGIC)?;
        temporary.write_all(&(manifest.len() as u64).to_le_bytes())?;
        temporary.write_all(&manifest)?;
        temporary.write_all(&image.to_container_bytes()?)?;
        temporary.persist(&self.path).map_err(|error| error.error)?;
        Ok(())
    }
}
