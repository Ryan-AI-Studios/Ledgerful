use crate::local_model::client::types::ChoiceMessage;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Choice {
    pub message: ChoiceMessage,
    /// OpenAI `finish_reason` (`stop`, `length`, `tool_calls`, `content_filter`, …).
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CompletionResponse {
    pub choices: Vec<Choice>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_length_stop_openai_finish_reason_length_and_omitted() {
        let length: CompletionResponse = serde_json::from_str(
            r#"{"choices":[{"message":{"content":"abc"},"finish_reason":"length"}]}"#,
        )
        .unwrap();
        assert_eq!(length.choices[0].finish_reason.as_deref(), Some("length"));
        assert!(super::super::is_length_stop(
            length.choices[0].finish_reason.as_deref()
        ));

        let omitted: CompletionResponse =
            serde_json::from_str(r#"{"choices":[{"message":{"content":"abc"}}]}"#).unwrap();
        assert_eq!(omitted.choices[0].finish_reason, None);
        assert!(!super::super::is_length_stop(
            omitted.choices[0].finish_reason.as_deref()
        ));

        let stop: CompletionResponse = serde_json::from_str(
            r#"{"choices":[{"message":{"content":"abc"},"finish_reason":"stop"}]}"#,
        )
        .unwrap();
        assert!(!super::super::is_length_stop(
            stop.choices[0].finish_reason.as_deref()
        ));
    }
}
