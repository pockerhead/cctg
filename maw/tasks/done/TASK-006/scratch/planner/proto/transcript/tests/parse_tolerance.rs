use transcript::{Block, Role, ai_title, parse};

const PLAIN_TEXT: &str = include_str!("fixtures/plain_text.jsonl");
const USER_OK: &str = r#"{"type":"user","message":{"content":"first"}}"#;
const ASSISTANT_OK: &str =
    r#"{"type":"assistant","message":{"content":[{"type":"text","text":"second"}]}}"#;

fn texts(src: &str) -> Vec<String> {
    parse(src)
        .into_iter()
        .flat_map(|turn| turn.blocks)
        .map(|block| match block {
            Block::Text(text) => text,
            other => format!("{other:?}"),
        })
        .collect()
}

#[test]
fn empty_and_ignored_only_inputs_give_nothing() {
    for src in [
        "",
        "\n\n",
        "  \t \r\n ",
        "{\"type\":\"mode\"}\n{\"type\":\"summary\"}\n{\"type\":\"cost-state\"}\n",
    ] {
        assert!(parse(src).is_empty());
        assert_eq!(ai_title(src), None);
    }
    let ignored: String = PLAIN_TEXT
        .lines()
        .filter(|line| {
            !line.contains(r#""type":"user""#) && !line.contains(r#""type":"assistant""#)
        })
        .map(|line| format!("{line}\n"))
        .collect();
    assert_eq!(ignored.lines().count(), 8);
    assert!(parse(&ignored).is_empty());
}

#[test]
fn unknown_record_and_bad_lines_keep_neighbours() {
    let src = format!(
        "{USER_OK}\n{{\"type\":\"brand-new-type\",\"x\":1}}\nnot json at all\n{ASSISTANT_OK}\n"
    );
    assert_eq!(texts(&src), ["first", "second"]);
}

#[test]
fn truncated_last_line_keeps_earlier_turns() {
    let src = format!("{PLAIN_TEXT}\n{{\"type\":\"assistant\",\"mess");
    assert_eq!(parse(&src), parse(PLAIN_TEXT));
    assert_eq!(parse(&src).len(), 2);
}

#[test]
fn unknown_and_malformed_blocks_drop_only_themselves() {
    let rec = r#"{"type":"assistant","message":{"content":[{"type":"image","source":{}},{"type":"text","text":5},{"type":"thinking","thinking":"t","signature":"s"},{"text":"no tag"},{"type":"text","text":"keep me","id":5}]}}"#;
    assert_eq!(texts(&format!("{USER_OK}\n{rec}")), ["first", "keep me"]);
}

#[test]
fn irrelevant_wrong_typed_fields_do_not_drop_a_turn() {
    let rec =
        r#"{"type":"user","aiTitle":5,"uuid":7,"timestamp":[],"message":{"content":"still here"}}"#;
    assert_eq!(texts(rec), ["still here"]);
}

#[test]
fn both_content_shapes_for_both_roles() {
    let src = [
        r#"{"type":"user","message":{"content":"u-str"}}"#,
        r#"{"type":"user","message":{"content":[{"type":"text","text":"u-arr"}]}}"#,
        r#"{"type":"assistant","message":{"content":"a-str"}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"a-arr"}]}}"#,
    ]
    .join("\n");
    let roles: Vec<Role> = parse(&src).iter().map(|turn| turn.role).collect();
    assert_eq!(
        roles,
        [Role::User, Role::User, Role::Assistant, Role::Assistant]
    );
    assert_eq!(texts(&src), ["u-str", "u-arr", "a-str", "a-arr"]);
}

#[test]
fn unusable_message_shapes_are_skipped() {
    for rec in [
        r#"{"type":"user"}"#,
        r#"{"type":"user","message":null}"#,
        r#"{"type":"user","message":5}"#,
        r#"{"type":"user","message":{"content":null}}"#,
        r#"{"type":"user","message":{"content":5}}"#,
        r#"{"type":"user","message":{"content":[]}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"t"}]}}"#,
    ] {
        assert!(parse(rec).is_empty(), "{rec}");
    }
}

#[test]
fn known_block_with_missing_fields_keeps_defaults() {
    let rec = r#"{"type":"assistant","message":{"content":[{"type":"text"}]}}"#;
    assert_eq!(texts(rec), [""]);
}

#[test]
fn flags_are_tolerant() {
    let flags = |rec: &str| {
        parse(rec)
            .iter()
            .map(|turn| (turn.is_meta, turn.is_sidechain))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        flags(r#"{"type":"user","isMeta":true,"isSidechain":true,"message":{"content":"x"}}"#),
        [(true, true)]
    );
    assert_eq!(
        flags(r#"{"type":"user","isMeta":false,"message":{"content":"x"}}"#),
        [(false, false)]
    );
    assert_eq!(
        flags(r#"{"type":"user","isMeta":"true","isSidechain":1,"message":{"content":"x"}}"#),
        [(false, false)]
    );
}

#[test]
fn tool_result_shapes() {
    let result = |content: &str, extra: &str| {
        let rec = format!(
            r#"{{"type":"user"{extra},"message":{{"content":[{{"type":"tool_result","tool_use_id":"t1","content":{content}}}]}}}}"#
        );
        parse(&rec)
            .into_iter()
            .flat_map(|turn| turn.blocks)
            .collect::<Vec<_>>()
    };
    let expect = |content: &str, is_error: bool, agent_id: Option<&str>| {
        vec![Block::ToolResult {
            tool_use_id: "t1".into(),
            content: content.into(),
            is_error,
            agent_id: agent_id.map(Into::into),
        }]
    };
    assert_eq!(result(r#""plain""#, ""), expect("plain", false, None));
    assert_eq!(
        result(
            r#"[{"type":"text","text":"a"},{"type":"tool_reference","tool_name":"x"},{"type":"text","text":"b"}]"#,
            ""
        ),
        expect("a\nb", false, None)
    );
    assert_eq!(result("{}", ""), expect("", false, None));
    assert_eq!(
        result(r#""e","is_error":"yes""#, ""),
        expect("e", false, None)
    );
    assert_eq!(
        result(r#""e","is_error":true"#, ""),
        expect("e", true, None)
    );
    assert_eq!(
        result(r#""r""#, r#","toolUseResult":"Error: text""#),
        expect("r", false, None)
    );
    assert_eq!(
        result(r#""r""#, r#","toolUseResult":{"agentId":5}"#),
        expect("r", false, None)
    );
    assert_eq!(
        result(r#""r""#, r#","toolUseResult":{"agentId":"a1"}"#),
        expect("r", false, Some("a1"))
    );
}

#[test]
fn crlf_matches_lf() {
    let lf = PLAIN_TEXT.replace("\r\n", "\n");
    let crlf = lf.replace('\n', "\r\n");
    assert_eq!(parse(&crlf), parse(&lf));
    assert_eq!(parse(&lf).len(), 2);
}

#[test]
fn ai_title_rules() {
    let src = [
        r#"{"type":"ai-title","aiTitle":""}"#,
        r#"{"type":"ai-title","aiTitle":5}"#,
        r#"{"type":"ai-title"}"#,
        r#"{"type":"user","aiTitle":"not a title record","message":{"content":"x"}}"#,
        r#"{"type":"ai-title","aiTitle":"Real title"}"#,
        r#"{"type":"ai-title","aiTitle":"Later"}"#,
    ]
    .join("\n");
    assert_eq!(ai_title(&src).as_deref(), Some("Real title"));
    assert!(parse(r#"{"type":"ai-title","aiTitle":"T"}"#).is_empty());
}

#[test]
fn deep_nesting_is_skipped_without_panic() {
    for depth in [200, 100_000] {
        let deep = format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"x","name":"n","input":{}{}}}]}}}}"#,
            "[".repeat(depth),
            "]".repeat(depth)
        );
        let src = format!("{USER_OK}\n{deep}\n{ASSISTANT_OK}");
        assert_eq!(texts(&src), ["first", "second"]);
    }
}

#[test]
fn garbage_never_panics() {
    let bytes: Vec<u8> = (0..=255).collect();
    let lossy = String::from_utf8_lossy(&bytes).into_owned();
    let big = "x".repeat(1 << 20);
    let brackets = "[".repeat(100_000);
    let garbage: Vec<&str> = vec![
        "{",
        "}",
        "null",
        "[]",
        "\"str\"",
        "42",
        "{\"type\":\"user\"}",
        "{\"type\":\"user\",\"message\":{\"content\":[1,null,{\"type\":7}]}}",
        "\u{feff}{\"type\":\"user\",\"message\":{\"content\":\"x\"}}",
        "\u{0}\u{1}\u{fffd}",
        "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\"}]}}",
        "[\"user\",{\"content\":\"positional\"}]",
        &brackets,
        &big,
        &lossy,
    ];
    for src in &garbage {
        let _ = parse(src);
        let _ = ai_title(src);
    }
    let all = garbage.join("\n");
    let _ = parse(&all);
    let _ = ai_title(&all);
}

#[test]
fn stop_reason_is_tolerant() {
    let stop = |message: &str| {
        let line = format!(r#"{{"type":"assistant","message":{{"content":"x"{message}}}}}"#);
        let turns = parse(&line);
        assert_eq!(turns.len(), 1, "{line}");
        turns[0].stop_reason.clone()
    };
    assert_eq!(
        stop(r#","stop_reason":"end_turn""#).as_deref(),
        Some("end_turn")
    );
    assert_eq!(
        stop(r#","stop_reason":"tool_use""#).as_deref(),
        Some("tool_use")
    );
    assert_eq!(stop(""), None);
    assert_eq!(stop(r#","stop_reason":null"#), None);
    assert_eq!(stop(r#","stop_reason":7"#), None);
    assert_eq!(stop(r#","stop_reason":{"a":1}"#), None);
}
