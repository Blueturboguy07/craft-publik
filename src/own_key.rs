//! The person's own key. The prefix picks the service; every service is reached
//! through its OpenAI-compatible endpoint, so one client serves all of them.
//! The model names here belong to the person's own vendor account, which is
//! the one place a vendor model name is correct (publik calls use tier aliases).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Service {
    OpenAi,
    OpenRouter,
    Groq,
    Google,
    Anthropic,
    /// An unrecognised prefix: the person gives the base URL themselves.
    Other,
}

impl Service {
    pub fn detect(key: &str) -> Self {
        let k = key.trim();
        if k.starts_with("sk-ant-") {
            Self::Anthropic
        } else if k.starts_with("sk-or-") {
            Self::OpenRouter
        } else if k.starts_with("gsk_") {
            Self::Groq
        } else if k.starts_with("AIza") {
            Self::Google
        } else if k.starts_with("sk-") {
            Self::OpenAi
        } else {
            Self::Other
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
            Self::OpenRouter => "OpenRouter",
            Self::Groq => "Groq",
            Self::Google => "Google",
            Self::Anthropic => "Anthropic",
            Self::Other => "custom endpoint",
        }
    }

    pub fn base_url(self) -> &'static str {
        match self {
            Self::OpenAi => "https://api.openai.com/v1",
            Self::OpenRouter => "https://openrouter.ai/api/v1",
            Self::Groq => "https://api.groq.com/openai/v1",
            Self::Google => "https://generativelanguage.googleapis.com/v1beta/openai",
            Self::Anthropic => "https://api.anthropic.com/v1",
            Self::Other => "",
        }
    }

    /// A sensible default when the person does not pick one.
    pub fn default_model(self) -> &'static str {
        match self {
            Self::OpenAi => "gpt-4.1-mini",
            Self::OpenRouter => "openai/gpt-4.1-mini",
            Self::Groq => "llama-3.3-70b-versatile",
            Self::Google => "gemini-2.5-flash",
            Self::Anthropic => "claude-sonnet-4-5",
            Self::Other => "",
        }
    }

    /// Image, transcription and speech lines exist only on OpenAI's own API among these.
    pub fn has_media_lines(self) -> bool {
        matches!(self, Self::OpenAi)
    }

    /// Headers a service wants beside the bearer key.
    pub fn extra_headers(self, api_key: &str) -> Vec<(String, String)> {
        match self {
            Self::Anthropic => vec![
                ("x-api-key".into(), api_key.to_string()),
                ("anthropic-version".into(), "2023-06-01".into()),
            ],
            _ => vec![],
        }
    }
}

/// `GET /models` with the key: `Ok(model ids)` when the key works.
pub fn probe(service: Service, base_url: &str, api_key: &str) -> Result<Vec<String>, String> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("Enter the service's base URL (it ends in /v1).".into());
    }
    let res = crate::http::get(
        &format!("{base}/models"),
        Some(api_key),
        service.extra_headers(api_key),
        20,
    )?;
    if res.status == 401 || res.status == 403 {
        return Err("The service did not accept this key.".into());
    }
    if !(200..300).contains(&res.status) {
        return Err(format!("The service answered {}.", res.status));
    }
    let v = res.json();
    let mut ids: Vec<String> = v["data"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| m["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    Ok(ids)
}

/// Picks the model to use from a probed list: the default when present, else the first chat-looking id.
pub fn pick_model(service: Service, ids: &[String]) -> String {
    let d = service.default_model();
    if ids.iter().any(|i| i == d) {
        return d.to_string();
    }
    let preferred = [
        "gpt-4.1-mini",
        "gpt-4o-mini",
        "gpt-5-mini",
        "claude-sonnet",
        "gemini-2.5-flash",
        "llama-3.3",
        "gpt-4.1",
        "gpt-4o",
    ];
    for p in preferred {
        if let Some(i) = ids.iter().find(|i| i.contains(p)) {
            return i.clone();
        }
    }
    ids.first().cloned().unwrap_or_else(|| d.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_pick_the_service() {
        assert_eq!(Service::detect("sk-ant-abc"), Service::Anthropic);
        assert_eq!(Service::detect("sk-or-v1-abc"), Service::OpenRouter);
        assert_eq!(Service::detect("gsk_abc"), Service::Groq);
        assert_eq!(Service::detect("AIzaabc"), Service::Google);
        assert_eq!(Service::detect("sk-proj-abc"), Service::OpenAi);
        assert_eq!(Service::detect("xyz"), Service::Other);
    }

    #[test]
    fn pick_prefers_the_default_then_a_known_family() {
        let ids = vec!["gpt-4o".to_string(), "gpt-4.1-mini".to_string()];
        assert_eq!(pick_model(Service::OpenAi, &ids), "gpt-4.1-mini");
        let ids = vec![
            "models/gemini-2.5-flash".to_string(),
            "models/gemini-pro".to_string(),
        ];
        assert_eq!(pick_model(Service::Google, &ids), "models/gemini-2.5-flash");
    }
}
