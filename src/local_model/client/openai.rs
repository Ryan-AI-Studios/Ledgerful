use crate::local_model::client::types::ChoiceMessage;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Choice {
    pub message: ChoiceMessage,
}

#[derive(Debug, Deserialize)]
pub struct CompletionResponse {
    pub choices: Vec<Choice>,
}
