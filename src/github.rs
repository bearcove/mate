use eyre::Result;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Deserialize)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub labels: Vec<Label>,
    pub comments: Vec<Comment>,
    pub state: String,
    pub assignees: Vec<Assignee>,
    pub milestone: Option<Milestone>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Label {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Milestone {
    pub title: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Assignee {
    pub login: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub login: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Comment {
    pub author: Option<User>,
    pub body: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct IssueSyncResult {
    pub base_dir: PathBuf,
    pub index_path: PathBuf,
    pub open_dir: PathBuf,
    pub closed_dir: PathBuf,
    pub labels_dir: Option<PathBuf>,
    pub milestones_dir: Option<PathBuf>,
    pub all_dir: PathBuf,
    pub open_count: usize,
    pub closed_count: usize,
}

pub fn infer_repo() -> Result<String> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(eyre::eyre!(
            "failed to infer repo from git remote origin: {stderr}"
        ));
    }

    let raw = String::from_utf8(output.stdout)?.trim().to_string();
    parse_repo_from_remote(&raw)
}

fn parse_repo_from_remote(remote: &str) -> Result<String> {
    let path = if let Some((_, rest)) = remote.split_once("github.com:") {
        rest
    } else if let Some((_, rest)) = remote.split_once("github.com/") {
        rest
    } else {
        return Err(eyre::eyre!(
            "unsupported remote URL format for GitHub: {remote}"
        ));
    };

    let cleaned = path.trim().trim_start_matches('/').trim_end_matches(".git");
    let mut parts = cleaned.split('/');
    let owner = parts.next().unwrap_or_default();
    let repo = parts.next().unwrap_or_default();
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return Err(eyre::eyre!(
            "could not parse owner/repo from remote URL: {remote}"
        ));
    }
    Ok(format!("{owner}/{repo}"))
}

pub fn sync_issues(repo: &str) -> Result<Vec<Issue>> {
    let output = Command::new("gh")
        .args([
            "issue",
            "list",
            "-R",
            repo,
            "--json",
            "number,title,body,labels,comments,state,assignees,milestone,createdAt,updatedAt",
            "--limit",
            "100",
        ])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(eyre::eyre!("gh issue list failed: {stderr}"));
    }
    let issues: Vec<Issue> = serde_json::from_slice(&output.stdout)?;
    Ok(issues)
}

pub fn write_issue_files(repo: &str, issues: &[Issue]) -> Result<IssueSyncResult> {
    let dir_name = repo.replace('/', "-");
    let dir = PathBuf::from("/tmp/bud-issues").join(dir_name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }

    let all_dir = dir.join("all");
    let open_dir = dir.join("open");
    let closed_dir = dir.join("closed");
    std::fs::create_dir_all(&all_dir)?;
    std::fs::create_dir_all(&open_dir)?;
    std::fs::create_dir_all(&closed_dir)?;

    let mut open_issues: Vec<&Issue> = Vec::new();
    let mut closed_issues: Vec<&Issue> = Vec::new();

    for issue in issues {
        let filename = issue_filename(issue);
        let all_path = all_dir.join(&filename);
        std::fs::write(&all_path, render_issue(issue))?;

        let link_target = format!("../all/{filename}");
        if issue.state.eq_ignore_ascii_case("open") {
            create_symlink(&link_target, &open_dir.join(&filename))?;
            open_issues.push(issue);
        } else {
            create_symlink(&link_target, &closed_dir.join(&filename))?;
            closed_issues.push(issue);
        }
    }

    let has_any_labels = issues.iter().any(|issue| !issue.labels.is_empty());
    let labels_dir = if has_any_labels {
        let root = dir.join("labels");
        std::fs::create_dir_all(&root)?;
        for issue in issues {
            let filename = issue_filename(issue);
            for label in &issue.labels {
                let label_dir = root.join(sanitize_title_for_filename(&label.name));
                std::fs::create_dir_all(&label_dir)?;
                create_symlink(&format!("../../all/{filename}"), &label_dir.join(&filename))?;
            }
        }
        Some(root)
    } else {
        None
    };

    let has_any_milestones = issues.iter().any(|issue| issue.milestone.is_some());
    let milestones_dir = if has_any_milestones {
        let root = dir.join("milestones");
        std::fs::create_dir_all(&root)?;
        for issue in issues {
            let Some(milestone) = issue.milestone.as_ref() else {
                continue;
            };
            let filename = issue_filename(issue);
            let milestone_dir = root.join(sanitize_title_for_filename(&milestone.title));
            std::fs::create_dir_all(&milestone_dir)?;
            create_symlink(
                &format!("../../all/{filename}"),
                &milestone_dir.join(&filename),
            )?;
        }
        Some(root)
    } else {
        None
    };

    open_issues.sort_by(|a, b| b.number.cmp(&a.number));
    closed_issues.sort_by(|a, b| b.number.cmp(&a.number));

    let index_path = dir.join("INDEX.md");
    std::fs::write(
        &index_path,
        render_index(repo, &open_issues, &closed_issues, issues),
    )?;

    Ok(IssueSyncResult {
        base_dir: dir,
        index_path,
        open_dir,
        closed_dir,
        labels_dir,
        milestones_dir,
        all_dir,
        open_count: open_issues.len(),
        closed_count: closed_issues.len(),
    })
}

fn create_symlink(target: &str, link: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        if link.exists() {
            std::fs::remove_file(link)?;
        }
        symlink(target, link)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (target, link);
        Err(eyre::eyre!("symlink support requires unix"))
    }
}

fn issue_filename(issue: &Issue) -> String {
    let mut title = sanitize_title_for_filename(&issue.title);
    if title.is_empty() {
        title = "untitled".to_string();
    }
    format!("{} - {}.md", issue.number, title)
}

fn sanitize_title_for_filename(title: &str) -> String {
    let replaced: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            _ => c,
        })
        .collect();
    let squashed = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    squashed.chars().take(80).collect::<String>().trim().to_string()
}

fn render_issue(issue: &Issue) -> String {
    let labels = if issue.labels.is_empty() {
        "none".to_string()
    } else {
        issue.labels
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let assignees = if issue.assignees.is_empty() {
        "none".to_string()
    } else {
        issue.assignees
            .iter()
            .map(|a| a.login.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let milestone = issue
        .milestone
        .as_ref()
        .map(|m| m.title.as_str())
        .unwrap_or("none");
    let body = issue.body.as_deref().unwrap_or("").trim();
    let body = if body.is_empty() {
        "(no description)"
    } else {
        body
    };

    let mut content = String::new();
    content.push_str(&format!("# #{}: {}\n\n", issue.number, issue.title));
    content.push_str(&format!("**State:** {}\n", issue.state.to_lowercase()));
    content.push_str(&format!("**Labels:** {labels}\n"));
    content.push_str(&format!("**Milestone:** {milestone}\n"));
    content.push_str(&format!("**Created:** {}\n", short_date(&issue.created_at)));
    content.push_str(&format!("**Updated:** {}\n", short_date(&issue.updated_at)));
    content.push_str(&format!("**Assignees:** {assignees}\n\n"));
    content.push_str("---\n\n");
    content.push_str(body);
    content.push_str("\n\n---\n\n## Comments\n\n");
    if issue.comments.is_empty() {
        content.push_str("(no comments)\n");
    } else {
        for comment in &issue.comments {
            let author = comment
                .author
                .as_ref()
                .map(|u| u.login.as_str())
                .unwrap_or("unknown");
            let comment_body = comment.body.as_deref().unwrap_or("").trim();
            let comment_body = if comment_body.is_empty() {
                "(no comment body)"
            } else {
                comment_body
            };
            content.push_str(&format!(
                "### @{author} ({}):\n{comment_body}\n\n",
                short_date(&comment.created_at)
            ));
        }
    }
    content
}

fn render_index(repo: &str, open_issues: &[&Issue], closed_issues: &[&Issue], all_issues: &[Issue]) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Issues for {repo}\n\n"));
    out.push_str(&format!("Synced: {}\n\n", today_ymd()));

    out.push_str(&format!("## Open ({})\n\n", open_issues.len()));
    for issue in open_issues {
        let filename = issue_filename(issue);
        out.push_str(&format!(
            "- [#{} - {}](all/{})\n",
            issue.number,
            issue.title,
            encode_path_component(&filename)
        ));
    }
    if open_issues.is_empty() {
        out.push_str("- none\n");
    }

    out.push_str(&format!("\n## Closed ({})\n\n", closed_issues.len()));
    for issue in closed_issues {
        let filename = issue_filename(issue);
        out.push_str(&format!(
            "- [#{} - {}](all/{})\n",
            issue.number,
            issue.title,
            encode_path_component(&filename)
        ));
    }
    if closed_issues.is_empty() {
        out.push_str("- none\n");
    }

    let has_any_milestone = all_issues.iter().any(|i| i.milestone.is_some());
    if has_any_milestone {
        out.push_str("\n## By Milestone\n\n");

        let mut groups: BTreeMap<String, Vec<&Issue>> = BTreeMap::new();
        for issue in all_issues {
            let key = issue
                .milestone
                .as_ref()
                .map(|m| m.title.clone())
                .unwrap_or_else(|| "No milestone".to_string());
            groups.entry(key).or_default().push(issue);
        }

        for issues in groups.values_mut() {
            issues.sort_by(|a, b| b.number.cmp(&a.number));
        }

        for (milestone, issues) in groups {
            out.push_str(&format!("### {milestone} ({} issues)\n", issues.len()));
            for issue in issues {
                let state = if issue.state.eq_ignore_ascii_case("open") {
                    "open"
                } else {
                    "closed"
                };
                let filename = issue_filename(issue);
                out.push_str(&format!(
                    "- [#{} - {}](all/{}) ({state})\n",
                    issue.number,
                    issue.title,
                    encode_path_component(&filename)
                ));
            }
            out.push('\n');
        }
    }

    out
}

fn short_date(value: &str) -> &str {
    value.get(..10).unwrap_or(value)
}

fn today_ymd() -> String {
    let output = Command::new("date").arg("+%F").output();
    if let Ok(output) = output
        && output.status.success()
    {
        return String::from_utf8_lossy(&output.stdout).trim().to_string();
    }
    "unknown".to_string()
}

fn encode_path_component(input: &str) -> String {
    let mut out = String::new();
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
