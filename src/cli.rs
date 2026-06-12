//! Command-line interface and subcommand orchestration.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::browser_login;
use crate::client::Client;
use crate::models::{short_id, Course, HomeworkStatus};
use crate::paths;
use owo_colors::{OwoColorize, Stream::Stdout};

/// Dims IDs and secondary details when stdout supports color.
fn dim(s: &str) -> String {
    format!("{}", s.if_supports_color(Stdout, |t| t.dimmed()))
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
        /// Homework ID (`xszyid`) to show details and attachments.
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
        /// Announcement ID to show the full body.
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

    /// Submit homework.
    Submit {
        /// Student homework ID (`xszyid`), visible in `thu-learn hw -a`.
        homework_id: String,
        /// File path to submit.
        file: PathBuf,
        /// Optional text comment.
        #[arg(short, long, default_value = "")]
        comment: String,
    },
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
        /// File ID shown by `thu-learn f ls`.
        file_id: String,
    },
    /// Download a course file.
    Get {
        /// File ID shown by `thu-learn f ls`.
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

    for h in &hws {
        let ddl = match h.deadline {
            Some(d) => {
                let remain = d.signed_duration_since(now);
                let days = remain.num_days();
                let when = d.format("%Y-%m-%d %H:%M").to_string();
                if remain.num_seconds() < 0 {
                    format!(
                        "{when} ({})",
                        "overdue".if_supports_color(Stdout, |t| t.bright_red())
                    )
                } else if days == 0 {
                    let tag = format!("within {}h today", remain.num_hours());
                    format!(
                        "{when} ({})",
                        tag.if_supports_color(Stdout, |t| t.bright_red())
                    )
                } else if days <= 3 {
                    let tag = format!("{days} days left");
                    format!("{when} ({})", tag.if_supports_color(Stdout, |t| t.yellow()))
                } else {
                    let tag = format!("{days} days left");
                    format!("{when} ({})", tag.if_supports_color(Stdout, |t| t.green()))
                }
            }
            None => h.deadline_raw.clone(),
        };
        let grade = h
            .grade
            .as_deref()
            .map(|g| format!(" grade:{g}"))
            .unwrap_or_default();
        let course = if h.course_name.is_empty() {
            "(previous course)"
        } else {
            &h.course_name
        };
        // Color submitted and graded statuses for quick scanning.
        let status = match h.status {
            HomeworkStatus::Submitted => {
                format!(
                    "{}",
                    h.status.label().if_supports_color(Stdout, |t| t.green())
                )
            }
            HomeworkStatus::Graded => {
                format!(
                    "{}",
                    h.status.label().if_supports_color(Stdout, |t| t.cyan())
                )
            }
            HomeworkStatus::Pending => h.status.label().to_string(),
        };
        println!(
            "[{}] {} | {}\n      Deadline: {} | {}{}",
            status,
            course.if_supports_color(Stdout, |t| t.bold()),
            h.title,
            ddl,
            dim(&format!("id: {}", short_id(&h.student_homework_id))),
            grade
        );
    }
    Ok(())
}

async fn cmd_debug() -> Result<()> {
    let (client, courses) = prepare().await?;
    client.debug_dump(&courses).await
}

async fn cmd_homework_show(xszyid: String, download: Option<PathBuf>, json: bool) -> Result<()> {
    let client = login_only().await?;
    let courses = courses_with_prev(&client).await?;
    let hws = client.homework_list(&courses).await?;
    let hw = hws
        .iter()
        .find(|h| short_id(&h.student_homework_id) == xszyid || h.student_homework_id == xszyid)
        .ok_or_else(|| {
            anyhow::anyhow!("Homework ID not found: {xszyid} (run `thu-learn hw -a` to list IDs)")
        })?;

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

    if json {
        println!("{}", serde_json::to_string_pretty(&notes)?);
        return Ok(());
    }
    if notes.is_empty() {
        println!("No announcements");
        return Ok(());
    }
    for n in &notes {
        // Highlight unread announcements with a red dot and bold title.
        let (flag, title) = if n.read {
            (" ".to_string(), n.title.clone())
        } else {
            (
                format!("{}", "●".if_supports_color(Stdout, |t| t.bright_red())),
                format!("{}", n.title.if_supports_color(Stdout, |t| t.bold())),
            )
        };
        println!(
            "{flag} {} | {}\n    {}",
            n.course_name.if_supports_color(Stdout, |t| t.cyan()),
            title,
            dim(&format!(
                "{} · {} · id: {}",
                n.publisher,
                n.publish_time,
                short_id(&n.id)
            )),
        );
    }
    eprintln!("\nRun `thu-learn ann <id>` to read an announcement.");
    Ok(())
}

async fn cmd_announcement_show(id: String, json: bool) -> Result<()> {
    let (client, courses) = prepare().await?;
    let notes = client.notification_list(&courses).await?;
    let n = notes
        .iter()
        .find(|n| short_id(&n.id) == id || n.id == id)
        .ok_or_else(|| {
            anyhow::anyhow!("Announcement ID not found: {id} (run `thu-learn ann` to list IDs)")
        })?;

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

    if json {
        let all: Vec<&crate::models::CourseFile> = results
            .iter()
            .filter_map(|(_, r)| r.as_ref().ok())
            .flatten()
            .collect();
        println!("{}", serde_json::to_string_pretty(&all)?);
        return Ok(());
    }

    for (course, res) in &results {
        match res {
            Ok(files) if !files.is_empty() => {
                println!("# {}", course.name.if_supports_color(Stdout, |t| t.bold()));
                for f in files {
                    println!(
                        "  {} | {} | {} | {}",
                        f.title,
                        f.size,
                        f.upload_time,
                        dim(&format!("id: {}", short_id(&f.id))),
                    );
                }
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
    let f = files
        .iter()
        .find(|f| short_id(&f.id) == id || f.id == id)
        .ok_or_else(|| {
            anyhow::anyhow!("File ID not found: {id} (run `thu-learn f ls` to list IDs)")
        })?;

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
        let wjid = files
            .iter()
            .find(|f| short_id(&f.id) == file_id || f.id == file_id)
            .map(|f| f.id.clone())
            .ok_or_else(|| {
                anyhow::anyhow!("File ID not found: {file_id} (run `thu-learn f ls` to list IDs)")
            })?;
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
    let xszyid = hws
        .iter()
        .find(|h| {
            short_id(&h.student_homework_id) == homework_id || h.student_homework_id == homework_id
        })
        .map(|h| h.student_homework_id.clone())
        .unwrap_or(homework_id); // Preserve full IDs that are not in the list.

    client.submit_homework(&xszyid, &file, &comment).await?;
    println!("Submission succeeded.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{prev_semester, Cli};
    use clap::CommandFactory;

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

        assert!(help.contains("Tsinghua Learn command-line client"));
        assert!(!help.contains("清华"));
        assert!(!help.contains("作业"));
    }
}
