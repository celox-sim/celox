//! Linking the C functions a design calls through SystemVerilog DPI-C
//! imports.
//!
//! A compiled design calls each imported function through a table of
//! function addresses whose address the state header holds, so generated
//! code and native images contain no host addresses. The table is resolved
//! from [`DpiSymbols`] when a simulator is built.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use celox_design::ExternFunction;
use thiserror::Error;

/// Where the C functions of DPI-C imports are found.
///
/// A function registered by name is found first; otherwise each shared
/// library is searched in the order it was added.
#[derive(Clone, Default)]
pub struct DpiSymbols {
    functions: Vec<(String, usize)>,
    libraries: Vec<PathBuf>,
}

impl fmt::Debug for DpiSymbols {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DpiSymbols")
            .field(
                "functions",
                &self
                    .functions
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .field("libraries", &self.libraries)
            .finish()
    }
}

/// A failure to link the C functions of DPI-C imports.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DpiError {
    /// No registered function or library provides the C symbol.
    #[error("DPI-C import `{name}` is not provided by any registered function or library")]
    UnresolvedFunction { name: String },
    /// A shared library could not be loaded.
    #[error("cannot load DPI-C library `{}`: {reason}", path.display())]
    LibraryLoad { path: PathBuf, reason: String },
    /// Shared libraries cannot be loaded on this target.
    #[error("DPI-C libraries cannot be loaded on this target")]
    LibrariesUnsupported,
}

impl DpiSymbols {
    pub(crate) fn add_function(&mut self, name: String, address: usize) {
        self.functions.push((name, address));
    }

    pub(crate) fn add_library(&mut self, path: PathBuf) {
        self.libraries.push(path);
    }

    /// The addresses of `functions`, in order.
    pub(crate) fn resolve(
        &self,
        functions: &[ExternFunction],
    ) -> Result<ExternFunctionTable, DpiError> {
        let addresses = functions
            .iter()
            .map(|function| self.address(&function.name))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ExternFunctionTable {
            addresses: addresses.into(),
        })
    }

    fn address(&self, name: &str) -> Result<usize, DpiError> {
        if let Some(&(_, address)) = self.functions.iter().find(|(known, _)| known == name) {
            return Ok(address);
        }
        for path in &self.libraries {
            if let Some(address) = library_symbol(path, name)? {
                return Ok(address);
            }
        }
        Err(DpiError::UnresolvedFunction {
            name: name.to_string(),
        })
    }
}

/// The address of `name` in the shared library at `path`, if it defines it.
///
/// Libraries stay loaded for the life of the process: simulators may outlive
/// any scope, and unloading code that a simulator may still call is unsound.
#[cfg(feature = "host-runtime")]
fn library_symbol(path: &std::path::Path, name: &str) -> Result<Option<usize>, DpiError> {
    use crate::HashMap;
    use std::sync::{LazyLock, Mutex};

    static LIBRARIES: LazyLock<Mutex<HashMap<PathBuf, &'static libloading::Library>>> =
        LazyLock::new(|| Mutex::new(HashMap::default()));

    let library = {
        let mut libraries = LIBRARIES.lock().unwrap_or_else(|error| error.into_inner());
        match libraries.get(path) {
            Some(library) => *library,
            None => {
                // SAFETY: loading a library runs its initializers; the caller
                // chose to link it into the simulation.
                let library = unsafe { libloading::Library::new(path) }.map_err(|error| {
                    DpiError::LibraryLoad {
                        path: path.to_path_buf(),
                        reason: error.to_string(),
                    }
                })?;
                let library: &'static libloading::Library = Box::leak(Box::new(library));
                libraries.insert(path.to_path_buf(), library);
                library
            }
        }
    };
    // SAFETY: the symbol is only read as an address here; it is called with
    // the C signature its DPI-C import declares.
    let symbol = unsafe { library.get::<unsafe extern "C" fn()>(name.as_bytes()) };
    Ok(symbol.ok().map(|symbol| *symbol as usize))
}

#[cfg(not(feature = "host-runtime"))]
fn library_symbol(_path: &std::path::Path, _name: &str) -> Result<Option<usize>, DpiError> {
    Err(DpiError::LibrariesUnsupported)
}

/// The addresses of a design's extern functions, indexed like its
/// `ExternCall` instructions.
#[derive(Clone, Debug, Default)]
pub(crate) struct ExternFunctionTable {
    addresses: Arc<[usize]>,
}

impl ExternFunctionTable {
    /// The table's address, which the state header holds.
    pub(crate) fn as_ptr(&self) -> *const usize {
        self.addresses.as_ptr()
    }

    /// Call function `func` with C integer arguments and return its C integer
    /// result.
    ///
    /// # Safety
    ///
    /// The function must take `args.len()` integer arguments and return an
    /// integer or nothing, as its DPI-C import declares.
    pub(crate) unsafe fn call(&self, func: u32, args: &[u64]) -> u64 {
        // SAFETY: forwarded from the caller.
        unsafe { call_c(self.addresses[func as usize], args) }
    }
}

/// Call the C function at `address` with integer arguments.
///
/// Every argument is passed and the result read as a 64-bit integer. On the
/// supported C ABIs, an argument or result of a narrower integer type uses
/// the low bits of the same register, so this calls a function of any
/// integer signature correctly; the caller truncates the result.
///
/// # Safety
///
/// `address` must be a C function taking `args.len()` integer arguments.
unsafe fn call_c(address: usize, args: &[u64]) -> u64 {
    macro_rules! call {
        ($($arg:ident),*) => {{
            // SAFETY: forwarded from the caller.
            let function: extern "C" fn($($arg: u64),*) -> u64 =
                unsafe { std::mem::transmute(address) };
            let [$($arg),*] = args else { unreachable!() };
            function($(*$arg),*)
        }};
    }
    match args.len() {
        0 => call!(),
        1 => call!(a0),
        2 => call!(a0, a1),
        3 => call!(a0, a1, a2),
        4 => call!(a0, a1, a2, a3),
        5 => call!(a0, a1, a2, a3, a4),
        6 => call!(a0, a1, a2, a3, a4, a5),
        7 => call!(a0, a1, a2, a3, a4, a5, a6),
        8 => call!(a0, a1, a2, a3, a4, a5, a6, a7),
        9 => call!(a0, a1, a2, a3, a4, a5, a6, a7, a8),
        10 => call!(a0, a1, a2, a3, a4, a5, a6, a7, a8, a9),
        11 => call!(a0, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10),
        12 => call!(a0, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11),
        13 => call!(a0, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12),
        14 => call!(a0, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13),
        15 => call!(
            a0, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14
        ),
        16 => call!(
            a0, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14, a15
        ),
        count => panic!("an extern function takes at most 16 arguments, not {count}"),
    }
}
