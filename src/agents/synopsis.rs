//! One-off Translator calls for synopsis and title fields.
//!
//! No continuity/reference context; rerolls raise temperature for variety.

use crate::agents::prompts::translator_system;
use crate::llm::client::{LlmClient, LlmError, Result};
use crate::llm::structured::{chat_structured, translator_schema};
use crate::llm::{ChatRequest, Message, Usage};
use crate::model::{TargetLanguage, TranslatorOut};

/// Map a reroll attempt to a capped sampling temperature.
pub fn reroll_temperature(attempt: u32) -> f32 {
    match attempt {
        0 => 0.3,
        1 => 0.5,
        2 => 0.7,
        _ => 0.9,
    }
}

/// Translate source synopsis text into the selected target language.
pub async fn translate_synopsis(
    client: &dyn LlmClient,
    model: &str,
    target_language: TargetLanguage,
    raw: &str,
    temperature: f32,
) -> Result<(String, Usage)> {
    let instruction = match target_language {
        TargetLanguage::Thai => {
            "นี่คือเรื่องย่อของทั้งเล่ม แปลเป็นภาษาไทยที่สละสลวยและเป็นธรรมชาติ คงสาระให้ครบถ้วน"
        }
        TargetLanguage::English => {
            "Translate this volume synopsis into concise, polished, natural English for light-novel readers while preserving every material plot point"
        }
    };
    let user = format!(
        "<<VOLUME_SYNOPSIS_JP: {instruction}>>\n\
         {raw}\n\
         <<END_VOLUME_SYNOPSIS_JP>>",
        raw = raw.trim(),
    );

    let req = ChatRequest {
        model: model.to_string(),
        messages: vec![
            Message::system(translator_system(target_language)),
            Message::user(user),
        ],
        temperature: Some(temperature),
        ..ChatRequest::default()
    };

    let (out, usage) =
        chat_structured::<TranslatorOut>(client, req, "translation_result", translator_schema(), 2)
            .await?;
    // Treat cut-off or empty completions as failures.
    let translated = out.translated_text.trim().to_string();
    if translated.is_empty() {
        return Err(LlmError::Api {
            status: 0,
            message: "translator returned an empty synopsis".to_string(),
        });
    }
    Ok((translated, usage))
}

/// Translate a novel title into one Thai line.
pub async fn translate_title(
    client: &dyn LlmClient,
    model: &str,
    target_language: TargetLanguage,
    raw: &str,
    temperature: f32,
) -> Result<(String, Usage)> {
    let instruction = match target_language {
        TargetLanguage::Thai => {
            "นี่คือชื่อเรื่องของไลท์โนเวล แปลเป็นชื่อภาษาไทยที่เป็นธรรมชาติและน่าสนใจ คงความหมายและอารมณ์ของต้นฉบับ ตอบเป็นชื่อเรื่องบรรทัดเดียว ไม่ต้องอธิบาย"
        }
        TargetLanguage::English => {
            "Translate this light-novel title into one natural, appealing English title that preserves the meaning, hook, and tone; return the title only"
        }
    };
    let user = format!(
        "<<NOVEL_TITLE_JP: {instruction}>>\n\
         {raw}\n\
         <<END_NOVEL_TITLE_JP>>",
        raw = raw.trim(),
    );

    let req = ChatRequest {
        model: model.to_string(),
        messages: vec![
            Message::system(translator_system(target_language)),
            Message::user(user),
        ],
        temperature: Some(temperature),
        ..ChatRequest::default()
    };

    let (out, usage) =
        chat_structured::<TranslatorOut>(client, req, "translation_result", translator_schema(), 2)
            .await?;
    let translated = out
        .translated_text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if translated.is_empty() {
        return Err(LlmError::Api {
            status: 0,
            message: "translator returned an empty title".to_string(),
        });
    }
    Ok((translated, usage))
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
        let (translated, usage) =
            translate_title(&client, "honya/mock", TargetLanguage::Thai, "夜の影", 0.3)
                .await
                .unwrap();
        assert_eq!(translated, "เงาแห่ง ราตรี");
        assert!(usage.total_tokens > 0);
    }

    #[tokio::test]
    async fn translate_synopsis_returns_thai() {
        let client = MockClient::new("เรื่องย่อภาษาไทย");
        let (translated, usage) = translate_synopsis(
            &client,
            "honya/mock",
            TargetLanguage::Thai,
            "原文のあらすじ",
            0.3,
        )
        .await
        .unwrap();
        assert_eq!(translated, "เรื่องย่อภาษาไทย");
        assert!(usage.total_tokens > 0);
    }
}
