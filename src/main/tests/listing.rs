use crate::listing::{
    AgentListRow, IdleTracker, RequestListRow, classify_agent_role, format_agent_task_summary,
    format_context_line, format_idle_seconds, format_status, render_agent_blocks,
    render_request_blocks, render_session_groups,
};

#[tokio::test]
async fn idle_tracker_updates_and_resets_on_activity() {
    let root = std::env::temp_dir().join(format!("mate-idle-test-{}", uuid::Uuid::new_v4()));
    fs_err::tokio::create_dir_all(&root)
        .await
        .expect("create idle test directory");

    let mut tracker = IdleTracker::new(100, root.clone());
    assert_eq!(
        tracker
            .update("sess", "%42", &crate::pane::AgentState::Idle)
            .await,
        Some(0)
    );

    let mut tracker = IdleTracker::new(108, root.clone());
    assert_eq!(
        tracker
            .update("sess", "%42", &crate::pane::AgentState::Idle)
            .await,
        Some(8)
    );

    let mut tracker = IdleTracker::new(120, root.clone());
    assert_eq!(
        tracker
            .update("sess", "%42", &crate::pane::AgentState::Working)
            .await,
        None
    );

    let idle_file = root.join("sess").join("%42.idle");
    assert!(
        !idle_file.exists(),
        "idle tracking file should be removed after activity resumes"
    );

    let mut tracker = IdleTracker::new(130, root.clone());
    assert_eq!(
        tracker
            .update("sess", "%42", &crate::pane::AgentState::Idle)
            .await,
        Some(0)
    );

    fs_err::tokio::remove_dir_all(&root)
        .await
        .expect("remove idle test directory");
}

#[test]
fn list_headers_include_idle_seconds_column() {
    let request_blocks = render_request_blocks(&[RequestListRow {
        session: "sess".to_string(),
        id: "deadbeef".to_string(),
        source: "%1".to_string(),
        target: "%2".to_string(),
        title: Some("example title".to_string()),
        age: "12s".to_string(),
        idle_seconds: Some(42),
        response: "no".to_string(),
    }]);
    let agent_blocks = render_agent_blocks(&[AgentListRow {
        session: "sess".to_string(),
        pane_id: "%2".to_string(),
        agent: "Codex".to_string(),
        role: "Mate".to_string(),
        state: "Idle".to_string(),
        idle: "42".to_string(),
        context: "98% left".to_string(),
        activity: "Running checks".to_string(),
        tasks: vec!["deadbeef (Example)".to_string()],
    }]);

    assert!(request_blocks.contains("Age/Idle/Response:"));
    assert!(request_blocks.contains("42s"));
    assert!(agent_blocks.contains("Task: deadbeef (Example)"));
    assert!(agent_blocks.contains("Context: 98% left"));
    assert!(agent_blocks.contains("Status:"));
    assert!(!agent_blocks.contains("\nIdle:"));
    assert_eq!(format_idle_seconds(Some(42)), "42");
    assert_eq!(format_idle_seconds(None), "-");
}

#[test]
fn request_blocks_follow_grouped_shape() {
    let blocks = render_request_blocks(&[RequestListRow {
        session: "session-alpha".to_string(),
        id: "deadbeef".to_string(),
        source: "%1".to_string(),
        target: "%2".to_string(),
        title: Some("Long title for readability".to_string()),
        age: "12s".to_string(),
        idle_seconds: Some(7),
        response: "no".to_string(),
    }]);
    assert!(blocks.contains("Task: deadbeef @ session-alpha (%1 -> %2)"));
    assert!(blocks.contains("Title: Long title for readability"));
    assert!(blocks.contains("Age/Idle/Response: 12s / 7s / no"));
}

#[test]
fn agent_blocks_follow_grouped_shape() {
    let blocks = render_agent_blocks(&[AgentListRow {
        session: "3".to_string(),
        pane_id: "%24".to_string(),
        agent: "Claude".to_string(),
        role: "Mate".to_string(),
        state: "Working".to_string(),
        idle: "0".to_string(),
        context: "35% left".to_string(),
        activity: "17s - esc to interrupt".to_string(),
        tasks: vec!["805fbe4a (static-edit-verifier-167)".to_string()],
    }]);
    assert!(blocks.contains("Agent: Claude @ 3/%24 | Role: Mate"));
    assert!(blocks.contains("Task: 805fbe4a (static-edit-verifier-167)"));
    assert!(blocks.contains("Context: 35% left [####------]"));
    assert!(blocks.contains("Status: Working (17s - esc to interrupt)"));
    assert!(!blocks.contains("Working (Working"));
    assert!(!blocks.contains("\nIdle:"));
}

#[test]
fn block_renderer_separates_multiple_entries_with_blank_line() {
    let requests = render_request_blocks(&[
        RequestListRow {
            session: "s".to_string(),
            id: "aaaaaaaa".to_string(),
            source: "%1".to_string(),
            target: "%2".to_string(),
            title: Some("one".to_string()),
            age: "1m".to_string(),
            idle_seconds: Some(0),
            response: "no".to_string(),
        },
        RequestListRow {
            session: "s".to_string(),
            id: "bbbbbbbb".to_string(),
            source: "%1".to_string(),
            target: "%3".to_string(),
            title: Some("two".to_string()),
            age: "2m".to_string(),
            idle_seconds: Some(5),
            response: "yes".to_string(),
        },
    ]);
    assert!(requests.contains("no\n\nTask: bbbbbbbb"));
}

#[test]
fn agent_blocks_omit_task_line_when_none_assigned() {
    let blocks = render_agent_blocks(&[AgentListRow {
        session: "3".to_string(),
        pane_id: "%6".to_string(),
        agent: "Codex".to_string(),
        role: "Unknown".to_string(),
        state: "Idle".to_string(),
        idle: "0".to_string(),
        context: "-".to_string(),
        activity: "-".to_string(),
        tasks: Vec::new(),
    }]);
    assert!(!blocks.contains("Task: -"));
    assert!(!blocks.contains("\nTask:"));
    assert!(blocks.contains("Status: Idle (0s)"));
}

#[test]
fn agent_task_summary_includes_title_when_present() {
    assert_eq!(
        format_agent_task_summary("deadbeef", Some("My title")),
        "deadbeef (My title)"
    );
    assert_eq!(format_agent_task_summary("deadbeef", None), "deadbeef");
}

#[test]
fn claude_tokens_context_normalizes_to_percent_line() {
    assert_eq!(
        format_context_line("73740 tokens"),
        "Context: 73740 tokens -> 64% left [######----]"
    );
}

#[test]
fn session_grouping_contains_session_heading_and_both_sections() {
    let output = render_session_groups(
        &[RequestListRow {
            session: "3".to_string(),
            id: "805fbe4a".to_string(),
            source: "%6".to_string(),
            target: "%24".to_string(),
            title: Some("static-edit-verifier-167".to_string()),
            age: "35m".to_string(),
            idle_seconds: Some(0),
            response: "no".to_string(),
        }],
        &[AgentListRow {
            session: "3".to_string(),
            pane_id: "%24".to_string(),
            agent: "Codex".to_string(),
            role: "Mate".to_string(),
            state: "Working".to_string(),
            idle: "0".to_string(),
            context: "35% left".to_string(),
            activity: "17s - esc to interrupt".to_string(),
            tasks: vec!["805fbe4a (static-edit-verifier-167)".to_string()],
        }],
    );
    assert!(output.contains("Session 3"));
    assert!(output.contains("Tasks:"));
    assert!(output.contains("Agents:"));
    assert!(output.contains("Agent: Codex @ 3/%24"));
    assert!(output.contains("Task: 805fbe4a (static-edit-verifier-167)"));
}

#[test]
fn session_grouping_omits_empty_section_placeholders() {
    let output = render_session_groups(
        &[RequestListRow {
            session: "3".to_string(),
            id: "deadbeef".to_string(),
            source: "%6".to_string(),
            target: "%24".to_string(),
            title: None,
            age: "1m".to_string(),
            idle_seconds: Some(0),
            response: "no".to_string(),
        }],
        &[],
    );
    assert!(output.contains("Session 3"));
    assert!(output.contains("Tasks:"));
    assert!(!output.contains("Agents:"));
    assert!(!output.contains("Agent: -"));
    assert!(!output.contains("Task: -"));
}

#[test]
fn classify_agent_role_captain_buddy_mixed_unknown() {
    let requests = vec![
        RequestListRow {
            session: "3".to_string(),
            id: "a".to_string(),
            source: "%6".to_string(),
            target: "%24".to_string(),
            title: None,
            age: "1m".to_string(),
            idle_seconds: Some(0),
            response: "no".to_string(),
        },
        RequestListRow {
            session: "3".to_string(),
            id: "b".to_string(),
            source: "%24".to_string(),
            target: "%6".to_string(),
            title: None,
            age: "1m".to_string(),
            idle_seconds: Some(0),
            response: "no".to_string(),
        },
    ];
    assert_eq!(classify_agent_role("3", "%7", &requests), "Unknown");
    assert_eq!(classify_agent_role("3", "%6", &requests), "Mixed");
    assert_eq!(classify_agent_role("3", "%24", &requests), "Mixed");
    assert_eq!(
        classify_agent_role(
            "3",
            "%1",
            &[RequestListRow {
                session: "3".to_string(),
                id: "x".to_string(),
                source: "%1".to_string(),
                target: "%2".to_string(),
                title: None,
                age: "1m".to_string(),
                idle_seconds: Some(0),
                response: "no".to_string(),
            }]
        ),
        "Captain"
    );
    assert_eq!(
        classify_agent_role(
            "3",
            "%2",
            &[RequestListRow {
                session: "3".to_string(),
                id: "x".to_string(),
                source: "%1".to_string(),
                target: "%2".to_string(),
                title: None,
                age: "1m".to_string(),
                idle_seconds: Some(0),
                response: "no".to_string(),
            }]
        ),
        "Mate"
    );
}

#[test]
fn status_format_dedups_repeated_state_prefix() {
    assert_eq!(
        format_status("Working", "Working (17s - esc to interrupt)"),
        "Working (17s - esc to interrupt)"
    );
    assert_eq!(
        format_status("Working", "17s - esc to interrupt"),
        "Working (17s - esc to interrupt)"
    );
}

#[test]
fn idle_status_merges_idle_seconds_on_status_line() {
    let blocks = render_agent_blocks(&[AgentListRow {
        session: "3".to_string(),
        pane_id: "%6".to_string(),
        agent: "Codex".to_string(),
        role: "Captain".to_string(),
        state: "Idle".to_string(),
        idle: "24".to_string(),
        context: "67% left".to_string(),
        activity: "-".to_string(),
        tasks: vec!["deadbeef".to_string()],
    }]);
    assert!(blocks.contains("Status: Idle (24s)"));
    assert!(!blocks.contains("\nIdle:"));
}
