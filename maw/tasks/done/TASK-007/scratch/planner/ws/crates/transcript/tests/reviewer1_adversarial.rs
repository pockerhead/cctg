use transcript::{Subagent, SubagentBody, SubagentInput, parse, render_brief_with_subagents};

const PARENT: &str = include_str!("fixtures/tool_use_result.jsonl");
const SIDECHAIN: &str = include_str!("fixtures/sidechain.jsonl");

fn block(input: SubagentInput<'_>) -> Subagent {
    Subagent::new(SubagentInput {
        agent_id: "a0000000000000001",
        agent_type: Some("Explore"),
        ..input
    })
}

#[test]
fn last_message_must_match_a_complete_rendered_line() {
    let transcript = concat!(
        r#"{"type":"user","message":{"content":"spawn"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"not done"}]}}"#,
        "\n",
    );
    let subagent = block(SubagentInput {
        transcript: Some(transcript),
        last_assistant_message: Some("done"),
        ..SubagentInput::default()
    });
    assert_eq!(
        subagent.body(),
        &SubagentBody::LastMessage("done".to_owned())
    );
}

#[test]
fn sidechain_turns_do_not_leak_when_parent_input_is_contaminated() {
    let mut turns = parse(PARENT);
    turns.extend(parse(SIDECHAIN));
    let rendered = render_brief_with_subagents(
        &turns,
        &[block(SubagentInput {
            transcript: Some(SIDECHAIN),
            ..SubagentInput::default()
        })],
    );
    assert!(
        !rendered.contains("List the modules of the crate."),
        "{rendered}"
    );
    assert_eq!(
        rendered.matches("Modules: lib, parse.").count(),
        1,
        "{rendered}"
    );
}

#[test]
fn embedded_header_uses_meta_values() {
    let parent = parse(PARENT);
    let rendered = render_brief_with_subagents(
        &parent,
        &[block(SubagentInput {
            meta: Some(r#"{"agentType":"Plan","description":"From meta"}"#),
            report: Some("body"),
            ..SubagentInput::default()
        })],
    );
    assert!(
        rendered.contains("↳ Plan a0000000000000001: From meta\n  body"),
        "{rendered}"
    );
    assert!(!rendered.contains("↳ Explore a0000000000000001: Explore crate"));
}
