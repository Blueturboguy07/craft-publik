//! HTTP through the system `curl`. The key rides a curl config on stdin (`-K -`),
//! never argv, so it is invisible to `ps`. Bodies ride a private temp file.
//! Redirects are never followed.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Debug, Default)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    pub fn header_i64(&self, name: &str) -> Option<i64> {
        self.header(name).and_then(|v| v.trim().parse().ok())
    }
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

pub enum Body<'a> {
    None,
    Json(&'a serde_json::Value),
    /// Multipart form: `(field, value)` text parts and `(field, path)` file parts.
    Form {
        text: Vec<(&'a str, String)>,
        files: Vec<(&'a str, PathBuf)>,
    },
}

/// Headers the caller sets; the bearer key is passed separately so it can go on stdin.
pub struct Request<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub bearer: Option<&'a str>,
    pub headers: Vec<(String, String)>,
    pub body: Body<'a>,
    pub timeout_secs: u64,
}

fn tmp_file(tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("craft-publik-{tag}-{}-{n}", std::process::id()))
}

struct TempFile(PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn parse_headers(raw: &str) -> (u16, Vec<(String, String)>) {
    // Take the last status block (curl writes one per response, e.g. after a 100-continue).
    let block = raw
        .split("\r\n\r\n")
        .filter(|b| b.trim_start().starts_with("HTTP/"))
        .last()
        .unwrap_or("");
    let mut lines = block.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|l| {
            l.split_once(':')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .collect();
    (status, headers)
}

pub fn send(req: &Request<'_>) -> Result<Response, String> {
    let hdr = TempFile(tmp_file("hdr"));
    let out = TempFile(tmp_file("out"));
    let mut body_file: Option<TempFile> = None;
    let mut args: Vec<String> = vec![
        "-sS".into(),
        "-m".into(),
        req.timeout_secs.to_string(),
        "-X".into(),
        req.method.into(),
        req.url.into(),
        "-K".into(),
        "-".into(),
        "-D".into(),
        hdr.0.to_string_lossy().into_owned(),
        "-o".into(),
        out.0.to_string_lossy().into_owned(),
        "-H".into(),
        "Expect:".into(),
        "-A".into(),
        "craft-publik/0.1".into(),
    ];
    for (k, v) in &req.headers {
        args.push("-H".into());
        args.push(format!("{k}: {v}"));
    }
    match &req.body {
        Body::None => {}
        Body::Json(v) => {
            let p = tmp_file("body");
            crate::credential::write_private(&p, v.to_string().as_bytes())?;
            args.push("-H".into());
            args.push("content-type: application/json".into());
            args.push("--data-binary".into());
            args.push(format!("@{}", p.to_string_lossy()));
            body_file = Some(TempFile(p));
        }
        Body::Form { text, files } => {
            for (k, v) in text {
                args.push("-F".into());
                // A leading @ or < would make curl read a file; neutralise with the literal form.
                args.push(format!("{k}=\"{}\"", v.replace('"', "\\\"")));
            }
            for (k, p) in files {
                args.push("-F".into());
                args.push(format!("{k}=@{}", p.to_string_lossy()));
            }
        }
    }
    let mut child = Command::new("curl")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl could not start: {e}"))?;
    let mut config = String::new();
    if let Some(key) = req.bearer {
        config.push_str(&format!("header = \"Authorization: Bearer {key}\"\n"));
    }
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(config.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let done = child.wait_with_output().map_err(|e| e.to_string())?;
    drop(body_file);
    if !done.status.success() {
        let err = String::from_utf8_lossy(&done.stderr).trim().to_string();
        let code = done.status.code().unwrap_or(-1);
        return Err(match code {
            6 | 7 => {
                "publik API is unreachable right now. Check the connection, or use your own key."
                    .to_string()
            }
            28 => "The request timed out.".to_string(),
            _ => format!("curl failed ({code}): {err}"),
        });
    }
    let raw_hdr = std::fs::read_to_string(&hdr.0).unwrap_or_default();
    let (status, headers) = parse_headers(&raw_hdr);
    let body = std::fs::read(&out.0).unwrap_or_default();
    Ok(Response {
        status,
        headers,
        body,
    })
}

/// Shorthand for a JSON POST with a bearer key.
pub fn post_json(
    url: &str,
    bearer: Option<&str>,
    body: &serde_json::Value,
    timeout_secs: u64,
) -> Result<Response, String> {
    send(&Request {
        method: "POST",
        url,
        bearer,
        headers: vec![],
        body: Body::Json(body),
        timeout_secs,
    })
}

pub fn get(
    url: &str,
    bearer: Option<&str>,
    headers: Vec<(String, String)>,
    timeout_secs: u64,
) -> Result<Response, String> {
    send(&Request {
        method: "GET",
        url,
        bearer,
        headers,
        body: Body::None,
        timeout_secs,
    })
}

/// Whether a path is a readable file (multipart uploads).
pub fn readable(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_parsing_takes_the_last_block() {
        let raw = "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 402 Payment Required\r\nx-publik-balance: 1240\r\nContent-Type: application/json\r\n\r\n";
        let (status, headers) = parse_headers(raw);
        assert_eq!(status, 402);
        assert_eq!(
            headers
                .iter()
                .find(|(k, _)| k == "x-publik-balance")
                .map(|(_, v)| v.as_str()),
            Some("1240")
        );
    }
}
