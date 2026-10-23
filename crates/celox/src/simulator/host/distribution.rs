use super::*;
use celox_design::HostRequestSite;
use celox_runtime::{Halt, SimulatorErrorCode, distribution::Distribution};

impl<B: SimBackend> Simulator<B> {
    pub(crate) fn run_distribution_request(
        &mut self,
        site: &HostRequestSite<SignalRef>,
        _timestamp: u64,
    ) -> Result<(), Halt> {
        let error = |message| {
            Halt::Error(SimulatorErrorCode::Runtime {
                message,
                signals: Vec::new(),
            })
        };
        let distribution = Distribution::from_code(site.operation)
            .ok_or_else(|| error("invalid distribution operation".into()))?;
        let values: Vec<i32> = site
            .args
            .iter()
            .map(|arg| {
                let (value, mask) = self.backend.get_four_state(arg.id);
                // The reference integer arithmetic has no X/Z representation;
                // coerce each unknown bit to zero, as for integral random seeds.
                let known = value & (BigUint::from(u32::MAX) ^ mask);
                known.to_u32_digits().first().copied().unwrap_or(0) as i32
            })
            .collect();
        let mut seed = values[0];
        let value = distribution
            .sample(&mut seed, &values[1..])
            .map_err(error)?;
        self.backend
            .set_wide(site.results[0].id, BigUint::from(value as u32));
        self.backend
            .set_wide(site.results[1].id, BigUint::from(seed as u32));
        Ok(())
    }
}
