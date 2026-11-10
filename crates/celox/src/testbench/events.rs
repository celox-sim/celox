//! Assertion and display events of a running testbench.

use super::*;

pub(super) struct DrainedAssertionEvents {
    pub(super) last_message: Option<Vec<u8>>,
    pub(super) fatal_message: Option<Vec<u8>>,
    /// The design executed `$finish`, which ends the run (IEEE 1800-2023 20.2).
    pub(super) finished: bool,
}

/// Forward testbench `$display` / `$write` output without contaminating the
/// machine-readable test result emitted by the CLI on stdout.  In particular,
/// UART models commonly use one `$write("%c", ...)` per transmitted byte, so
/// preserve the event bytes verbatim and flush it promptly.
pub(super) fn forward_display(message: &[u8], newline: bool) {
    use std::io::Write as _;

    let mut stderr = std::io::stderr().lock();
    let _ = stderr.write_all(message);
    if newline {
        let _ = stderr.write_all(b"\n");
    }
    let _ = stderr.flush();
}

pub(super) fn publish_tb_assert_event<B: SimBackend>(
    sim: &mut Simulator<B>,
    site_id: u32,
    message: &Option<AssertMessage>,
) {
    let args = assert_event_args(message)
        .iter()
        .map(|arg| {
            let (value, mask) = eval_expr_four_state(sim, &arg.expr);
            (value.to_biguint(), mask)
        })
        .collect::<Vec<_>>();
    let layout = sim.layout();
    let Some(site_layout) = layout
        .runtime_event_site_layouts
        .get(site_id as usize)
        .cloned()
    else {
        return;
    };
    let capacity = layout.runtime_event_capacity;
    if capacity == 0 {
        return;
    }
    let slot_size = layout.runtime_event_slot_size;
    let (event_ptr_const, buffer_size) = sim.backend_ref().runtime_event_buffer_as_ptr();
    if RUNTIME_EVENT_HEADER_SIZE > buffer_size {
        return;
    }
    let event_ptr = event_ptr_const as *mut u8;
    let seq = load_u64_acquire(event_ptr_const, 0);
    let slot = (seq as usize) & (capacity - 1);
    let slot_base = RUNTIME_EVENT_HEADER_SIZE + slot * slot_size;
    if slot_base + slot_size > buffer_size {
        return;
    }

    store_u64_release(
        event_ptr,
        slot_base + RUNTIME_EVENT_SLOT_SEQ_OFFSET,
        RUNTIME_EVENT_WRITING,
    );
    write_u64_volatile(
        event_ptr,
        slot_base + RUNTIME_EVENT_SLOT_SITE_OFFSET,
        site_id as u64,
    );
    let arg_count = args.len().min(site_layout.args.len());
    write_u64_volatile(
        event_ptr,
        slot_base + RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET,
        arg_count as u64,
    );

    for (idx, (value, mask)) in args.iter().take(arg_count).enumerate() {
        let arg_layout = &site_layout.args[idx];
        let words = value.to_u64_digits();
        let mask_words = mask.to_u64_digits();
        for word_idx in 0..arg_layout.word_count {
            let value_word = words.get(word_idx).copied().unwrap_or(0);
            write_u64_volatile(
                event_ptr,
                slot_base
                    + RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
                    + (arg_layout.value_word_offset + word_idx) * 8,
                value_word,
            );
            write_u64_volatile(
                event_ptr,
                slot_base
                    + RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
                    + (arg_layout.mask_word_offset + word_idx) * 8,
                mask_words.get(word_idx).copied().unwrap_or(0),
            );
        }
    }

    store_u64_release(event_ptr, slot_base + RUNTIME_EVENT_SLOT_SEQ_OFFSET, seq);
    store_u64_release(event_ptr, 0, seq.wrapping_add(1));
}

pub(super) fn drain_runtime_assertions<B: SimBackend>(
    sim: &mut Simulator<B>,
    ctx: &mut DetailedExecContext,
    location: Option<&SourceLocation>,
) -> DrainedAssertionEvents {
    let format_ctx = RuntimeFormatContext {
        tb_time: Some(ctx.simulation_time()),
        scope: None,
    };
    let events = sim.drain_runtime_events_deferred_with_context(format_ctx);
    record_runtime_events(
        ctx,
        events.into_iter().map(|event| (event, location.cloned())),
    )
}

/// Forward the messages of runtime events and record their failures, each
/// at its source location.
pub(super) fn record_runtime_events(
    ctx: &mut DetailedExecContext,
    events: impl IntoIterator<Item = (RuntimeEvent, Option<SourceLocation>)>,
) -> DrainedAssertionEvents {
    let mut last_message = None;
    let mut fatal_message = None;
    let mut finished = false;
    for (event, location) in events {
        match event {
            // Only errors fail a test (IEEE 1800-2023 20.10).
            RuntimeEvent::Report(report) if report.severity < Severity::Error => {
                forward_display(&report.message, true)
            }
            RuntimeEvent::Report(report) => {
                let message = report.message;
                last_message = Some(message.clone());
                if report.severity == Severity::Fatal && fatal_message.is_none() {
                    fatal_message = Some(message.clone());
                }
                ctx.assertions.push(AssertionResult {
                    passed: false,
                    message: Some(message),
                    location,
                });
            }
            RuntimeEvent::Missed { count } => {
                let message = format!("missed {count} runtime events").into_bytes();
                last_message = Some(message.clone());
                ctx.assertions.push(AssertionResult {
                    passed: false,
                    message: Some(message),
                    location: None,
                });
            }
            RuntimeEvent::Display { message } => forward_display(&message, true),
            RuntimeEvent::Write { message } => forward_display(&message, false),
            // The testbench has no interactive mode to suspend into.
            RuntimeEvent::Finish | RuntimeEvent::Stop => finished = true,
        }
    }
    DrainedAssertionEvents {
        last_message,
        fatal_message,
        finished,
    }
}
