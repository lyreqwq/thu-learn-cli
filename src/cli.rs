//! Command-line interface and subcommand orchestration.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::browser_login;
use crate::client::Client;
use crate::models::{short_id, Course, HomeworkStatus};
use crate::paths;
use owo_colors::{OwoColorize, Stream::Stdout};
use unicode_width::UnicodeWidthStr;

/// Dims IDs and secondary details when stdout supports color.
fn dim(s: &str) -> String {
    format!("{}", s.if_supports_color(Stdout, |t| t.dimmed()))
}

#[derive(Debug, Clone)]
struct TableCell {
    text: String,
    width: usize,
}

impl TableCell {
    fn plain(raw: impl Into<String>) -> Self {
        let text = sanitize_table_text(&raw.into());
        let width = UnicodeWidthStr::width(text.as_str());
        Self { text, width }
    }

    fn styled(raw: &str, styled: String) -> Self {
        let text = sanitize_table_text(raw);
        let display = if text == raw { styled } else { text.clone() };
        Self {
            text: display,
            width: UnicodeWidthStr::width(text.as_str()),
        }
    }
}

fn sanitize_table_text(raw: &str) -> String {
    strip_terminal_controls(raw)
}

fn strip_terminal_controls(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            strip_escape_sequence(&mut chars);
        } else if c == '\u{9b}' {
            strip_csi_sequence(&mut chars);
        } else if is_table_format_char(c) {
            continue;
        } else if is_table_control_char(c) {
            out.push(' ');
        } else {
            out.push(c);
        }
    }

    out
}

fn strip_escape_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.peek().copied() {
        Some('[') => {
            chars.next();
            strip_csi_sequence(chars);
        }
        Some(']') => {
            chars.next();
            let mut saw_esc = false;
            for c in chars.by_ref() {
                if saw_esc && c == '\\' {
                    break;
                }
                saw_esc = c == '\u{1b}';
                if c == '\u{7}' {
                    break;
                }
            }
        }
        Some(_) => {
            chars.next();
        }
        None => {}
    }
}

fn strip_csi_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for c in chars.by_ref() {
        if ('@'..='~').contains(&c) {
            break;
        }
    }
}

fn is_table_control_char(c: char) -> bool {
    c.is_control()
}

fn is_table_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}'
            | '\u{034f}'
            | '\u{061c}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}'
    )
}

/// Renders a rounded box-drawing table with dimmed borders and bold headers.
fn render_table(headers: &[&str], rows: &[Vec<TableCell>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| UnicodeWidthStr::width(*h)).collect();
    for row in rows {
        for (idx, cell) in row.iter().enumerate() {
            if let Some(width) = widths.get_mut(idx) {
                *width = (*width).max(cell.width);
            }
        }
    }

    let bar = dim("│");
    let mut lines = Vec::with_capacity(rows.len() + 4);
    lines.push(table_rule('╭', '┬', '╮', &widths));
    lines.push(render_table_row(
        &bar,
        headers.iter().enumerate().map(|(idx, &header)| {
            let styled = format!("{}", header.if_supports_color(Stdout, |t| t.bold()));
            (styled, UnicodeWidthStr::width(header), widths[idx])
        }),
    ));
    lines.push(table_rule('├', '┼', '┤', &widths));
    for row in rows {
        lines.push(render_table_row(
            &bar,
            row.iter()
                .enumerate()
                .map(|(idx, cell)| (cell.text.clone(), cell.width, widths[idx])),
        ));
    }
    lines.push(table_rule('╰', '┴', '╯', &widths));
    lines.join("\n")
}

/// Builds a horizontal border line, dimmed when stdout supports color.
fn table_rule(left: char, mid: char, right: char, widths: &[usize]) -> String {
    let mut s = String::with_capacity(widths.iter().sum::<usize>() + widths.len() * 4 + 2);
    s.push(left);
    for (idx, width) in widths.iter().enumerate() {
        if idx > 0 {
            s.push(mid);
        }
        s.extend(std::iter::repeat_n('─', width + 2));
    }
    s.push(right);
    dim(&s)
}

/// Builds one content row: `bar` separates cells, each padded with a space on
/// both sides. `width` is the display width of `text` (ANSI codes excluded).
fn render_table_row(bar: &str, cells: impl IntoIterator<Item = (String, usize, usize)>) -> String {
    let mut s = String::new();
    s.push_str(bar);
    for (text, width, column_width) in cells {
        let padding = column_width.saturating_sub(width);
        s.push(' ');
        s.push_str(&text);
        s.push_str(&" ".repeat(padding + 1));
        s.push_str(bar);
    }
    s
}

fn format_deadline(
    deadline: Option<chrono::DateTime<chrono::Local>>,
    deadline_raw: &str,
    now: chrono::DateTime<chrono::Local>,
) -> (String, String) {
    match deadline {
        Some(d) => {
            let remain = d.signed_duration_since(now);
            let days = remain.num_days();
            let when = d.format("%Y-%m-%d %H:%M").to_string();
            if remain.num_seconds() < 0 {
                let raw = format!("{when} (overdue)");
                let styled = format!(
                    "{when} ({})",
                    "overdue".if_supports_color(Stdout, |t| t.bright_red())
                );
                (raw, styled)
            } else if days == 0 {
                let tag = format!("within {}h today", remain.num_hours());
                let styled = format!(
                    "{when} ({})",
                    tag.if_supports_color(Stdout, |t| t.bright_red())
                );
                (format!("{when} ({tag})"), styled)
            } else if days <= 3 {
                let tag = format!("{days} days left");
                let styled = format!("{when} ({})", tag.if_supports_color(Stdout, |t| t.yellow()));
                (format!("{when} ({tag})"), styled)
            } else {
                let tag = format!("{days} days left");
                let styled = format!("{when} ({})", tag.if_supports_color(Stdout, |t| t.green()));
                (format!("{when} ({tag})"), styled)
            }
        }
        None => (deadline_raw.to_string(), deadline_raw.to_string()),
    }
}

fn homework_status_cell(status: HomeworkStatus) -> TableCell {
    let raw = status.label();
    let styled = match status {
        HomeworkStatus::Submitted => format!("{}", raw.if_supports_color(Stdout, |t| t.green())),
        HomeworkStatus::Graded => format!("{}", raw.if_supports_color(Stdout, |t| t.cyan())),
        HomeworkStatus::Pending => raw.to_string(),
    };
    TableCell::styled(raw, styled)
}

fn render_homework_table(
    hws: &[crate::models::Homework],
    now: chrono::DateTime<chrono::Local>,
) -> String {
    let has_grade = hws.iter().any(|h| h.grade.is_some());
    let mut headers = vec!["ID", "Status", "Course", "Title", "Deadline"];
    if has_grade {
        headers.push("Grade");
    }

    let rows = hws
        .iter()
        .map(|h| {
            let course = if h.course_name.is_empty() {
                "(previous course)"
            } else {
                &h.course_name
            };
            let (deadline_raw, deadline_styled) = format_deadline(h.deadline, &h.deadline_raw, now);
            let id = short_id(&h.student_homework_id);
            let mut row = vec![
                TableCell::styled(&id, dim(&id)),
                homework_status_cell(h.status),
                TableCell::styled(
                    course,
                    format!("{}", course.if_supports_color(Stdout, |t| t.bold())),
                ),
                TableCell::plain(h.title.as_str()),
                TableCell::styled(&deadline_raw, deadline_styled),
            ];
            if has_grade {
                row.push(TableCell::plain(h.grade.as_deref().unwrap_or("")));
            }
            row
        })
        .collect::<Vec<_>>();

    render_table(&headers, &rows)
}

fn render_announcement_table(notes: &[crate::models::Notification]) -> String {
    let rows = notes
        .iter()
        .map(|n| {
            let state = if n.read { "Read" } else { "Unread" };
            let id = short_id(&n.id);
            let title = if n.read {
                n.title.clone()
            } else {
                format!("{}", n.title.if_supports_color(Stdout, |t| t.bold()))
            };
            vec![
                TableCell::styled(&id, dim(&id)),
                TableCell::plain(state),
                TableCell::styled(
                    &n.course_name,
                    format!("{}", n.course_name.if_supports_color(Stdout, |t| t.cyan())),
                ),
                TableCell::styled(&n.title, title),
                TableCell::plain(&n.publisher),
                TableCell::plain(&n.publish_time),
            ]
        })
        .collect::<Vec<_>>();

    render_table(
        &["ID", "State", "Course", "Title", "Publisher", "Published"],
        &rows,
    )
}

fn render_course_file_table(files: &[crate::models::CourseFile]) -> String {
    let rows = files
        .iter()
        .map(|f| {
            let id = short_id(&f.id);
            vec![
                TableCell::styled(&id, dim(&id)),
                TableCell::plain(f.title.as_str()),
                TableCell::plain(f.size.as_str()),
                TableCell::plain(f.upload_time.as_str()),
            ]
        })
        .collect::<Vec<_>>();

    render_table(&["ID", "Title", "Size", "Uploaded"], &rows)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CompletionItem {
    value: String,
    description: String,
}

fn completion_items_for_homework(hws: &[crate::models::Homework]) -> Vec<CompletionItem> {
    hws.iter()
        .map(|h| CompletionItem {
            value: short_id(&h.student_homework_id),
            description: format!("{} | {}", h.course_name, h.title),
        })
        .collect()
}

fn completion_items_for_announcements(
    notes: &[crate::models::Notification],
) -> Vec<CompletionItem> {
    notes
        .iter()
        .map(|n| CompletionItem {
            value: short_id(&n.id),
            description: format!("{} | {}", n.course_name, n.title),
        })
        .collect()
}

fn completion_items_for_files(files: &[crate::models::CourseFile]) -> Vec<CompletionItem> {
    files
        .iter()
        .map(|f| CompletionItem {
            value: short_id(&f.id),
            description: format!("{} | {}", f.course_name, f.title),
        })
        .collect()
}

fn filter_completion_items(items: &[CompletionItem], prefix: &str) -> Vec<CompletionItem> {
    let mut matches = items
        .iter()
        .filter(|item| item.value.starts_with(prefix))
        .cloned()
        .collect::<Vec<_>>();
    matches.sort_by(|a, b| a.value.cmp(&b.value));
    matches
}

enum IdResolution<'a, T> {
    Found(&'a T),
    NotFound,
    Ambiguous(Vec<String>),
}

fn resolve_item_prefix<'a, T>(
    items: &'a [T],
    query: &str,
    id_of: impl Fn(&T) -> &str,
) -> IdResolution<'a, T> {
    let exact = items
        .iter()
        .filter(|item| {
            let full = id_of(item);
            full == query || short_id(full) == query
        })
        .collect::<Vec<_>>();
    if let Some(item) = exact.first() {
        return IdResolution::Found(item);
    }

    let prefix_matches = if query.len() < 7 {
        items
            .iter()
            .filter(|item| short_id(id_of(item)).starts_with(query))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    match prefix_matches.as_slice() {
        [item] => IdResolution::Found(item),
        [] => IdResolution::NotFound,
        many => IdResolution::Ambiguous(
            many.iter()
                .map(|item| short_id(id_of(item)))
                .collect::<Vec<_>>(),
        ),
    }
}

fn resolve_item_by_id<'a, T>(
    items: &'a [T],
    query: &str,
    id_of: impl Fn(&T) -> &str,
    name: &str,
    hint: &str,
) -> Result<&'a T> {
    match resolve_item_prefix(items, query, id_of) {
        IdResolution::Found(item) => Ok(item),
        IdResolution::NotFound => anyhow::bail!("{name} ID not found: {query} ({hint})"),
        IdResolution::Ambiguous(candidates) => {
            let candidates = candidates.join(", ");
            anyhow::bail!("{name} ID prefix is ambiguous: {query} (candidates: {candidates})")
        }
    }
}

fn format_completion_line(item: &CompletionItem) -> String {
    format!("{}\t{}", item.value, item.description.replace('\t', " "))
}

fn completion_cache_key(kind: CompletionKind) -> &'static str {
    match kind {
        CompletionKind::Homework => "completion_homework",
        CompletionKind::Announcement => "completion_announcement",
        CompletionKind::File => "completion_file",
    }
}

fn write_completion_cache(kind: CompletionKind, items: &[CompletionItem]) {
    crate::cache::write_json(completion_cache_key(kind), items);
}

fn read_completion_cache(kind: CompletionKind) -> Vec<CompletionItem> {
    crate::cache::read_json(completion_cache_key(kind)).unwrap_or_default()
}

fn zsh_completion_script() -> &'static str {
    r#"#compdef thu-learn

_thu_learn_cached_ids() {
  local kind="$1"
  local -a lines
  lines=("${(@f)$("${words[1]}" __complete "$kind" "$PREFIX" 2>/dev/null)}")
  if (( ${#lines} )); then
    local -a candidates
    local line value desc
    for line in "${lines[@]}"; do
      value="${line%%$'\t'*}"
      desc="${line#*$'\t'}"
      candidates+=("${value}:${desc}")
    done
    _describe -t "thu-learn-${kind}-ids" "${kind} ids" candidates
  fi
}

_thu_learn() {
  local -a commands file_commands
  commands=(
    "login:log in through Chrome"
    "homework:list or show homework"
    "hw:list or show homework"
    "announcement:list or show announcements"
    "ann:list or show announcements"
    "file:list, show, or download course files"
    "f:list, show, or download course files"
    "submit:submit homework"
    "completion:print shell completion scripts"
  )
  file_commands=(
    "ls:list course files"
    "show:show file details"
    "get:download a course file"
  )

  case "$words[2]" in
    homework|hw)
      if (( CURRENT == 3 )); then
        _thu_learn_cached_ids homework
      else
        _arguments '*::arg:->args'
      fi
      ;;
    announcement|ann)
      if (( CURRENT == 3 )); then
        _thu_learn_cached_ids announcement
      else
        _arguments '*::arg:->args'
      fi
      ;;
    submit)
      if (( CURRENT == 3 )); then
        _thu_learn_cached_ids homework
      elif (( CURRENT == 4 )); then
        _files
      else
        _arguments '*::arg:->args'
      fi
      ;;
    file|f)
      if (( CURRENT == 3 )); then
        _describe -t thu-learn-file-commands 'file command' file_commands
      elif (( CURRENT == 4 )) && [[ "$words[3]" == (show|get) ]]; then
        _thu_learn_cached_ids file
      else
        _arguments '*::arg:->args'
      fi
      ;;
    completion)
      if (( CURRENT == 3 )); then
        _values 'shell' zsh
      fi
      ;;
    *)
      if (( CURRENT == 2 )); then
        _describe -t thu-learn-commands 'command' commands
      else
        _arguments '*::arg:->args'
      fi
      ;;
  esac
}

_thu_learn "$@"
"#
}

#[derive(Parser)]
#[command(
    name = "thu-learn",
    version,
    about = "Tsinghua Web Learning command-line client for homework deadlines, files, announcements, and submissions"
)]
pub struct Cli {
    /// Print JSON output for supported commands.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Log in through Chrome and save the session locally.
    Login,
    /// List homework, or show one homework item when an ID is provided.
    #[command(visible_alias = "hw")]
    Homework {
        /// Homework short ID, unique short-ID prefix, or full `xszyid`.
        id: Option<String>,
        /// Show all homework, including submitted, graded, and overdue items.
        #[arg(short, long)]
        all: bool,
        /// Show only overdue pending homework.
        #[arg(long)]
        overdue: bool,
        /// With <id>, download that homework item's attachments to this directory.
        #[arg(short, long)]
        download: Option<PathBuf>,
    },
    /// List course announcements, or show one announcement when an ID is provided.
    #[command(visible_alias = "ann")]
    Announcement {
        /// Announcement short ID, unique short-ID prefix, or full ID.
        id: Option<String>,
    },
    /// List, show, or download course files.
    #[command(visible_alias = "f")]
    File {
        #[command(subcommand)]
        command: FileCommands,
    },
    /// Print raw JSON from selected Learn endpoints for field investigation.
    #[command(hide = true)]
    Debug,

    /// Print shell completion scripts.
    Completion {
        #[command(subcommand)]
        shell: CompletionShell,
    },

    /// Print cached completion candidates.
    #[command(name = "__complete", hide = true)]
    Complete {
        kind: CompletionKind,
        prefix: Option<String>,
    },

    /// Submit homework.
    Submit {
        /// Homework short ID, unique short-ID prefix, or full `xszyid`.
        homework_id: String,
        /// File path to submit.
        file: PathBuf,
        /// Optional text comment.
        #[arg(short, long, default_value = "")]
        comment: String,
    },
}

#[derive(Subcommand)]
enum CompletionShell {
    /// Print zsh completion script.
    Zsh,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum CompletionKind {
    Homework,
    Announcement,
    File,
}

#[derive(Subcommand)]
enum FileCommands {
    /// List course files, optionally filtered by course name.
    Ls {
        /// Show only courses whose names contain this substring.
        #[arg(short, long)]
        course: Option<String>,
    },
    /// Show file details including type, size, and description.
    Show {
        /// File short ID, unique short-ID prefix, or full ID.
        file_id: String,
    },
    /// Download a course file.
    Get {
        /// File short ID, unique short-ID prefix, or full ID.
        file_id: String,
        /// Output path. Defaults to the server-provided file name in the current directory.
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    let json = cli.json;
    match cli.command {
        Commands::Login => cmd_login().await,
        Commands::Homework {
            id,
            all,
            overdue,
            download,
        } => match id {
            Some(xszyid) => cmd_homework_show(xszyid, download, json).await,
            None => cmd_homework(all, overdue, json).await,
        },
        Commands::Announcement { id } => match id {
            Some(x) => cmd_announcement_show(x, json).await,
            None => cmd_announcement(json).await,
        },
        Commands::Debug => cmd_debug().await,
        Commands::Completion { shell } => cmd_completion(shell),
        Commands::Complete { kind, prefix } => cmd_complete(kind, prefix),
        Commands::File { command } => match command {
            FileCommands::Ls { course } => cmd_file_ls(course, json).await,
            FileCommands::Show { file_id } => cmd_file_show(file_id, json).await,
            FileCommands::Get { file_id, out } => cmd_file_get(file_id, out).await,
        },
        Commands::Submit {
            homework_id,
            file,
            comment,
        } => cmd_submit(homework_id, file, comment).await,
    }
}

// ---------- Login and course loading ----------

/// Reuses saved session cookies, or asks the user to run `login` again.
async fn login_only() -> Result<Client> {
    let client = Client::new(paths::cookie_path())?;
    client.confirm_session().await.map_err(|_| {
        anyhow::anyhow!("Not logged in or the session has expired. Run `thu-learn login` first.")
    })?;
    Ok(client)
}

async fn prepare() -> Result<(Client, Vec<Course>)> {
    let client = login_only().await?;
    let semester = client.current_semester().await?;
    let courses = client.course_list(&semester).await?;
    Ok((client, courses))
}

// ---------- Commands ----------

async fn cmd_login() -> Result<()> {
    let client = Client::new(paths::cookie_path())?;
    let cookies = browser_login::login_via_browser().await?;
    if cookies.is_empty() {
        anyhow::bail!("No cookies were captured. Login may be incomplete or timed out; try again.");
    }
    let n = client.import_cookies(&cookies)?;
    client.save_cookies()?;
    client
        .confirm_session()
        .await
        .context("Imported cookies could not verify a logged-in session")?;
    crate::cache::clear(); // Clear stale cache after account changes or re-login.
    println!(
        "Login succeeded. Saved {n} cookies locally.\n   You can now run thu-learn hw, ann, f, and related commands without logging in again."
    );
    Ok(())
}

/// Returns the previous semester ID used to backfill older homework course names.
fn prev_semester(s: &str) -> Option<String> {
    let p: Vec<&str> = s.split('-').collect();
    if p.len() != 3 {
        return None;
    }
    let (y1, y2, t): (i32, i32, i32) = (p[0].parse().ok()?, p[1].parse().ok()?, p[2].parse().ok()?);
    if t > 1 {
        Some(format!("{y1}-{y2}-{}", t - 1))
    } else {
        Some(format!("{}-{}-2", y1 - 1, y2 - 1))
    }
}

/// Loads current and previous semester courses for cross-semester homework lookup.
async fn courses_with_prev(client: &Client) -> Result<Vec<Course>> {
    let sem = client.current_semester().await?;
    let mut courses = client.course_list(&sem).await?;
    if let Some(prev) = prev_semester(&sem) {
        if let Ok(mut pc) = client.course_list(&prev).await {
            courses.append(&mut pc);
        }
    }
    Ok(courses)
}

async fn cmd_homework(all: bool, overdue: bool, json: bool) -> Result<()> {
    let client = login_only().await?;
    let courses = courses_with_prev(&client).await?;
    let mut hws = client.homework_list(&courses).await?;
    let now = chrono::Local::now();
    write_completion_cache(
        CompletionKind::Homework,
        &completion_items_for_homework(&hws),
    );

    if !all {
        // Default to pending homework.
        hws.retain(|h| h.status == HomeworkStatus::Pending);
        // Only homework with a past explicit deadline is overdue.
        let is_overdue = |h: &crate::models::Homework| h.deadline.is_some_and(|d| d < now);
        if overdue {
            hws.retain(|h| is_overdue(h));
        } else {
            hws.retain(|h| !is_overdue(h));
        }
    }
    // Sort by deadline, with missing deadlines last.
    hws.sort_by(|a, b| match (a.deadline, b.deadline) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });

    if json {
        println!("{}", serde_json::to_string_pretty(&hws)?);
        return Ok(());
    }

    if hws.is_empty() {
        let what = if all {
            ""
        } else if overdue {
            "overdue pending "
        } else {
            "pending "
        };
        println!("No {what}homework.");
        return Ok(());
    }

    println!("{}", render_homework_table(&hws, now));
    Ok(())
}

async fn cmd_debug() -> Result<()> {
    let (client, courses) = prepare().await?;
    client.debug_dump(&courses).await
}

fn cmd_completion(shell: CompletionShell) -> Result<()> {
    match shell {
        CompletionShell::Zsh => print!("{}", zsh_completion_script()),
    }
    Ok(())
}

fn cmd_complete(kind: CompletionKind, prefix: Option<String>) -> Result<()> {
    let prefix = prefix.unwrap_or_default();
    for item in filter_completion_items(&read_completion_cache(kind), &prefix) {
        println!("{}", format_completion_line(&item));
    }
    Ok(())
}

async fn cmd_homework_show(xszyid: String, download: Option<PathBuf>, json: bool) -> Result<()> {
    let client = login_only().await?;
    let courses = courses_with_prev(&client).await?;
    let hws = client.homework_list(&courses).await?;
    let hw = resolve_item_by_id(
        &hws,
        &xszyid,
        |h| h.student_homework_id.as_str(),
        "Homework",
        "run `thu-learn hw -a` to list IDs",
    )?;

    // Fetch details and attachments concurrently.
    let (desc, atts) = futures::join!(
        client.homework_detail(&hw.base_id),
        client.homework_attachments(&hw.course_id, &hw.student_homework_id),
    );
    let desc = desc.unwrap_or_default();
    let atts = atts.unwrap_or_default();

    if json {
        let obj = serde_json::json!({
            "homework": hw,
            "description": desc,
            "attachments": atts,
        });
        println!("{}", serde_json::to_string_pretty(&obj)?);
        // In JSON mode, downloads still run but do not print human-facing text.
        if let Some(dir) = download {
            download_attachments(&client, &atts, &dir).await;
        }
        return Ok(());
    }

    let course = if hw.course_name.is_empty() {
        "(previous course)"
    } else {
        &hw.course_name
    };
    let ddl = hw
        .deadline
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| hw.deadline_raw.clone());

    println!(
        "# {} | {}",
        course.if_supports_color(Stdout, |t| t.bold()),
        hw.title
    );
    println!("Status: {}   Deadline: {ddl}", hw.status.label());
    if let Some(g) = &hw.grade {
        let extra = if hw.grade_time.is_empty() {
            String::new()
        } else {
            let who = if hw.grader.is_empty() {
                String::new()
            } else {
                format!("{} · ", hw.grader)
            };
            format!("  {}", dim(&format!("({who}{})", hw.grade_time)))
        };
        println!("Grade: {g}{extra}");
    }

    if !desc.is_empty() {
        println!("\nDescription\n{desc}");
    }

    if !hw.comment.is_empty() {
        println!("\nComment\n{}", hw.comment);
    }

    if atts.is_empty() {
        println!("\n(No attachments)");
    } else {
        println!("\nAttachments");
        for a in &atts {
            println!("  [{}] {}", a.section, a.filename);
        }
    }

    if let Some(dir) = download {
        if atts.is_empty() {
            println!("\nNo attachments to download.");
            return Ok(());
        }
        println!();
        download_attachments(&client, &atts, &dir).await;
    }
    Ok(())
}

/// Downloads homework attachments to a directory concurrently.
async fn download_attachments(
    client: &Client,
    atts: &[crate::models::HomeworkAttachment],
    dir: &std::path::Path,
) {
    tokio::fs::create_dir_all(dir).await.ok();
    let futs = atts.iter().map(|a| {
        let dest = dir.join(&a.filename);
        async move {
            let r = client.download(&a.download_path, &dest).await;
            (a.filename.clone(), dest, r)
        }
    });
    let results = futures::future::join_all(futs).await;
    let mut n = 0;
    for (name, dest, r) in results {
        match r {
            Ok(b) => {
                println!("Downloaded {b} bytes -> {}", dest.display());
                n += 1;
            }
            Err(e) => eprintln!("Failed to download {name}: {e}"),
        }
    }
    println!("Downloaded {n} attachments");
}

async fn cmd_announcement(json: bool) -> Result<()> {
    let (client, courses) = prepare().await?;
    let notes = client.notification_list(&courses).await?;
    write_completion_cache(
        CompletionKind::Announcement,
        &completion_items_for_announcements(&notes),
    );

    if json {
        println!("{}", serde_json::to_string_pretty(&notes)?);
        return Ok(());
    }
    if notes.is_empty() {
        println!("No announcements");
        return Ok(());
    }
    println!("{}", render_announcement_table(&notes));
    eprintln!("\nRun `thu-learn ann <id>` to read an announcement.");
    Ok(())
}

async fn cmd_announcement_show(id: String, json: bool) -> Result<()> {
    let (client, courses) = prepare().await?;
    let notes = client.notification_list(&courses).await?;
    let n = resolve_item_by_id(
        &notes,
        &id,
        |n| n.id.as_str(),
        "Announcement",
        "run `thu-learn ann` to list IDs",
    )?;

    if json {
        println!("{}", serde_json::to_string_pretty(n)?);
        return Ok(());
    }

    println!(
        "# {} | {}",
        n.course_name.if_supports_color(Stdout, |t| t.bold()),
        n.title
    );
    let read = if n.read { "" } else { " · unread" };
    println!(
        "{}",
        dim(&format!("{} · {}{read}", n.publisher, n.publish_time))
    );
    if n.content.is_empty() {
        println!("\n(No body)");
    } else {
        println!("\n{}", n.content);
    }
    Ok(())
}

async fn cmd_file_ls(filter: Option<String>, json: bool) -> Result<()> {
    let (client, courses) = prepare().await?;
    let targets: Vec<&Course> = courses
        .iter()
        .filter(|c| filter.as_ref().is_none_or(|f| c.name.contains(f.as_str())))
        .collect();

    // Fetch each course's files concurrently.
    let client_ref = &client;
    let futs = targets.iter().map(|c| {
        let course: &Course = c;
        async move { (course, client_ref.file_list(course).await) }
    });
    let results = futures::future::join_all(futs).await;
    let all_files = results
        .iter()
        .filter_map(|(_, r)| r.as_ref().ok())
        .flat_map(|files| files.iter().cloned())
        .collect::<Vec<_>>();
    write_completion_cache(
        CompletionKind::File,
        &completion_items_for_files(&all_files),
    );

    if json {
        println!("{}", serde_json::to_string_pretty(&all_files)?);
        return Ok(());
    }

    for (course, res) in &results {
        match res {
            Ok(files) if !files.is_empty() => {
                println!("# {}", course.name.if_supports_color(Stdout, |t| t.bold()));
                println!("{}", render_course_file_table(files));
            }
            Ok(_) => {}
            Err(e) => eprintln!("  (failed to fetch files for {}: {e})", course.name),
        }
    }
    Ok(())
}

/// Fetches files for several courses concurrently and flattens the result.
async fn fetch_all_files(client: &Client, courses: &[Course]) -> Vec<crate::models::CourseFile> {
    let futs = courses
        .iter()
        .map(|c| async move { client.file_list(c).await.unwrap_or_default() });
    futures::future::join_all(futs)
        .await
        .into_iter()
        .flatten()
        .collect()
}

async fn cmd_file_show(id: String, json: bool) -> Result<()> {
    let (client, courses) = prepare().await?;
    let files = fetch_all_files(&client, &courses).await;
    let f = resolve_item_by_id(
        &files,
        &id,
        |f| f.id.as_str(),
        "File",
        "run `thu-learn f ls` to list IDs",
    )?;

    if json {
        println!("{}", serde_json::to_string_pretty(f)?);
        return Ok(());
    }

    println!(
        "# {} | {}",
        f.course_name.if_supports_color(Stdout, |t| t.bold()),
        f.title
    );
    println!(
        "{}",
        dim(&format!("{} · {} · {}", f.file_type, f.size, f.upload_time))
    );
    if !f.description.is_empty() {
        println!("\n{}", f.description);
    }
    println!("\nDownload: thu-learn f get {}", short_id(&f.id));
    Ok(())
}

async fn cmd_file_get(file_id: String, out: Option<PathBuf>) -> Result<()> {
    // Full `wjid` values can be downloaded directly; short IDs need lookup.
    let (client, wjid) = if file_id.contains("_KJ_") || file_id.contains("_ZY_") {
        (login_only().await?, file_id.clone())
    } else {
        let (client, courses) = prepare().await?;
        let files = fetch_all_files(&client, &courses).await;
        let wjid = resolve_item_by_id(
            &files,
            &file_id,
            |f| f.id.as_str(),
            "File",
            "run `thu-learn f ls` to list IDs",
        )?
        .id
        .clone();
        (client, wjid)
    };

    let url = client.file_download_url(&wjid);
    let (bytes, server_name) = client.fetch_download(&url).await?;
    // Fall back to <id>.bin when the server omits a file name.
    let fallback = || format!("{wjid}.bin");

    let dest = match out {
        // Existing directory: place the server-named file inside it.
        Some(p) if p.is_dir() => p.join(server_name.clone().unwrap_or_else(fallback)),
        // Explicit path: use it exactly.
        Some(p) => p,
        // No output option: use the server-provided file name.
        None => PathBuf::from(server_name.clone().unwrap_or_else(fallback)),
    };

    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    tokio::fs::write(&dest, &bytes).await?;
    println!("Downloaded {} bytes -> {}", bytes.len(), dest.display());
    Ok(())
}

async fn cmd_submit(homework_id: String, file: PathBuf, comment: String) -> Result<()> {
    if !file.exists() {
        anyhow::bail!("File does not exist: {}", file.display());
    }
    let client = login_only().await?;

    // Resolve short IDs to full `xszyid` values when possible.
    let courses = courses_with_prev(&client).await?;
    let hws = client.homework_list(&courses).await?;
    let xszyid = match resolve_item_prefix(&hws, &homework_id, |h| h.student_homework_id.as_str()) {
        IdResolution::Found(h) => h.student_homework_id.clone(),
        IdResolution::NotFound => homework_id, // Preserve full IDs that are not in the list.
        IdResolution::Ambiguous(candidates) => {
            let candidates = candidates.join(", ");
            anyhow::bail!(
                "Homework ID prefix is ambiguous: {homework_id} (candidates: {candidates})"
            );
        }
    };

    client.submit_homework(&xszyid, &file, &comment).await?;
    println!("Submission succeeded.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        completion_items_for_homework, filter_completion_items, format_completion_line,
        prev_semester, render_announcement_table, render_course_file_table, render_homework_table,
        render_table, resolve_item_by_id, zsh_completion_script, Cli, TableCell,
    };
    use crate::models::{short_id, CourseFile, Homework, HomeworkStatus, Notification};
    use chrono::TimeZone;
    use clap::CommandFactory;

    /// Removes ANSI SGR escape sequences so layout assertions stay stable
    /// regardless of whether the test runs with color forced on or off.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for skip in chars.by_ref() {
                    if skip == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn local_dt(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
    ) -> chrono::DateTime<chrono::Local> {
        chrono::Local
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .unwrap()
    }

    fn homework(
        student_homework_id: &str,
        course_name: &str,
        title: &str,
        deadline: Option<chrono::DateTime<chrono::Local>>,
        grade: Option<&str>,
        status: HomeworkStatus,
    ) -> Homework {
        Homework {
            student_homework_id: student_homework_id.to_string(),
            base_id: format!("base-{student_homework_id}"),
            course_id: "course-1".to_string(),
            course_name: course_name.to_string(),
            title: title.to_string(),
            deadline,
            deadline_raw: "No deadline".to_string(),
            submit_time: String::new(),
            grade: grade.map(str::to_string),
            comment: String::new(),
            grader: String::new(),
            grade_time: String::new(),
            status,
        }
    }

    fn notification(id: &str, read: bool) -> Notification {
        Notification {
            id: id.to_string(),
            course_id: "course-1".to_string(),
            course_name: "Operating Systems".to_string(),
            title: if read { "Slides posted" } else { "Exam notice" }.to_string(),
            publish_time: "2026-06-01 09:00".to_string(),
            publisher: "Teacher".to_string(),
            read,
            content: String::new(),
        }
    }

    fn course_file(id: &str) -> CourseFile {
        CourseFile {
            id: id.to_string(),
            course_id: "course-1".to_string(),
            course_name: "Operating Systems".to_string(),
            title: "Lecture 讲义.pdf".to_string(),
            size: "1.5 MB".to_string(),
            file_type: "pdf".to_string(),
            upload_time: "2026-06-02 10:30".to_string(),
            description: String::new(),
        }
    }

    #[test]
    fn prev_semester_spring_to_fall() {
        assert_eq!(prev_semester("2025-2026-2").as_deref(), Some("2025-2026-1"));
    }

    #[test]
    fn prev_semester_fall_to_prev_year() {
        assert_eq!(prev_semester("2025-2026-1").as_deref(), Some("2024-2025-2"));
    }

    #[test]
    fn prev_semester_invalid() {
        assert!(prev_semester("garbage").is_none());
        assert!(prev_semester("2025-2026").is_none());
    }

    #[test]
    fn help_text_is_english() {
        let mut help = Vec::new();
        Cli::command().write_long_help(&mut help).unwrap();
        let help = String::from_utf8(help).unwrap();

        assert!(help.contains("Tsinghua Web Learning command-line client"));
        assert!(!help.contains("清华"));
        assert!(!help.contains("作业"));
    }

    #[test]
    fn table_renderer_pads_wide_unicode_cells() {
        let table = render_table(
            &["Name", "Value"],
            &[
                vec![TableCell::plain("作业"), TableCell::plain("x")],
                vec![TableCell::plain("abc"), TableCell::plain("y")],
            ],
        );

        let expected = concat!(
            "╭──────┬───────╮\n",
            "│ Name │ Value │\n",
            "├──────┼───────┤\n",
            "│ 作业 │ x     │\n",
            "│ abc  │ y     │\n",
            "╰──────┴───────╯",
        );
        assert_eq!(strip_ansi(&table), expected);
    }

    #[test]
    fn table_renderer_removes_terminal_control_sequences_from_cells() {
        let table = render_table(
            &["Title", "Published"],
            &[vec![
                TableCell::styled(
                    "WeChat Group for \"Physics(1)\"\u{200b}\u{1b}[118;1:3u",
                    "WeChat Group for \"Physics(1)\"\u{200b}\u{1b}[118;1:3u".to_string(),
                ),
                TableCell::plain("2026-02-25 21:19"),
            ]],
        );

        assert!(!table.contains("\u{1b}[118;1:3u"));
        assert!(!table.contains('\u{200b}'));
        let table = strip_ansi(&table);
        assert!(table.contains("WeChat Group for \"Physics(1)\""));
        assert!(table.contains("│ WeChat Group for \"Physics(1)\" │"));
    }

    #[test]
    fn homework_table_includes_grade_only_when_present() {
        let now = local_dt(2026, 6, 1, 12, 0);
        let ungraded = homework(
            "student-homework-one",
            "Data Structures",
            "Heap lab",
            Some(local_dt(2026, 6, 5, 23, 59)),
            None,
            HomeworkStatus::Pending,
        );
        let graded = homework(
            "student-homework-two",
            "Computer Networks",
            "Protocol report",
            Some(local_dt(2026, 6, 7, 10, 0)),
            Some("95"),
            HomeworkStatus::Graded,
        );

        let with_grade = render_homework_table(&[ungraded.clone(), graded.clone()], now);
        // Line 0 is the top border; the header row is line 1.
        let header = with_grade.lines().nth(1).unwrap();
        assert!(header.contains("ID"));
        assert!(header.contains("Status"));
        assert!(header.contains("Course"));
        assert!(header.contains("Title"));
        assert!(header.contains("Deadline"));
        assert!(header.contains("Grade"));
        assert!(with_grade.contains(&short_id("student-homework-one")));
        assert!(with_grade.contains("Pending"));
        assert!(with_grade.contains("Data Structures"));
        assert!(with_grade.contains("Heap lab"));
        assert!(with_grade.contains("2026-06-05 23:59"));
        assert!(with_grade.contains("95"));

        let without_grade = render_homework_table(&[ungraded], now);
        assert!(!without_grade.lines().nth(1).unwrap().contains("Grade"));
    }

    #[test]
    fn announcement_table_renders_short_ids_and_read_state() {
        let unread = notification("announcement-unread", false);
        let read = notification("announcement-read", true);
        let table = render_announcement_table(&[unread.clone(), read.clone()]);

        assert!(table.contains(&short_id(&unread.id)));
        assert!(table.contains("Unread"));
        assert!(table.contains("Exam notice"));
        assert!(table.contains(&short_id(&read.id)));
        assert!(table.contains("Read"));
        assert!(table.contains("Slides posted"));
    }

    #[test]
    fn course_file_table_renders_file_metadata() {
        let file = course_file("course-file-one");
        let table = render_course_file_table(std::slice::from_ref(&file));

        assert!(table.contains(&short_id(&file.id)));
        assert!(table.contains("Lecture 讲义.pdf"));
        assert!(table.contains("1.5 MB"));
        assert!(table.contains("2026-06-02 10:30"));
    }

    #[test]
    fn completion_items_match_short_id_prefix() {
        let hw = homework(
            "student-homework-one",
            "Data Structures",
            "Heap lab",
            None,
            None,
            HomeworkStatus::Pending,
        );
        let prefix = &short_id(&hw.student_homework_id)[..3];
        let items = completion_items_for_homework(std::slice::from_ref(&hw));
        let matches = filter_completion_items(&items, prefix);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].value, short_id(&hw.student_homework_id));
        assert!(matches[0].description.contains("Data Structures"));
        assert!(matches[0].description.contains("Heap lab"));
        assert_eq!(
            format_completion_line(&matches[0]),
            format!(
                "{}\t{}",
                short_id(&hw.student_homework_id),
                matches[0].description
            )
        );
    }

    #[test]
    fn id_prefix_resolution_rejects_ambiguous_matches() {
        let first = homework(
            "student-homework-one",
            "Data Structures",
            "Heap lab",
            None,
            None,
            HomeworkStatus::Pending,
        );
        let first_short = short_id(&first.student_homework_id);
        let second = homework(
            "student-homework-two",
            "Computer Networks",
            "Protocol report",
            None,
            None,
            HomeworkStatus::Pending,
        );
        let shared_prefix = first_short
            .chars()
            .next()
            .expect("short IDs are never empty")
            .to_string();
        let second_id = (0..1000)
            .map(|idx| format!("second-homework-{idx}"))
            .find(|id| short_id(id).starts_with(&shared_prefix))
            .expect("one hex-prefix collision should exist in 1000 attempts");
        let second = Homework {
            student_homework_id: second_id,
            ..second
        };
        let items = vec![first, second];

        let err = resolve_item_by_id(
            &items,
            &shared_prefix,
            |h| h.student_homework_id.as_str(),
            "Homework",
            "run `thu-learn hw -a` to list IDs",
        )
        .unwrap_err();

        assert!(err.to_string().contains("ambiguous"));
    }

    #[test]
    fn zsh_completion_script_uses_cached_candidate_command() {
        let script = zsh_completion_script();

        assert!(script.contains("#compdef thu-learn"));
        assert!(script.contains("__complete"));
        assert!(script.contains("_thu_learn_cached_ids homework"));
        assert!(script.contains("_thu_learn_cached_ids file"));
    }
}
