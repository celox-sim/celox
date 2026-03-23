/// Cross-platform timing helper.
///
/// On native targets, delegates to `std::time::Instant`.
/// On wasm32, provides a no-op implementation (always returns zero duration).

#[cfg(not(target_arch = "wasm32"))]
pub fn now() -> std::time::Instant {
    std::time::Instant::now()
}

#[cfg(target_arch = "wasm32")]
pub fn now() -> WasmInstant {
    WasmInstant
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy)]
pub struct WasmInstant;

#[cfg(target_arch = "wasm32")]
impl WasmInstant {
    pub fn elapsed(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }
}
