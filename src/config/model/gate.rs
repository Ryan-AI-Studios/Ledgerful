use serde::{Deserialize, Serialize};

const OBSERVE: &str = "observe";
const ENFORCE: &str = "enforce";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GateConfig {
    #[serde(default = "default_gate_mode")]
    pub mode: String,
}

fn default_gate_mode() -> String {
    OBSERVE.to_string()
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            mode: default_gate_mode(),
        }
    }
}

impl GateConfig {
    pub fn is_enforce(&self) -> bool {
        self.mode == ENFORCE
    }

    pub fn is_observe(&self) -> bool {
        self.mode == OBSERVE
    }

    pub fn valid_modes() -> &'static [&'static str] {
        &[OBSERVE, ENFORCE]
    }
}
