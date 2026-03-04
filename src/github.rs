use eyre::Result;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
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
pub struct RepoLabel {
    pub name: String,
    pub description: Option<String>,
    pub color: Option<String>,
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

#[derive(Debug, Clone, Default)]
pub struct IssueRelationships {
    pub blocked_by: Vec<u64>,
    pub blocking: Vec<u64>,
    pub parent: Option<u64>,
    pub sub_issues: Vec<u64>,
    pub tracked_in: Vec<u64>,
    pub tracked_issues: Vec<u64>,
}

#[derive(Debug, Clone)]
pub struct NewIssue {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub milestone: Option<String>,
    pub assignees: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct IssueSyncResult {
    pub base_dir: PathBuf,
    pub index_path: PathBuf,
    pub open_dir: PathBuf,
    pub closed_dir: PathBuf,
    pub by_created_dir: PathBuf,
    pub by_updated_dir: PathBuf,
    pub labels_dir: Option<PathBuf>,
    pub milestones_dir: Option<PathBuf>,
    pub deps_path: Option<PathBuf>,
    pub deps_markdown_path: PathBuf,
    pub labels_markdown_path: PathBuf,
    pub milestones_markdown_path: PathBuf,
    pub all_dir: PathBuf,
    pub new_dir: PathBuf,
    pub issue_edits_applied: Vec<IssueEditReport>,
    pub issue_edit_errors: Vec<String>,
    pub open_count: usize,
    pub closed_count: usize,
}

#[derive(Debug, Clone)]
pub struct ParsedIssueFile {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub labels: Vec<String>,
    pub milestone: Option<String>,
    pub assignees: Vec<String>,
    pub body: String,
}

#[derive(Debug, Clone, Default)]
struct IssueFieldDiff {
    title: Option<String>,
    state: Option<String>,
    body: Option<String>,
    milestone: Option<Option<String>>,
    added_labels: Vec<String>,
    removed_labels: Vec<String>,
    added_assignees: Vec<String>,
    removed_assignees: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct IssueEditReport {
    pub number: u64,
    pub changes: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct IssueEditSummary {
    pub applied: Vec<IssueEditReport>,
    pub failed: Vec<String>,
}

const NEW_ISSUE_TEMPLATE: &str = r#"# Issue title here

**Labels:** label1, label2
**Milestone:** milestone name
**Assignees:** username1, username2

---

Issue body goes here. Describe the problem or feature request.
"#;

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

pub fn issue_repo_dir(repo: &str) -> PathBuf {
    PathBuf::from("/tmp/bud-issues").join(repo)
}

pub fn parse_issue_file(content: &str) -> Result<ParsedIssueFile> {
    let lines: Vec<&str> = content.lines().collect();
    let mut idx = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .ok_or_else(|| eyre::eyre!("issue file is empty"))?;

    let title_line = lines[idx].trim();
    let title_suffix = title_line
        .strip_prefix("# #")
        .ok_or_else(|| eyre::eyre!("issue file must start with '# #<number>: <title>'"))?;
    let (number_raw, title_raw) = title_suffix
        .split_once(':')
        .ok_or_else(|| eyre::eyre!("issue file header must be '# #<number>: <title>'"))?;
    let number = number_raw
        .trim()
        .parse::<u64>()
        .map_err(|_| eyre::eyre!("invalid issue number in header: {number_raw}"))?;
    let title = title_raw.trim().to_string();

    let mut labels = Vec::new();
    let mut state = "open".to_string();
    let mut milestone = None;
    let mut assignees = Vec::new();

    let mut body_start = None;
    while idx + 1 < lines.len() {
        idx += 1;
        let line = lines[idx].trim();
        if line == "---" {
            body_start = Some(idx + 1);
            break;
        }
        if let Some(value) = line.strip_prefix("**Labels:**") {
            labels = split_csv(value);
            continue;
        }
        if let Some(value) = line.strip_prefix("**State:**") {
            let parsed = value.trim().to_ascii_lowercase();
            match parsed.as_str() {
                "open" | "closed" => state = parsed,
                other => {
                    return Err(eyre::eyre!(
                        "invalid issue state '{other}' (expected 'open' or 'closed')"
                    ));
                }
            }
        }
        if let Some(value) = line.strip_prefix("**Milestone:**") {
            let value = value.trim();
            if !value.is_empty() && !value.eq_ignore_ascii_case("none") {
                milestone = Some(value.to_string());
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("**Assignees:**") {
            assignees = split_csv(value);
            continue;
        }
    }

    let body_start =
        body_start.ok_or_else(|| eyre::eyre!("issue file is missing body separator"))?;

    let mut body_end = lines.len();
    for (offset, line) in lines.iter().enumerate().skip(body_start) {
        if line.trim() == "---" {
            body_end = offset;
            break;
        }
    }

    let body = lines[body_start..body_end].join("\n").trim().to_string();

    Ok(ParsedIssueFile {
        number,
        title,
        state,
        labels,
        milestone,
        assignees,
        body,
    })
}

pub fn sync_local_issue_edits(repo: &str) -> Result<IssueEditSummary> {
    let base_dir = issue_repo_dir(repo);
    let all_dir = base_dir.join("all");
    let snapshot_dir = base_dir.join(".snapshots");

    if !all_dir.is_dir() {
        return Ok(IssueEditSummary::default());
    }

    let mut summary = IssueEditSummary::default();
    let entries = match std::fs::read_dir(&all_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(summary),
        Err(e) => return Err(e.into()),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file() {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }

        let edited_content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) => {
                summary
                    .failed
                    .push(format!("failed to read issue file {}: {e}", path.display()));
                continue;
            }
        };

        let edited = match parse_issue_file(&edited_content) {
            Ok(value) => value,
            Err(e) => {
                summary.failed.push(format!(
                    "failed to parse edited issue {}: {e}",
                    path.display()
                ));
                continue;
            }
        };

        let snapshot_path = snapshot_dir.join(format!("{}.md", edited.number));
        let baseline_content = match std::fs::read_to_string(&snapshot_path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                continue;
            }
            Err(e) => {
                summary.failed.push(format!(
                    "failed to read snapshot {}: {e}",
                    snapshot_path.display()
                ));
                continue;
            }
        };

        if edited_content == baseline_content {
            continue;
        }

        let baseline = match parse_issue_file(&baseline_content) {
            Ok(value) => value,
            Err(e) => {
                summary.failed.push(format!(
                    "failed to parse snapshot for {}: {e}",
                    path.display()
                ));
                continue;
            }
        };

        let diff = diff_issue_fields(&baseline, &edited);
        if diff.is_empty() {
            continue;
        }

        if let Err(e) = apply_issue_edits(repo, &edited, &diff) {
            let detail = format!("failed to sync issue #{} edits: {e}", edited.number);
            summary.failed.push(detail);
            continue;
        }

        summary.applied.push(IssueEditReport {
            number: edited.number,
            changes: diff.changes(edited.number),
        });
    }

    Ok(summary)
}

pub fn read_issue_file(repo: &str, number: u64) -> Result<String> {
    let all_dir = issue_repo_dir(repo).join("all");
    let prefix = format!("{number} - ");

    let entries = std::fs::read_dir(&all_dir)?;
    for entry in entries {
        let entry = entry?;
        if !entry.file_type().is_ok_and(|file_type| file_type.is_file()) {
            continue;
        }

        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if !name.starts_with(&prefix) {
            continue;
        }

        if !name.ends_with(".md") {
            continue;
        }

        return std::fs::read_to_string(entry.path())
            .map_err(|e| eyre::eyre!("failed to read issue file {}: {e}", entry.path().display()));
    }

    Err(eyre::eyre!(
        "Issue #{number} not found. Run 'bud issues' first to sync."
    ))
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

pub fn sync_labels(repo: &str) -> Result<Vec<RepoLabel>> {
    let output = Command::new("gh")
        .args([
            "label",
            "list",
            "-R",
            repo,
            "--json",
            "name,description,color",
            "--limit",
            "100",
        ])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(eyre::eyre!("gh label list failed: {stderr}"));
    }
    let labels: Vec<RepoLabel> = serde_json::from_slice(&output.stdout)?;
    Ok(labels)
}

pub fn sync_milestones(repo: &str) -> Result<Vec<String>> {
    let (owner, name) = split_repo(repo)?;
    let endpoint = format!("repos/{owner}/{name}/milestones");
    let output = Command::new("gh")
        .args(["api", &endpoint, "--jq", ".[].title"])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(eyre::eyre!("gh api milestones failed: {stderr}"));
    }
    let milestones = String::from_utf8(output.stdout)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect();
    Ok(milestones)
}

pub fn fetch_issue_relationships(
    repo: &str,
    issue_numbers: &[u64],
) -> Result<BTreeMap<u64, IssueRelationships>> {
    let (owner, name) = split_repo(repo)?;
    let mut map: BTreeMap<u64, IssueRelationships> = BTreeMap::new();
    for &number in issue_numbers {
        map.insert(number, IssueRelationships::default());
    }
    if issue_numbers.is_empty() {
        return Ok(map);
    }

    const BATCH_SIZE: usize = 20;
    for chunk in issue_numbers.chunks(BATCH_SIZE) {
        let mut query = String::from("query {");
        for number in chunk {
            query.push_str(&format!(
                r#"
issue_{number}: repository(owner: "{owner}", name: "{name}") {{
  issue(number: {number}) {{
    blockedBy(first: 100) {{ nodes {{ number }} }}
    blocking(first: 100) {{ nodes {{ number }} }}
    parent {{ number }}
    subIssues(first: 100) {{ nodes {{ number }} }}
    trackedInIssues(first: 100) {{ nodes {{ number }} }}
    trackedIssues(first: 100) {{ nodes {{ number }} }}
  }}
}}"#
            ));
        }
        query.push('}');

        let output = Command::new("gh")
            .args(["api", "graphql", "-f"])
            .arg(format!("query={query}"))
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(eyre::eyre!("gh api graphql failed: {stderr}"));
        }

        let root: Value = serde_json::from_slice(&output.stdout)?;
        let data = root
            .get("data")
            .ok_or_else(|| eyre::eyre!("graphql response missing data"))?;

        for number in chunk {
            let alias = format!("issue_{number}");
            let issue = data
                .get(&alias)
                .and_then(|v| v.get("issue"))
                .filter(|v| !v.is_null());
            let Some(issue) = issue else {
                continue;
            };
            map.insert(
                *number,
                IssueRelationships {
                    blocked_by: extract_numbers(issue, "blockedBy"),
                    blocking: extract_numbers(issue, "blocking"),
                    parent: issue
                        .get("parent")
                        .and_then(|v| v.get("number"))
                        .and_then(Value::as_u64),
                    sub_issues: extract_numbers(issue, "subIssues"),
                    tracked_in: extract_numbers(issue, "trackedInIssues"),
                    tracked_issues: extract_numbers(issue, "trackedIssues"),
                },
            );
        }
    }

    Ok(map)
}

pub fn write_issue_files(repo: &str, issues: &[Issue]) -> Result<IssueSyncResult> {
    let dir = issue_repo_dir(repo);

    let edit_summary = sync_local_issue_edits(repo)?;
    if dir.exists() {
        let sync_paths = [
            "all",
            "open",
            "closed",
            "by-created",
            "by-updated",
            "labels",
            "milestones",
            "deps",
            "INDEX.md",
            "DEPS.md",
            "LABELS.md",
            "MILESTONES.md",
        ];
        for path in sync_paths {
            let path = dir.join(path);
            if !path.exists() {
                continue;
            }
            if path.is_dir() {
                std::fs::remove_dir_all(&path)?;
            } else {
                std::fs::remove_file(&path)?;
            }
        }
    }

    let all_dir = dir.join("all");
    let open_dir = dir.join("open");
    let closed_dir = dir.join("closed");
    let by_created_dir = dir.join("by-created");
    let by_updated_dir = dir.join("by-updated");
    let new_dir = dir.join("new");
    let snapshot_dir = dir.join(".snapshots");
    std::fs::create_dir_all(&all_dir)?;
    std::fs::create_dir_all(&open_dir)?;
    std::fs::create_dir_all(&closed_dir)?;
    std::fs::create_dir_all(&by_created_dir)?;
    std::fs::create_dir_all(&by_updated_dir)?;
    std::fs::create_dir_all(&new_dir)?;
    std::fs::create_dir_all(&snapshot_dir)?;
    std::fs::write(new_dir.join("TEMPLATE.md"), NEW_ISSUE_TEMPLATE)?;

    let mut number_to_filename: BTreeMap<u64, String> = BTreeMap::new();
    let mut open_issues: Vec<&Issue> = Vec::new();
    let mut closed_issues: Vec<&Issue> = Vec::new();

    for issue in issues {
        let filename = issue_filename(issue);
        let issue_content = render_issue(issue);
        number_to_filename.insert(issue.number, filename.clone());
        std::fs::write(all_dir.join(&filename), &issue_content)?;
        std::fs::write(
            snapshot_dir.join(format!("{}.md", issue.number)),
            issue_content,
        )?;

        let link_target = format!("../all/{filename}");
        if issue.state.eq_ignore_ascii_case("open") {
            create_symlink(&link_target, &open_dir.join(&filename))?;
            open_issues.push(issue);
        } else {
            create_symlink(&link_target, &closed_dir.join(&filename))?;
            closed_issues.push(issue);
        }

        let created_link_name = format!(
            "{} #{} - {}.md",
            short_date(&issue.created_at),
            issue.number,
            sanitize_title_for_filename(&issue.title)
        );
        create_symlink(
            &format!("../../all/{filename}"),
            &by_created_dir.join(created_link_name),
        )?;

        let updated_link_name = format!(
            "{} #{} - {}.md",
            short_date(&issue.updated_at),
            issue.number,
            sanitize_title_for_filename(&issue.title)
        );
        create_symlink(
            &format!("../../all/{filename}"),
            &by_updated_dir.join(updated_link_name),
        )?;
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

    let issue_numbers = issues.iter().map(|i| i.number).collect::<Vec<_>>();
    let deps_markdown_path = dir.join("DEPS.md");
    let deps_path = match fetch_issue_relationships(repo, &issue_numbers) {
        Ok(relationships) => write_deps(
            repo,
            &dir,
            &deps_markdown_path,
            &relationships,
            &number_to_filename,
        )?,
        Err(e) => {
            eprintln!("Warning: could not fetch issue dependencies (GraphQL): {e}");
            let deps_md = format!(
                "# Dependencies for {repo}\n\nDependency information unavailable (GraphQL query failed).\n"
            );
            std::fs::write(&deps_markdown_path, deps_md)?;
            None
        }
    };

    let labels_markdown_path = dir.join("LABELS.md");
    let labels = sync_labels(repo)?;
    std::fs::write(&labels_markdown_path, render_labels_markdown(repo, &labels))?;

    let milestones_markdown_path = dir.join("MILESTONES.md");
    let milestones = sync_milestones(repo)?;
    std::fs::write(
        &milestones_markdown_path,
        render_milestones_markdown(repo, &milestones),
    )?;

    Ok(IssueSyncResult {
        base_dir: dir,
        index_path,
        open_dir,
        closed_dir,
        by_created_dir,
        by_updated_dir,
        labels_dir,
        milestones_dir,
        issue_edits_applied: edit_summary.applied,
        issue_edit_errors: edit_summary.failed,
        deps_path,
        deps_markdown_path,
        labels_markdown_path,
        milestones_markdown_path,
        all_dir,
        new_dir,
        open_count: open_issues.len(),
        closed_count: closed_issues.len(),
    })
}

pub fn parse_new_issue(content: &str) -> Result<NewIssue> {
    let lines: Vec<&str> = content.lines().collect();
    let title_idx = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .ok_or_else(|| eyre::eyre!("new issue file is empty"))?;
    let title_line = lines[title_idx].trim();
    let title = title_line
        .strip_prefix("# ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| eyre::eyre!("new issue file must start with '# <title>'"))?
        .to_string();

    let mut labels = Vec::new();
    let mut milestone = None;
    let mut assignees = Vec::new();

    let mut separator_idx = None;
    let mut body_prefix: Vec<&str> = Vec::new();
    for (idx, raw_line) in lines.iter().enumerate().skip(title_idx + 1) {
        let line = raw_line.trim();
        if line == "---" {
            separator_idx = Some(idx);
            break;
        }
        if line.is_empty() {
            continue;
        }
        if let Some(value) = line.strip_prefix("**Labels:**") {
            labels = split_csv(value);
            continue;
        }
        if let Some(value) = line.strip_prefix("**Milestone:**") {
            let value = value.trim();
            if !value.is_empty() {
                milestone = Some(value.to_string());
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("**Assignees:**") {
            assignees = split_csv(value);
            continue;
        }
        body_prefix.push(*raw_line);
    }

    let body_lines: Vec<&str> = if let Some(idx) = separator_idx {
        lines.iter().skip(idx + 1).copied().collect()
    } else {
        body_prefix
    };
    let body = body_lines.join("\n").trim().to_string();

    Ok(NewIssue {
        title,
        body,
        labels,
        milestone,
        assignees,
    })
}

fn diff_issue_fields(old: &ParsedIssueFile, new: &ParsedIssueFile) -> IssueFieldDiff {
    let mut diff = IssueFieldDiff::default();

    if old.title != new.title {
        diff.title = Some(new.title.clone());
    }
    if old.state != new.state {
        diff.state = Some(new.state.clone());
    }
    if old.body != new.body {
        diff.body = Some(new.body.clone());
    }

    if old.milestone != new.milestone {
        diff.milestone = Some(new.milestone.clone());
    }

    let old_labels: BTreeSet<_> = old.labels.iter().map(|s| s.to_string()).collect();
    let new_labels: BTreeSet<_> = new.labels.iter().map(|s| s.to_string()).collect();
    for label in new_labels.difference(&old_labels) {
        diff.added_labels.push(label.clone());
    }
    for label in old_labels.difference(&new_labels) {
        diff.removed_labels.push(label.clone());
    }

    let old_assignees: BTreeSet<_> = old.assignees.iter().map(|s| s.to_string()).collect();
    let new_assignees: BTreeSet<_> = new.assignees.iter().map(|s| s.to_string()).collect();
    for assignee in new_assignees.difference(&old_assignees) {
        diff.added_assignees.push(assignee.clone());
    }
    for assignee in old_assignees.difference(&new_assignees) {
        diff.removed_assignees.push(assignee.clone());
    }

    diff
}

impl IssueFieldDiff {
    fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.state.is_none()
            && self.body.is_none()
            && self.milestone.is_none()
            && self.added_labels.is_empty()
            && self.removed_labels.is_empty()
            && self.added_assignees.is_empty()
            && self.removed_assignees.is_empty()
    }

    fn changes(&self, issue_number: u64) -> Vec<String> {
        let mut changes = Vec::new();
        if self.title.is_some() {
            changes.push("changed title".to_string());
        }
        if let Some(state) = self.state.as_deref() {
            changes.push(format!("state -> {state}"));
        }
        if self.body.is_some() {
            changes.push("updated body".to_string());
        }
        match &self.milestone {
            Some(Some(m)) => changes.push(format!("set milestone to '{m}'")),
            Some(None) => changes.push("cleared milestone".to_string()),
            None => {}
        }
        for label in &self.added_labels {
            changes.push(format!("added label '{label}'"));
        }
        for label in &self.removed_labels {
            changes.push(format!("removed label '{label}'"));
        }
        for assignee in &self.added_assignees {
            changes.push(format!("added assignee '{assignee}'"));
        }
        for assignee in &self.removed_assignees {
            changes.push(format!("removed assignee '{assignee}'"));
        }
        if changes.is_empty() {
            changes.push(format!("updated local content for #{issue_number}"));
        }
        changes
    }
}

fn apply_issue_edits(repo: &str, edited: &ParsedIssueFile, diff: &IssueFieldDiff) -> Result<()> {
    if diff.is_empty() {
        return Ok(());
    }

    let mut cmd = Command::new("gh");
    cmd.args(["issue", "edit", "-R", repo, &edited.number.to_string()]);

    if let Some(title) = diff.title.as_deref() {
        cmd.args(["--title", title]);
    }
    if let Some(body) = diff.body.as_deref() {
        cmd.args(["--body", body]);
    }
    if let Some(milestone) = diff.milestone.as_ref() {
        match milestone.as_deref() {
            Some(value) => cmd.args(["--milestone", value]),
            None => cmd.args(["--milestone", ""]),
        };
    }
    for label in &diff.added_labels {
        cmd.args(["--add-label", label]);
    }
    for label in &diff.removed_labels {
        cmd.args(["--remove-label", label]);
    }
    for assignee in &diff.added_assignees {
        cmd.args(["--add-assignee", assignee]);
    }
    for assignee in &diff.removed_assignees {
        cmd.args(["--remove-assignee", assignee]);
    }

    let output = cmd.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(eyre::eyre!("gh issue edit failed: {stderr}"));
    }

    if let Some(state) = diff.state.as_deref() {
        let mut state_cmd = Command::new("gh");
        match state {
            "closed" => {
                state_cmd.args(["issue", "close", "-R", repo, &edited.number.to_string()]);
            }
            "open" => {
                state_cmd.args(["issue", "reopen", "-R", repo, &edited.number.to_string()]);
            }
            other => {
                return Err(eyre::eyre!(
                    "invalid desired state '{other}' for issue #{}",
                    edited.number
                ));
            }
        }
        let state_output = state_cmd.output()?;
        if !state_output.status.success() {
            let stderr = String::from_utf8_lossy(&state_output.stderr)
                .trim()
                .to_string();
            return Err(eyre::eyre!(
                "gh issue {} failed for #{}: {stderr}",
                if state == "closed" { "close" } else { "reopen" },
                edited.number
            ));
        }
    }

    Ok(())
}

pub fn create_issue(repo: &str, issue: &NewIssue) -> Result<(u64, String)> {
    let mut cmd = Command::new("gh");
    cmd.args([
        "issue",
        "create",
        "-R",
        repo,
        "--title",
        &issue.title,
        "--body",
        &issue.body,
    ]);
    for label in &issue.labels {
        cmd.args(["--label", label]);
    }
    if let Some(milestone) = issue.milestone.as_deref() {
        cmd.args(["--milestone", milestone]);
    }
    for assignee in &issue.assignees {
        cmd.args(["--assignee", assignee]);
    }

    let output = cmd.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(eyre::eyre!("gh issue create failed: {stderr}"));
    }

    let stdout = String::from_utf8(output.stdout)?.trim().to_string();
    let issue_number = parse_issue_number_from_create_output(&stdout).ok_or_else(|| {
        eyre::eyre!("could not parse issue number from gh issue create output: {stdout}")
    })?;
    Ok((issue_number, stdout))
}

pub fn ensure_label_exists(repo: &str, label: &str) -> Result<()> {
    let output = Command::new("gh")
        .args([
            "label",
            "create",
            label,
            "-R",
            repo,
            "--color",
            "BFD4F2",
            "--description",
            "Created by bud issue-create",
        ])
        .output()?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if stderr.contains("already exists") {
        return Ok(());
    }
    Err(eyre::eyre!(
        "gh label create failed for '{label}': {}",
        stderr.trim()
    ))
}

pub fn ensure_milestone_exists(repo: &str, milestone: &str) -> Result<()> {
    let (owner, name) = split_repo(repo)?;
    let endpoint = format!("repos/{owner}/{name}/milestones");
    let output = Command::new("gh")
        .args(["api", &endpoint, "-X", "POST", "-f"])
        .arg(format!("title={milestone}"))
        .output()?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if stderr.contains("already_exists") || stderr.contains("already exists") {
        return Ok(());
    }
    Err(eyre::eyre!(
        "gh milestone create failed for '{milestone}': {}",
        stderr.trim()
    ))
}

pub fn issue_filename_for_number_title(number: u64, title: &str) -> String {
    let mut safe = sanitize_title_for_filename(title);
    if safe.is_empty() {
        safe = "untitled".to_string();
    }
    format!("{number} - {safe}.md")
}

fn write_deps(
    repo: &str,
    dir: &Path,
    deps_markdown_path: &Path,
    relationships: &BTreeMap<u64, IssueRelationships>,
    number_to_filename: &BTreeMap<u64, String>,
) -> Result<Option<PathBuf>> {
    let mut blocked_by_lines = Vec::new();
    let mut blocking_lines = Vec::new();
    let mut parent_sub_lines = Vec::new();
    let mut tracked_in_lines = Vec::new();

    let mut deps_dir_created = false;
    let deps_dir = dir.join("deps");

    for (number, rel) in relationships {
        if !rel.blocked_by.is_empty() {
            blocked_by_lines.push(format!(
                "#{} is blocked by: {}",
                number,
                format_numbers(&rel.blocked_by)
            ));
            for other in &rel.blocked_by {
                if let Some(filename) = number_to_filename.get(number) {
                    let target_dir = deps_dir.join(format!("blocked-by-{other}"));
                    std::fs::create_dir_all(&target_dir)?;
                    create_symlink(&format!("../../all/{filename}"), &target_dir.join(filename))?;
                    deps_dir_created = true;
                }
            }
        }
        if !rel.blocking.is_empty() {
            blocking_lines.push(format!(
                "#{} blocks: {}",
                number,
                format_numbers(&rel.blocking)
            ));
            for other in &rel.blocking {
                if let Some(filename) = number_to_filename.get(number) {
                    let target_dir = deps_dir.join(format!("blocks-{other}"));
                    std::fs::create_dir_all(&target_dir)?;
                    create_symlink(&format!("../../all/{filename}"), &target_dir.join(filename))?;
                    deps_dir_created = true;
                }
            }
        }
        if !rel.sub_issues.is_empty() {
            parent_sub_lines.push(format!(
                "#{number} has sub-issues: {}",
                format_numbers(&rel.sub_issues)
            ));
        }
        if let Some(parent) = rel.parent {
            parent_sub_lines.push(format!("#{number} parent: #{parent}"));
        }
        if !rel.tracked_in.is_empty() {
            tracked_in_lines.push(format!(
                "#{number} tracked in: {}",
                format_numbers(&rel.tracked_in)
            ));
        }
        if !rel.tracked_issues.is_empty() {
            tracked_in_lines.push(format!(
                "#{number} tracks: {}",
                format_numbers(&rel.tracked_issues)
            ));
        }
    }

    let mut deps_md = format!("# Dependencies for {repo}\n\n");
    if !blocked_by_lines.is_empty() {
        deps_md.push_str("## Blocked By\n");
        for line in &blocked_by_lines {
            deps_md.push_str(&format!("- {line}\n"));
        }
        deps_md.push('\n');
    }
    if !blocking_lines.is_empty() {
        deps_md.push_str("## Blocking\n");
        for line in &blocking_lines {
            deps_md.push_str(&format!("- {line}\n"));
        }
        deps_md.push('\n');
    }
    if !parent_sub_lines.is_empty() {
        deps_md.push_str("## Parent / Sub-Issues\n");
        for line in &parent_sub_lines {
            deps_md.push_str(&format!("- {line}\n"));
        }
        deps_md.push('\n');
    }
    if !tracked_in_lines.is_empty() {
        deps_md.push_str("## Tracked In\n");
        for line in &tracked_in_lines {
            deps_md.push_str(&format!("- {line}\n"));
        }
        deps_md.push('\n');
    }
    std::fs::write(deps_markdown_path, deps_md)?;

    Ok(if deps_dir_created {
        Some(deps_dir)
    } else {
        None
    })
}

fn render_labels_markdown(repo: &str, labels: &[RepoLabel]) -> String {
    let mut labels = labels.to_vec();
    labels.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    let mut out = format!("# Available Labels for {repo}\n\n");
    if labels.is_empty() {
        out.push_str("- (no labels)\n");
        return out;
    }
    for label in labels {
        let description = label
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .unwrap_or("No description");
        let mut line = format!("- **{}** — {}", label.name, description);
        if let Some(color) = label.color.as_deref()
            && !color.trim().is_empty()
        {
            line.push_str(&format!(" (#{})", color.trim()));
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn render_milestones_markdown(repo: &str, milestones: &[String]) -> String {
    let mut milestones = milestones.to_vec();
    milestones.sort_by_key(|m| m.to_lowercase());
    let mut out = format!("# Available Milestones for {repo}\n\n");
    if milestones.is_empty() {
        out.push_str("- (no milestones)\n");
        return out;
    }
    for milestone in milestones {
        out.push_str(&format!("- {milestone}\n"));
    }
    out
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
    issue_filename_for_number_title(issue.number, &issue.title)
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
    squashed
        .chars()
        .take(80)
        .collect::<String>()
        .trim()
        .to_string()
}

fn render_issue(issue: &Issue) -> String {
    let labels = if issue.labels.is_empty() {
        "none".to_string()
    } else {
        issue
            .labels
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let assignees = if issue.assignees.is_empty() {
        "none".to_string()
    } else {
        issue
            .assignees
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

fn render_index(
    repo: &str,
    open_issues: &[&Issue],
    closed_issues: &[&Issue],
    all_issues: &[Issue],
) -> String {
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

fn split_repo(repo: &str) -> Result<(String, String)> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| eyre::eyre!("repo must be in owner/repo format: {repo}"))?;
    if owner.is_empty() || name.is_empty() {
        return Err(eyre::eyre!("repo must be in owner/repo format: {repo}"));
    }
    Ok((owner.to_string(), name.to_string()))
}

fn parse_repo_from_remote(remote: &str) -> Result<String> {
    let path = if let Some((_, rest)) = remote.split_once("github.com:") {
        rest
    } else if let Some((_, rest)) = remote.split_once("github.com/") {
        rest
    } else {
        return Err(eyre::eyre!(
            "bud issues only supports GitHub repositories. Remote origin points to: {remote}"
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

fn extract_numbers(issue: &Value, field: &str) -> Vec<u64> {
    issue
        .get(field)
        .and_then(|v| v.get("nodes"))
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|node| node.get("number").and_then(Value::as_u64))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn format_numbers(numbers: &[u64]) -> String {
    numbers
        .iter()
        .map(|n| format!("#{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn parse_issue_number_from_create_output(output: &str) -> Option<u64> {
    let token = output.split_whitespace().last()?;
    token
        .trim_end_matches('/')
        .split('/')
        .next_back()?
        .parse::<u64>()
        .ok()
}

pub fn sync_labels_set(repo: &str) -> Result<BTreeSet<String>> {
    let labels = sync_labels(repo)?;
    Ok(labels.into_iter().map(|l| l.name).collect())
}

pub fn sync_milestones_set(repo: &str) -> Result<BTreeSet<String>> {
    let milestones = sync_milestones(repo)?;
    Ok(milestones.into_iter().collect())
}
