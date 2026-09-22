use transcript::{
    Subagent, SubagentBody, SubagentInput, SubagentMeta, parse, parse_subagent_meta, render_brief,
    render_brief_with_subagents, render_full, render_full_with_subagents,
};

const IN_PROGRESS_MARKER: &str = "в работе…";

// parent: `Agent` toolu_demo20 -> a0000000000000001 (Explore, "Explore crate"); subagent: SIDECHAIN
const TOOL_USE_RESULT: &str = include_str!("fixtures/tool_use_result.jsonl");
const SIDECHAIN: &str = include_str!("fixtures/sidechain.jsonl");
// parent: `Agent` toolu_demo32 -> a0000000000000002; subagent: SUBAGENT (+ META)
const FINAL_ANSWER: &str = include_str!("fixtures/final_answer.jsonl");
const SUBAGENT: &str = include_str!("fixtures/subagent_handback.jsonl");
const META: &str = include_str!("fixtures/subagent_handback.meta.json");

const REPORT: &str = "Modules: lib, render, split.";
const SUBAGENT_BRIEF: &str = "• Bash: List source files\n• SubagentHandback\nReport handed back.";

fn side(transcript: &str) -> Subagent {
    Subagent::new(SubagentInput {
        agent_id: "a0000000000000001",
        agent_type: Some("Explore"),
        transcript: Some(transcript),
        ..SubagentInput::default()
    })
}

fn explore(report: Option<&str>) -> Subagent {
    Subagent::new(SubagentInput {
        agent_id: "a0000000000000002",
        agent_type: Some("general-purpose"),
        meta: Some(META),
        report,
        transcript: Some(SUBAGENT),
        last_assistant_message: Some("Report handed back."),
    })
}

/// The first `lines` records of the subagent fixture: a transcript that lags the subagent.
fn head(lines: usize) -> String {
    SUBAGENT
        .lines()
        .take(lines)
        .map(|line| format!("{line}\n"))
        .collect()
}

fn body(
    report: Option<&str>,
    transcript: Option<&str>,
    last_assistant_message: Option<&str>,
) -> SubagentBody {
    Subagent::new(SubagentInput {
        agent_id: "a0000000000000002",
        report,
        transcript,
        last_assistant_message,
        ..SubagentInput::default()
    })
    .body()
    .clone()
}

#[test]
fn sidechain_fixture_is_one_block_outside_the_parent_turns() {
    let parent = parse(TOOL_USE_RESULT);
    assert!(parent.iter().all(|turn| !turn.is_sidechain));
    let block = side(SIDECHAIN);
    assert_eq!(block.agent_id(), "a0000000000000001");
    assert_eq!(
        block.body(),
        &SubagentBody::Transcript("Modules: lib, parse.".to_owned())
    );
    assert_eq!(
        block.render(),
        "↳ Explore a0000000000000001\nModules: lib, parse."
    );
    let brief = render_brief_with_subagents(&parent, &[block]);
    assert_eq!(
        brief,
        format!(
            "• Bash: Run tests\n• Read: C:\\work\\demo\\src\\lib.rs\n• Bash: Fail on purpose\n\
             ↳ Explore a0000000000000001: Explore crate\n  Modules: lib, parse.\n{IN_PROGRESS_MARKER}"
        )
    );
    // the spawn prompt belongs to the parent's `Agent` input, not to the block
    assert!(!brief.contains("List the modules of the crate."));
    assert_eq!(brief.matches("↳ ").count(), 1);
    // no subagents: the parent renders exactly as before
    assert_eq!(
        render_brief_with_subagents(&parent, &[]),
        render_brief(&parent)
    );
    assert_eq!(
        render_full_with_subagents(&parent, &[]),
        render_full(&parent)
    );
}

#[test]
fn meta_fixture_is_parsed() {
    assert_eq!(
        parse_subagent_meta(META),
        SubagentMeta {
            agent_type: Some("Explore".to_owned()),
            description: Some("Explore crate".to_owned()),
        }
    );
    assert_eq!(
        parse_subagent_meta(&format!("\u{feff}{META}")),
        parse_subagent_meta(META)
    );
}

#[test]
fn meta_description_and_type_win() {
    // the hook said general-purpose; the meta file says Explore
    assert_eq!(
        explore(None).render(),
        format!("↳ Explore a0000000000000002: Explore crate\n{SUBAGENT_BRIEF}")
    );
    let noisy = Subagent::new(SubagentInput {
        agent_id: "a1",
        meta: Some(r#"{"agentType":" Explore\n","description":"line one\n  line two"}"#),
        ..SubagentInput::default()
    });
    assert_eq!(noisy.render(), "↳ Explore a1: line one line two");
}

#[test]
fn missing_or_broken_meta_falls_back_without_error() {
    for broken in [
        "",
        "{",
        "null",
        "[]",
        "\"Explore\"",
        r#"{"agentType":7,"description":null}"#,
        r#"{"agentType":"  ","description":""}"#,
        "not json at all",
    ] {
        assert_eq!(
            parse_subagent_meta(broken),
            SubagentMeta::default(),
            "{broken}"
        );
        for meta in [Some(broken), None] {
            let block = Subagent::new(SubagentInput {
                agent_id: "a1",
                agent_type: Some("Explore"),
                meta,
                ..SubagentInput::default()
            });
            assert_eq!(block.render(), "↳ Explore a1", "{broken}");
        }
    }
    let unknown = Subagent::new(SubagentInput {
        agent_id: "a1",
        agent_type: Some(" "),
        ..SubagentInput::default()
    });
    assert_eq!(unknown.render(), "↳ agent a1");
}

#[test]
fn report_replaces_the_transcript_final_text() {
    let block = explore(Some(REPORT));
    assert_eq!(block.body(), &SubagentBody::Report(REPORT.to_owned()));
    assert_eq!(
        block.render(),
        format!("↳ Explore a0000000000000002: Explore crate\n{REPORT}")
    );
    assert!(!block.render().contains("Report handed back."));
    // surrounding whitespace is trimmed; a blank report is no report
    assert_eq!(
        body(Some(" \n done \n"), None, None),
        SubagentBody::Report("done".to_owned())
    );
    assert_eq!(
        body(Some(" \n "), Some(SUBAGENT), None),
        SubagentBody::Transcript(SUBAGENT_BRIEF.to_owned())
    );
}

#[test]
fn body_is_brief_even_in_a_full_parent() {
    let parent = parse(FINAL_ANSWER);
    let block = explore(None);
    let full = render_full_with_subagents(&parent, std::slice::from_ref(&block));
    let indented = SUBAGENT_BRIEF
        .lines()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        full.contains(&format!(
            "↳ Explore a0000000000000002: Explore crate\n{indented}\n  {{\"description\""
        )),
        "{full}"
    );
    // full-only parts of the subagent transcript never appear: tool inputs, results, thinking
    for hidden in [
        "ls crates/transcript/src",
        "render.rs\nsplit.rs",
        "Report delivered to the caller.",
        "SECRET-THINKING-MARKER",
        "SECRET-SIGNATURE-MARKER",
        "Your final report is delivered",
    ] {
        assert!(!full.contains(hidden), "{hidden}");
        assert!(!block.render().contains(hidden), "{hidden}");
    }
    // the rest of the parent full view is unchanged
    assert_eq!(
        full.replace(&format!("{indented}\n"), ""),
        render_full(&parent)
    );
    let brief = render_brief_with_subagents(&parent, &[explore(Some(REPORT))]);
    assert!(brief.contains(&format!(
        "↳ Explore a0000000000000002: Explore crate\n  {REPORT}\nThe crate has three modules."
    )));
}

/// (report, transcript, last_assistant_message, expected body)
type Case<'a> = (
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
    SubagentBody,
);

#[test]
fn body_source_order_is_fixed() {
    let finished = Some(SUBAGENT);
    let behind = head(6); // ends on the Bash tool result: unfinished
    let prompt_only = head(2);
    let last = Some("Report handed back.");
    let other = Some("A newer final answer.");
    let cases: [Case; 11] = [
        (
            Some(REPORT),
            finished,
            last,
            SubagentBody::Report(REPORT.to_owned()),
        ),
        (
            Some(REPORT),
            None,
            None,
            SubagentBody::Report(REPORT.to_owned()),
        ),
        (
            None,
            finished,
            last,
            SubagentBody::Transcript(SUBAGENT_BRIEF.to_owned()),
        ),
        (
            None,
            finished,
            None,
            SubagentBody::Transcript(SUBAGENT_BRIEF.to_owned()),
        ),
        // the file lags: it finished on an earlier answer than the hook reports
        (
            None,
            finished,
            other,
            SubagentBody::LastMessage("A newer final answer.".to_owned()),
        ),
        (
            None,
            Some(&behind),
            last,
            SubagentBody::LastMessage("Report handed back.".to_owned()),
        ),
        (
            None,
            None,
            last,
            SubagentBody::LastMessage("Report handed back.".to_owned()),
        ),
        (
            None,
            Some(&behind),
            None,
            SubagentBody::InProgress(format!("• Bash: List source files\n{IN_PROGRESS_MARKER}")),
        ),
        (
            None,
            Some(&prompt_only),
            None,
            SubagentBody::InProgress(IN_PROGRESS_MARKER.to_owned()),
        ),
        (None, Some(""), None, SubagentBody::Empty),
        (None, Some("not json\n{"), Some("  "), SubagentBody::Empty),
    ];
    for (report, transcript, last, expected) in cases {
        assert_eq!(
            body(report, transcript, last),
            expected,
            "{report:?} {transcript:?} {last:?}"
        );
    }
    assert_eq!(body(None, None, None), SubagentBody::Empty);
    let empty = Subagent::new(SubagentInput {
        agent_id: "a1",
        ..SubagentInput::default()
    });
    assert_eq!(empty.body().text(), "");
    assert_eq!(empty.render(), "↳ agent a1");
}

#[test]
fn only_known_agents_get_a_body() {
    let parent = parse(FINAL_ANSWER);
    let stranger = Subagent::new(SubagentInput {
        agent_id: "a0000000000000009",
        report: Some(REPORT),
        ..SubagentInput::default()
    });
    assert_eq!(
        render_brief_with_subagents(&parent, &[stranger]),
        render_brief(&parent)
    );
    let empty = Subagent::new(SubagentInput {
        agent_id: "a0000000000000002",
        ..SubagentInput::default()
    });
    assert_eq!(
        render_full_with_subagents(&parent, &[empty]),
        render_full(&parent)
    );
}
