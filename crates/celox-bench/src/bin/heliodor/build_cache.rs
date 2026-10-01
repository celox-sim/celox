//! Local, opt-in cache of trusted native compiler artifacts.
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use celox::NativeProgramImage;
use celox_frontend_veryl::FileDependency;

use super::{CeloxHeliodorError, Options};

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedFileDependency {
    path: PathBuf,
    content_hash: Option<String>,
    fixed_location_hash: Option<String>,
}

fn location_hash(path: &Path) -> io::Result<String> {
    let physical = fs::canonicalize(path)?;
    Ok(blake3::hash(physical.as_os_str().as_encoded_bytes())
        .to_hex()
        .to_string())
}

const MAGIC: &[u8] = b"CELOX-BUILD-CACHE-2\n";

pub(super) struct BuildCache {
    path: PathBuf,
    root: PathBuf,
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

fn is_project_relative(path: &Path) -> bool {
    !path.components().any(|component| {
        matches!(
            component,
            std::path::Component::Prefix(_) | std::path::Component::RootDir
        )
    })
}

/// Preserve lookup symlinks below the project root. Only aliases of the root
/// itself are normalized, so changing a nested symlink is observed on reload.
pub(super) fn relative_path(path: &Path, root: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if let Ok(relative) = absolute.strip_prefix(root) {
        return Ok(relative.to_path_buf());
    }
    for ancestor in absolute.ancestors().collect::<Vec<_>>().into_iter().rev() {
        if ancestor == root || fs::canonicalize(ancestor).is_ok_and(|base| base == root) {
            return Ok(absolute.strip_prefix(ancestor).unwrap().to_path_buf());
        }
    }
    pathdiff::diff_paths(absolute, root)
        // On Windows, differing drive/UNC prefixes can produce Some(absolute).
        .filter(|path| is_project_relative(path))
        .ok_or_else(|| io::Error::other("cache path has no project-relative representation"))
}

pub(super) fn write_relative_image(
    image: &NativeProgramImage,
    root: &Path,
    output: &Path,
) -> Result<(), CeloxHeliodorError> {
    let mut image = image.clone();
    image.try_map_paths(|path| relative_path(path, root))?;
    image.write_container(output)?;
    Ok(())
}

pub(super) fn bind_path(path: &Path, root: &Path) -> PathBuf {
    if path.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(path)
    }
}

// Normalize only metadata fields declared as filesystem paths. Compiler
// arguments, descriptions, and other literal strings retain their exact value.
fn relative_metadata(value: &mut serde_json::Value, root: &Path) -> io::Result<()> {
    fn paths(value: &mut serde_json::Value, root: &Path) -> io::Result<()> {
        match value {
            serde_json::Value::String(text) if Path::new(text).is_absolute() => {
                *text = relative_path(Path::new(text), root)?
                    .to_string_lossy()
                    .into_owned();
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    paths(value, root)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let metadata = &mut value[0];
    for pointer in [
        "/build/source",
        "/build/sources",
        "/build/target/path",
        "/build/sourcemap_target/path",
        "/doc/path",
        "/test/include_files",
        "/test/waveform_target/path",
    ] {
        if let Some(value) = metadata.pointer_mut(pointer) {
            paths(value, root)?;
        }
    }
    if let Some(components) = metadata["components"].as_array_mut() {
        for component in components {
            paths(&mut component["path"], root)?;
            paths(&mut component["wasm"], root)?;
        }
    }
    if let Some(dependencies) = metadata["dependencies"].as_object_mut() {
        for dependency in dependencies.values_mut() {
            if let Some(entry) = dependency.as_object_mut() {
                for name in ["path", "git"] {
                    if let Some(value) = entry.get_mut(name) {
                        paths(value, root)?;
                    }
                }
            }
        }
    }
    if let Some(projects) = value[1].as_array_mut() {
        for project in projects {
            let source = &mut project["source"];
            if let Some(repository) = source.as_object_mut() {
                for name in ["url", "path", "override"] {
                    if let Some(value) = repository.get_mut(name) {
                        paths(value, root)?;
                    }
                }
            } else {
                paths(source, root)?;
            }
        }
    }
    Ok(())
}

fn component_inputs(
    hash: &mut blake3::Hasher,
    components: &[veryl_metadata::Component],
    root: &Path,
    target_dir: &Path,
    project_root: &Path,
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
            field(
                hash,
                relative_path(&native, project_root)?
                    .as_os_str()
                    .as_encoded_bytes(),
            );
            field(hash, &[u8::from(native.is_file())]);
        }
        for (path, track_mtime) in paths {
            field(
                hash,
                relative_path(&path, project_root)?
                    .as_os_str()
                    .as_encoded_bytes(),
            );
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
        let namespaces = metadata
            .paths::<&Path>(&[], false, true)?
            .into_iter()
            .map(|path| (path.src, path.prj))
            .collect::<fxhash::FxHashMap<_, _>>();
        let root = metadata.project_path();
        let mut hash = blake3::Hasher::new();
        field(&mut hash, MAGIC);
        // The exact compiler binary covers revisions, dirty builds, Cargo
        // features, target architecture, and changes to image serialization.
        field(&mut hash, file_hash(&std::env::current_exe()?)?.as_bytes());
        // x86 codegen selects instructions and its state-base strategy from
        // CPU and OS capabilities, which can change without rebuilding the runner.
        #[cfg(any(
            feature = "x86_64-codegen",
            all(target_arch = "x86_64", not(feature = "arm64-codegen"))
        ))]
        field(
            &mut hash,
            &[celox::native_backend::features::detected_image_feature_bits()],
        );
        field(
            &mut hash,
            relative_path(&std::env::current_dir()?, &root)?
                .as_os_str()
                .as_encoded_bytes(),
        );
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
        let mut resolved =
            serde_json::to_value((&metadata, metadata.lockfile.projects(), &metadata.pubfile))
                .map_err(io::Error::other)?;
        relative_metadata(&mut resolved, &root)?;
        field(
            &mut hash,
            &serde_json::to_vec(&resolved).map_err(io::Error::other)?,
        );
        component_inputs(
            &mut hash,
            &metadata.components,
            &root,
            &root.join("target/veryl-components"),
            &root,
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
                &root,
            )?;
        }
        for (source, path) in sources {
            field(
                &mut hash,
                relative_path(path, &root)?.as_os_str().as_encoded_bytes(),
            );
            // Match the compiler's exact source-to-namespace lookup. A supplied
            // external source can fall back to the root namespace after relocation.
            field(
                &mut hash,
                namespaces
                    .get(path)
                    .unwrap_or(&metadata.project.name)
                    .as_bytes(),
            );
            field(&mut hash, source.as_bytes());
        }
        Ok(Self {
            path: dir.join(format!("{}.cache", hash.finalize().to_hex())),
            root,
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
        let dependencies: Vec<CachedFileDependency> =
            serde_json::from_slice(manifest).map_err(io::Error::other)?;
        for dependency in dependencies {
            if !is_project_relative(&dependency.path) {
                return Err(invalid().into());
            }
            let path = bind_path(&dependency.path, &self.root);
            if dependency_hash(&path)? != dependency.content_hash {
                return Ok(None);
            }
            if let Some(expected) = dependency.fixed_location_hash
                && location_hash(&path)? != expected
            {
                return Ok(None);
            }
        }
        let mut image = NativeProgramImage::from_container_bytes(&rest[length..])?;
        image.try_map_paths(|path| {
            if !is_project_relative(path) {
                return Err(invalid());
            }
            Ok(bind_path(path, &self.root))
        })?;
        Ok(Some(image))
    }

    pub(super) fn store(&self, image: &NativeProgramImage, dependencies: &[FileDependency]) {
        if let Err(error) = self.try_store(image, dependencies) {
            eprintln!("build cache write skipped {}: {error}", self.path.display());
        }
    }

    fn try_store(
        &self,
        image: &NativeProgramImage,
        dependencies: &[FileDependency],
    ) -> Result<(), CeloxHeliodorError> {
        let dependencies = dependencies
            .iter()
            .map(|dependency| {
                Ok(CachedFileDependency {
                    path: relative_path(&dependency.path, &self.root)?,
                    content_hash: dependency_hash(&dependency.path)?,
                    fixed_location_hash: dependency
                        .fixed_location
                        .then(|| location_hash(&dependency.path))
                        .transpose()?,
                })
            })
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
        let mut image = image.clone();
        image.try_map_paths(|path| relative_path(path, &self.root))?;
        temporary.write_all(&image.to_container_bytes()?)?;
        temporary.persist(&self.path).map_err(|error| error.error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_paths_must_preserve_the_project_root_when_joined() {
        for path in ["", "src/test.veryl", "../dependency/mem.hex"] {
            assert!(is_project_relative(Path::new(path)));
        }
        assert!(!is_project_relative(&std::env::current_dir().unwrap()));
        #[cfg(windows)]
        for path in [
            r"C:\memory.hex",
            r"C:memory.hex",
            r"\memory.hex",
            r"\\host\share\memory.hex",
        ] {
            assert!(!is_project_relative(Path::new(path)));
        }
    }

    #[cfg(windows)]
    #[test]
    fn rejects_paths_on_other_drives_or_unc_shares() {
        for (path, root) in [
            (r"D:\memory.hex", r"C:\project"),
            (r"\\host\other\memory.hex", r"\\host\project\checkout"),
        ] {
            assert!(relative_path(Path::new(path), Path::new(root)).is_err());
        }
    }
}
