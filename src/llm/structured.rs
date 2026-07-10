//! Strict `json_schema` chat with fence-stripping and retry-on-parse-failure.

use serde::de::DeserializeOwned;

use super::client::{LlmClient, LlmError, Result};
use super::{ChatRequest, JsonSchemaSpec, ResponseFormat, Usage};

/// Run a strict structured-output chat and deserialize the assistant content.
///
/// `retries` is extra attempts on parse failure, so total attempts = 1 + retries.
pub async fn chat_structured<T: DeserializeOwned>(
    client: &dyn LlmClient,
    mut req: ChatRequest,
    schema_name: &'static str,
    schema: serde_json::Value,
    retries: usize,
) -> Result<(T, Usage)> {
    // Structured output and tool_choice are mutually exclusive, so clear tool_choice.
    req.response_format = Some(ResponseFormat::JsonSchema {
        json_schema: JsonSchemaSpec {
            name: schema_name.to_string(),
            strict: true,
            schema,
        },
    });
    req.tool_choice = None;

    let mut last_err: Option<LlmError> = None;

    for _ in 0..=retries {
        let resp = client.chat(&req).await?;
        let usage = resp.usage.unwrap_or_default();
        let choice = resp.choices.first().ok_or(LlmError::EmptyChoices)?;
        let finish_reason = choice.finish_reason.clone();
        let raw = structured_payload(&choice.message);
        let cleaned = strip_fences(&raw);

        if cleaned.is_empty() {
            last_err = Some(LlmError::EmptyContent {
                target: schema_name,
                finish_reason: finish_reason.unwrap_or_else(|| "unknown".to_string()),
            });
            continue;
        }

        match serde_json::from_str::<T>(cleaned) {
            Ok(value) => return Ok((value, usage)),
            Err(source) => {
                last_err = Some(LlmError::Parse {
                    target: schema_name,
                    source,
                    raw,
                });
            }
        }
    }

    Err(last_err.unwrap_or(LlmError::EmptyChoices))
}

/// Run a strict structured-output chat while streaming selected string fields.
pub enum StructuredStreamDelta<'a> {
    Field(&'static str, &'a str),
    Reasoning(&'a str),
}

pub async fn chat_structured_stream_fields<T, F>(
    client: &dyn LlmClient,
    mut req: ChatRequest,
    schema_name: &'static str,
    schema: serde_json::Value,
    retries: usize,
    field_names: &[&'static str],
    mut on_delta: F,
) -> Result<(T, Usage, bool)>
where
    T: DeserializeOwned,
    F: for<'a> FnMut(StructuredStreamDelta<'a>) + Send,
{
    req.response_format = Some(ResponseFormat::JsonSchema {
        json_schema: JsonSchemaSpec {
            name: schema_name.to_string(),
            strict: true,
            schema,
        },
    });
    req.tool_choice = None;

    let mut last_err: Option<LlmError> = None;
    let mut streamed_any = false;

    for _ in 0..=retries {
        let mut field_streams = field_names
            .iter()
            .copied()
            .map(JsonStringFieldStream::new)
            .collect::<Vec<_>>();
        let mut raw_delta = |delta: crate::llm::StreamDelta| match delta {
            crate::llm::StreamDelta::Content(delta) => {
                for field_stream in &mut field_streams {
                    let field_delta = field_stream.push(delta);
                    if !field_delta.is_empty() {
                        streamed_any = true;
                        on_delta(StructuredStreamDelta::Field(
                            field_stream.field_name,
                            &field_delta,
                        ));
                    }
                }
            }
            crate::llm::StreamDelta::Reasoning(delta) => {
                on_delta(StructuredStreamDelta::Reasoning(delta))
            }
        };

        let resp = client.chat_stream(&req, &mut raw_delta).await?;
        let usage = resp.usage.unwrap_or_default();
        let choice = resp.choices.first().ok_or(LlmError::EmptyChoices)?;
        let finish_reason = choice.finish_reason.clone();
        let raw = structured_payload(&choice.message);
        let cleaned = strip_fences(&raw);

        if cleaned.is_empty() {
            last_err = Some(LlmError::EmptyContent {
                target: schema_name,
                finish_reason: finish_reason.unwrap_or_else(|| "unknown".to_string()),
            });
            continue;
        }

        match serde_json::from_str::<T>(cleaned) {
            Ok(value) => return Ok((value, usage, streamed_any)),
            Err(source) => {
                last_err = Some(LlmError::Parse {
                    target: schema_name,
                    source,
                    raw,
                });
            }
        }
    }

    Err(last_err.unwrap_or(LlmError::EmptyChoices))
}

#[derive(Debug)]
struct JsonStringFieldStream {
    field_name: &'static str,
    state: FieldStreamState,
    reading_string: bool,
    string_escape: bool,
    last_string: String,
    value_escape: bool,
    value_unicode: Option<UnicodeEscape>,
    pending_high_surrogate: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldStreamState {
    Seeking,
    AfterString,
    SeekingValue,
    InValue,
    Done,
}

#[derive(Debug)]
struct UnicodeEscape {
    value: u16,
    digits: u8,
}

impl UnicodeEscape {
    fn new() -> Self {
        Self {
            value: 0,
            digits: 0,
        }
    }

    fn push(&mut self, ch: char) -> Option<u16> {
        let digit = ch.to_digit(16).unwrap_or(0) as u16;
        self.value = (self.value << 4) | digit;
        self.digits += 1;
        (self.digits == 4).then_some(self.value)
    }
}

impl JsonStringFieldStream {
    fn new(field_name: &'static str) -> Self {
        Self {
            field_name,
            state: FieldStreamState::Seeking,
            reading_string: false,
            string_escape: false,
            last_string: String::new(),
            value_escape: false,
            value_unicode: None,
            pending_high_surrogate: None,
        }
    }

    fn push(&mut self, input: &str) -> String {
        let mut out = String::new();
        for ch in input.chars() {
            self.push_char(ch, &mut out);
        }
        out
    }

    fn push_char(&mut self, ch: char, out: &mut String) {
        match self.state {
            FieldStreamState::Seeking => self.seek_key(ch),
            FieldStreamState::AfterString => self.after_string(ch),
            FieldStreamState::SeekingValue => self.seek_value(ch),
            FieldStreamState::InValue => self.push_value_char(ch, out),
            FieldStreamState::Done => {}
        }
    }

    fn seek_key(&mut self, ch: char) {
        if self.reading_string {
            if self.string_escape {
                self.string_escape = false;
                self.last_string.push(ch);
                return;
            }
            match ch {
                '\\' => self.string_escape = true,
                '"' => {
                    self.reading_string = false;
                    self.state = FieldStreamState::AfterString;
                }
                _ => self.last_string.push(ch),
            }
        } else if ch == '"' {
            self.reading_string = true;
            self.string_escape = false;
            self.last_string.clear();
        }
    }

    fn after_string(&mut self, ch: char) {
        if ch.is_whitespace() {
            return;
        }

        if ch == ':' && self.last_string == self.field_name {
            self.state = FieldStreamState::SeekingValue;
            return;
        }

        self.state = FieldStreamState::Seeking;
        self.reading_string = false;
        self.string_escape = false;
        if ch == '"' {
            self.reading_string = true;
            self.last_string.clear();
        }
    }

    fn seek_value(&mut self, ch: char) {
        if ch.is_whitespace() {
            return;
        }

        if ch == '"' {
            self.state = FieldStreamState::InValue;
            self.value_escape = false;
            self.value_unicode = None;
            self.pending_high_surrogate = None;
        } else {
            self.state = FieldStreamState::Done;
        }
    }

    fn push_value_char(&mut self, ch: char, out: &mut String) {
        if let Some(unicode) = &mut self.value_unicode {
            if let Some(unit) = unicode.push(ch) {
                self.value_unicode = None;
                self.push_code_unit(unit, out);
            }
            return;
        }

        if self.value_escape {
            self.value_escape = false;
            match ch {
                '"' | '\\' | '/' => out.push(ch),
                'b' => out.push('\u{0008}'),
                'f' => out.push('\u{000c}'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'u' => self.value_unicode = Some(UnicodeEscape::new()),
                other => out.push(other),
            }
            return;
        }

        match ch {
            '\\' => self.value_escape = true,
            '"' => {
                if self.pending_high_surrogate.take().is_some() {
                    out.push(std::char::REPLACEMENT_CHARACTER);
                }
                self.state = FieldStreamState::Done;
            }
            _ => out.push(ch),
        }
    }

    fn push_code_unit(&mut self, unit: u16, out: &mut String) {
        if let Some(high) = self.pending_high_surrogate.take() {
            if (0xdc00..=0xdfff).contains(&unit) {
                let codepoint = 0x10000 + (((high - 0xd800) as u32) << 10) + (unit - 0xdc00) as u32;
                if let Some(ch) = char::from_u32(codepoint) {
                    out.push(ch);
                }
                return;
            }
            out.push(std::char::REPLACEMENT_CHARACTER);
        }

        if (0xd800..=0xdbff).contains(&unit) {
            self.pending_high_surrogate = Some(unit);
        } else if (0xdc00..=0xdfff).contains(&unit) {
            out.push(std::char::REPLACEMENT_CHARACTER);
        } else if let Some(ch) = char::from_u32(unit as u32) {
            out.push(ch);
        }
    }
}

/// Pull the structured-output payload out of a response message.
///
/// Normally that's the assistant `content`. But some OpenRouter providers honor a
/// strict `json_schema` request by emitting the JSON as a tool/function call and
/// leaving `content` null — the dashboard reconstructs and shows the JSON, while a
/// naive reader sees empty content. Fall back to the first tool call's `arguments`.
fn structured_payload(msg: &super::ResponseMessage) -> String {
    let content = msg.content.clone().unwrap_or_default();
    if !content.trim().is_empty() {
        return content;
    }
    msg.tool_calls
        .as_ref()
        .and_then(|calls| calls.first())
        .map(|call| call.function.arguments.clone())
        .unwrap_or_default()
}

/// Strip a single ```json/```/~~~ fence wrapping JSON; returns a trimmed borrowed slice.
pub fn strip_fences(s: &str) -> &str {
    let trimmed = s.trim();

    let fence: Option<&str> = if trimmed.starts_with("```") {
        Some("```")
    } else if trimmed.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    };

    let Some(fence) = fence else {
        return trimmed;
    };

    // Drop the opening fence line, which may carry a language tag like `json`.
    let after_open = &trimmed[fence.len()..];
    let body_start = match after_open.find('\n') {
        Some(nl) => &after_open[nl + 1..],
        // Single-line fenced blob with no newline: just strip the marker.
        None => after_open,
    };

    let body = body_start.trim_end();
    let inner = body.strip_suffix(fence).unwrap_or(body);

    inner.trim()
}

/// Strict `translation_result` schema for `model::TranslatorOut`.
pub fn translator_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["thought_process", "translated_text", "pov", "new_characters", "new_terms", "continuity_notes"],
        "properties": {
            "thought_process": {
                "type": "object",
                "additionalProperties": false,
                "required": ["scene_analysis", "glossary_check"],
                "properties": {
                    "scene_analysis": { "type": "string" },
                    "glossary_check": { "type": "string" }
                }
            },
            "translated_text": { "type": "string" },
            "pov": {
                "type": "string",
                "description": "Narrator at the END of this chunk: first-person POV character + their Thai self-pronoun (e.g. 'ฮิคาริ (บุรุษที่ 1, สรรพนามตัวเอง=ฉัน)'), or 'บุรุษที่ 3' for third-person. Empty if unknown."
            },
            "new_characters": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["jp_name", "translated_name", "gender", "notes"],
                    "properties": {
                        "jp_name": { "type": "string" },
                        "translated_name": { "type": "string" },
                        "gender": { "type": "string" },
                        "notes": { "type": "string" }
                    }
                }
            },
            "new_terms": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["jp_term", "translated_term", "category", "gloss"],
                    "properties": {
                        "jp_term": { "type": "string" },
                        "translated_term": { "type": "string" },
                        "category": { "type": "string" },
                        "gloss": { "type": "string" }
                    }
                }
            },
            "continuity_notes": {
                "type": "array",
                "items": { "type": "string" }
            }
        }
    })
}

/// Strict `review_result` schema for `model::ReviewerOut`.
pub fn reviewer_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["status", "feedback"],
        "properties": {
            "status": { "type": "string", "enum": ["approve", "reject"] },
            "feedback": {
                "type": "array",
                "items": { "type": "string" }
            }
        }
    })
}

/// Strict `prepass_result` schema → `agents::prepass::PrepassOut`.
pub fn prepass_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["characters", "terms", "style_examples"],
        "properties": {
            "characters": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["jp_name", "translated_name", "romaji", "gender", "aliases", "honorific", "speech_style", "notes"],
                    "properties": {
                        "jp_name": { "type": "string" },
                        "translated_name": { "type": "string" },
                        "romaji": { "type": "string" },
                        "gender": { "type": "string" },
                        "aliases": { "type": "array", "items": { "type": "string" } },
                        "honorific": { "type": "string" },
                        "speech_style": { "type": "string" },
                        "notes": { "type": "string" }
                    }
                }
            },
            "terms": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["jp_term", "translated_term", "romaji", "category", "gloss"],
                    "properties": {
                        "jp_term": { "type": "string" },
                        "translated_term": { "type": "string" },
                        "romaji": { "type": "string" },
                        "category": { "type": "string" },
                        "gloss": { "type": "string" }
                    }
                }
            },
            "style_examples": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["jp", "translated_text", "note"],
                    "properties": {
                        "jp": { "type": "string" },
                        "translated_text": { "type": "string" },
                        "note": { "type": "string" }
                    }
                }
            }
        }
    })
}

/// Strict `coherence_result` schema → `agents::coherence::CoherenceOut`.
pub fn coherence_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["issues"],
        "properties": {
            "issues": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["severity", "note"],
                    "properties": {
                        "severity": { "type": "string", "enum": ["info", "warning", "conflict"] },
                        "note": { "type": "string" },
                        "resolve_kind": { "type": "string", "enum": ["term", "character", ""] },
                        "resolve_jp": { "type": "string" },
                        "resolve_canonical_translation": { "type": "string" }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{
        JsonStringFieldStream, StructuredStreamDelta, chat_structured,
        chat_structured_stream_fields, coherence_schema, prepass_schema, translator_schema,
    };
    use crate::llm::client::{LlmClient, LlmError, Result};
    use crate::llm::{
        ChatRequest, ChatResponse, Choice, FunctionCall, ResponseMessage, StreamDelta, ToolCall,
    };
    use async_trait::async_trait;

    struct OneShotClient {
        message: ResponseMessage,
        finish_reason: Option<String>,
        reasoning: Option<String>,
    }

    #[async_trait]
    impl LlmClient for OneShotClient {
        async fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse> {
            Ok(ChatResponse {
                id: None,
                model: None,
                usage: None,
                service_tier: None,
                choices: vec![Choice {
                    index: 0,
                    message: self.message.clone(),
                    finish_reason: self.finish_reason.clone(),
                }],
            })
        }

        async fn chat_stream(
            &self,
            req: &ChatRequest,
            on_delta: &mut (dyn for<'a> FnMut(StreamDelta<'a>) + Send),
        ) -> Result<ChatResponse> {
            if let Some(reasoning) = self.reasoning.as_deref() {
                on_delta(StreamDelta::Reasoning(reasoning));
            }
            let response = self.chat(req).await?;
            if let Some(content) = response
                .choices
                .first()
                .and_then(|choice| choice.message.content.as_deref())
            {
                on_delta(StreamDelta::Content(content));
            }
            Ok(response)
        }
    }

    fn message(content: Option<&str>, tool_calls: Option<Vec<ToolCall>>) -> ResponseMessage {
        ResponseMessage {
            role: Some("assistant".to_string()),
            content: content.map(str::to_string),
            tool_calls,
        }
    }

    #[derive(Debug, serde::Deserialize)]
    struct Dummy {
        ok: bool,
    }

    async fn run(client: &OneShotClient) -> Result<(Dummy, crate::llm::Usage)> {
        chat_structured::<Dummy>(
            client,
            ChatRequest::new("m", vec![]),
            "review_result",
            serde_json::json!({}),
            0,
        )
        .await
    }

    #[tokio::test]
    async fn stream_fields_emits_multiple_targets() {
        let client = OneShotClient {
            message: message(
                Some(
                    r#"{"thought_process":{"scene_analysis":"tone","glossary_check":"term"},"translated_text":"ไทย","pov":"","new_characters":[],"new_terms":[],"continuity_notes":[]}"#,
                ),
                None,
            ),
            finish_reason: Some("stop".to_string()),
            reasoning: Some("checking context".to_string()),
        };
        let mut got = Vec::new();
        let mut reasoning = String::new();

        let (_value, _usage, streamed) = chat_structured_stream_fields::<serde_json::Value, _>(
            &client,
            ChatRequest::new("m", vec![]),
            "translation_result",
            serde_json::json!({}),
            0,
            &["scene_analysis", "glossary_check", "translated_text"],
            |delta| match delta {
                StructuredStreamDelta::Field(field, delta) => got.push((field, delta.to_string())),
                StructuredStreamDelta::Reasoning(delta) => reasoning.push_str(delta),
            },
        )
        .await
        .expect("structured JSON should parse");

        assert!(streamed);
        assert_eq!(reasoning, "checking context");
        assert_eq!(
            got,
            vec![
                ("scene_analysis", "tone".to_string()),
                ("glossary_check", "term".to_string()),
                ("translated_text", "ไทย".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn empty_content_reports_finish_reason_not_eof_parse_error() {
        let client = OneShotClient {
            message: message(None, None),
            finish_reason: Some("length".to_string()),
            reasoning: None,
        };
        match run(&client).await.unwrap_err() {
            LlmError::EmptyContent {
                target,
                finish_reason,
            } => {
                assert_eq!(target, "review_result");
                assert_eq!(finish_reason, "length");
            }
            other => panic!("expected EmptyContent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn falls_back_to_tool_call_arguments_when_content_is_null() {
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            kind: "function".to_string(),
            function: FunctionCall {
                name: "review_result".to_string(),
                arguments: r#"{"ok":true}"#.to_string(),
            },
        };
        let client = OneShotClient {
            message: message(None, Some(vec![tool_call])),
            finish_reason: Some("tool_calls".to_string()),
            reasoning: None,
        };
        let (value, _usage) = run(&client).await.expect("tool-call args should parse");
        assert!(value.ok);
    }

    #[test]
    fn field_stream_emits_only_target_string_value() {
        let mut stream = JsonStringFieldStream::new("translated_text");
        let mut out = String::new();

        for chunk in [
            r#"{"thought_process":{"scene_analysis":"mentions \"translated_text\" only as text","#,
            r#""glossary_check":"ok"},"translated"#,
            "_text\":\"บรรทัดแรก\\n",
            "บรรทัดที่สอง \\u0e01\",\"new_characters\":[]}",
        ] {
            out.push_str(&stream.push(chunk));
        }

        assert_eq!(out, "บรรทัดแรก\nบรรทัดที่สอง ก");
    }

    #[test]
    fn field_stream_decodes_split_surrogate_pair() {
        let mut stream = JsonStringFieldStream::new("translated_text");
        assert_eq!(stream.push(r#"{"translated_text":"a \u"#), "a ");
        assert_eq!(stream.push("d83d\\udc4d\"}"), "👍");
    }

    #[test]
    fn model_schemas_expose_only_target_neutral_field_names() {
        let schemas = serde_json::json!({
            "translator": translator_schema(),
            "prepass": prepass_schema(),
            "coherence": coherence_schema(),
        })
        .to_string();

        for field in [
            "translated_name",
            "translated_term",
            "translated_text",
            "resolve_canonical_translation",
        ] {
            assert!(schemas.contains(field), "missing neutral field {field}");
        }
        for legacy in [
            "thai_name",
            "thai_term",
            "forbidden_thai",
            "resolve_canonical_th",
            "\"th\"",
        ] {
            assert!(
                !schemas.contains(legacy),
                "legacy field leaked into model schema: {legacy}"
            );
        }
    }
}
