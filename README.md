# thu-learn-cli

`thu-learn-cli` is a Rust command-line client for Tsinghua University's Web Learning (<https://learn.tsinghua.edu.cn>). It helps students inspect homework deadlines, read announcements, download course files, and submit homework from a terminal.

## Features

* Browser-based login through the official Tsinghua authentication flow.
* Homework listing, detail view, attachment download, and submission.
* Course announcement listing and detail view.
* Course file listing, detail view, and download.
* Structured JSON output for scriptable commands.

## Installation

Build the release binary from the repository root:

```bash
cargo build --release
```

The binary is written to `target/release/thu-learn`.

## Authentication

Run:

```bash
thu-learn login
```

The command opens a Google Chrome window and lets you complete Tsinghua's normal web login flow, including captcha, multi-factor authentication, and device checks. After the Learn home page is reached, the CLI imports the session cookies and reuses them for later commands. It never stores an account password.

Requirements and storage behavior:

* Google Chrome must be installed locally.
* Chromedriver is downloaded and managed by `thirtyfour::WebDriver::managed`.
* Session cookies are stored at `~/.config/thu-learn-cli/cookies.json`.
* Semester and course-list cache files are stored under `~/.cache/thu-learn-cli/`.
* Running `thu-learn login` clears the cache after importing cookies.

## Usage

List entries show a seven-character short ID. Use that short ID for detail, download, and submit commands; full server IDs are also accepted where supported.

```bash
thu-learn login

# Homework
thu-learn hw
thu-learn hw -a
thu-learn hw --overdue
thu-learn hw <id>
thu-learn hw <id> -d ./dir

# Announcements
thu-learn ann
thu-learn ann <id>

# Course files
thu-learn f ls
thu-learn f ls -c "Data Structures"
thu-learn f show <id>
thu-learn f get <id>

# Homework submission
thu-learn submit <id> ./hw.pdf -c "submission note"

# JSON output
thu-learn hw --json
```

Notes:

* Human output uses color when stdout supports it; `--json` output is plain structured data.
* The current semester and course list are cached for six hours.
* Homework, announcement, and file requests are intentionally concurrent.
* Session expiration is reported by any command that needs authentication; run `thu-learn login` again to refresh cookies.

## Development

Recommended local checks:

```bash
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo build --release
```

Focused examples:

```bash
cargo test api::tests::parse_deadline_invalid
cargo test client::tests::csrf_extracted
cargo test cli::tests::prev_semester_invalid
```

The hidden `thu-learn debug` command prints selected raw Learn API responses for field investigation. It requires a valid login session.

## Data Safety

`~/.config/thu-learn-cli/cookies.json` contains session credentials. Do not print it, copy it into a repository, commit it, or share it. Rebuildable cache data is stored under `~/.cache/thu-learn-cli/`. Automated tests should not require a live Web Learning account, Chrome, chromedriver, or an existing cookie file.

## Acknowledgements

* Learn API behavior references [thu-learn-lib](https://github.com/Harry-Chen/thu-learn-lib) (MIT, copyright 2019-2022 Harry Chen).
* Browser-login behavior references [reserves-lib-tsinghua-downloader](https://github.com/libthu/reserves-lib-tsinghua-downloader) (GPL-3.0).

## License

This project is licensed under [GPL-3.0-or-later](LICENSE).

Use this tool only with your own Tsinghua account and follow the Learn platform terms of service. Do not use it for bulk scraping or abuse.
