use super::*;
use celox_design::HostRequestSite;
use celox_runtime::{Halt, SimulatorErrorCode};

impl<B: SimBackend> Simulator<B> {
    pub(crate) fn run_fread_request(
        &mut self,
        site: &HostRequestSite<SignalRef>,
    ) -> Result<(), Halt> {
        let error = |message: &str| {
            Halt::Error(SimulatorErrorCode::Runtime {
                message: message.into(),
                signals: Vec::new(),
            })
        };
        let integer = |index: usize| -> Result<i128, Halt> {
            let arg = &site.args[index];
            let (value, mask) = self.backend.get_four_state(arg.id);
            if mask != BigUint::default() {
                return Err(error("$fread descriptor/address contains X or Z"));
            }
            let mut value = num_bigint::BigInt::from(value);
            if arg.signed && arg.width > 0 && value.bit(arg.width as u64 - 1) {
                value -= num_bigint::BigInt::from(1u8) << arg.width;
            }
            value
                .try_into()
                .map_err(|_| error("$fread argument exceeds supported integer range"))
        };
        let fd = integer(0)? as u32;
        let metadata = |index: usize| {
            site.strings[index]
                .parse::<i128>()
                .map_err(|_| error("invalid $fread memory metadata"))
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
            return Err(error("$fread start outside array bounds"));
        }
        let count = if memory && metadata(5)? != 0 {
            let count = integer(2)?;
            if count < 0 {
                return Err(error("negative $fread count"));
            }
            count.min(available)
        } else {
            available
        };
        let target = site.results[1].id;
        let (data, unknown) = self.backend.get_four_state(target);
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
                let byte = match self.file_io.read_byte(fd) {
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
        if self.backend.layout().four_state && target.is_4state {
            self.backend.set_four_state(target, data, unknown);
        } else {
            self.backend
                .set_wide(target, data & (&whole_mask ^ unknown));
        }
        self.backend
            .set(site.results[0].id, if failed { 0 } else { read });
        Ok(())
    }
}
