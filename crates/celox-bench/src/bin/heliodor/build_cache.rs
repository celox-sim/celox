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

fn component_inputs(
    hash: &mut blake3::Hasher,
    components: &[veryl_metadata::Component],
    root: &Path,
    target_dir: &Path,
) -> io::Result<()> {
    for component in components {
        let crate_dir = root.join(&component.path);
        let mut paths = vec![
            (crate_dir.join("Cargo.toml"), false),
            (
                crate_dir.join(veryl_metadata::COMMITTED_MANIFEST_FILE),
                true,
            ),
        ];
        if let Some(name) = veryl_metadata::component_crate_name(&crate_dir) {
            paths.push((
                veryl_metadata::sidecar_manifest_path(target_dir, &name),
                true,
            ));
        }
        if let Some(wasm) = &component.wasm {
            paths.push((root.join(wasm), false));
        }
        // The image embeds the runtime library selected by the compiler.
        // Match its native candidate, including a Cargo [lib].name override.
        // Only presence affects compilation: library contents are loaded anew
        // when executing the testbench.
        if let Some(name) = fs::read_to_string(crate_dir.join("Cargo.toml"))
            .ok()
            .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
            .and_then(|value| {
                value
                    .get("lib")
                    .and_then(|lib| lib.get("name"))
                    .and_then(toml::Value::as_str)
                    .or_else(|| {
                        value
                            .get("package")
                            .and_then(|package| package.get("name"))
                            .and_then(toml::Value::as_str)
                    })
                    .map(str::to_owned)
            })
        {
            let native = target_dir.join("release").join(format!(
                "{}{}{}",
                std::env::consts::DLL_PREFIX,
                name.replace('-', "_"),
                std::env::consts::DLL_SUFFIX
            ));
            field(hash, native.as_os_str().as_encoded_bytes());
            field(hash, &[u8::from(native.is_file())]);
        }
        for (path, track_mtime) in paths {
            field(hash, path.as_os_str().as_encoded_bytes());
            let content = dependency_hash(&path)?;
            field(
                hash,
                &serde_json::to_vec(&content).map_err(io::Error::other)?,
            );
            if track_mtime && content.is_some() {
                // The newest valid sidecar/committed manifest wins. A touch
                // alone can change the selected interface without changing bytes.
                let modified = fs::metadata(&path)?.modified()?;
                field(
                    hash,
                    &serde_json::to_vec(&modified).map_err(io::Error::other)?,
                );
            }
        }
    }
    Ok(())
}

impl BuildCache {
    pub(super) fn new(
        dir: &Path,
        opts: &Options,
        sources: &[(String, PathBuf)],
        metadata: &veryl_metadata::Metadata,
    ) -> Result<Self, CeloxHeliodorError> {
        // Source loading deliberately excludes dependency discovery. Resolve
        // the same namespaces and properties as the compiler before hashing.
        let mut metadata = metadata.clone();
        metadata.paths::<&Path>(&[], false, true)?;
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
        // Lockfile's active lock_table is skipped by serde; projects() reads
        // that table in stable order, including refreshed dependency properties.
        let resolved =
            serde_json::to_value((&metadata, metadata.lockfile.projects(), &metadata.pubfile))
                .map_err(io::Error::other)?;
        field(
            &mut hash,
            &serde_json::to_vec(&resolved).map_err(io::Error::other)?,
        );
        let root = metadata.project_path();
        component_inputs(
            &mut hash,
            &metadata.components,
            &root,
            &root.join("target/veryl-components"),
        )?;
        let mut dependencies = metadata.collect_dependency_components()?;
        dependencies.sort_by(|a, b| a.project.cmp(&b.project));
        for dependency in dependencies {
            field(&mut hash, dependency.project.as_bytes());
            component_inputs(
                &mut hash,
                &dependency.components,
                &dependency.root,
                &dependency.target_dir,
            )?;
        }
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
