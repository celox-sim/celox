use super::*;
use celox_design::{HostRequestSite, ValueKind};
use celox_runtime::Halt;
use celox_runtime::scan::{Input, SliceInput};
use num_bigint::BigUint;
use num_traits::{ToPrimitive, Zero};

struct FileInput<'a> {
    files: &'a mut celox_runtime::file_io::FileIo,
    descriptor: u32,
}
impl Input for FileInput<'_> {
    fn read_byte(&mut self) -> std::io::Result<Option<u8>> {
        self.files.read_byte(self.descriptor)
    }
    fn unread_byte(&mut self, byte: u8) -> std::io::Result<()> {
        self.files.unread_byte(self.descriptor, byte)
    }
}
fn packed_bytes(value: BigUint, width: usize) -> Vec<u8> {
    let bytes = value.to_bytes_be();
    let size = width.div_ceil(8);
    let mut result = vec![0; size.saturating_sub(bytes.len())];
    result.extend(bytes);
    result
}
fn trim_packed_padding(bytes: &[u8]) -> &[u8] {
    bytes
        .iter()
        .position(|byte| *byte != 0)
        .map_or(&[], |index| &bytes[index..])
}
impl<B: SimBackend> Simulator<B> {
    pub(crate) fn run_scan_request(
        &mut self,
        site: &HostRequestSite<SignalRef>,
        _timestamp: u64,
    ) -> Result<(), Halt> {
        let error = |message| {
            Halt::Error(RuntimeErrorCode::Runtime {
                message,
                signals: Vec::new(),
            })
        };
        let (source, source_mask) = self.backend.get_four_state(site.args[0].id);
        let (format, format_mask) = self.backend.get_four_state(site.args[1].id);
        let mut count = -1i32;
        if source_mask.is_zero() && format_mask.is_zero() {
            // IEEE 1800-2023 21.3.4.3 permits both packed and string
            // source/format expressions. String values are heap handles.
            let format = if site.args[1].value_kind == ValueKind::StrRef {
                self.backend
                    .heap()
                    .string(format.to_u32().unwrap_or_default())
                    .to_vec()
            } else {
                packed_bytes(format, site.args[1].width)
            };
            let format = if site.args[1].value_kind == ValueKind::StrRef {
                format.as_slice()
            } else {
                trim_packed_padding(&format)
            };
            let destinations = site.results[1..]
                .iter()
                .map(|value| celox_runtime::scan::Destination {
                    width: value.width,
                    float_precision: value.float_precision,
                })
                .collect::<Vec<_>>();
            let outcome = if site.operation == 0 {
                // Packed strings are right-aligned; leading zero padding is
                // outside their textual contents, including for %c.
                let source = if site.args[0].value_kind == ValueKind::StrRef {
                    self.backend
                        .heap()
                        .string(source.to_u32().unwrap_or_default())
                        .to_vec()
                } else {
                    source.to_bytes_be()
                };
                let source = if site.args[0].value_kind == ValueKind::StrRef {
                    source.as_slice()
                } else {
                    trim_packed_padding(&source)
                };
                celox_runtime::scan::scan_typed(
                    &mut SliceInput::new(source),
                    format,
                    &destinations,
                    true,
                )
            } else {
                let descriptor = source.to_u32().unwrap_or_default();
                celox_runtime::scan::scan_typed(
                    &mut FileInput {
                        files: &mut self.file_io,
                        descriptor,
                    },
                    format,
                    &destinations,
                    false,
                )
            }
            .map_err(error)?;
            count = outcome.count;
            for (target, value) in site.results[1..].iter().zip(outcome.values) {
                if let Some(value) = value {
                    if self.backend.layout().four_state && target.id.is_4state {
                        self.backend
                            .set_four_state(target.id, value.value, value.mask);
                    } else {
                        let mask = (BigUint::from(1u8) << target.width) - BigUint::from(1u8);
                        self.backend
                            .set_wide(target.id, value.value & (&mask ^ (value.mask & &mask)));
                    }
                }
            }
        }
        self.backend
            .set_wide(site.results[0].id, BigUint::from(count as u32));
        self.dirty = true;
        Ok(())
    }
}
