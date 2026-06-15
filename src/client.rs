//! Tsinghua Learn HTTP client for session reuse, CSRF handling, and raw requests.
//!
//! Browser login is handled by [`crate::browser_login`]. After cookies are
//! imported, this client reuses them and extracts the `_csrf` token required by
//! Learn endpoints.

use anyhow::{anyhow, Context, Result};
use reqwest::Client as HttpClient;
use reqwest_cookie_store::{CookieStore, CookieStoreMutex};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/133.0.0.0 Safari/537.36";

const LEARN_COURSE_LIST_PAGE: &str = "https://learn.tsinghua.edu.cn/f/wlxt/index/course/student/";

pub struct Client {
    http: HttpClient,
    cookie_store: Arc<CookieStoreMutex>,
    cookie_path: PathBuf,
    csrf: tokio::sync::Mutex<Option<String>>,
}

impl Client {
    /// Builds a client and loads saved cookies from disk when present.
    pub fn new(cookie_path: PathBuf) -> Result<Self> {
        let store = load_cookie_store(&cookie_path);
        let cookie_store = Arc::new(CookieStoreMutex::new(store));

        let http = HttpClient::builder()
            .user_agent(UA)
            .cookie_provider(cookie_store.clone())
            .build()
            .context("Failed to build HTTP client")?;

        Ok(Self {
            http,
            cookie_store,
            cookie_path,
            csrf: tokio::sync::Mutex::new(None),
        })
    }

    /// Confirms the saved cookies still produce a `_csrf` token.
    pub async fn confirm_session(&self) -> Result<()> {
        let token = self.fetch_csrf().await?;
        self.set_csrf(token).await;
        self.save_cookies()?;
        Ok(())
    }

    /// Imports cookies captured from the browser and returns the inserted count.
    pub fn import_cookies(&self, cookies: &[(String, String, String, String)]) -> Result<usize> {
        let mut store = self.cookie_store.lock().unwrap();
        let mut n = 0;
        for (name, value, domain, path) in cookies {
            let bare = domain.trim_start_matches('.');
            // Broad cookie domains need a concrete host URL for insertion.
            let host = if bare == "tsinghua.edu.cn" {
                "learn.tsinghua.edu.cn"
            } else {
                bare
            };
            let Ok(url) = url::Url::parse(&format!("https://{host}/")) else {
                continue;
            };
            let mut raw = cookie_store::RawCookie::new(name.clone(), value.clone());
            raw.set_domain(domain.clone());
            raw.set_path(path.clone());
            if store.insert_raw(&raw, &url).is_ok() {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Fetches the course page and extracts `_csrf`; missing token means not logged in.
    async fn fetch_csrf(&self) -> Result<String> {
        let html = self
            .http
            .get(LEARN_COURSE_LIST_PAGE)
            .send()
            .await?
            .text()
            .await?;
        extract_csrf(&html)
            .ok_or_else(|| anyhow!("No _csrf token found on the page; not logged in"))
    }

    async fn set_csrf(&self, token: String) {
        *self.csrf.lock().await = Some(token);
    }

    async fn csrf(&self) -> Result<String> {
        self.csrf
            .lock()
            .await
            .clone()
            .ok_or_else(|| anyhow!("Not logged in; no _csrf token is available"))
    }

    /// Sends a GET with `_csrf` and parses the response body as JSON.
    pub async fn get_json(&self, url: &str) -> Result<serde_json::Value> {
        let csrf = self.csrf().await?;
        let resp = self
            .http
            .get(url)
            .query(&[("_csrf", csrf.as_str())])
            .send()
            .await?;
        self.save_cookies()?;
        let text = resp.text().await?;
        serde_json::from_str(&text).with_context(|| {
            format!(
                "Failed to parse JSON; first 200 response chars: {}",
                snippet(&text)
            )
        })
    }

    /// Sends a GET with `_csrf` and returns the raw response text.
    pub async fn get_text(&self, url: &str) -> Result<String> {
        let csrf = self.csrf().await?;
        let resp = self
            .http
            .get(url)
            .query(&[("_csrf", csrf.as_str())])
            .send()
            .await?;
        self.save_cookies()?;
        Ok(resp.text().await?)
    }

    /// Sends a multipart POST with `_csrf` and parses the response as JSON.
    pub async fn post_multipart_json(
        &self,
        url: &str,
        form: reqwest::multipart::Form,
    ) -> Result<serde_json::Value> {
        let csrf = self.csrf().await?;
        let resp = self
            .http
            .post(url)
            .query(&[("_csrf", csrf.as_str())])
            .multipart(form)
            .send()
            .await?;
        self.save_cookies()?;
        let text = resp.text().await?;
        serde_json::from_str(&text).with_context(|| {
            format!(
                "Failed to parse JSON; first 200 response chars: {}",
                snippet(&text)
            )
        })
    }

    /// Downloads a file to the requested path with `_csrf` included.
    pub async fn download(&self, url: &str, dest: &Path) -> Result<u64> {
        let csrf = self.csrf().await?;
        let resp = self
            .http
            .get(url)
            .query(&[("_csrf", csrf.as_str())])
            .send()
            .await?
            .error_for_status()?;
        self.save_cookies()?;
        let bytes = resp.bytes().await?;
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        tokio::fs::write(dest, &bytes).await?;
        Ok(bytes.len() as u64)
    }

    /// Downloads bytes and returns the file name suggested by `Content-Disposition`.
    pub async fn fetch_download(&self, url: &str) -> Result<(Vec<u8>, Option<String>)> {
        let csrf = self.csrf().await?;
        let resp = self
            .http
            .get(url)
            .query(&[("_csrf", csrf.as_str())])
            .send()
            .await?
            .error_for_status()?;
        self.save_cookies()?;
        let cd = resp.headers().get(reqwest::header::CONTENT_DISPOSITION);
        let name = cd.and_then(|v| filename_from_content_disposition(v.as_bytes()));
        let bytes = resp.bytes().await?.to_vec();
        Ok((bytes, name))
    }

    /// Saves current cookies to disk for later reuse.
    #[allow(deprecated)]
    pub fn save_cookies(&self) -> Result<()> {
        if let Some(parent) = self.cookie_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut writer = std::io::BufWriter::new(std::fs::File::create(&self.cookie_path)?);
        let store = self.cookie_store.lock().unwrap();
        // Learn session cookies are nonpersistent, so ordinary save_json would drop them.
        store
            .save_incl_expired_and_nonpersistent_json(&mut writer)
            .map_err(|e| anyhow!("Failed to save cookies: {e}"))?;
        Ok(())
    }
}

#[allow(deprecated)]
fn load_cookie_store(path: &Path) -> CookieStore {
    let Ok(file) = std::fs::File::open(path) else {
        return CookieStore::default();
    };
    let reader = std::io::BufReader::new(file);
    // load_json_all restores session cookies; expired cookies are filtered on use.
    CookieStore::load_json_all(reader).unwrap_or_default()
}

/// Extracts `_csrf` from course-page links without assuming a token alphabet.
fn extract_csrf(html: &str) -> Option<String> {
    let re = regex::Regex::new(r#"_csrf=([^"'&\s]+)"#).ok()?;
    re.captures(html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

fn snippet(s: &str) -> String {
    s.chars().take(200).collect()
}

/// Percent-decodes a string into raw bytes.
fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(x) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(x);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Replaces path separators so server file names cannot escape the target directory.
fn sanitize_filename(name: &str) -> String {
    name.trim().trim_matches('"').replace(['/', '\\'], "_")
}

/// Parses a file name from `Content-Disposition`, preferring RFC5987 `filename*`.
fn filename_from_content_disposition(raw: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(raw);

    if let Some(idx) = s.find("filename*=") {
        let rest = &s[idx + "filename*=".len()..];
        let val = rest.split(';').next().unwrap_or(rest).trim();
        let mut parts = val.splitn(3, '\'');
        let _charset = parts.next();
        let _lang = parts.next();
        if let Some(pct) = parts.next() {
            if let Ok(name) = String::from_utf8(percent_decode(pct)) {
                let name = sanitize_filename(&name);
                if !name.is_empty() {
                    return Some(name);
                }
            }
        }
    }

    if let Some(idx) = s.find("filename=") {
        let rest = &s[idx + "filename=".len()..];
        let val = rest.split(';').next().unwrap_or(rest);
        let name = sanitize_filename(val);
        // Replacement characters mean the raw header was probably not UTF-8, so ignore it.
        if !name.is_empty() && !name.contains('\u{FFFD}') {
            return Some(name);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{extract_csrf, filename_from_content_disposition};

    #[test]
    fn cd_filename_ascii() {
        let cd = br#"attachment; filename="Physics(1)_Mock Exam.pdf""#;
        assert_eq!(
            filename_from_content_disposition(cd).as_deref(),
            Some("Physics(1)_Mock Exam.pdf")
        );
    }

    #[test]
    fn cd_filename_utf8_bytes() {
        // Realistic Learn fixture: filename="<UTF-8 bytes>".
        let mut cd = b"attachment; filename=\"".to_vec();
        cd.extend_from_slice("数分2作业13.pdf".as_bytes());
        cd.push(b'"');
        assert_eq!(
            filename_from_content_disposition(&cd).as_deref(),
            Some("数分2作业13.pdf")
        );
    }

    #[test]
    fn cd_filename_rfc5987() {
        let cd = b"attachment; filename*=UTF-8''%E6%95%B0.pdf";
        assert_eq!(
            filename_from_content_disposition(cd).as_deref(),
            Some("数.pdf")
        );
    }

    #[test]
    fn cd_filename_sanitizes_slash() {
        let cd = br#"attachment; filename="a/b.pdf""#;
        assert_eq!(
            filename_from_content_disposition(cd).as_deref(),
            Some("a_b.pdf")
        );
    }

    #[test]
    fn csrf_extracted() {
        let html = r#"<a href="/x?course=1&_csrf=abc-123-def">link</a>"#;
        assert_eq!(extract_csrf(html).as_deref(), Some("abc-123-def"));
    }
}
