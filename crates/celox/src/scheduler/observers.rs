//! Combinational observer activation in generated code. Snapshots cross the
//! host boundary once per scheduler invocation, not once per settle.
use super::*;
use crate::simulator::native_io::{self, ArenaHeader};
use celox_sir::native_memory::NativeMemoryOp as Memory;
use std::mem::offset_of;

struct Read {
    signal: AbsoluteAddr,
    lsb: usize,
    width: usize,
    planes: usize,
}

pub(super) struct Observers {
    reads: Vec<Vec<Read>>,
    pub(super) groups: Vec<usize>,
    sites: Vec<u32>,
    site_count: usize,
    deferred: bool,
}

impl Observers {
    pub(super) fn new(program: &LaidOutProgram) -> Self {
        let schema = &program.runtime.runtime_schema;
        let layout = program.layout();
        let mut groups = HashMap::default();
        Self {
            reads: schema
                .comb_observers
                .iter()
                .map(|observer| {
                    observer
                        .sensitivity
                        .iter()
                        .map(|atom| {
                            let signal = atom.id;
                            let width = atom
                                .access
                                .msb
                                .checked_sub(atom.access.lsb)
                                .map_or(0, |n| n + 1);
                            let wide = layout.widths[&signal] > 64
                                || layout.unpacked_arrays.contains_key(&signal)
                                || width == 0;
                            let mask = layout.four_state
                                && layout.is_4states.get(&signal).copied().unwrap_or(false);
                            Read {
                                signal,
                                lsb: atom.access.lsb,
                                width,
                                planes: if wide || mask { 2 } else { 1 },
                            }
                        })
                        .chain(observer.occurrence_counters.iter().map(|&signal| Read {
                            signal,
                            lsb: 0,
                            width: 32,
                            planes: 1,
                        }))
                        .collect()
                })
                .collect(),
            groups: schema
                .comb_observers
                .iter()
                .map(|observer| {
                    let next = groups.len();
                    *groups.entry(observer.activation_group).or_insert(next)
                })
                .collect(),
            sites: schema
                .comb_observers
                .iter()
                .map(|observer| observer.site_id)
                .collect(),
            site_count: schema.runtime_event_sites.len(),
            deferred: schema.comb_observers.iter().any(|observer| {
                schema.runtime_event_sites[observer.site_id as usize]
                    .deferred
                    .is_some()
            }),
        }
    }
}

impl Generator<'_> {
    fn observer_flags(&mut self) -> RegisterId {
        let offset = self.constant(
            celox_state_layout::STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET as u64,
            64,
        );
        self.native_load(Memory::ReadState, vec![offset])
    }

    fn clear_observer_flags(&mut self, observers: &Observers) {
        let flags = self.observer_flags();
        self.words(observers.site_count.div_ceil(8), |g, word| {
            let zero = g.constant(0, 64);
            g.arena_store(flags, word, zero);
        });
    }

    /// The encoding matches ObserverReads::pack, including a zero mask plane
    /// for wide two-state reads and logical (not physical array-stride) slices.
    fn observer_words(&mut self, read: &Read) -> Vec<RegisterId> {
        let mut values = Vec::new();
        let mut masks = Vec::new();
        for offset in (0..read.width).step_by(64) {
            let width = (read.width - offset).min(64);
            let unknowns = self.keeps_unknowns(read.signal);
            let mut value = self.load_state(read.signal, read.lsb + offset, width);
            if width < 64 {
                let padding = if unknowns {
                    self.b.alloc_logic(64 - width)
                } else {
                    self.b.alloc_bit(64 - width, false)
                };
                self.b
                    .emit(SIRInstruction::Imm(padding, SIRValue::new(0u64)));
                let padded = if unknowns {
                    self.b.alloc_logic(64)
                } else {
                    self.b.alloc_bit(64, false)
                };
                self.b
                    .emit(SIRInstruction::Concat(padded, vec![padding, value]));
                value = padded;
            }
            let (bits, mask) = if unknowns {
                let planes = self.b.alloc_bit(128, false);
                self.b.emit(SIRInstruction::Unary(
                    planes,
                    celox_design::UnaryOp::ToPlanes,
                    value,
                ));
                let bits = self.b.alloc_bit(64, false);
                let mask = self.b.alloc_bit(64, false);
                self.b.emit(SIRInstruction::Slice(bits, planes, 0, 64));
                self.b.emit(SIRInstruction::Slice(mask, planes, 64, 64));
                (bits, mask)
            } else {
                (value, self.constant(0, 64))
            };
            values.push(bits);
            if read.planes == 2 {
                masks.push(mask);
            }
        }
        values.extend(masks);
        values
    }

    pub(super) fn observers_before(&mut self, failed: celox_sir::BlockId) {
        let Some(observers) = self.observers else {
            return;
        };
        self.clear_observer_flags(observers);
        let pointer = self.arena_header_load(offset_of!(ArenaHeader, observers));
        let initial = self.arena_header_load(offset_of!(ArenaHeader, observer_initial));
        let initial = self.equals(initial, 1);
        let mut active = vec![initial; observers.groups.iter().copied().max().unwrap_or(0) + 1];
        let mut at = 0;
        for (reads, &group) in observers.reads.iter().zip(&observers.groups) {
            for read in reads {
                for current in self.observer_words(read) {
                    let index = self.constant(at, 64);
                    at += 1;
                    let before = self.arena_load(pointer, index);
                    let changed = self.binary(1, before, BinaryOp::Ne, current);
                    active[group] = self.binary(1, active[group], BinaryOp::Or, changed);
                }
            }
        }
        let zero = self.constant(0, 1);
        for (&site, &group) in observers.sites.iter().zip(&observers.groups) {
            self.b.emit(SIRInstruction::CombCaptureEnableIfChanged {
                old: zero,
                new: active[group],
                sites: vec![site],
            });
        }
        let zero = self.constant(0, 64);
        self.arena_header_store(offset_of!(ArenaHeader, observer_initial), zero);
        let one = self.constant(1, 64);
        self.arena_header_store(offset_of!(ArenaHeader, observer_evaluating), one);
        if observers.deferred {
            let time = self.load(self.slots.now, 64);
            self.service_call(
                self.observer_service.unwrap(),
                0,
                native_io::COMB_BEFORE,
                time,
                failed,
            );
        }
    }

    pub(super) fn observers_after(&mut self, failed: celox_sir::BlockId) {
        let Some(observers) = self.observers else {
            return;
        };
        if observers.deferred {
            let time = self.load(self.slots.now, 64);
            self.service_call(
                self.observer_service.unwrap(),
                0,
                native_io::COMB_AFTER,
                time,
                failed,
            );
        }
        let pointer = self.arena_header_load(offset_of!(ArenaHeader, observers));
        let mut at = 0;
        for reads in &observers.reads {
            for read in reads {
                for current in self.observer_words(read) {
                    let index = self.constant(at, 64);
                    at += 1;
                    self.arena_store(pointer, index, current);
                }
            }
        }
        self.clear_observer_flags(observers);
        let zero = self.constant(0, 64);
        self.arena_header_store(offset_of!(ArenaHeader, observer_evaluating), zero);
    }
}

#[cfg(all(test, target_arch = "x86_64", not(feature = "arm64-codegen")))]
mod tests {
    use crate::{KernelSchedule, SchedulerMode, Simulation};

    #[test]
    fn observers_of_host_clocked_rtl_need_no_process_kernel() {
        let source = r#"`timescale 1ns/1ns
module Top(input logic clk, output logic [7:0] count);
    initial count = 0;
    always_ff @(posedge clk) count <= count + 1;
    always_comb if (count[0]) $display("count %0d", count);
endmodule"#;
        for four_state in [false, true] {
            let mut sim = Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("compiled-host-observers.sv"))],
                "Top",
            )
            .four_state(four_state)
            .scheduler_mode(SchedulerMode::DifferentialRuns)
            .build_native()
            .unwrap();
            sim.add_clock("clk", 2, 1);
            sim.run_until(2).unwrap();
            let before = sim.scheduler_stats().reference_steps;
            sim.run_until(12).unwrap();
            assert_eq!(sim.scheduler_stats().reference_steps, before);
            assert!(sim.scheduler_stats().compiled_slots > 0);
            assert!(sim.scheduler_stats().checked_steps > 0);
            assert_eq!(sim.get(sim.signal("count")), 6u8.into());
        }
    }

    #[test]
    fn observers_remain_compiled_across_host_boundaries() {
        for body in [
            r#"always_comb begin
                if (count[0]) $display("count %0d", count);
                if (count[1]) $display("wide %h", wide[68:3]);
            end"#,
            r#"always_comb if (words[0][0]) $display("array %h", words[1]);"#,
            r#"always_comb assert #0 (count[0]) $display("deferred %0d", count);"#,
        ] {
            let source = format!(
                r#"`timescale 1ns/1ns
module Top;
    int count = 0;
    logic [72:0] wide = 0;
    logic [12:0] words[3];
    initial begin words[0] = 0; words[1] = 1; words[2] = 0; end
    always #1 begin
        count = count + 1;
        wide = (wide << 17) + count;
        words[0] = count; words[1] = count[1] ? 'x : count;
    end
    {body}
endmodule"#
            );
            for (four_state, hotspot) in
                [(false, false), (true, false), (false, true), (true, true)]
            {
                let mut sim = Simulation::from_sv_sources(
                    vec![(
                        source.as_str(),
                        std::path::Path::new("compiled-observers.sv"),
                    )],
                    "Top",
                )
                .four_state(four_state)
                .hotspot_jit(hotspot)
                .scheduler_mode(SchedulerMode::DifferentialRuns)
                .build_scheduler_test()
                .unwrap();
                sim.run_until(1).unwrap_or_else(|e| {
                    panic!("four_state={four_state}, hotspot={hotspot}: {e:?}")
                });
                let before = sim.scheduler_stats();
                assert!(
                    before
                        .kernels
                        .iter()
                        .all(|k| *k == KernelSchedule::Compiled),
                    "{before:?}"
                );
                for time in 2..=12 {
                    sim.run_until(time).unwrap();
                }
                let after = sim.scheduler_stats();
                assert_eq!(after.reference_steps, before.reference_steps, "{after:?}");
                assert!(
                    after.compiled_slots > before.compiled_slots && after.checked_steps > 0,
                    "{after:?}"
                );
                assert_eq!(sim.get(sim.signal("count")), 12u8.into());
            }
        }
    }

    #[test]
    fn combinational_finish_preserves_later_unit_writes() {
        use crate::ir::RegionedAbsoluteAddr;
        use crate::{NativeBackend, Simulator, SimulatorOptions};
        use celox_sir::{BinaryOp, SIRBuilder, SIRInstruction, SIROffset, SIRValue};
        let source = r#"module Top(input bit clk, output int tail);
            int q = 0;
            initial tail = 0;
            always_ff @(posedge clk) q <= q + 1;
            always_comb if (clk) $finish;
        endmodule"#;
        for four_state in [false, true] {
            for mode in [SchedulerMode::Reference, SchedulerMode::Compiled] {
                let options = SimulatorOptions {
                    four_state,
                    emit_triggers: true,
                    scheduler: mode,
                    ..Default::default()
                };
                let (mut program, warnings) = crate::compile_sv_to_sir(
                    &[(source, std::path::Path::new("comb-finish-phase.sv"))],
                    "Top",
                    &[],
                    &[],
                    four_state,
                    &Default::default(),
                    None,
                    None,
                    None,
                    None,
                    &[],
                    &options.optimize_options,
                )
                .unwrap();
                assert!(program.sir.eval_comb.iter().any(|unit| {
                    super::super::instructions(unit).any(|instruction| {
                        matches!(instruction, SIRInstruction::CombCaptureEvent { .. })
                    })
                }));
                let address = |name| {
                    RegionedAbsoluteAddr::from_absolute_addr(
                        celox_design::STABLE_REGION,
                        program.runtime.get_addr(&[], &[name]).unwrap(),
                    )
                };
                let mut b = SIRBuilder::new();
                let clk = b.alloc_bit(1, false);
                b.emit(SIRInstruction::Load(
                    clk,
                    address("clk"),
                    SIROffset::Static(0),
                    1,
                ));
                let zero = b.alloc_bit(31, false);
                b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u32)));
                let value = b.alloc_bit(32, false);
                b.emit(SIRInstruction::Concat(value, vec![zero, clk]));
                let bias = b.alloc_bit(32, false);
                b.emit(SIRInstruction::Imm(bias, SIRValue::new(41u32)));
                let sum = b.alloc_bit(32, false);
                b.emit(SIRInstruction::Binary(sum, value, BinaryOp::Add, bias));
                b.emit(SIRInstruction::Store(
                    address("tail"),
                    SIROffset::Static(0),
                    32,
                    sum,
                    vec![],
                    vec![],
                ));
                // Keep a separate final unit: an early finish check must not
                // skip its write after the preceding unit captured $finish.
                program.sir.eval_comb.push(b.flush_eu().unwrap());
                let mut program = program.into_laid_out(four_state);
                if mode == SchedulerMode::Compiled {
                    super::super::enable_native_services(&mut program);
                    super::super::attach(&mut program, true);
                    assert!(
                        program
                            .runtime
                            .runtime_schema
                            .scheduler
                            .as_ref()
                            .unwrap()
                            .unit
                            .is_some()
                    );
                }
                let backend = NativeBackend::new(&program, &options).unwrap();
                let mut sim = Simulator::with_backend_and_program_for_simulation(
                    backend,
                    program.into_runtime(),
                    warnings,
                );
                sim.apply_initial_values();
                let mut sim = Simulation::new(sim);
                sim.state.set_scheduler_mode(mode);
                sim.add_clock("clk", 4, 2);
                sim.run_until(1).unwrap();
                assert_eq!(sim.get(sim.signal("tail")), 41u32.into());
                let before = sim.scheduler_stats().reference_steps;
                sim.run_until(8).unwrap();
                assert!(sim.is_finished());
                assert_eq!(
                    sim.get(sim.signal("tail")),
                    42u32.into(),
                    "{mode:?}, four_state={four_state}"
                );
                if mode == SchedulerMode::Compiled {
                    assert_eq!(sim.scheduler_stats().reference_steps, before);
                }
            }
        }
    }

    #[test]
    fn observer_terminal_actions_preserve_their_activation() {
        for action in ["$finish;", "$fatal(1, \"comb fatal\");"] {
            let source = format!(
                r#"`timescale 1ns/1ns
module Top;
    int count = 0;
    always #1 count = count + 1;
    always_comb if (count == 4) {action}
endmodule"#
            );
            for four_state in [false, true] {
                for mode in [SchedulerMode::Reference, SchedulerMode::Compiled] {
                    let mut sim = Simulation::from_sv_sources(
                        vec![(
                            source.as_str(),
                            std::path::Path::new("compiled-comb-terminal.sv"),
                        )],
                        "Top",
                    )
                    .four_state(four_state)
                    .scheduler_mode(mode)
                    .build_native()
                    .unwrap();
                    sim.run_until(1).unwrap();
                    let before = sim.scheduler_stats().reference_steps;
                    let result = sim.run_until(12);
                    if action.contains("fatal") {
                        let error = result.unwrap_err();
                        assert!(error.to_string().contains("comb fatal"), "{error}");
                    } else {
                        result.unwrap();
                        assert!(sim.is_finished());
                    }
                    assert_eq!(sim.get(sim.signal("count")), 4u8.into());
                    assert_eq!(sim.time(), 4);
                    if mode == SchedulerMode::Compiled {
                        assert_eq!(sim.scheduler_stats().reference_steps, before);
                    }
                }
            }
        }
    }
}
