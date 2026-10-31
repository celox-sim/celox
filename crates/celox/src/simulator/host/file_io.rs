use super::*;
use celox_design::HostRequestSite;
use celox_runtime::{Halt, SimulatorErrorCode};

impl<B: SimBackend> Simulator<B> {
    /// The hierarchical name of the instance that runs `process`, which a
    /// `%m` of its host requests prints.
    pub(super) fn process_scope(&self, process: usize) -> Option<String> {
        let process = &self.program.runtime_schema.processes[process];
        self.program
            .design
            .instance(process.status.instance_id)
            .map(|instance| {
                let top = self
                    .program
                    .design
                    .root_instance()
                    .map(|root| root.module_name.as_str())
                    .unwrap_or("");
                std::iter::once(top)
                    .chain(instance.display_path.iter().map(String::as_str))
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(".")
            })
    }

    pub(crate) fn run_file_io_request(
        &mut self,
        site: &HostRequestSite<SignalRef>,
        timestamp: u64,
    ) -> Result<(), Halt> {
        if site.operation == 13 {
            return self.run_fread_request(site);
        }
        let error = |message: String| {
            Halt::Error(SimulatorErrorCode::Runtime {
                message,
                signals: Vec::new(),
            })
        };
        let values: Vec<_> = site
            .args
            .iter()
            .map(|arg| {
                if arg.id.is_4state {
                    self.backend.get_four_state(arg.id)
                } else {
                    (self.backend.get(arg.id), BigUint::default())
                }
            })
            .collect();
        let number = |index: usize| -> Result<u32, Halt> {
            let (value, mask) = &values[index];
            if *mask != BigUint::default() {
                return Err(error("file I/O descriptor/address contains X or Z".into()));
            }
            Ok(value.to_u32_digits().first().copied().unwrap_or(0))
        };
        let string = |index: usize| -> Result<String, Halt> {
            let (value, mask) = &values[index];
            if *mask != BigUint::default() {
                return Err(error("file I/O string contains X or Z".into()));
            }
            let bytes = value.to_bytes_be();
            let bytes = bytes
                .into_iter()
                .skip_while(|byte| *byte == 0)
                .collect::<Vec<_>>();
            String::from_utf8(bytes).map_err(|_| error("file I/O path or mode is not UTF-8".into()))
        };
        match site.operation {
            0 => {
                let path = string(0)?;
                let mode = if site.args.len() > 1 {
                    Some(string(1)?)
                } else {
                    None
                };
                let descriptor = self.file_io.open(path, mode.as_deref());
                self.backend.set(site.results[0].id, descriptor);
            }
            1 => self
                .file_io
                .close(number(0)?)
                .map_err(|err| error(format!("$fclose: {err}")))?,
            2 | 3 => {
                let args: Vec<_> = site
                    .args
                    .iter()
                    .zip(&values)
                    .skip(1)
                    .map(|(arg, (value, mask))| RuntimeEventArgValue {
                        values: value.to_u64_digits(),
                        masks: mask.to_u64_digits(),
                        width: arg.width,
                        signed: arg.signed,
                        is_string: arg.is_string,
                        float_precision: arg.float_precision,
                        handle: false,
                    })
                    .collect();
                let scope = self.process_scope(site.process);
                let format = RuntimeEventSite {
                    kind: RuntimeEventKind::Write,
                    template: site.strings.first().cloned(),
                    sizing: DisplaySizing::Ieee,
                    scope,
                    location: None,
                    assertion: None,
                    deferred: None,
                    timestamp_arg: false,
                    arg_widths: Vec::new(),
                    arg_signed: Vec::new(),
                    arg_is_string: Vec::new(),
                    arg_float_precision: Vec::new(),
                    arg_is_handle: Vec::new(),
                };
                let mut message = render_runtime_event_message(
                    &format,
                    &args,
                    RuntimeFormatContext {
                        tb_time: Some(timestamp),
                        ..Default::default()
                    },
                );
                if site.operation == 3 {
                    message.push(b'\n');
                }
                let (stdout, write_result) = self.file_io.write_with_stdout(number(0)?, &message);
                if stdout {
                    let mut format = format;
                    format.kind = if site.operation == 3 {
                        RuntimeEventKind::Display
                    } else {
                        RuntimeEventKind::Write
                    };
                    self.queue_host_runtime_event(format, args);
                }
                write_result.map_err(|err| error(format!("file output: {err}")))?;
            }
            6 => {
                let descriptor = if site.args.is_empty() {
                    None
                } else {
                    Some(number(0)?)
                };
                self.file_io
                    .flush(descriptor)
                    .map_err(|err| error(format!("$fflush: {err}")))?;
            }
            7..=12 => {
                let descriptor = number(usize::from(site.operation == 11))?;
                let code: i32 = match site.operation {
                    7 => self
                        .file_io
                        .tell(descriptor)
                        .ok()
                        .and_then(|position| i32::try_from(position).ok())
                        .unwrap_or(-1),
                    8 => {
                        let offset = number(1)? as i32;
                        let operation = number(2)? as i32;
                        if self
                            .file_io
                            .seek(descriptor, i64::from(offset), operation)
                            .is_ok()
                        {
                            0
                        } else {
                            -1
                        }
                    }
                    9 => {
                        if self.file_io.rewind(descriptor).is_ok() {
                            0
                        } else {
                            -1
                        }
                    }
                    10 => self
                        .file_io
                        .read_byte(descriptor)
                        .ok()
                        .flatten()
                        .map_or(-1, i32::from),
                    11 => {
                        let character = number(0)?;
                        if character != u32::MAX
                            && self
                                .file_io
                                .unread_byte(descriptor, character as u8)
                                .is_ok()
                        {
                            0
                        } else {
                            -1
                        }
                    }
                    12 => i32::from(self.file_io.eof(descriptor).unwrap_or(false)),
                    _ => unreachable!(),
                };
                self.backend.set(site.results[0].id, code);
            }
            4 | 5 => {
                let path = string(0)?;
                let left: i128 = site.strings[0]
                    .parse()
                    .map_err(|_| error("invalid memory metadata".into()))?;
                let right: i128 = site.strings[1]
                    .parse()
                    .map_err(|_| error("invalid memory metadata".into()))?;
                let width: usize = site.strings[2]
                    .parse()
                    .map_err(|_| error("invalid memory metadata".into()))?;
                let address = |index: usize| -> Result<i128, Halt> {
                    let (value, mask) = &values[index];
                    if *mask != BigUint::default() {
                        return Err(error("$writemem address contains X or Z".into()));
                    }
                    let mut value = num_bigint::BigInt::from(value.clone());
                    let arg = &site.args[index];
                    if arg.signed && arg.width > 0 && value.bit(arg.width as u64 - 1) {
                        value -= num_bigint::BigInt::from(1u8) << arg.width;
                    }
                    value
                        .try_into()
                        .map_err(|_| error("$writemem address exceeds supported range".into()))
                };
                let start = if site.args.len() > 2 {
                    address(2)?
                } else {
                    left.min(right)
                };
                let finish = if site.args.len() > 3 {
                    address(3)?
                } else {
                    left.max(right)
                };
                if start < left.min(right)
                    || start > left.max(right)
                    || finish < left.min(right)
                    || finish > left.max(right)
                {
                    return Err(error("$writemem address outside array bounds".into()));
                }
                let mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
                let words: Vec<_> = (0..=start.abs_diff(finish))
                    .map(|offset| {
                        let index = if start <= finish {
                            start + offset as i128
                        } else {
                            start - offset as i128
                        };
                        let shift = index.abs_diff(left) as usize * width;
                        (
                            (&values[1].0 >> shift) & &mask,
                            (&values[1].1 >> shift) & &mask,
                        )
                    })
                    .collect();
                self.file_io
                    .write_memory_with_backend(
                        path,
                        site.operation == 5,
                        words.iter().map(|(value, mask)| DisplayFormatArg {
                            value,
                            mask: Some(mask),
                            width,
                            signed: false,
                            is_string: false,
                            float_precision: None,
                        }),
                    )
                    .map_err(|err| error(format!("$writemem: {err}")))?;
            }
            _ => return Err(error("unknown file I/O request".into())),
        }
        Ok(())
    }
}

#[cfg(all(
    test,
    feature = "systemverilog",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod tests {
    #[test]
    fn detached_drain_includes_host_messages_in_order_without_replaying_records() {
        let source = r#"module Top;
          initial begin
            $display("before");
            $fwrite(1, "host");
            $display("after");
          end
        endmodule"#;
        let mut simulation = crate::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("drain.sv"))],
            "Top",
        )
        .build_native()
        .unwrap();
        let mut drain = simulation.simulator.runtime_event_drain().unwrap();
        simulation.step().unwrap();
        assert_eq!(
            drain.drain(),
            vec![
                crate::RuntimeEvent::Display {
                    message: b"before".to_vec()
                },
                crate::RuntimeEvent::Write {
                    message: b"host".to_vec()
                },
                crate::RuntimeEvent::Display {
                    message: b"after".to_vec()
                },
            ]
        );
        drop(drain);
        assert!(simulation.drain_runtime_events().is_empty());
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod queue_tests {
    use super::*;
    use std::path::Path;

    const CONTEXT_SOURCE: &str = r#"module Top;
      timeunit 1ps; timeprecision 1ps;
      initial begin
        #7;
        $display("%m:%0t", $time);
        $fwrite(1, "host:%0t,", $time);
        $display("%m:%0t", $time);
      end
    endmodule"#;

    fn late_context<B: SimBackend>(build: impl Fn(&str) -> crate::Simulation<B>) {
        for veryl_context in [false, true] {
            for owned in [false, true] {
                let mut simulation = build(CONTEXT_SOURCE);
                if veryl_context {
                    // The legacy formatter consumes time from the drain context;
                    // IEEE %t instead consumes the captured expression operand.
                    for site in &mut simulation
                        .simulator
                        .program
                        .runtime_schema
                        .runtime_event_sites
                    {
                        site.sizing = DisplaySizing::Veryl;
                        site.template = Some("%m:%t".into());
                    }
                }
                let mut drain = if owned {
                    simulation.simulator.runtime_event_drain()
                } else {
                    None
                };
                if owned && drain.is_none() {
                    continue;
                }
                simulation.run_until(7).unwrap();
                let context = RuntimeFormatContext {
                    scope: Some("caller"),
                    tb_time: Some(999),
                };
                let events = if let Some(drain) = &mut drain {
                    drain.drain_with_context(context)
                } else {
                    simulation
                        .simulator
                        .drain_runtime_events_with_context(context)
                };
                let ordinary = if veryl_context {
                    b"caller:999".to_vec()
                } else {
                    b"caller:7".to_vec()
                };
                assert_eq!(
                    events,
                    vec![
                        RuntimeEvent::Display {
                            message: ordinary.clone()
                        },
                        RuntimeEvent::Write {
                            message: b"host:7,".to_vec()
                        },
                        RuntimeEvent::Display { message: ordinary },
                    ]
                );
                drop(drain);
                assert!(simulation.drain_runtime_events().is_empty());
            }
        }
    }

    fn bounded_output<B: SimBackend>(build: impl Fn(&str) -> crate::Simulation<B>) {
        let capacity = celox_state_layout::RUNTIME_EVENT_CAPACITY;
        for owned in [false, true] {
            for scenario in 0..3 {
                let count = if scenario == 1 {
                    capacity / 2 + 7
                } else {
                    capacity + 7
                };
                let body = match scenario {
                    0 => "$fwrite(1, \"%0d,\", i);",
                    1 => "$display(\"%0d\", i * 2); $fwrite(1, \"%0d,\", i * 2 + 1);",
                    _ => "$display(\"%0d\", i);",
                };
                let prefix = if scenario == 2 {
                    "$fwrite(1, \"old\");"
                } else {
                    ""
                };
                let source = format!(
                    "module Top; initial begin {prefix} for (int i = 0; i < {count}; i++) begin {body} end end endmodule"
                );
                let mut simulation = build(&source);
                let mut drain = if owned {
                    simulation.simulator.runtime_event_drain()
                } else {
                    None
                };
                if owned && drain.is_none() {
                    continue;
                }
                simulation.run_until(0).unwrap();
                let events = if let Some(drain) = &mut drain {
                    drain.drain()
                } else {
                    simulation.drain_runtime_events()
                };
                let total = if scenario == 1 {
                    count * 2
                } else {
                    count + usize::from(scenario == 2)
                };
                let missed = total - capacity;
                assert_eq!(
                    events.first(),
                    Some(&RuntimeEvent::Missed {
                        count: missed as u64
                    })
                );
                assert_eq!(events.len(), capacity + 1);
                for (offset, event) in events.iter().skip(1).enumerate() {
                    let index = missed + offset;
                    let expected = match scenario {
                        0 => RuntimeEvent::Write {
                            message: format!("{index},").into_bytes(),
                        },
                        1 if index % 2 == 1 => RuntimeEvent::Write {
                            message: format!("{index},").into_bytes(),
                        },
                        1 => RuntimeEvent::Display {
                            message: index.to_string().into_bytes(),
                        },
                        _ => RuntimeEvent::Display {
                            message: (index - 1).to_string().into_bytes(),
                        },
                    };
                    assert_eq!(event, &expected, "scenario {scenario}, record {index}");
                }
                drop(drain);
                assert!(simulation.drain_runtime_events().is_empty());
            }
        }
    }

    macro_rules! regressions {
        ($name:ident, $backend:ident) => {
            mod $name {
                use super::*;
                #[test]
                fn context_is_applied_at_the_eventual_drain() {
                    late_context(|source| {
                        crate::Simulation::from_sv_sources(
                            vec![(source, Path::new("queue_context.sv"))],
                            "Top",
                        )
                        .$backend()
                        .unwrap()
                    });
                }
                #[test]
                fn stdout_and_normal_events_share_overwrite_capacity() {
                    bounded_output(|source| {
                        crate::Simulation::from_sv_sources(
                            vec![(source, Path::new("queue_capacity.sv"))],
                            "Top",
                        )
                        .$backend()
                        .unwrap()
                    });
                }
            }
        };
    }
    regressions!(interpreter, build_interpreter);
    #[cfg(feature = "cranelift")]
    regressions!(cranelift, build_cranelift);
    #[cfg(feature = "wasm-runtime")]
    regressions!(wasm, build_wasm);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    regressions!(native, build_native);
}
