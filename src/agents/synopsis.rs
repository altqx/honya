//! Translate a volume synopsis (เรื่องย่อ) with the Translator agent.
//!
//! Reuses the Translator's system prompt + strict-JSON schema but feeds a single
//! free-standing block instead of a chapter chunk: no continuity, no reference
//! context. `temperature` rises with each reroll so the user gets a genuinely
//! different take rather than the same sampling.

use crate::agents::prompts::TRANSLATOR_SYSTEM;
use crate::llm::client::{LlmClient, Result};
use crate::llm::structured::{chat_structured, translator_schema};
use crate::llm::{ChatRequest, Message, Usage};
use crate::model::TranslatorOut;

/// Map a reroll attempt (0-based) to a sampling temperature: the first roll is
/// conservative, each reroll loosens up so the alternatives actually diverge.
pub fn reroll_temperature(attempt: u32) -> f32 {
    match attempt {
        0 => 0.3,
        1 => 0.5,
        2 => 0.7,
        _ => 0.9,
    }
}

/// Translate `raw` source synopsis text into Thai. Returns the Thai string plus
/// token `Usage`. `temperature` controls reroll variety (see [`reroll_temperature`]).
pub async fn translate_synopsis(
    client: &dyn LlmClient,
    model: &str,
    raw: &str,
    temperature: f32,
) -> Result<(String, Usage)> {
    let user = format!(
        "<<VOLUME_SYNOPSIS_JP: นี่คือเรื่องย่อของทั้งเล่ม แปลเป็นภาษาไทยที่สละสลวยและเป็นธรรมชาติ คงสาระให้ครบถ้วน>>\n\
         {raw}\n\
         <<END_VOLUME_SYNOPSIS_JP>>",
        raw = raw.trim(),
    );

    let req = ChatRequest {
        model: model.to_string(),
        messages: vec![Message::system(TRANSLATOR_SYSTEM), Message::user(user)],
        temperature: Some(temperature),
        ..ChatRequest::default()
    };

    let (out, usage) =
        chat_structured::<TranslatorOut>(client, req, "translation_result", translator_schema(), 2)
            .await?;
    Ok((out.translated_text.trim().to_string(), usage))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::mock::MockClient;

    #[test]
    fn reroll_temperature_rises_then_caps() {
        assert!(reroll_temperature(0) < reroll_temperature(1));
        assert!(reroll_temperature(1) < reroll_temperature(2));
        // Saturates at 0.9 for any further reroll.
        assert_eq!(reroll_temperature(3), reroll_temperature(9));
        assert!(reroll_temperature(9) <= 0.9);
    }

    #[tokio::test]
    async fn translate_synopsis_returns_thai() {
        let client = MockClient::new("เรื่องย่อภาษาไทย");
        let (th, usage) = translate_synopsis(&client, "honya/mock", "原文のあらすじ", 0.3)
            .await
            .unwrap();
        assert_eq!(th, "เรื่องย่อภาษาไทย");
        assert!(usage.total_tokens > 0);
    }
}
