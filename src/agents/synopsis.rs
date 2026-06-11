//! One-off Translator calls for synopsis and title fields.
//!
//! No continuity/reference context; rerolls raise temperature for variety.

use crate::agents::prompts::TRANSLATOR_SYSTEM;
use crate::llm::client::{LlmClient, LlmError, Result};
use crate::llm::structured::{chat_structured, translator_schema};
use crate::llm::{ChatRequest, Message, Usage};
use crate::model::TranslatorOut;

/// Map a reroll attempt to a capped sampling temperature.
pub fn reroll_temperature(attempt: u32) -> f32 {
    match attempt {
        0 => 0.3,
        1 => 0.5,
        2 => 0.7,
        _ => 0.9,
    }
}

/// Translate source synopsis text into Thai.
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
    // Treat cut-off or empty completions as failures.
    let thai = out.translated_text.trim().to_string();
    if thai.is_empty() {
        return Err(LlmError::Api {
            status: 0,
            message: "translator returned an empty synopsis".to_string(),
        });
    }
    Ok((thai, usage))
}

/// Translate a novel title into one Thai line.
pub async fn translate_title(
    client: &dyn LlmClient,
    model: &str,
    raw: &str,
    temperature: f32,
) -> Result<(String, Usage)> {
    let user = format!(
        "<<NOVEL_TITLE_JP: นี่คือชื่อเรื่องของไลท์โนเวล แปลเป็นชื่อภาษาไทยที่เป็นธรรมชาติและน่าสนใจ \
         คงความหมายและอารมณ์ของต้นฉบับ ตอบเป็นชื่อเรื่องบรรทัดเดียว ไม่ต้องอธิบาย>>\n\
         {raw}\n\
         <<END_NOVEL_TITLE_JP>>",
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
    let thai = out
        .translated_text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if thai.is_empty() {
        return Err(LlmError::Api {
            status: 0,
            message: "translator returned an empty title".to_string(),
        });
    }
    Ok((thai, usage))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::mock::MockClient;

    #[test]
    fn reroll_temperature_rises_then_caps() {
        assert!(reroll_temperature(0) < reroll_temperature(1));
        assert!(reroll_temperature(1) < reroll_temperature(2));
        assert_eq!(reroll_temperature(3), reroll_temperature(9));
        assert!(reroll_temperature(9) <= 0.9);
    }

    #[tokio::test]
    async fn translate_title_returns_single_line_thai() {
        let client = MockClient::new("เงาแห่ง\nราตรี");
        let (th, usage) = translate_title(&client, "honya/mock", "夜の影", 0.3)
            .await
            .unwrap();
        assert_eq!(th, "เงาแห่ง ราตรี");
        assert!(usage.total_tokens > 0);
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
