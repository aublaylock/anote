//! Evaluation of captured regions with the Gemini API.

// Imports
use anyhow::Context;
use base64::Engine;
use gtk4::{gio, prelude::*};
use serde::Deserialize;
use std::time::Duration;

/// The environment variable that overrides the API key from the settings.
pub(crate) const API_KEY_ENV_VAR: &str = "GEMINI_API_KEY";
/// The model used when none is configured.
pub(crate) const DEFAULT_MODEL: &str = "gemini-3.5-flash";
const API_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);
/// Delays before retrying requests that failed with a transient error.
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(8),
];
const PROMPT: &str = "The image is a handwritten note containing a mathematical expression. \
Transcribe the expression and then simplify or evaluate it. \
If it is an equation, solve it. \
Respond with the transcribed expression and the final result only, without intermediate steps. \
Write both in plain text using Unicode math symbols (e.g. ², √, π, ≤), not LaTeX.";

/// Substrings of model names that are not suited for evaluating images of expressions.
const EXCLUDED_MODEL_KINDS: [&str; 8] = [
    "embedding",
    "tts",
    "-image",
    "audio",
    "live",
    "computer-use",
    "robotics",
    "aqa",
];

/// The API key, from the environment variable or else the settings. `None` if not set.
pub(crate) fn api_key(app_settings: Option<&gio::Settings>) -> Option<String> {
    std::env::var(API_KEY_ENV_VAR)
        .ok()
        .or_else(|| app_settings.map(|settings| settings.string("gemini-api-key").to_string()))
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
}

/// The configured model, or the default model if none is configured.
pub(crate) fn model(app_settings: Option<&gio::Settings>) -> String {
    app_settings
        .map(|settings| settings.string("gemini-model").trim().to_string())
        .filter(|model| !model.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

/// Lists the ids of the Gemini models available to the API key that can evaluate expressions,
/// newest first.
///
/// This is blocking and should be called off the main thread.
pub(crate) fn list_models(api_key: &str) -> anyhow::Result<Vec<String>> {
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .new_agent();
    let mut response = agent
        .get(format!("{API_BASE_URL}?pageSize=1000"))
        .header("x-goog-api-key", api_key)
        .call()
        .context("Requesting Gemini models failed")?;
    let status = response.status();
    let response_json = response
        .body_mut()
        .read_json::<serde_json::Value>()
        .with_context(|| format!("Reading Gemini models response failed (HTTP {status})"))?;
    if !status.is_success() {
        let message = response_json
            .pointer("/error/message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        return Err(anyhow::anyhow!("Gemini returned HTTP {status}: {message}"));
    }

    let mut models = response_json
        .get("models")
        .and_then(|models| models.as_array())
        .into_iter()
        .flatten()
        .filter(|model| {
            model
                .get("supportedGenerationMethods")
                .and_then(|methods| methods.as_array())
                .is_some_and(|methods| methods.iter().any(|m| m == "generateContent"))
        })
        .filter_map(|model| model.get("name")?.as_str())
        .map(|name| name.trim_start_matches("models/").to_string())
        .filter(|name| {
            name.starts_with("gemini")
                && !EXCLUDED_MODEL_KINDS.iter().any(|kind| name.contains(kind))
        })
        .collect::<Vec<String>>();
    models.sort_by(|a, b| b.cmp(a));
    models.dedup();
    Ok(models)
}

/// The result of evaluating a captured expression.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Evaluation {
    /// The expression as recognized by the model.
    pub(crate) expression: String,
    /// The simplified or evaluated result.
    pub(crate) result: String,
}

/// Sends the PNG image to Gemini and asks it to simplify or evaluate the expression it contains.
///
/// This is blocking and should be called off the main thread.
pub(crate) fn evaluate(api_key: &str, model: &str, png_bytes: &[u8]) -> anyhow::Result<Evaluation> {
    let body = serde_json::json!({
        "contents": [{
            "parts": [
                {
                    "inline_data": {
                        "mime_type": "image/png",
                        "data": base64::engine::general_purpose::STANDARD.encode(png_bytes),
                    }
                },
                { "text": PROMPT },
            ]
        }],
        "generationConfig": {
            "responseMimeType": "application/json",
            "responseSchema": {
                "type": "OBJECT",
                "properties": {
                    "expression": { "type": "STRING" },
                    "result": { "type": "STRING" },
                },
                "required": ["expression", "result"],
            },
        },
    });

    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build()
        .new_agent();
    // Overloaded (503), rate limited (429) and transient server errors are retried with backoff
    let mut attempt = 0;
    let response_json = loop {
        let mut response = agent
            .post(format!("{API_BASE_URL}/{model}:generateContent"))
            .header("x-goog-api-key", api_key)
            .send_json(&body)
            .context("Sending request to Gemini failed")?;
        let status = response.status();
        let response_json = response
            .body_mut()
            .read_json::<serde_json::Value>()
            .with_context(|| format!("Reading Gemini response failed (HTTP {status})"))?;

        if status.is_success() {
            break response_json;
        }
        let retryable = matches!(status.as_u16(), 429 | 500 | 502 | 503 | 504);
        if retryable && attempt < RETRY_DELAYS.len() {
            std::thread::sleep(RETRY_DELAYS[attempt]);
            attempt += 1;
            continue;
        }

        let message = response_json
            .pointer("/error/message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        let attempts_note = if attempt > 0 {
            format!(" (after {} attempts)", attempt + 1)
        } else {
            String::new()
        };
        return Err(anyhow::anyhow!(
            "Gemini returned HTTP {status}{attempts_note}: {message}"
        ));
    };

    let text = response_json
        .pointer("/candidates/0/content/parts")
        .and_then(|parts| parts.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(|t| t.as_str()))
                .collect::<String>()
        })
        .filter(|text| !text.is_empty())
        .with_context(|| {
            let reason = response_json
                .pointer("/candidates/0/finishReason")
                .or_else(|| response_json.pointer("/promptFeedback/blockReason"))
                .and_then(|r| r.as_str())
                .unwrap_or("unknown");
            format!("Gemini returned no answer (reason: {reason})")
        })?;

    // Fall back to the raw text if the model did not return the requested JSON
    Ok(
        serde_json::from_str::<Evaluation>(&text).unwrap_or_else(|_| Evaluation {
            expression: String::new(),
            result: text,
        }),
    )
}
