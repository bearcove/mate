use bud::pane::{AgentState, AgentType, PaneState, parse_pane_content};
use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct AsciicastHeader {
    version: u8,
    term: AsciicastTerm,
}

#[derive(Debug, Deserialize)]
struct AsciicastTerm {
    cols: u16,
    rows: u16,
}

#[derive(Debug, Clone)]
struct ReplayFrame {
    index: usize,
    timestamp: f64,
    pane: PaneState,
    tail: String,
}

#[derive(Debug)]
struct ReplayStats {
    frames: usize,
    claude_frames: usize,
    codex_frames: usize,
    working_frames: usize,
    idle_frames: usize,
    type_flip: bool,
}

fn replay_cast(path: &Path) -> (ReplayStats, Vec<ReplayFrame>) {
    let content = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed reading {}: {e}", path.display()));
    let mut lines = content.lines();
    let header_line = lines
        .next()
        .unwrap_or_else(|| panic!("missing header line in {}", path.display()));
    let header: AsciicastHeader = serde_json::from_str(header_line)
        .unwrap_or_else(|e| panic!("invalid header in {}: {e}", path.display()));
    assert_eq!(
        header.version,
        3,
        "expected asciicast v3 for {}",
        path.display()
    );

    let mut parser = vt100::Parser::new(header.term.rows, header.term.cols, 0);
    let mut stats = ReplayStats {
        frames: 0,
        claude_frames: 0,
        codex_frames: 0,
        working_frames: 0,
        idle_frames: 0,
        type_flip: false,
    };

    let mut frames: Vec<ReplayFrame> = Vec::new();
    let mut seen_type: Option<AgentType> = None;
    let mut elapsed = 0.0_f64;

    for (idx, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }

        let event: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|e| {
            panic!(
                "invalid event JSON at {} line {}: {e}",
                path.display(),
                idx + 2
            )
        });
        let arr = event
            .as_array()
            .unwrap_or_else(|| panic!("event is not array at {} line {}", path.display(), idx + 2));
        if arr.len() != 3 {
            continue;
        }
        if arr[1].as_str() != Some("o") {
            continue;
        }
        elapsed += arr[0].as_f64().unwrap_or(0.0);
        let timestamp = elapsed;
        let data = arr[2].as_str().unwrap_or_default();

        parser.process(data.as_bytes());
        let frame = parser.screen().contents();
        let pane = parse_pane_content(&frame);
        let tail = frame
            .lines()
            .rev()
            .take(10)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");

        stats.frames += 1;
        match pane.agent_type {
            Some(AgentType::Claude) => stats.claude_frames += 1,
            Some(AgentType::Codex) => stats.codex_frames += 1,
            None => {}
        }
        match pane.state {
            AgentState::Working => stats.working_frames += 1,
            AgentState::Idle => stats.idle_frames += 1,
            AgentState::Unknown => {}
        }

        if let Some(current) = pane.agent_type.clone() {
            if let Some(previous) = seen_type.as_ref()
                && previous != &current
            {
                stats.type_flip = true;
            }
            if seen_type.is_none() {
                seen_type = Some(current);
            }
        }

        frames.push(ReplayFrame {
            index: stats.frames - 1,
            timestamp,
            pane,
            tail,
        });
    }

    (stats, frames)
}

fn state_change_points(frames: &[ReplayFrame]) -> Vec<&ReplayFrame> {
    let mut out = Vec::new();
    let mut prev_state: Option<(Option<AgentType>, AgentState)> = None;
    for frame in frames {
        let current = (frame.pane.agent_type.clone(), frame.pane.state.clone());
        if prev_state.as_ref() != Some(&current) {
            out.push(frame);
            prev_state = Some(current);
        }
    }
    out
}

struct WorkingSegmentExpectation {
    start_index: usize,
    stop_index: usize,
    expected_agent: AgentType,
    require_stop_agent_match: bool,
    expected_stop_state: AgentState,
    min_secs: f64,
    max_secs: f64,
}

fn assert_working_segment(frames: &[ReplayFrame], expectation: WorkingSegmentExpectation) {
    let WorkingSegmentExpectation {
        start_index,
        stop_index,
        expected_agent,
        require_stop_agent_match,
        expected_stop_state,
        min_secs,
        max_secs,
    } = expectation;
    assert!(start_index < stop_index, "invalid segment bounds");
    let start = frames
        .iter()
        .find(|f| f.index == start_index)
        .unwrap_or_else(|| panic!("missing frame {start_index}"));
    let stop = frames
        .iter()
        .find(|f| f.index == stop_index)
        .unwrap_or_else(|| panic!("missing frame {stop_index}"));

    assert_eq!(
        start.pane.agent_type,
        Some(expected_agent.clone()),
        "start frame should identify expected agent"
    );
    assert_eq!(
        start.pane.state,
        AgentState::Working,
        "start frame must be Working"
    );
    if require_stop_agent_match {
        assert_eq!(
            stop.pane.agent_type,
            Some(expected_agent),
            "stop frame should identify expected agent"
        );
    }
    assert_eq!(
        stop.pane.state, expected_stop_state,
        "unexpected stop state"
    );

    for frame in frames
        .iter()
        .filter(|f| f.index >= start_index && f.index < stop_index)
    {
        assert_eq!(
            frame.pane.state,
            AgentState::Working,
            "frame {} in segment should be Working",
            frame.index
        );
    }

    let elapsed = stop.timestamp - start.timestamp;
    assert!(
        elapsed >= min_secs && elapsed <= max_secs,
        "segment duration out of range: {elapsed:.2}s not in [{min_secs}, {max_secs}]"
    );
}

#[test]
fn replay_claude_cast() {
    let path = Path::new("tests/fixtures/sess-claude.cast");
    let (stats, frames) = replay_cast(path);
    assert!(stats.frames > 0, "no output frames replayed");
    assert!(
        stats.claude_frames > 0,
        "expected at least some Claude detections"
    );
    assert!(
        stats.working_frames > 0,
        "expected Working frames for Claude"
    );
    assert!(stats.idle_frames > 0, "expected Idle frames for Claude");
    assert!(
        !stats.type_flip,
        "agent type should not flip once detected in Claude cast"
    );
    let first_working = frames
        .iter()
        .find(|frame| {
            frame.pane.agent_type == Some(AgentType::Claude)
                && frame.pane.state == AgentState::Working
        })
        .expect("Claude cast should include Working frames");
    let first_idle_after_working = frames
        .iter()
        .find(|frame| {
            frame.index > first_working.index
                && frame.pane.agent_type == Some(AgentType::Claude)
                && frame.pane.state == AgentState::Idle
        })
        .expect("Claude cast should return to Idle after Working");

    assert_eq!(first_working.index, 43);
    assert_eq!(first_idle_after_working.index, 166);

    let stable_start = frames
        .iter()
        .find(|frame| frame.index == 43)
        .expect("expected stable Claude segment start");
    let stable_end = frames
        .iter()
        .rev()
        .find(|frame| frame.pane.agent_type == Some(AgentType::Claude))
        .expect("expected Claude detections");
    for frame in frames
        .iter()
        .filter(|frame| frame.index >= stable_start.index && frame.index <= stable_end.index)
    {
        assert_eq!(
            frame.pane.agent_type,
            Some(AgentType::Claude),
            "Claude segment should not flip type at frame {}",
            frame.index
        );
        assert_ne!(
            frame.pane.state,
            AgentState::Unknown,
            "Claude segment should not contain Unknown at frame {}",
            frame.index
        );
    }
}

#[test]
fn replay_codex_cast() {
    const CODEX_WORKING_START_FRAME: usize = 81;
    const CODEX_WORKING_STOP_FRAME: usize = 1126;
    const CODEX_STABLE_START_FRAME: usize = 43;
    const CODEX_STABLE_END_FRAME: usize = 2356;

    let path = Path::new("tests/fixtures/sess-codex.cast");
    let (stats, frames) = replay_cast(path);
    assert!(stats.frames > 0, "no output frames replayed");
    assert!(
        stats.codex_frames > 0,
        "expected at least some Codex detections"
    );
    assert!(
        stats.idle_frames > 0,
        "expected at least one Idle frame for Codex"
    );
    assert!(
        !stats.type_flip,
        "agent type should not flip once detected in Codex cast"
    );
    for frame in frames.iter().filter(|frame| {
        frame.index >= CODEX_STABLE_START_FRAME && frame.index <= CODEX_STABLE_END_FRAME
    }) {
        assert_eq!(
            frame.pane.agent_type,
            Some(AgentType::Codex),
            "Codex segment should not flip type at frame {}",
            frame.index
        );
        assert_ne!(
            frame.pane.state,
            AgentState::Unknown,
            "Codex segment should not contain Unknown at frame {}",
            frame.index
        );
    }

    let first_idle = frames
        .iter()
        .find(|frame| {
            frame.pane.agent_type == Some(AgentType::Codex) && frame.pane.state == AgentState::Idle
        })
        .expect("Codex cast should include Idle frames");
    assert_eq!(first_idle.index, 19);

    assert_working_segment(
        &frames,
        WorkingSegmentExpectation {
            start_index: CODEX_WORKING_START_FRAME,
            stop_index: CODEX_WORKING_STOP_FRAME,
            expected_agent: AgentType::Codex,
            require_stop_agent_match: true,
            expected_stop_state: AgentState::Idle,
            min_secs: 30.0,
            max_secs: 40.0,
        },
    );
}

#[test]
#[ignore]
fn print_state_change_frames() {
    for path in [
        Path::new("tests/fixtures/sess-claude.cast"),
        Path::new("tests/fixtures/sess-codex.cast"),
    ] {
        let (_, frames) = replay_cast(path);
        println!("== {} ==", path.display());
        for frame in state_change_points(&frames) {
            println!(
                "frame={} ts={:.3} agent={:?} state={:?}\n{}\n---",
                frame.index, frame.timestamp, frame.pane.agent_type, frame.pane.state, frame.tail
            );
        }
    }
}
