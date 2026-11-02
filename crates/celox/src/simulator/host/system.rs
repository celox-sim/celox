use super::*;
use celox_design::{HostRequestSite, ValueKind};
use celox_runtime::{Halt, SimulatorErrorCode};

impl<B: SimBackend> Simulator<B> {
    /// IEEE 1800-2023 20.17.1: preserve the C system() return value,
    /// including its platform-specific exit-status encoding.
    pub(crate) fn run_system_request(
        &mut self,
        site: &HostRequestSite<SignalRef>,
    ) -> Result<(), Halt> {
        let command = site.args.first().map(|arg| {
            if arg.value_kind == ValueKind::StrRef {
                let handle = self.backend.get(arg.id).to_u32_digits();
                self.backend
                    .heap()
                    .string(handle.first().copied().unwrap_or(0))
                    .to_vec()
            } else {
                self.backend
                    .get(arg.id)
                    .to_bytes_be()
                    .into_iter()
                    .filter(|&byte| byte != 0)
                    .collect()
            }
        });
        let result = execute(command.as_deref()).map_err(|message| {
            Halt::Error(SimulatorErrorCode::Runtime {
                message,
                signals: Vec::new(),
            })
        })?;
        self.backend.set(site.results[0].id, result as u32);
        self.dirty = true;
        Ok(())
    }
}

#[cfg(not(target_family = "wasm"))]
fn execute(command: Option<&[u8]>) -> Result<i32, String> {
    unsafe extern "C" {
        fn system(command: *const std::ffi::c_char) -> std::ffi::c_int;
    }
    let command = command
        .map(std::ffi::CString::new)
        .transpose()
        .map_err(|_| "$system command contains an embedded NUL byte".to_owned())?;
    // SAFETY: the pointer is NULL for an omitted argument, otherwise points
    // to a live, NUL-terminated command for the duration of system().
    Ok(unsafe { system(command.as_ref().map_or(std::ptr::null(), |s| s.as_ptr())) })
}

#[cfg(target_family = "wasm")]
fn execute(_command: Option<&[u8]>) -> Result<i32, String> {
    Err("$system is unavailable on a WebAssembly host".to_owned())
}
