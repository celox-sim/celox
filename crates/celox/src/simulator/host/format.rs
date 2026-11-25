use super::*;
use celox_design::{FloatPrecision, HostRequestSite, ValueKind};
use celox_runtime::Halt;

impl<B: SimBackend> Simulator<B> {
    /// `$sformatf` (IEEE 1800-2023 21.3.3): the result is what `$write`
    /// prints for the same format and arguments.
    pub(crate) fn run_format_request(
        &mut self,
        site: &HostRequestSite<SignalRef>,
        timestamp: u64,
    ) -> Result<(), Halt> {
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
        let process_scope = self.process_scope(site.process.index());
        let message = format_message(site, &values, process_scope, timestamp, self.backend.heap());
        let handle = self.backend.heap_mut().intern(&message);
        self.backend.set(site.results[0].id, handle);
        self.dirty = true;
        Ok(())
    }
}

/// The text `$sformatf` request `site` makes of the values of its arguments
/// (`values`, value and unknown planes), in a process whose `%m` names
/// `process_scope`, at `timestamp`: what `$write` prints for the same format
/// and arguments (IEEE 1800-2023 21.3.3).
pub(crate) fn format_message(
    site: &HostRequestSite<SignalRef>,
    values: &[(BigUint, BigUint)],
    process_scope: Option<String>,
    timestamp: u64,
    heap: &celox_heap::Heap,
) -> Vec<u8> {
    let mut args: Vec<_> = site
        .args
        .iter()
        .zip(values)
        .map(|(arg, (value, mask))| {
            let mut value = RuntimeEventArgValue {
                values: value.to_u64_digits(),
                masks: mask.to_u64_digits(),
                width: arg.width,
                signed: arg.signed,
                is_string: false,
                float_precision: arg.float_precision,
                handle: arg.value_kind == ValueKind::StrRef,
            };
            value.resolve_handle(heap);
            value
        })
        .collect();
    // The format of `%t` the design's `$timeformat` calls set when the call
    // ran, its last argument.
    let time_format = if site
        .strings
        .get(1)
        .is_some_and(|text| text == celox_design::TIME_FORMAT_ARGUMENT)
    {
        args.pop()
            .map_or_else(TimeFormat::default, |format| TimeFormat::of(&format))
    } else {
        TimeFormat::default()
    };
    let template = if site.operation == 0 {
        site.strings[0].clone().into_bytes()
    } else {
        // A run-time format is the text of its value: the bytes of a string,
        // or of an integral value without its NUL bytes, unknown bits reading
        // as zero. Bytes that are not UTF-8 stay as they are.
        let format = args.remove(0);
        let bytes = runtime_event_words_to_biguint(&format.values, format.width)
            .to_bytes_be()
            .into_iter()
            .filter(|&byte| byte != 0)
            .collect::<Vec<_>>();
        let ticks_per_unit = site.strings[0].parse::<u64>().unwrap_or(1);
        scale_time_arguments(&String::from_utf8_lossy(&bytes), &mut args, ticks_per_unit);
        bytes
    };
    // A run-time format's `%m` names the scope of the call: a named block,
    // generate block or subroutine under the instance that runs it, or a
    // scope of a package or the compilation unit.
    let scope = match &site.scope {
        Some(celox_design::FormatScope::Absolute(name)) => Some(name.clone()),
        Some(celox_design::FormatScope::Relative(path)) => Some(match process_scope {
            Some(instance) => format!("{instance}.{path}"),
            None => path.clone(),
        }),
        None => process_scope,
    };
    let format = RuntimeEventSite {
        kind: RuntimeEventKind::Write,
        template: None,
        sizing: DisplaySizing::Ieee,
        scope,
        location: None,
        assertion: None,
        deferred: None,
        timestamp_arg: false,
        time_format_arg: false,
        arg_widths: Vec::new(),
        arg_signed: Vec::new(),
        arg_is_string: Vec::new(),
        arg_float_precision: Vec::new(),
        arg_is_handle: Vec::new(),
        home: None,
    };
    render_bytes(
        &template,
        format,
        &args,
        RuntimeFormatContext {
            tb_time: Some(timestamp),
            ..Default::default()
        },
        &time_format,
    )
}

/// What `$write` prints for `template` and `args`. Text outside conversions
/// is copied byte for byte, and each conversion, which is ASCII, is rendered
/// as `$display` renders it.
fn render_bytes(
    template: &[u8],
    mut site: RuntimeEventSite,
    args: &[RuntimeEventArgValue],
    ctx: RuntimeFormatContext<'_>,
    time_format: &TimeFormat,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(template.len());
    let mut position = 0;
    let mut index = 0;
    while position < template.len() {
        if template[position] != b'%' {
            let end = template[position..]
                .iter()
                .position(|&byte| byte == b'%')
                .map_or(template.len(), |offset| position + offset);
            out.extend_from_slice(&template[position..end]);
            position = end;
            continue;
        }
        // The flags, width and precision `FieldSpec::parse` reads, then the
        // conversion character.
        let start = position;
        position += 1;
        while template
            .get(position)
            .is_some_and(|byte| b"-+ #0".contains(byte))
        {
            position += 1;
        }
        while template.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        if template.get(position) == Some(&b'.') {
            position += 1;
            while template.get(position).is_some_and(u8::is_ascii_digit) {
                position += 1;
            }
        }
        let spec = template.get(position).copied();
        if spec.is_some_and(|spec| !spec.is_ascii()) {
            // An unknown conversion prints `%` and its character, as
            // `$display` prints one.
            out.push(b'%');
            continue;
        }
        if spec.is_some() {
            position += 1;
        }
        // A conversion without its character converts an argument as `%d`.
        let converts = spec.is_none_or(|spec| b"bBoOhHxXdDiIcCsStTeEfFgGpP".contains(&spec));
        site.template = Some(
            String::from_utf8(template[start..position].to_vec()).expect("a conversion is ASCII"),
        );
        out.extend(render_formatted_message(
            &site,
            args.get(index..).unwrap_or_default(),
            ctx,
            time_format,
        ));
        if converts {
            index += 1;
        }
    }
    out
}

/// Scale each value that a `%t` of a run-time `template` converts from the
/// time unit of the calling scope to simulation time units, as the analyzer
/// scales the values of a literal format: the product of the value and a
/// signed 64-bit factor (IEEE 1800-2023 11.6.1, 20.4.3).
fn scale_time_arguments(template: &str, args: &mut [RuntimeEventArgValue], ticks_per_unit: u64) {
    if ticks_per_unit == 1 {
        return;
    }
    let mut chars = template.chars().peekable();
    let mut index = 0;
    while let Some(ch) = chars.next() {
        if ch != '%' {
            continue;
        }
        while chars
            .next_if(|ch| matches!(ch, '-' | '+' | ' ' | '#' | '0'))
            .is_some()
        {}
        while chars.next_if(char::is_ascii_digit).is_some() {}
        if chars.next_if_eq(&'.').is_some() {
            while chars.next_if(char::is_ascii_digit).is_some() {}
        }
        let Some(spec) = chars.next() else {
            break;
        };
        if !"bBoOhHxXdDiIcCsStTeEfFgGpP".contains(spec) {
            continue;
        }
        if matches!(spec, 't' | 'T')
            && let Some(arg) = args.get_mut(index)
        {
            scale_time_argument(arg, ticks_per_unit);
        }
        index += 1;
    }
}

fn scale_time_argument(arg: &mut RuntimeEventArgValue, ticks_per_unit: u64) {
    let bits = runtime_event_words_to_biguint(&arg.values, arg.width);
    match arg.float_precision {
        Some(FloatPrecision::F64) => {
            let value = f64::from_bits(bits.iter_u64_digits().next().unwrap_or(0));
            arg.values = vec![(value * ticks_per_unit as f64).to_bits()];
        }
        Some(FloatPrecision::F32) => {
            let value = f32::from_bits(bits.iter_u32_digits().next().unwrap_or(0));
            arg.values = vec![u64::from((value * ticks_per_unit as f32).to_bits())];
        }
        None => {
            // The product has the larger width of its operands and is signed
            // only if the value is (11.8.1); an unknown bit makes it unknown.
            let width = arg.width.max(64);
            let modulus = BigUint::from(1u8) << width;
            let mask = runtime_event_words_to_biguint(&arg.masks, arg.width);
            if mask.bits() > 0 {
                arg.values = vec![0; width.div_ceil(64)];
                arg.masks = (&modulus - 1u8).to_u64_digits();
            } else {
                let negative = arg.signed && arg.width > 0 && bits.bit(arg.width as u64 - 1);
                let extended = if negative {
                    bits + (&modulus - (BigUint::from(1u8) << arg.width))
                } else {
                    bits
                };
                arg.values = ((extended * ticks_per_unit) % &modulus).to_u64_digits();
                arg.masks = Vec::new();
            }
            arg.width = width;
        }
    }
}
