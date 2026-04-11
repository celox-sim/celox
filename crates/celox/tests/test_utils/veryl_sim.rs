//! Adapter that wraps `veryl_simulator::Simulator` with an API compatible
//! with Celox's `Simulator<B>`, so that the same test body produced by
//! `all_backends!` compiles for the Veryl reference backend.
#![allow(dead_code)]

use celox::{AddrLookupError, RuntimeErrorCode};
use num_bigint::BigUint;
use std::path::Path;
use veryl_analyzer::ir as air;
use veryl_analyzer::value::Value;
use veryl_analyzer::{Analyzer, Context, attribute_table, symbol_table};
use veryl_metadata::Metadata;
use veryl_parser::Parser;
use veryl_simulator::Simulator as VerylSim;
use veryl_simulator::ir::{Config, Event, build_ir};

// ---------------------------------------------------------------------------
// Handle types (Copy, like Celox's SignalRef / EventRef)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct VerylSignalRef(usize);

#[derive(Clone, Copy, Debug)]
pub struct VerylEventRef(usize);

// ---------------------------------------------------------------------------
// IO context (for `modify(|io| io.set(...))`)
// ---------------------------------------------------------------------------

pub struct VerylIOContext<'a> {
    sim: &'a mut VerylSim,
    names: &'a [String],
}

impl VerylIOContext<'_> {
    pub fn set<T: Copy>(&mut self, signal: VerylSignalRef, val: T) {
        let name = &self.names[signal.0];
        self.sim.set(name, t_to_value(val));
    }

    pub fn set_wide(&mut self, signal: VerylSignalRef, val: BigUint) {
        let name = &self.names[signal.0];
        let width = val.bits() as usize;
        self.sim
            .set(name, Value::new_biguint(val, width.max(1), false));
    }

    pub fn set_four_state(&mut self, _signal: VerylSignalRef, _val: BigUint, _mask: BigUint) {
        unimplemented!("four_state set not supported in veryl adapter");
    }
}

// ---------------------------------------------------------------------------
// Value conversion helpers
// ---------------------------------------------------------------------------

fn t_to_value<T: Copy>(val: T) -> Value {
    let size = std::mem::size_of::<T>();
    let width = size * 8;
    let mut payload = 0u64;
    unsafe {
        std::ptr::copy_nonoverlapping(
            &val as *const T as *const u8,
            &mut payload as *mut u64 as *mut u8,
            size.min(8),
        );
    }
    Value::new(payload, width, false)
}

fn value_to_biguint(v: Value) -> BigUint {
    v.payload().into_owned()
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

pub struct VerylSimAdapter {
    sim: VerylSim,
    /// Signal name table: VerylSignalRef(i) → names[i]
    names: Vec<String>,
    /// Event table: VerylEventRef(i) → events[i]
    events: Vec<Event>,
}

impl VerylSimAdapter {
    pub fn signal(&mut self, name: &str) -> VerylSignalRef {
        // Reuse existing entry if present
        if let Some(idx) = self.names.iter().position(|n| n == name) {
            return VerylSignalRef(idx);
        }
        let idx = self.names.len();
        self.names.push(name.to_string());
        VerylSignalRef(idx)
    }

    pub fn event(&mut self, port: &str) -> VerylEventRef {
        let ev = self
            .sim
            .get_clock(port)
            .unwrap_or_else(|| panic!("event '{port}' not found in veryl-simulator"));
        let idx = self.events.len();
        self.events.push(ev);
        VerylEventRef(idx)
    }

    pub fn modify<F>(&mut self, f: F) -> Result<(), RuntimeErrorCode>
    where
        F: FnOnce(&mut VerylIOContext<'_>),
    {
        // Split borrow: names is read-only, sim is mutated through ctx
        let names_ptr = &self.names as *const Vec<String>;
        let mut ctx = VerylIOContext {
            sim: &mut self.sim,
            names: unsafe { &*names_ptr },
        };
        f(&mut ctx);
        Ok(())
    }

    pub fn get(&mut self, signal: VerylSignalRef) -> BigUint {
        let name = &self.names[signal.0];
        if let Some(v) = self.sim.get(name) {
            return value_to_biguint(v);
        }
        if let Some(v) = self.sim.get_var(name) {
            return value_to_biguint(v);
        }
        panic!("signal '{name}' not found in veryl-simulator");
    }

    pub fn get_as<T: Default + Copy>(&mut self, signal: VerylSignalRef) -> T {
        let biguint = self.get(signal);
        let mut result = T::default();
        let bytes = biguint.to_bytes_le();
        let size = std::mem::size_of::<T>();
        let copy_len = bytes.len().min(size);
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                &mut result as *mut T as *mut u8,
                copy_len,
            );
        }
        result
    }

    pub fn get_four_state(&mut self, _signal: VerylSignalRef) -> (BigUint, BigUint) {
        unimplemented!("four_state get not supported in veryl adapter");
    }

    pub fn tick(&mut self, event: VerylEventRef) -> Result<(), RuntimeErrorCode> {
        self.sim.step(&self.events[event.0]);
        Ok(())
    }

    pub fn set<T: Copy>(&mut self, signal: VerylSignalRef, val: T) {
        let name = &self.names[signal.0];
        self.sim.set(name, t_to_value(val));
        self.sim.mark_comb_dirty();
    }

    pub fn set_wide(&mut self, signal: VerylSignalRef, val: BigUint) {
        let name = &self.names[signal.0];
        let width = val.bits() as usize;
        self.sim
            .set(name, Value::new_biguint(val, width.max(1), false));
        self.sim.mark_comb_dirty();
    }

    pub fn child_signal(&mut self, instance_path: &[(&str, usize)], var: &str) -> VerylSignalRef {
        let mut parts = Vec::new();
        for (name, _idx) in instance_path {
            parts.push(*name);
        }
        parts.push(var);
        let joined = parts.join(".");
        self.signal(&joined)
    }

    pub fn try_signal(&mut self, name: &str) -> Result<VerylSignalRef, AddrLookupError> {
        Ok(self.signal(name))
    }

    pub fn eval_comb(&mut self) -> Result<(), RuntimeErrorCode> {
        self.sim.ensure_comb_updated();
        Ok(())
    }

    pub fn try_event(&mut self, port: &str) -> Result<VerylEventRef, AddrLookupError> {
        self.sim
            .get_clock(port)
            .map(|ev| {
                let idx = self.events.len();
                self.events.push(ev);
                VerylEventRef(idx)
            })
            .ok_or_else(|| AddrLookupError::VariableNotFound {
                path: port.to_string(),
            })
    }

}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

pub fn build_veryl_adapter(sources: &[(&str, &Path)], top: &str) -> VerylSimAdapter {
    // Clear global tables (same as Celox does)
    symbol_table::clear();
    attribute_table::clear();

    let metadata = Metadata::create_default("prj").unwrap();
    let analyzer = Analyzer::new(&metadata);

    let mut parsers = Vec::new();
    for (code, path) in sources {
        let parsed = Parser::parse(code, path).unwrap();
        analyzer.analyze_pass1("prj", &parsed.veryl);
        parsers.push(parsed);
    }

    Analyzer::analyze_post_pass1();

    let mut context = Context::default();
    let mut ir = air::Ir::default();
    for parsed in &parsers {
        analyzer.analyze_pass2("prj", &parsed.veryl, &mut context, Some(&mut ir));
    }
    Analyzer::analyze_post_pass2();

    let top_id = veryl_parser::resource_table::insert_str(top);
    let config = Config {
        use_4state: false,
        use_jit: false,
        ..Default::default()
    };

    let sim_ir = build_ir(&ir, top_id, &config).unwrap_or_else(|e| {
        panic!("veryl-simulator build_ir failed: {e:?}");
    });

    let mut sim = VerylSim::new(sim_ir, None);
    sim.ensure_comb_updated();

    VerylSimAdapter {
        sim,
        names: Vec::new(),
        events: Vec::new(),
    }
}
