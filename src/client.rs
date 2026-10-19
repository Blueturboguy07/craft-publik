//! The calls a feature makes: chat, image generation and edits, transcription,
//! speech. Every answer carries what it charged and what is left, read from the
//! `x-publik-*` headers, so the app can show a dollar line.

use serde_json::{json, Value};
use std::path::Path;

use crate::http::{self, Body, Request, Response};
use crate::{Line, Provider, IMAGE_MODEL, SPEECH_MODEL, TRANSCRIBE_MODEL};

/// A successful answer.
#[derive(Clone, Debug, Default)]
pub struct Reply {
    pub text: String,
    pub bytes: Vec<u8>,
    pub charge_micros: Option<i64>,
    pub balance_micros: Option<i64>,
    pub claim_state: Option<String>,
    pub model_served: Option<String>,
}

impl Reply {
    /// "Charged $0.0012 · $4.98 left", or empty for an own key.
    pub fn charge_line(&self) -> String {
        match (self.charge_micros, self.balance_micros) {
            (Some(c), Some(b)) => {
                format!("Charged {} · {} left", crate::dollars(c), crate::dollars(b))
            }
            (Some(c), None) => format!("Charged {}", crate::dollars(c)),
            (None, Some(b)) => format!("{} of publik balance left", crate::dollars(b)),
            (None, None) => String::new(),
        }
    }
}

/// A readable failure: what to show, and the one link to render (a 402's `top_up_url`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fail {
    pub status: u16,
    pub kind: String,
    pub message: String,
    pub link_label: String,
    pub link_url: Option<String>,
    /// The key was revoked and the app should provision again.
    pub reprovision: bool,
    pub retry_after_secs: Option<u64>,
}

impl Fail {
    fn transport(message: String) -> Self {
        Fail {
            status: 0,
            kind: "unreachable".into(),
            message,
            ..Default::default()
        }
    }
    pub fn not_set_up() -> Self {
        Fail { status: 0, kind: "not_set_up".into(), message: "AI is not set up on this computer yet. Choose publik API or paste your own key in Options.".into(), ..Default::default() }
    }
    pub fn unsupported(line: Line) -> Self {
        let what = match line {
            Line::Chat => "text",
            Line::Images => "images",
            Line::Transcribe => "transcription",
            Line::Speech => "speech",
        };
        Fail { status: 0, kind: "unsupported".into(), message: format!("Your own key's service does not offer {what} through this app. publik API or an OpenAI key does."), ..Default::default() }
    }
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for Fail {}

fn err_field<'a>(v: &'a Value, key: &str) -> &'a Value {
    let nested = &v["error"][key];
    if nested.is_null() {
        &v[key]
    } else {
        nested
    }
}

/// Map a non-2xx gateway answer to what the UI shows (publik contract §1).
pub fn map_error(res: &Response, publik: bool) -> Fail {
    let v = res.json();
    let kind = err_field(&v, "type").as_str().unwrap_or("").to_string();
    let server_message = err_field(&v, "message").as_str().map(str::to_string);
    let mut f = Fail {
        status: res.status,
        kind: kind.clone(),
        ..Default::default()
    };
    match res.status {
        402 => {
            f.message = server_message
                .unwrap_or_else(|| "Not enough publik API balance for this request.".into());
            f.link_url = crate::publik_link(err_field(&v, "top_up_url").as_str());
            let claimed = err_field(&v, "claim_state").as_str() == Some("claimed")
                || res.header("x-publik-claim-state") == Some("claimed");
            f.link_label = if claimed {
                crate::CTA_ADD_PLAN.into()
            } else {
                crate::CTA_SIGN_IN.into()
            };
        }
        401 => {
            f.reprovision =
                kind == "key_revoked" && err_field(&v, "reprovision").as_bool().unwrap_or(false);
            f.message = if publik {
                if f.reprovision {
                    "This computer's publik API key was retired after a long idle time. Set it up again from Options.".into()
                } else {
                    server_message.unwrap_or_else(|| "publik API did not accept this computer's key. Set it up again from Options.".into())
                }
            } else {
                "The service did not accept your key. Check it in Options.".into()
            };
        }
        429 => {
            f.retry_after_secs = res
                .header("retry-after")
                .and_then(|s| s.trim().parse().ok());
            f.message = server_message
                .unwrap_or_else(|| "Too many requests right now. Try again in a moment.".into());
        }
        503 => {
            f.retry_after_secs = res
                .header("retry-after")
                .and_then(|s| s.trim().parse().ok());
            f.message = server_message.unwrap_or_else(|| {
                "The AI service is unavailable right now. Try again in a moment.".into()
            });
        }
        413 => f.message = "That request is too large. Try a smaller selection or image.".into(),
        _ => {
            f.message =
                server_message.unwrap_or_else(|| format!("The AI service answered {}.", res.status))
        }
    }
    f
}

fn finish(res: Response, publik: bool) -> Result<(Response, Reply), Fail> {
    if !(200..300).contains(&res.status) {
        return Err(map_error(&res, publik));
    }
    let reply = Reply {
        charge_micros: res.header_i64("x-publik-charge-micros"),
        balance_micros: res.header_i64("x-publik-balance"),
        claim_state: res.header("x-publik-claim-state").map(str::to_string),
        model_served: res.header("x-publik-model").map(str::to_string),
        ..Default::default()
    };
    Ok((res, reply))
}

fn creds(p: &Provider, line: Line) -> Result<(&str, &str, &str, bool), Fail> {
    match p {
        Provider::Publik {
            base_url,
            api_key,
            model,
        } => Ok((base_url, api_key, model, true)),
        Provider::Own {
            base_url,
            api_key,
            model,
            ..
        } => {
            if !p.supports(line) {
                return Err(Fail::unsupported(line));
            }
            Ok((base_url, api_key, model, false))
        }
        Provider::NotSetUp => Err(Fail::not_set_up()),
    }
}

fn extra_headers(p: &Provider) -> Vec<(String, String)> {
    match p {
        Provider::Own {
            service, api_key, ..
        } => service.extra_headers(api_key),
        _ => vec![],
    }
}

/// One chat turn. `system` frames the task; `user` is the content. Plain text back.
pub fn chat(p: &Provider, system: &str, user: &str) -> Result<Reply, Fail> {
    chat_messages(
        p,
        json!([{"role": "system", "content": system}, {"role": "user", "content": user}]),
        2048,
    )
}

/// A chat turn whose user content carries an image (`image/png` or `image/jpeg` bytes).
pub fn chat_with_image(
    p: &Provider,
    system: &str,
    user: &str,
    image: &[u8],
    mime: &str,
) -> Result<Reply, Fail> {
    let data = format!("data:{mime};base64,{}", base64(image));
    chat_messages(
        p,
        json!([
            {"role": "system", "content": system},
            {"role": "user", "content": [
                {"type": "text", "text": user},
                {"type": "image_url", "image_url": {"url": data}}
            ]}
        ]),
        2048,
    )
}

pub fn chat_messages(p: &Provider, messages: Value, max_tokens: u32) -> Result<Reply, Fail> {
    let (base, key, model, publik) = creds(p, Line::Chat)?;
    let body = json!({"model": model, "messages": messages, "max_tokens": max_tokens});
    let res = http::send(&Request {
        method: "POST",
        url: &format!("{base}/chat/completions"),
        bearer: Some(key),
        headers: extra_headers(p),
        body: Body::Json(&body),
        timeout_secs: 180,
    })
    .map_err(Fail::transport)?;
    let (res, mut reply) = finish(res, publik)?;
    let v = res.json();
    let text = v["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_default();
    reply.text = text.trim().to_string();
    Ok(reply)
}

/// Text to image. `size` is `1024x1024`, `1536x1024` or `1024x1536`. PNG bytes back.
pub fn image_generate(p: &Provider, prompt: &str, size: &str) -> Result<Reply, Fail> {
    let (base, key, _, publik) = creds(p, Line::Images)?;
    let body = json!({"model": IMAGE_MODEL, "prompt": prompt, "n": 1, "size": size, "output_format": "png"});
    let res = http::send(&Request {
        method: "POST",
        url: &format!("{base}/images/generations"),
        bearer: Some(key),
        headers: extra_headers(p),
        body: Body::Json(&body),
        timeout_secs: 300,
    })
    .map_err(Fail::transport)?;
    let (res, mut reply) = finish(res, publik)?;
    let v = res.json();
    let b64 = v["data"][0]["b64_json"].as_str().unwrap_or("");
    reply.bytes = unbase64(b64).ok_or_else(|| Fail {
        status: res.status,
        kind: "bad_image".into(),
        message: "The service returned no image.".into(),
        ..Default::default()
    })?;
    Ok(reply)
}

/// Edit (or inpaint, when `mask` is given: transparent pixels are replaced). PNG files on disk.
pub fn image_edit(
    p: &Provider,
    prompt: &str,
    image: &Path,
    mask: Option<&Path>,
    size: &str,
) -> Result<Reply, Fail> {
    let (base, key, _, publik) = creds(p, Line::Images)?;
    if !http::readable(image) {
        return Err(Fail {
            kind: "bad_input".into(),
            message: "The image to edit could not be read.".into(),
            ..Default::default()
        });
    }
    let mut files = vec![("image", image.to_path_buf())];
    if let Some(m) = mask {
        files.push(("mask", m.to_path_buf()));
    }
    let text = vec![
        ("model", IMAGE_MODEL.to_string()),
        ("prompt", prompt.to_string()),
        ("n", "1".into()),
        ("size", size.to_string()),
    ];
    let res = http::send(&Request {
        method: "POST",
        url: &format!("{base}/images/edits"),
        bearer: Some(key),
        headers: extra_headers(p),
        body: Body::Form { text, files },
        timeout_secs: 300,
    })
    .map_err(Fail::transport)?;
    let (res, mut reply) = finish(res, publik)?;
    let v = res.json();
    reply.bytes =
        unbase64(v["data"][0]["b64_json"].as_str().unwrap_or("")).ok_or_else(|| Fail {
            status: res.status,
            kind: "bad_image".into(),
            message: "The service returned no image.".into(),
            ..Default::default()
        })?;
    Ok(reply)
}

/// Audio file → text (`whisper-1` line). `language` is an ISO-639-1 hint or empty.
pub fn transcribe(p: &Provider, audio: &Path, language: &str) -> Result<Reply, Fail> {
    let (base, key, _, publik) = creds(p, Line::Transcribe)?;
    if !http::readable(audio) {
        return Err(Fail {
            kind: "bad_input".into(),
            message: "The audio file could not be read.".into(),
            ..Default::default()
        });
    }
    let mut text = vec![
        ("model", TRANSCRIBE_MODEL.to_string()),
        ("response_format", "verbose_json".into()),
    ];
    if !language.trim().is_empty() {
        text.push(("language", language.trim().to_string()));
    }
    let res = http::send(&Request {
        method: "POST",
        url: &format!("{base}/audio/transcriptions"),
        bearer: Some(key),
        headers: extra_headers(p),
        body: Body::Form {
            text,
            files: vec![("file", audio.to_path_buf())],
        },
        timeout_secs: 600,
    })
    .map_err(Fail::transport)?;
    let (res, mut reply) = finish(res, publik)?;
    let v = res.json();
    reply.text = v["text"].as_str().unwrap_or("").trim().to_string();
    // Segments (start, end, text) ride along as JSON for caption tracks.
    reply.bytes = v["segments"].to_string().into_bytes();
    Ok(reply)
}

/// Text → speech (`tts-1` line). MP3 bytes back. `voice` is one of the service's named voices.
pub fn speech(p: &Provider, text: &str, voice: &str) -> Result<Reply, Fail> {
    let (base, key, _, publik) = creds(p, Line::Speech)?;
    let body = json!({"model": SPEECH_MODEL, "input": text, "voice": if voice.is_empty() { "alloy" } else { voice }, "response_format": "mp3"});
    let res = http::send(&Request {
        method: "POST",
        url: &format!("{base}/audio/speech"),
        bearer: Some(key),
        headers: extra_headers(p),
        body: Body::Json(&body),
        timeout_secs: 300,
    })
    .map_err(Fail::transport)?;
    let (res, mut reply) = finish(res, publik)?;
    reply.bytes = res.body;
    Ok(reply)
}

/// `GET /wallet` (publik only): the live balance and claim state.
pub fn wallet(p: &Provider) -> Result<Value, Fail> {
    let (base, key, _, publik) = creds(p, Line::Chat)?;
    if !publik {
        return Err(Fail {
            kind: "not_publik".into(),
            message: "Only publik API has a wallet.".into(),
            ..Default::default()
        });
    }
    let res =
        http::get(&format!("{base}/wallet"), Some(key), vec![], 20).map_err(Fail::transport)?;
    let (res, _) = finish(res, true)?;
    Ok(res.json())
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

pub fn unbase64(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' => continue,
            _ => return None,
        };
        buf = (buf << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for s in ["", "a", "ab", "abc", "hello world", "\u{1F600} png bytes"] {
            assert_eq!(
                unbase64(&base64(s.as_bytes())).unwrap_or_default(),
                s.as_bytes()
            );
        }
        let binary: Vec<u8> = (0..=255u8).collect();
        assert_eq!(unbase64(&base64(&binary)).unwrap_or_default(), binary);
        assert_eq!(base64(b"Man"), "TWFu");
    }

    #[test]
    fn a_402_renders_one_link() {
        let res = Response {
            status: 402,
            headers: vec![("x-publik-claim-state".into(), "anonymous".into())],
            body: br#"{"error":{"type":"insufficient_credit","message":"Not enough balance. Link this computer at the link below.","top_up_url":"https://publikhq.com/claim/AB12","claim_url":"https://publikhq.com/claim/AB12"}}"#.to_vec(),
        };
        let f = map_error(&res, true);
        assert_eq!(
            f.link_url.as_deref(),
            Some("https://publikhq.com/claim/AB12")
        );
        assert_eq!(f.link_label, crate::CTA_SIGN_IN);
        assert!(f.message.starts_with("Not enough"));
    }

    #[test]
    fn a_foreign_top_up_link_is_dropped() {
        let res = Response { status: 402, headers: vec![], body: br#"{"error":{"type":"insufficient_credit","message":"x","top_up_url":"https://evil.example/x"}}"#.to_vec() };
        assert_eq!(map_error(&res, true).link_url, None);
    }

    #[test]
    fn charge_line_reads_in_dollars() {
        let r = Reply {
            charge_micros: Some(1_200),
            balance_micros: Some(4_980_000),
            ..Default::default()
        };
        assert_eq!(r.charge_line(), "Charged $0.0012 · $4.98 left");
    }
}
