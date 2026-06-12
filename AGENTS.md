# AGENTS.md

## Project Facts

* Single Rust 2021 binary crate named `thu-learn`; `src/main.rs` is the only binary entrypoint and release output is `target/release/thu-learn`.
* The CLI talks to Tsinghua Learn at `learn.tsinghua.edu.cn`; project-owned docs, comments, help text, and runtime output should be English. Preserve Chinese only for Learn protocol literals and realistic fixtures, and document those cases when they appear.

## Module Boundaries

* `src/cli.rs`: clap command surface, aliases, human vs `--json` output, login flow, cache clearing, and command orchestration.
* `src/client.rs`: reqwest client, cookie store, `_csrf` extraction, authenticated GET/POST/download helpers, and cookie persistence.
* `src/api.rs`: Learn API endpoints plus JSON/HTML parsing for courses, homework, announcements, files, and submit.
* `src/browser_login.rs`: Chrome WebDriver login; `thirtyfour::WebDriver::managed` downloads/starts chromedriver, but local Google Chrome is still required.
* `src/cache.rs` and `src/paths.rs`: current-semester/course cache and `cookies.json` location; `src/models.rs` owns serialized output models and short ids.

## Local Checks

* Run from the repo root: `cargo test`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, then `cargo build --release`.
* Focused examples: `cargo test api::tests::parse_deadline_invalid`, `cargo test client::tests::csrf_extracted`, `cargo test cli::tests::prev_semester_invalid`.
* There is no CI, task runner, rustfmt config, or clippy config in this repo; trust Cargo commands and source tests over assumptions.

## Runtime And Data Safety

* Session cookies are stored at `~/.config/thu-learn-cli/cookies.json`; this file contains session credentials and must never be printed, copied into the repo, committed, shared, or exposed.
* The cache is under `~/.cache/thu-learn-cli/`; `thu-learn login` clears it after importing cookies.
* The old `./cookies.json` file and old macOS app support cache are not migrated automatically, so users may need to log in again.
* Do not make automated tests require live `learn.tsinghua.edu.cn`, a real Tsinghua account, Chrome, chromedriver, or an existing `cookies.json`.

## Learn API Quirks

* Network Learning JSON fields are pinyin abbreviations and can vary; keep the `serde_json::Value` plus candidate-key parsing style in `src/api.rs`.
* Explain pinyin API fields near parsing/model code with both Chinese and English names, for example `xszyid`: 学生作业 ID / student homework ID.
* Session cookies are nonpersistent. `src/client.rs` intentionally saves with `save_incl_expired_and_nonpersistent_json` and loads with `load_json_all`; replacing these with ordinary cookie-store save/load breaks login reuse.
* Authenticated GET, POST, and download requests need `_csrf` from the course page before calling Learn endpoints.
* Course and announcement/file fetching is intentionally concurrent in several paths; avoid serializing it unless debugging or fixing a measured issue.

## Testing And CLI Behavior

* Existing tests are inline unit tests in `src/api.rs`, `src/client.rs`, and `src/cli.rs`; add pure parser/helper tests near the code being changed.
* Use `--json` for scriptable CLI checks when supported; human output uses colors only when stdout supports them.
* Hidden command `thu-learn debug` prints raw JSON for field checks, but it requires a valid login session.
