use wasm_bindgen::prelude::*;

use celox::wasm_codegen;
use celox::{MemoryLayout, OptimizeOptions, Program};
// MemoryLayout imported for SimHandle::layout() return type

/// Initialize panic hook for better error messages in the browser console.
#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
}

/// A compiled simulation handle.
///
/// Compiles Veryl source to WASM bytecode and exposes metadata (layout, events,
/// hierarchy) so the JS side can instantiate and drive the simulation.
#[wasm_bindgen]
pub struct SimHandle {
    program: Program,
    four_state: bool,
}

impl SimHandle {
    fn layout(&self) -> &MemoryLayout {
        self.program.layout.as_ref().unwrap()
    }
}

#[wasm_bindgen]
impl SimHandle {
    /// Compile Veryl source code and produce a simulation handle.
    ///
    /// `source` is the Veryl source text, `top` is the top-level module name.
    #[wasm_bindgen(constructor)]
    pub fn new(source: &str, top: &str) -> Result<SimHandle, JsError> {
        let trace_opts = celox::TraceOptions::default();
        let optimize_options = OptimizeOptions::default();

        let (mut program, _warnings) = celox::compile_to_sir(
            &[(source, std::path::Path::new("input.veryl"))],
            top,
            &[],
            &[],
            false,
            &trace_opts,
            None,
            None,
            None,
            None,
            &[],
            &optimize_options,
        )
        .map_err(|e| JsError::new(&e.to_string()))?;

        program.build_layout(false);

        Ok(SimHandle {
            program,
            four_state: false,
        })
    }

    /// Returns the WASM module bytes for eval_comb (combinational logic evaluation).
    #[wasm_bindgen(js_name = "combWasmBytes")]
    pub fn comb_wasm_bytes(&self) -> Vec<u8> {
        let wasm = wasm_codegen::compile_units(
            &self.program.eval_comb,
            self.layout(),
            self.four_state,
            false,
        );
        wasm.bytes
    }

    /// Returns the WASM module bytes for a specific clock/reset event.
    ///
    /// `event_name` should match a clock or reset port name (e.g. "clk", "rst").
    #[wasm_bindgen(js_name = "eventWasmBytes")]
    pub fn event_wasm_bytes(&self, event_name: &str) -> Result<Vec<u8>, JsError> {
        // Search through eval_apply_ffs to find matching event
        for (addr, units) in &self.program.eval_apply_ffs {
            let event_path = self.program.get_path(addr);
            if event_path == event_name {
                let wasm = wasm_codegen::compile_units(units, self.layout(), self.four_state, false);
                return Ok(wasm.bytes);
            }
        }

        Err(JsError::new(&format!(
            "Event '{}' not found. Available events: {}",
            event_name,
            self.program
                .eval_apply_ffs
                .keys()
                .map(|addr| self.program.get_path(addr))
                .collect::<Vec<_>>()
                .join(", ")
        )))
    }

    /// Returns the signal layout as a JSON string.
    ///
    /// The layout maps signal paths to their memory offsets and widths.
    #[wasm_bindgen(js_name = "layoutJson")]
    pub fn layout_json(&self) -> String {
        use std::collections::BTreeMap;

        let mut layout_map: BTreeMap<String, serde_json::Value> = BTreeMap::new();

        for (instance_id, module_id) in &self.program.instance_module {
            let variables = &self.program.module_variables[module_id];
            let path_index = &self.program.module_var_path_index[module_id];

            for info in variables.values() {
                if path_index.get(&info.path) == Some(&None) {
                    continue;
                }
                let name = info
                    .path
                    .0
                    .iter()
                    .map(|s| {
                        veryl_parser::resource_table::get_str_value(*s)
                            .unwrap()
                            .to_string()
                    })
                    .collect::<Vec<_>>()
                    .join(".");

                let addr = celox::AbsoluteAddr {
                    instance_id: *instance_id,
                    var_id: info.id,
                };

                if let Some(&offset) = self.layout().offsets.get(&addr) {
                    let width = self.layout().widths.get(&addr).copied().unwrap_or(0);
                    let byte_size = celox::get_byte_size(width);
                    layout_map.insert(
                        name,
                        serde_json::json!({
                            "offset": offset,
                            "width": width,
                            "byteSize": byte_size,
                        }),
                    );
                }
            }
        }

        serde_json::to_string(&layout_map).unwrap_or_else(|_| "{}".to_string())
    }

    /// Returns the event name-to-ID mapping as a JSON string.
    #[wasm_bindgen(js_name = "eventsJson")]
    pub fn events_json(&self) -> String {
        use std::collections::BTreeMap;

        let mut events: BTreeMap<String, usize> = BTreeMap::new();
        let mut next_id = 0usize;

        for addr in self.program.eval_apply_ffs.keys() {
            let name = self.program.get_path(addr);
            events.insert(name, next_id);
            next_id += 1;
        }

        serde_json::to_string(&events).unwrap_or_else(|_| "{}".to_string())
    }

    /// Returns the stable region size in bytes.
    #[wasm_bindgen(js_name = "stableSize")]
    pub fn stable_size(&self) -> usize {
        self.layout().total_size
    }

    /// Returns the total memory size in bytes (stable + working + triggered bits + scratch).
    #[wasm_bindgen(js_name = "totalSize")]
    pub fn total_size(&self) -> usize {
        self.layout().merged_total_size
    }
}
