use serde_json::json;
use transcript::{Block, Role, Turn, ai_title, parse};

const PLAIN_TEXT: &str = include_str!("fixtures/plain_text.jsonl");
const TOOL_USE_RESULT: &str = include_str!("fixtures/tool_use_result.jsonl");
const THINKING_AI_TITLE: &str = include_str!("fixtures/thinking_ai_title.jsonl");
const SIDECHAIN: &str = include_str!("fixtures/sidechain.jsonl");
const STRING_CONTENT: &str = include_str!("fixtures/string_content.jsonl");
const ALL: [&str; 5] = [
    PLAIN_TEXT,
    TOOL_USE_RESULT,
    THINKING_AI_TITLE,
    SIDECHAIN,
    STRING_CONTENT,
];

fn turn(role: Role, block: Block) -> Turn {
    Turn {
        role,
        blocks: vec![block],
        is_meta: false,
        is_sidechain: false,
    }
}

fn text(s: &str) -> Block {
    Block::Text(s.to_owned())
}

fn tool_use(id: &str, name: &str, input: serde_json::Value) -> Block {
    Block::ToolUse {
        id: id.to_owned(),
        name: name.to_owned(),
        input,
    }
}

fn tool_result(id: &str, content: &str, is_error: bool, agent_id: Option<&str>) -> Block {
    Block::ToolResult {
        tool_use_id: id.to_owned(),
        content: content.to_owned(),
        is_error,
        agent_id: agent_id.map(str::to_owned),
    }
}

#[test]
fn plain_text_fixture() {
    assert_eq!(
        parse(PLAIN_TEXT),
        vec![
            turn(Role::User, text("Summarize the build status.")),
            turn(Role::Assistant, text("The build is green.")),
        ]
    );
    assert_eq!(ai_title(PLAIN_TEXT), None);
}

#[test]
fn tool_use_result_fixture() {
    assert_eq!(
        parse(TOOL_USE_RESULT),
        vec![
            turn(
                Role::Assistant,
                tool_use(
                    "toolu_demo01",
                    "Bash",
                    json!({"command": "cargo test", "description": "Run tests"})
                )
            ),
            turn(
                Role::User,
                tool_result("toolu_demo01", "test result: ok", false, None)
            ),
            turn(
                Role::Assistant,
                tool_use(
                    "toolu_demo02",
                    "Read",
                    json!({"file_path": r"C:\work\demo\src\lib.rs"})
                )
            ),
            turn(
                Role::User,
                tool_result("toolu_demo02", "fn main() {}", false, None)
            ),
            turn(
                Role::Assistant,
                tool_use(
                    "toolu_demo10",
                    "Bash",
                    json!({"command": "false", "description": "Fail on purpose"})
                )
            ),
            turn(
                Role::User,
                tool_result("toolu_demo10", "Exit code 1", true, None)
            ),
            turn(
                Role::Assistant,
                tool_use(
                    "toolu_demo20",
                    "Agent",
                    json!({"description": "Explore crate", "prompt": "List the modules.", "subagent_type": "Explore"})
                )
            ),
            turn(
                Role::User,
                tool_result(
                    "toolu_demo20",
                    "Async agent launched.\nagentId: a0000000000000001",
                    false,
                    Some("a0000000000000001")
                )
            ),
        ]
    );
}

#[test]
fn thinking_ai_title_fixture() {
    assert_eq!(
        parse(THINKING_AI_TITLE),
        vec![
            turn(Role::User, text("Why does the parser test flake?")),
            turn(Role::Assistant, text("The test depends on HashMap order.")),
        ]
    );
    assert_eq!(
        ai_title(THINKING_AI_TITLE).as_deref(),
        Some("Fix flaky parser test")
    );
}

#[test]
fn thinking_never_escapes() {
    assert!(THINKING_AI_TITLE.contains("SECRET-THINKING-MARKER"));
    assert!(THINKING_AI_TITLE.contains("SECRET-SIGNATURE-MARKER"));
    for src in ALL {
        let shown = format!("{:?} {:?}", parse(src), ai_title(src));
        assert!(!shown.contains("SECRET-THINKING-MARKER"));
        assert!(!shown.contains("SECRET-SIGNATURE-MARKER"));
    }
}

#[test]
fn sidechain_fixture() {
    let side = |role, block| Turn {
        is_sidechain: true,
        ..turn(role, block)
    };
    assert_eq!(
        parse(SIDECHAIN),
        vec![
            side(Role::User, text("List the modules of the crate.")),
            side(Role::Assistant, text("Modules: lib, parse.")),
        ]
    );
}

#[test]
fn string_content_fixture() {
    let meta = |block| Turn {
        is_meta: true,
        ..turn(Role::User, block)
    };
    assert_eq!(
        parse(STRING_CONTENT),
        vec![
            meta(text(
                "<local-command-caveat>Caveat: demo meta record.</local-command-caveat>"
            )),
            turn(Role::User, text("Привет, add a test for empty input.")),
            meta(text("Array-form prompt text.")),
        ]
    );
}

fn has_bot_token_shape(src: &str) -> bool {
    let bytes = src.as_bytes();
    bytes.iter().enumerate().any(|(i, &b)| {
        if b != b':' {
            return false;
        }
        let digits = bytes[..i]
            .iter()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .count();
        let tail = bytes[i + 1..]
            .iter()
            .take_while(|c| c.is_ascii_alphanumeric() || **c == b'_' || **c == b'-')
            .count();
        (8..=10).contains(&digits) && tail >= 30
    })
}

fn has_supergroup_id(src: &str) -> bool {
    src.match_indices("-100")
        .any(|(i, _)| src[i + 4..].bytes().take_while(u8::is_ascii_digit).count() >= 6)
}

#[test]
fn fixtures_have_no_private_data() {
    for src in ALL {
        let lower = src.to_lowercase();
        for marker in ["users\\", "users/", "/home/", ".claude"] {
            assert!(!lower.contains(marker), "fixture contains {marker}");
        }
        assert!(!has_supergroup_id(src));
        assert!(!has_bot_token_shape(src));
        for line in src.lines() {
            assert!(serde_json::from_str::<serde_json::Value>(line).is_ok());
        }
    }
}

#[test]
fn privacy_detectors_fire() {
    assert!(has_bot_token_shape(
        "x 123456789:AAabcdefghijklmnopqrstuvwxyz0123456 y"
    ));
    assert!(has_supergroup_id("chat -1001234567890"));
    assert!("c:\\users\\someone".contains("users\\"));
}
