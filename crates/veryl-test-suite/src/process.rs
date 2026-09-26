use crate::{Backend, BigUint, Result, SignalPath};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub(crate) struct ProcessBackend {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    transcript: File,
    top: String,
    edges: BTreeMap<String, bool>,
}

impl ProcessBackend {
    pub(crate) fn spawn(
        mut command: Command,
        directory: &std::path::Path,
        top: String,
        edges: BTreeMap<String, bool>,
    ) -> Result<Self> {
        let mut child = command
            .current_dir(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(File::create(directory.join("runtime.log"))?))
            .spawn()?;
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Ok(Self {
            child,
            input,
            output,
            transcript: File::create(directory.join("protocol.log"))?,
            top,
            edges,
        })
    }

    fn command(&mut self, command: &str) -> Result<String> {
        writeln!(self.transcript, "> {command}")?;
        writeln!(self.input, "{command}")?;
        self.input.flush()?;
        loop {
            let mut line = String::new();
            if self.output.read_line(&mut line)? == 0 {
                return Err(format!("simulator exited while processing {command}").into());
            }
            write!(self.transcript, "< {line}")?;
            if let Some(reply) = line.trim_end().strip_prefix("@suite ") {
                if let Some(error) = reply.strip_prefix("error ") {
                    return Err(error.to_owned().into());
                }
                return Ok(reply.into());
            }
        }
    }

    fn path(&self, signal: &SignalPath) -> String {
        let mut path = self.top.clone();
        for instance in &signal.instances {
            path.push('.');
            path.push_str(&instance.name);
            if instance.index != 0 {
                path.push_str(&format!("[{}]", instance.index));
            }
        }
        format!("{path}.{}", signal.name)
    }
}

impl Backend for ProcessBackend {
    fn write(&mut self, signal: &SignalPath, payload: BigUint, mask: BigUint) -> Result<()> {
        self.command(&format!(
            "write {} {}",
            self.path(signal),
            encode_bits(&payload, &mask)
        ))?;
        Ok(())
    }
    fn read(&mut self, signal: &SignalPath) -> Result<(BigUint, BigUint)> {
        let value = self.command(&format!("read {}", self.path(signal)))?;
        decode_bits(&value)
    }
    fn eval_comb(&mut self) -> Result<()> {
        self.command("eval")?;
        Ok(())
    }
    fn tick(&mut self, event: &str) -> Result<()> {
        let rising = *self
            .edges
            .get(event)
            .ok_or_else(|| format!("no edge for event {event}"))?;
        let path = format!("{}.{event}", self.top);
        let original = self.command(&format!("read {path}"))?;
        for level in [!rising, rising] {
            self.command(&format!("write {path} {}", u8::from(level)))?;
            self.eval_comb()?;
        }
        // Native event APIs do not change the driven clock/reset level.
        // Restore it so an explicitly asserted reset remains asserted.
        if original != u8::from(rising).to_string() {
            self.command(&format!("write {path} {original}"))?;
            self.eval_comb()?;
        }
        Ok(())
    }
}

impl Drop for ProcessBackend {
    fn drop(&mut self) {
        // Let the simulator finish and reap its timeout supervisor, including on
        // assertion failure. Its wall-clock limit bounds a stuck simulator.
        let _ = writeln!(self.input, "quit");
        let _ = self.input.flush();
        let _ = self.child.wait();
    }
}

fn encode_bits(payload: &BigUint, mask: &BigUint) -> String {
    let width = payload.bits().max(mask.bits()).max(1);
    (0..width)
        .rev()
        .map(|bit| match (payload.bit(bit), mask.bit(bit)) {
            (false, false) => '0',
            (true, false) => '1',
            (false, true) => 'z',
            (true, true) => 'x',
        })
        .collect()
}

fn decode_bits(bits: &str) -> Result<(BigUint, BigUint)> {
    let mut payload = BigUint::default();
    let mut mask = BigUint::default();
    for (bit, value) in bits.bytes().rev().enumerate() {
        let (p, m) = match value {
            b'0' => (false, false),
            b'1' => (true, false),
            b'x' | b'X' => (true, true),
            b'z' | b'Z' => (false, true),
            _ => return Err(format!("invalid simulator value: {bits}").into()),
        };
        payload.set_bit(bit as u64, p);
        mask.set_bit(bit as u64, m);
    }
    if bits.is_empty() {
        return Err("empty simulator value".into());
    }
    Ok((payload, mask))
}

pub(crate) fn write_if_changed(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    if std::fs::read(path).ok().as_deref() != Some(contents) {
        std::fs::write(path, contents)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn four_state_wire_encoding_preserves_wide_values() {
        let bits = format!("10xz{}Z", "01zx".repeat(100));
        let (payload, mask) = decode_bits(&bits).unwrap();
        assert_eq!(encode_bits(&payload, &mask), bits.to_lowercase());
        assert!(decode_bits("garbage").is_err());
        assert!(decode_bits("").is_err());
    }
}
