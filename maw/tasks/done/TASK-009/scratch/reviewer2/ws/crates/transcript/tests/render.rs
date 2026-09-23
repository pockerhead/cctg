use std::time::{Duration, Instant};

use serde_json::json;
use transcript::{
    Block, Role, SplitOptions, Turn, last_prompts, parse, render_brief, render_full,
    split_for_telegram,
};

const IN_PROGRESS_MARKER: &str = "в работе…";

const FINAL_ANSWER: &str = include_str!("fixtures/final_answer.jsonl");
const COMPACT_SUMMARY: &str = include_str!("fixtures/compact_summary.jsonl");
const THINKING_AI_TITLE: &str = include_str!("fixtures/thinking_ai_title.jsonl");
const TOOL_USE_RESULT: &str = include_str!("fixtures/tool_use_result.jsonl");
const SIDECHAIN: &str = include_str!("fixtures/sidechain.jsonl");
const SLASH_COMMAND: &str = include_str!("fixtures/slash_command.jsonl");
const ALL: [&str; 9] = [
    FINAL_ANSWER,
    COMPACT_SUMMARY,
    THINKING_AI_TITLE,
    TOOL_USE_RESULT,
    SIDECHAIN,
    include_str!("fixtures/plain_text.jsonl"),
    include_str!("fixtures/string_content.jsonl"),
    include_str!("fixtures/null_fields.jsonl"),
    SLASH_COMMAND,
];

const FINAL_ANSWER_BRIEF: &str = "\
> Check the build \u{1F680} and explain.
• Bash: Run workspace tests
All 27 tests pass. Готово.

> Explore the crate
↳ Explore a0000000000000002: Explore crate
The crate has three modules.";

const FINAL_ANSWER_FULL: &str = r#"> Check the build 🚀 and explain.
Let me run the tests first.
• Bash: Run workspace tests
  {"command":"cargo test --workspace","description":"Run workspace tests"}
  ← Bash: test result: ok. 27 passed
All 27 tests pass. Готово.

> Explore the crate
↳ Explore a0000000000000002: Explore crate
  {"description":"Explore crate","prompt":"List the modules.","subagent_type":"Explore"}
  ← Agent: Modules: lib, render, split.
  agentId: a0000000000000002
The crate has three modules."#;

fn turn(role: Role, stop: Option<&str>, blocks: Vec<Block>) -> Turn {
    Turn {
        role,
        blocks,
        is_meta: false,
        is_sidechain: false,
        stop_reason: stop.map(str::to_owned),
    }
}

fn prompt(text: &str) -> Turn {
    turn(Role::User, None, vec![Block::Text(text.to_owned())])
}

fn say(stop: Option<&str>, text: &str) -> Turn {
    turn(Role::Assistant, stop, vec![Block::Text(text.to_owned())])
}

fn bash(id: &str, command: &str) -> Turn {
    turn(
        Role::Assistant,
        Some("tool_use"),
        vec![Block::ToolUse {
            id: id.to_owned(),
            name: "Bash".to_owned(),
            input: json!({ "command": command }),
        }],
    )
}

fn result(id: &str, content: &str) -> Turn {
    turn(
        Role::User,
        None,
        vec![Block::ToolResult {
            tool_use_id: id.to_owned(),
            content: content.to_owned(),
            is_error: false,
            agent_id: None,
        }],
    )
}

#[test]
fn final_answer_fixture_brief_and_full() {
    let turns = parse(FINAL_ANSWER);
    assert_eq!(render_brief(&turns), FINAL_ANSWER_BRIEF);
    assert_eq!(render_full(&turns), FINAL_ANSWER_FULL);
}

#[test]
fn brief_has_one_line_per_tool_call_and_no_io() {
    let turns = parse(TOOL_USE_RESULT);
    let brief = render_brief(&turns);
    assert_eq!(
        brief,
        "• Bash: Run tests\n• Read: C:\\work\\demo\\src\\lib.rs\n• Bash: Fail on purpose\n\
         ↳ Explore a0000000000000001: Explore crate\n{IN_PROGRESS_MARKER}"
            .replace("{IN_PROGRESS_MARKER}", IN_PROGRESS_MARKER)
    );
    assert!(!brief.contains("test result: ok"));
    assert!(!brief.contains("cargo test"));
    let full = render_full(&turns);
    assert!(full.contains("  ← Bash: test result: ok"));
    assert!(full.contains("  ← error: Exit code 1"));
    assert!(full.contains(r#"  {"command":"cargo test","description":"Run tests"}"#));
}

#[test]
fn thinking_never_rendered() {
    for src in ALL {
        let turns = parse(src);
        for shown in [render_brief(&turns), render_full(&turns)] {
            assert!(!shown.contains("SECRET-THINKING-MARKER"));
            assert!(!shown.contains("SECRET-SIGNATURE-MARKER"));
        }
    }
}

#[test]
fn unfinished_tail_is_marked_in_progress() {
    // real fixture: the last record is a text block with stop_reason "tool_use"
    let turns = parse(THINKING_AI_TITLE);
    let expected = format!("> Why does the parser test flake?\n{IN_PROGRESS_MARKER}");
    assert_eq!(render_brief(&turns), expected);
    assert!(render_full(&turns).contains("The test depends on HashMap order."));
    // a prompt with no answer yet, and a pending tool call
    assert_eq!(
        render_brief(&[prompt("go")]),
        format!("> go\n{IN_PROGRESS_MARKER}")
    );
    assert!(render_brief(&[prompt("go"), bash("t1", "ls")]).ends_with(IN_PROGRESS_MARKER));
    assert!(
        render_brief(&[prompt("go"), bash("t1", "ls"), result("t1", "ok")])
            .ends_with(IN_PROGRESS_MARKER)
    );
}

#[test]
fn finished_exchanges_have_no_marker() {
    let done = [
        prompt("go"),
        say(Some("tool_use"), "checking"),
        bash("t1", "ls"),
        result("t1", "ok"),
        say(Some("end_turn"), "done"),
    ];
    assert_eq!(render_brief(&done), "> go\n• Bash: ls\ndone");
    assert_eq!(render_brief(&[]), "");
    assert_eq!(
        render_brief(&[prompt("go"), prompt("[Request interrupted by user]")]),
        "> go\n\n> [Request interrupted by user]"
    );
}

#[test]
fn null_stop_reason_falls_back_to_structure() {
    // subagent transcripts: stop_reason is null on every record except the last of a response
    let turns = parse(SIDECHAIN);
    assert_eq!(
        render_brief(&turns),
        "> List the modules of the crate.\nModules: lib, parse."
    );
    let intermediate = [prompt("go"), say(None, "looking"), bash("t1", "ls")];
    assert_eq!(
        render_brief(&intermediate),
        format!("> go\n• Bash: ls\n{IN_PROGRESS_MARKER}")
    );
}

fn meta(text: &str) -> Turn {
    Turn {
        is_meta: true,
        ..prompt(text)
    }
}

#[test]
fn service_records_are_hidden_in_brief_and_keep_the_state() {
    for service in [
        "<task-notification>\n<task-id>x</task-id>\n</task-notification>",
        "<command-message>clear</command-message>",
        "<local-command-stdout>ok</local-command-stdout>",
        "<bash-input>ls</bash-input>",
        "<bash-stdout>a.txt</bash-stdout><bash-stderr></bash-stderr>",
    ] {
        let done = [prompt("go"), say(Some("end_turn"), "done"), prompt(service)];
        assert_eq!(render_brief(&done), "> go\ndone", "{service}");
        assert_eq!(
            render_full(&done),
            format!("> go\ndone\n\n> {service}"),
            "{service}"
        );
        assert_eq!(render_brief(&[prompt(service)]), "", "{service}");
        // a service record is not a prompt boundary: null text before a later tool call stays hidden
        let pending = [
            prompt("go"),
            say(None, "looking"),
            prompt(service),
            bash("t1", "ls"),
        ];
        assert_eq!(
            render_brief(&pending),
            format!("> go\n• Bash: ls\n{IN_PROGRESS_MARKER}"),
            "{service}"
        );
    }
}

#[test]
fn slash_commands_are_one_line_prompts() {
    let turns = parse(SLASH_COMMAND);
    assert_eq!(
        render_brief(&turns),
        "> /model opus\n\n> Summarize the build status.\nThe build is green.\n\n> /compact\n\n> /review check a<b and c>d second line\n\n> /maw-tasks add a task\nв работе…"
    );
    assert_eq!(
        render_full(&turns),
        "> /model opus\n\n> <local-command-stdout>Set model to opus</local-command-stdout>\n\n> Summarize the build status.\nThe build is green.\n\n> /compact\n\n> /review check a<b and c>d second line\n\n> /maw-tasks add a task\nв работе…"
    );
}

#[test]
fn slash_command_without_args_is_still_a_prompt() {
    let turns = [prompt(
        "<command-message>model</command-message>\n<command-name>/model</command-name>",
    )];
    let expected = format!("> /model\n{IN_PROGRESS_MARKER}");
    assert_eq!(render_brief(&turns), expected);
    assert_eq!(render_full(&turns), expected);
}

#[test]
fn slash_command_args_use_the_last_closing_tag() {
    let turns = [prompt(
        "<command-name>/x</command-name><command-message>x</command-message><command-args>a </command-args> b</command-args>",
    )];
    let expected = format!("> /x a </command-args> b\n{IN_PROGRESS_MARKER}");
    assert_eq!(render_brief(&turns), expected);
    assert_eq!(render_full(&turns), expected);
}

#[test]
fn compact_summary_is_hidden_in_brief_and_keeps_the_state() {
    let turns = parse(COMPACT_SUMMARY);
    assert_eq!(
        render_brief(&turns),
        format!("> Continue the work\n• Bash: cargo test\n{IN_PROGRESS_MARKER}")
    );
    let full = render_full(&turns);
    assert!(full.contains("This session is being continued from a previous conversation"));
    assert!(full.contains("Intermediate text before the compact summary."));
}

#[test]
fn only_channel_meta_is_shown_and_user_tags_stay_visible() {
    let turns = [
        meta("<local-command-caveat>Caveat</local-command-caveat>"),
        meta("<channel source=\"cctg\" chat_id=\"1\">hi from telegram</channel>"),
        say(Some("end_turn"), "hello"),
        prompt("<pasted_content>user text</pasted_content>"),
        say(Some("end_turn"), "seen"),
    ];
    let expected =
        "> hi from telegram\nhello\n\n> <pasted_content>user text</pasted_content>\nseen";
    assert_eq!(render_brief(&turns), expected);
    assert_eq!(render_full(&turns), expected);
}

#[test]
fn channel_attribute_may_contain_a_greater_than_sign() {
    let turns = [
        meta("<channel source=\"cctg\" user=\"a>b\">hi from telegram</channel>"),
        say(Some("end_turn"), "hello"),
    ];
    assert_eq!(render_brief(&turns), "> hi from telegram\nhello");
    assert_eq!(render_full(&turns), "> hi from telegram\nhello");
}

#[test]
fn multiline_prompt_keeps_its_text_verbatim() {
    let turns = [
        prompt("first line\nsecond line"),
        say(Some("end_turn"), "done"),
    ];
    assert_eq!(render_brief(&turns), "> first line\nsecond line\ndone");
    assert_eq!(render_full(&turns), "> first line\nsecond line\ndone");
}

#[test]
fn full_truncates_long_results_safely() {
    let long = "я".repeat(5000);
    let full = render_full(&[prompt("go"), bash("t1", "ls"), result("t1", &long)]);
    assert!(full.contains("… [+3500 chars]"));
    assert!(full.chars().count() < 2000);
}

#[test]
fn slices_render_independently() {
    let turns = parse(FINAL_ANSWER);
    let head = render_brief(&turns[..6]);
    let tail = render_brief(&turns[6..]);
    assert_eq!(format!("{head}\n\n{tail}"), FINAL_ANSWER_BRIEF);
}

fn synthetic(exchanges: usize) -> Vec<Turn> {
    let mut turns = Vec::with_capacity(exchanges * 4);
    for i in 0..exchanges {
        let id = format!("t{i}");
        turns.push(prompt(&format!("prompt {i} with some words 🚀")));
        turns.push(bash(&id, &format!("cargo test -p crate{i}")));
        turns.push(result(&id, &"line of output\n".repeat(20)));
        turns.push(say(Some("end_turn"), &format!("answer {i} ").repeat(10)));
    }
    turns
}

fn best_of_three(turns: &[Turn]) -> Duration {
    (0..3)
        .map(|_| {
            let start = Instant::now();
            let brief = render_brief(turns);
            let full = render_full(turns);
            let chunks = split_for_telegram(&full, SplitOptions::default());
            assert!(!brief.is_empty() && !chunks.chunks.is_empty());
            start.elapsed()
        })
        .min()
        .unwrap_or_default()
}

#[test]
fn five_thousand_turns_render_in_linear_time() {
    let small = synthetic(1250); // 5000 turns
    let large = synthetic(5000); // 20000 turns
    let t_small = best_of_three(&small);
    let t_large = best_of_three(&large);
    assert!(
        t_small < Duration::from_secs(2),
        "5000 turns took {t_small:?}"
    );
    // 4x input: measured ~4.2x linear; an injected O(n) scan per line gave ~12x
    assert!(
        t_large < t_small * 8 + Duration::from_millis(50),
        "{t_small:?} -> {t_large:?}"
    );
}

#[test]
fn last_prompts_keeps_the_last_n_exchanges() {
    let turns = parse(FINAL_ANSWER);
    assert_eq!(
        render_brief(last_prompts(&turns, 1)),
        "> Explore the crate
↳ Explore a0000000000000002: Explore crate
The crate has three modules."
    );
    assert_eq!(render_brief(last_prompts(&turns, 2)), FINAL_ANSWER_BRIEF);
    assert_eq!(render_full(last_prompts(&turns, 99)), FINAL_ANSWER_FULL);
    assert!(last_prompts(&turns, 0).is_empty());
    assert!(last_prompts(&[], 3).is_empty());
}

#[test]
fn last_prompts_counts_only_what_renders_as_a_prompt() {
    let turns = [
        prompt("first"),
        say(Some("end_turn"), "one"),
        prompt("second"),
        bash("t1", "ls"),
        result("t1", "a.txt"),
        prompt(
            "<task-notification>
<task-id>x</task-id>
</task-notification>",
        ),
        meta("<system-reminder>hidden</system-reminder>"),
        say(Some("end_turn"), "two"),
    ];
    // Tool results, service records and hidden meta turns are not boundaries.
    assert_eq!(
        render_brief(last_prompts(&turns, 1)),
        "> second
• Bash: ls
two"
    );
    assert_eq!(last_prompts(&turns, 2).len(), turns.len());

    let channel = [
        prompt("typed"),
        say(Some("end_turn"), "a"),
        meta("<channel source=\"cctg\" chat_id=\"1\">from telegram</channel>"),
        say(Some("end_turn"), "b"),
    ];
    assert_eq!(
        render_brief(last_prompts(&channel, 1)),
        "> from telegram
b"
    );
}
