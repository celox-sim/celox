// Fingerprint the compiled verifier independently of the embedded case corpus.
// Hashing the executable itself would invalidate every case after adding one.
#[cfg(not(feature = "external"))]
fn main() {}

#[cfg(feature = "external")]
fn main() {
    if let Err(error) = fingerprint::emit() {
        // An incomplete dependency graph must never enable result reuse.
        println!("cargo:warning=incremental verification disabled: {error}");
        println!("cargo:rustc-env=CELOX_VERIFICATION_BUILD_HASH=unavailable");
    }
}

#[cfg(feature = "external")]
mod fingerprint {
    use sha2::{Digest, Sha256};
    use std::collections::{BTreeMap, BTreeSet};
    use std::fmt::Write;
    use std::path::{Path, PathBuf};

    fn add(hash: &mut Sha256, bytes: &[u8]) {
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }

    fn file(hash: &mut Sha256, path: &Path, label: &str) -> std::io::Result<()> {
        println!("cargo:rerun-if-changed={}", path.display());
        add(hash, label.as_bytes());
        add(hash, &std::fs::read(path)?);
        Ok(())
    }

    fn sources(hash: &mut Sha256, root: &Path, directory: &Path, own: bool) -> std::io::Result<()> {
        println!("cargo:rerun-if-changed={}", directory.display());
        let mut paths: Vec<PathBuf> = std::fs::read_dir(directory)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<_>>()?;
        paths.sort();
        for path in paths {
            let relative = path.strip_prefix(root).unwrap();
            // Cases (including their resolved stdlib sources) are hashed per
            // result. Registry/git dependency revisions are covered by Cargo.
            if own
                && (relative.starts_with("src/veryl/cases") || relative.starts_with("src/sv/cases"))
            {
                continue;
            }
            if path.is_dir() {
                sources(hash, root, &path, own)?;
            } else {
                file(hash, &path, &relative.to_string_lossy())?;
            }
        }
        Ok(())
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "Cargo build inputs are unrelated to simulator diagnostics"
    )]
    pub fn emit() -> Result<(), Box<dyn std::error::Error>> {
        let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
        let mut features: Vec<_> = std::env::vars()
            .filter_map(|(name, _)| {
                name.strip_prefix("CARGO_FEATURE_")
                    .map(|feature| feature.to_lowercase().replace('_', "-"))
            })
            .collect();
        features.sort();
        // Limit resolution to the platform Cargo is actually building. Without
        // this, offline metadata requires unrelated Android/Windows packages
        // that a clean host-only CI build has never downloaded.
        // Failure disables reuse instead of trusting a partial fingerprint.
        let metadata = std::process::Command::new(std::env::var("CARGO")?)
            .args([
                "metadata",
                "--offline",
                "--locked",
                "--format-version",
                "1",
                "--features",
            ])
            .arg(features.join(","))
            .arg("--filter-platform")
            .arg(std::env::var("TARGET")?)
            .current_dir(&manifest)
            .output()?;
        if !metadata.status.success() {
            return Err(String::from_utf8_lossy(&metadata.stderr)
                .into_owned()
                .into());
        }
        let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout)?;
        let packages: BTreeMap<_, _> = metadata["packages"]
            .as_array()
            .ok_or("missing packages")?
            .iter()
            .map(|package| (package["id"].as_str().unwrap(), package))
            .collect();
        let nodes: BTreeMap<_, _> = metadata["resolve"]["nodes"]
            .as_array()
            .ok_or("missing dependency graph")?
            .iter()
            .map(|node| (node["id"].as_str().unwrap(), node))
            .collect();
        let root_id = packages
            .iter()
            .find(|(_, package)| {
                package["manifest_path"]
                    .as_str()
                    .is_some_and(|path| Path::new(path) == manifest.join("Cargo.toml"))
            })
            .map(|(id, _)| *id)
            .ok_or("missing suite package")?;
        let mut pending = vec![root_id];
        let mut visited = BTreeSet::new();
        let mut hash = Sha256::new();
        add(&mut hash, b"celox-verification-build-v1");
        while let Some(id) = pending.pop() {
            if !visited.insert(id) {
                continue;
            }
            let package = packages.get(id).ok_or("missing dependency package")?;
            let node = nodes.get(id).ok_or("missing dependency node")?;
            add(&mut hash, package["name"].as_str().unwrap().as_bytes());
            add(&mut hash, package["version"].as_str().unwrap().as_bytes());
            add(&mut hash, package["source"].to_string().as_bytes());
            add(&mut hash, node["features"].to_string().as_bytes());
            for dep in node["deps"].as_array().ok_or("missing dependencies")? {
                if dep["dep_kinds"]
                    .as_array()
                    .ok_or("missing dependency kinds")?
                    .iter()
                    .any(|kind| kind["kind"].as_str() != Some("dev"))
                {
                    pending.push(dep["pkg"].as_str().ok_or("missing dependency id")?);
                }
            }
            if package["source"].is_null() {
                let root = Path::new(package["manifest_path"].as_str().unwrap())
                    .parent()
                    .unwrap();
                file(&mut hash, &root.join("Cargo.toml"), "Cargo.toml")?;
                if root.join("build.rs").exists() {
                    file(&mut hash, &root.join("build.rs"), "build.rs")?;
                }
                if root.join("src").exists() {
                    sources(&mut hash, root, &root.join("src"), id == root_id)?;
                }
            }
        }
        let workspace = Path::new(
            metadata["workspace_root"]
                .as_str()
                .ok_or("missing workspace root")?,
        );
        for name in [
            "Cargo.lock",
            "Cargo.toml",
            ".cargo/config.toml",
            ".cargo/config",
        ] {
            let path = workspace.join(name);
            println!("cargo:rerun-if-changed={}", path.display());
            if path.exists() {
                file(&mut hash, &path, name)?;
            }
        }
        for name in [
            "TARGET",
            "PROFILE",
            "OPT_LEVEL",
            "CARGO_ENCODED_RUSTFLAGS",
            "RUSTC",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
        ] {
            println!("cargo:rerun-if-env-changed={name}");
            add(&mut hash, name.as_bytes());
            add(
                &mut hash,
                std::env::var(name).unwrap_or_default().as_bytes(),
            );
        }
        let rustc = std::process::Command::new(std::env::var("RUSTC")?)
            .args(["--version", "--verbose"])
            .output()?;
        if !rustc.status.success() {
            return Err("could not identify Rust compiler".into());
        }
        add(&mut hash, &rustc.stdout);
        let mut digest = String::with_capacity(64);
        for byte in hash.finalize() {
            write!(&mut digest, "{byte:02x}")?;
        }
        println!("cargo:rustc-env=CELOX_VERIFICATION_BUILD_HASH={digest}");
        Ok(())
    }
}
