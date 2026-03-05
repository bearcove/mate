    use super::IssueFieldDiff;
    use super::{issue_repo_dir, sync_local_issue_edits};
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct PathEnvGuard {
        old_path: Option<String>,
        old_log: Option<String>,
    }

    impl Drop for PathEnvGuard {
        fn drop(&mut self) {
            match &self.old_path {
                Some(value) => {
                    unsafe { std::env::set_var("PATH", value) };
                }
                None => unsafe { std::env::remove_var("PATH") },
            }
            match &self.old_log {
                Some(value) => {
                    unsafe { std::env::set_var("BUD_TEST_GH_LOG", value) };
                }
                None => unsafe { std::env::remove_var("BUD_TEST_GH_LOG") },
            }
        }
    }

    fn with_fake_gh_env(root: &Path) -> PathEnvGuard {
        let bin_dir = root.join("bin");
        let log_path = root.join("gh.log");
        std::fs::write(&log_path, "").expect("create gh log");
        std::fs::create_dir_all(&bin_dir).expect("create fake gh bin dir");
        let gh_path = bin_dir.join("gh");
        std::fs::write(
            &gh_path,
            r#"#!/bin/sh
echo "$@" >> "$BUD_TEST_GH_LOG"
if [ "$1" = "issue" ] && [ "$2" = "edit" ]; then
  echo "field to edit flag required..." >&2
  exit 1
fi
if [ "$1" = "issue" ] && [ "$2" = "close" ]; then
  exit 0
fi
if [ "$1" = "issue" ] && [ "$2" = "reopen" ]; then
  exit 0
fi
exit 0
"#,
        )
        .expect("write fake gh script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&gh_path)
                .expect("stat fake gh")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&gh_path, perms).expect("chmod fake gh");
        }
        let old_path = std::env::var("PATH").ok();
        let old_log = std::env::var("BUD_TEST_GH_LOG").ok();
        let path_sep = if cfg!(windows) { ";" } else { ":" };
        let new_path = match old_path.as_deref() {
            Some(existing) if !existing.is_empty() => {
                format!("{}{}{}", bin_dir.display(), path_sep, existing)
            }
            _ => bin_dir.display().to_string(),
        };
        unsafe { std::env::set_var("PATH", new_path) };
        unsafe { std::env::set_var("BUD_TEST_GH_LOG", &log_path) };

        PathEnvGuard { old_path, old_log }
    }

    fn issue_markdown(state: &str) -> String {
        format!(
            "# #31: issue 31\n\n**State:** {state}\n**Labels:** none\n**Milestone:** none\n**Assignees:** none\n\n---\n\nBody text.\n"
        )
    }

    fn setup_repo_files(repo: &str, edited_state: &str, baseline_state: &str) {
        let root = issue_repo_dir(repo);
        if root.exists() {
            std::fs::remove_dir_all(&root).expect("remove old repo test dir");
        }
        std::fs::create_dir_all(root.join("all")).expect("create all dir");
        std::fs::create_dir_all(root.join(".snapshots")).expect("create snapshots dir");
        std::fs::write(
            root.join("all/31 - issue-31.md"),
            issue_markdown(edited_state),
        )
        .expect("write edited issue file");
        std::fs::write(
            root.join(".snapshots/31.md"),
            issue_markdown(baseline_state),
        )
        .expect("write snapshot issue file");
    }

    fn read_log(root: &Path) -> String {
        std::fs::read_to_string(root.join("gh.log")).expect("read gh log")
    }

    #[test]
    fn has_non_state_edits_false_for_state_only_diff() {
        let diff = IssueFieldDiff {
            state: Some("closed".to_string()),
            ..IssueFieldDiff::default()
        };
        assert!(!diff.has_non_state_edits());
    }

    #[test]
    fn has_non_state_edits_true_when_other_fields_change() {
        let diff = IssueFieldDiff {
            state: Some("closed".to_string()),
            body: Some("updated".to_string()),
            ..IssueFieldDiff::default()
        };
        assert!(diff.has_non_state_edits());
    }

    #[test]
    fn changes_reports_close_and_reopen_actions() {
        let close_diff = IssueFieldDiff {
            state: Some("closed".to_string()),
            ..IssueFieldDiff::default()
        };
        assert_eq!(close_diff.changes(31), vec!["closed issue"]);

        let reopen_diff = IssueFieldDiff {
            state: Some("open".to_string()),
            ..IssueFieldDiff::default()
        };
        assert_eq!(reopen_diff.changes(31), vec!["reopened issue"]);
    }

    #[test]
    fn sync_local_issue_edits_uses_close_for_state_only_change() {
        let _env_guard = env_lock().lock().expect("acquire env lock");
        let suffix = uuid::Uuid::new_v4();
        let repo = format!("test-owner/test-repo-close-{suffix}");
        let root = std::env::temp_dir().join(format!("mate-gh-test-{suffix}"));
        std::fs::create_dir_all(&root).expect("create fake gh root");
        let _path_guard = with_fake_gh_env(&root);
        setup_repo_files(&repo, "closed", "open");

        let summary = sync_local_issue_edits(&repo).expect("sync local edits");
        assert!(
            summary.failed.is_empty(),
            "unexpected failures: {:?}",
            summary.failed
        );
        assert_eq!(summary.applied.len(), 1);
        assert_eq!(summary.applied[0].number, 31);
        assert_eq!(summary.applied[0].changes, vec!["closed issue"]);

        let log = read_log(&root);
        assert!(log.contains("issue close -R test-owner/test-repo-close-"));
        assert!(!log.contains("issue edit -R"));
    }

    #[test]
    fn sync_local_issue_edits_uses_reopen_for_state_only_change() {
        let _env_guard = env_lock().lock().expect("acquire env lock");
        let suffix = uuid::Uuid::new_v4();
        let repo = format!("test-owner/test-repo-reopen-{suffix}");
        let root = std::env::temp_dir().join(format!("mate-gh-test-{suffix}"));
        std::fs::create_dir_all(&root).expect("create fake gh root");
        let _path_guard = with_fake_gh_env(&root);
        setup_repo_files(&repo, "open", "closed");

        let summary = sync_local_issue_edits(&repo).expect("sync local edits");
        assert!(
            summary.failed.is_empty(),
            "unexpected failures: {:?}",
            summary.failed
        );
        assert_eq!(summary.applied.len(), 1);
        assert_eq!(summary.applied[0].number, 31);
        assert_eq!(summary.applied[0].changes, vec!["reopened issue"]);

        let log = read_log(&root);
        assert!(log.contains("issue reopen -R test-owner/test-repo-reopen-"));
        assert!(!log.contains("issue edit -R"));
    }
