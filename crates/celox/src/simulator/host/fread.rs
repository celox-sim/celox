use super::*;
use celox_design::HostRequestSite;
use celox_runtime::{Halt, SimulatorErrorCode};

impl<B: SimBackend> Simulator<B> {
    pub(crate) fn run_fread_request(
        &mut self,
        site: &HostRequestSite<SignalRef>,
    ) -> Result<(), Halt> {
        let args: Vec<_> = site
            .args
            .iter()
            .map(|arg| self.backend.get_four_state(arg.id))
            .collect();
        let target = site.results[1].id;
        let before = self.backend.get_four_state(target);
        let four_state = self.backend.layout().four_state && target.is_4state;
        let ((data, unknown), read) = fread(site, &args, before, self.files_mut(), four_state)
            .map_err(|message| {
                Halt::Error(SimulatorErrorCode::Runtime {
                    message,
                    signals: Vec::new(),
                })
            })?;
        if four_state {
            self.backend.set_four_state(target, data, unknown);
        } else {
            self.backend.set_wide(target, data);
        }
        self.backend.set(site.results[0].id, read);
        Ok(())
    }
}

/// `$fread` request `site` (IEEE 1800-2023 21.3.4.4) on `files`, with its
/// arguments' values `args` (value and unknown planes) and its target's value
/// `before`: the target's new value and the count `$fread` returns. Bytes fill
/// the target's elements from the most significant byte; only the bits read
/// change, and they become known. Without unknowns (`four_state` false) the
/// target's unknown bits read as zero.
pub(crate) fn fread(
    site: &HostRequestSite<SignalRef>,
    args: &[(BigUint, BigUint)],
    before: (BigUint, BigUint),
    files: &mut celox_runtime::file_io::FileIo,
    four_state: bool,
) -> Result<((BigUint, BigUint), u32), String> {
    let integer = |index: usize| -> Result<i128, String> {
        let arg = &site.args[index];
        let (value, mask) = &args[index];
        if *mask != BigUint::default() {
            return Err("$fread descriptor/address contains X or Z".into());
        }
        let mut value = num_bigint::BigInt::from(value.clone());
        if arg.signed && arg.width > 0 && value.bit(arg.width as u64 - 1) {
            value -= num_bigint::BigInt::from(1u8) << arg.width;
        }
        value
            .try_into()
            .map_err(|_| "$fread argument exceeds supported integer range".into())
    };
    let fd = integer(0)? as u32;
    let metadata = |index: usize| {
        site.strings[index]
            .parse::<i128>()
            .map_err(|_| String::from("invalid $fread memory metadata"))
    };
    let (left, right, width) = (metadata(0)?, metadata(1)?, metadata(2)? as usize);
    let memory = metadata(3)? != 0;
    let start = if memory && metadata(4)? != 0 {
        integer(1)?
    } else {
        left.min(right)
    };
    let available = left.max(right) - start + 1;
    if start < left.min(right) || start > left.max(right) {
        return Err("$fread start outside array bounds".into());
    }
    let count = if memory && metadata(5)? != 0 {
        let count = integer(2)?;
        if count < 0 {
            return Err("negative $fread count".into());
        }
        count.min(available)
    } else {
        available
    };
    let target = site.results[1].id;
    let (data, unknown) = before;
    let mut data = data.to_bytes_le();
    let mut unknown = unknown.to_bytes_le();
    data.resize(target.width.div_ceil(8), 0);
    unknown.resize(target.width.div_ceil(8), 0);
    let whole_mask = (BigUint::from(1u8) << target.width) - BigUint::from(1u8);
    let mut read = 0u32;
    let mut failed = false;
    'words: for index in start..start + count {
        let base = index.abs_diff(left) as usize * width;
        for byte_index in 0..width.div_ceil(8) {
            let byte = match files.read_byte(fd) {
                Ok(Some(byte)) => byte,
                Ok(None) => break 'words,
                Err(_) => {
                    failed = true;
                    break 'words;
                }
            };
            let shift = (width.div_ceil(8) - byte_index - 1) * 8;
            let bits = 8.min(width - shift);
            // Array elements need not be byte aligned (for example 9-bit
            // words). Update only the received bits, including partial EOF.
            for bit in 0..bits {
                let offset = base + shift + bit;
                let flag = 1u8 << (offset % 8);
                data[offset / 8] =
                    (data[offset / 8] & !flag) | (((byte >> bit) & 1) << (offset % 8));
                unknown[offset / 8] &= !flag;
            }
            read = read.wrapping_add(1);
        }
    }
    let data = BigUint::from_bytes_le(&data);
    let unknown = BigUint::from_bytes_le(&unknown);
    let value = if four_state {
        (data, unknown)
    } else {
        (data & (&whole_mask ^ unknown), BigUint::default())
    };
    Ok((value, if failed { 0 } else { read }))
}
