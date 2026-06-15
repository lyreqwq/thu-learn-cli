//! Browser login through Chrome WebDriver, followed by session cookie capture.
//!
//! The login session is stored in httpOnly cookies, so JavaScript cannot read
//! it; WebDriver's `get_all_cookies` can retrieve it through the driver protocol.
//!
//! `WebDriver::managed` downloads and starts a matching chromedriver, while a
//! local Google Chrome installation is still required.

use anyhow::{Context, Result};
use std::time::Duration;
use thirtyfour::prelude::*;

/// Start page, which redirects to unified authentication when needed.
const LEARN_HOME: &str = "https://learn.tsinghua.edu.cn/f/wlxt/index/course/student/";
/// Unified authentication host.
const AUTH_HOST: &str = "id.tsinghua.edu.cn";
/// Learn host reached after successful login.
const LEARN_HOST: &str = "learn.tsinghua.edu.cn";

const POLL_INTERVAL_MS: u64 = 1500;
const MAX_WAIT_MS: u64 = 300_000; // 5 minutes.

/// Captured cookie tuple: name, value, domain, and path.
pub type RawCookie = (String, String, String, String);

/// Opens Chrome, waits for login, and returns captured session cookies.
pub async fn login_via_browser() -> Result<Vec<RawCookie>> {
    let caps = DesiredCapabilities::chrome();
    // managed resolves, downloads, starts, and later cleans up chromedriver.
    let driver = WebDriver::managed(caps)
        .await
        .context("Failed to start managed chromedriver; make sure Google Chrome is installed")?;

    driver
        .goto(LEARN_HOME)
        .await
        .context("Failed to open Tsinghua Learn")?;

    eprintln!("A browser window is open. Complete Tsinghua authentication there.");
    eprintln!("After the Learn home page loads, this command continues automatically. If it does not, return here and press Enter.");

    let cookies = wait_for_login(&driver).await;

    driver.quit().await.ok();
    Ok(cookies)
}

/// Waits for the authentication redirect cycle, with Enter as a manual fallback.
async fn wait_for_login(driver: &WebDriver) -> Vec<RawCookie> {
    // Convert Enter into a cancellable channel event for tokio::select!.
    let (tx, mut enter_rx) = tokio::sync::mpsc::channel::<()>(1);
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::stdin().read_line(&mut s);
        let _ = tx.blocking_send(());
    });

    let mut seen_auth = false;
    let mut waited = 0u64;

    loop {
        tokio::select! {
            // Manual Enter fallback.
            _ = enter_rx.recv() => {
                return capture_cookies(driver).await;
            }
            // Poll the current URL periodically.
            _ = tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)) => {
                waited += POLL_INTERVAL_MS;
                match driver.current_url().await {
                    Ok(u) => {
                        let s = u.as_str();
                        if s.contains(AUTH_HOST) {
                            // The flow really reached the authentication page.
                            seen_auth = true;
                        } else if seen_auth && s.contains(LEARN_HOST) {
                            // After returning to Learn, wait briefly for cookies to settle.
                            tokio::time::sleep(Duration::from_millis(800)).await;
                            let cs = capture_cookies(driver).await;
                            if !cs.is_empty() {
                                return cs;
                            }
                        }
                    }
                    Err(_) => {
                        // The browser may have been closed manually.
                        return capture_cookies(driver).await;
                    }
                }
                if waited >= MAX_WAIT_MS {
                    return capture_cookies(driver).await;
                }
            }
        }
    }
}

async fn capture_cookies(driver: &WebDriver) -> Vec<RawCookie> {
    if let Ok(value) = driver.cdp().send_raw("Network.getAllCookies", ()).await {
        let cookies = cdp_cookies_to_raw(&value);
        if !cookies.is_empty() {
            return cookies;
        }
    }
    driver
        .get_all_cookies()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(webdriver_cookie_to_raw)
        .collect()
}

fn webdriver_cookie_to_raw(c: Cookie) -> RawCookie {
    (
        c.name,
        c.value,
        c.domain.unwrap_or_else(|| LEARN_HOST.into()),
        c.path.unwrap_or_else(|| "/".into()),
    )
}

fn cdp_cookies_to_raw(value: &serde_json::Value) -> Vec<RawCookie> {
    value
        .get("cookies")
        .and_then(|cookies| cookies.as_array())
        .into_iter()
        .flatten()
        .filter_map(|c| {
            Some((
                c.get("name")?.as_str()?.to_string(),
                c.get("value")?.as_str()?.to_string(),
                c.get("domain")
                    .and_then(|d| d.as_str())
                    .unwrap_or(LEARN_HOST)
                    .to_string(),
                c.get("path")
                    .and_then(|p| p.as_str())
                    .unwrap_or("/")
                    .to_string(),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::cdp_cookies_to_raw;

    #[test]
    fn cdp_cookie_capture_keeps_cross_domain_cookies() {
        let value = serde_json::json!({
            "cookies": [
                {
                    "name": "LEARN",
                    "value": "learn-session",
                    "domain": "learn.tsinghua.edu.cn",
                    "path": "/"
                },
                {
                    "name": "JSESSIONID",
                    "value": "id-session",
                    "domain": "id.tsinghua.edu.cn",
                    "path": "/"
                }
            ]
        });

        let cookies = cdp_cookies_to_raw(&value);

        assert_eq!(cookies.len(), 2);
        assert!(cookies
            .iter()
            .any(|(_, _, domain, _)| domain == "learn.tsinghua.edu.cn"));
        assert!(cookies
            .iter()
            .any(|(_, _, domain, _)| domain == "id.tsinghua.edu.cn"));
    }
}
