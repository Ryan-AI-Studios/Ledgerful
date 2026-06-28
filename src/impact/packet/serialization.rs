use serde::{Deserialize, Deserializer};

pub fn deserialize_score<'de, D: Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
    Ok(Option::<f32>::deserialize(d)?.unwrap_or(0.0))
}
