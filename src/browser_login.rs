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

    let out = cookies
        .into_iter()
        .map(|c| {
            (
                c.name,
                c.value,
                c.domain.unwrap_or_else(|| LEARN_HOST.into()),
                c.path.unwrap_or_else(|| "/".into()),
            )
        })
        .collect();

    driver.quit().await.ok();
    Ok(out)
}

/// Waits for the authentication redirect cycle, with Enter as a manual fallback.
async fn wait_for_login(driver: &WebDriver) -> Vec<Cookie> {
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
                return driver.get_all_cookies().await.unwrap_or_default();
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
                            let cs = driver.get_all_cookies().await.unwrap_or_default();
                            if !cs.is_empty() {
                                return cs;
                            }
                        }
                    }
                    Err(_) => {
                        // The browser may have been closed manually.
                        return driver.get_all_cookies().await.unwrap_or_default();
                    }
                }
                if waited >= MAX_WAIT_MS {
                    return driver.get_all_cookies().await.unwrap_or_default();
                }
            }
        }
    }
}
