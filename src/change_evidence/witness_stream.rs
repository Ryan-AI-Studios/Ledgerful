//! Metered, length-framed hashing over borrowed observation facts.
use super::pack::Work;

pub(super) struct WitnessStream(blake3::Hasher);

impl WitnessStream {
    pub(super) fn new(domain: &[u8], work: &mut Work<'_>) -> Result<Self, &'static str> {
        work.charge(domain.len())?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(domain);
        Ok(Self(hasher))
    }

    pub(super) fn record(
        &mut self,
        tag: u8,
        fields: &[&[u8]],
        work: &mut Work<'_>,
    ) -> Result<(), &'static str> {
        let count = u8::try_from(fields.len()).map_err(|_| "witness-field-cap")?;
        let bytes = fields.iter().try_fold(2, |total: usize, field| {
            total
                .checked_add(8)
                .and_then(|total| total.checked_add(field.len()))
                .ok_or("witness-byte-cap")
        })?;
        work.charge(bytes)?;
        self.0.update(&[tag, count]);
        for field in fields {
            self.0.update(&(field.len() as u64).to_be_bytes());
            self.0.update(field);
        }
        Ok(())
    }

    pub(super) fn finish(self, work: &mut Work<'_>) -> Result<[u8; 32], &'static str> {
        work.charge(32)?;
        let bytes = *self.0.finalize().as_bytes();
        work.charge(0)?;
        Ok(bytes)
    }
}
