// Synthetic edge probes for TASK-020 slash command rendering.
fn user(content: serde_json::Value, meta: bool) -> String {
    serde_json::json!({"type":"user","isMeta":meta,"message":{"role":"user","content":content}}).to_string()
}
fn asst(blocks: serde_json::Value, stop: Option<&str>) -> String {
    serde_json::json!({"type":"assistant","message":{"role":"assistant","content":blocks,"stop_reason":stop}}).to_string()
}
fn show(label: &str, lines: &[String]) {
    let t = transcript::parse(&lines.join("\n"));
    println!("== {label}\nBRIEF: {:?}\nFULL:  {:?}", transcript::render_brief(&t), transcript::render_full(&t));
}
fn main() {
    use serde_json::json;
    show("name-first real indent", &[user(json!("<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args>opus</command-args>"), false)]);
    show("empty args", &[user(json!("<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>"), false)]);
    show("message-first skill", &[user(json!("<command-message>maw-tasks</command-message>\n<command-name>/maw-tasks</command-name>\n<command-args>line1\n\nline2 <b> </command-name> x</command-args>"), false)]);
    show("unclosed args", &[user(json!("<command-name>/model</command-name><command-args>opus"), false)]);
    show("args contain <command-name>", &[user(json!("<command-message>m</command-message>\n<command-name>/m</command-name>\n<command-args>fix <command-name>/evil</command-name> parse</command-args>"), false)]);
    show("array content block", &[user(json!([{"type":"text","text":"<command-name>/model</command-name><command-args>opus</command-args>"}]), false)]);
    show("meta command stays hidden", &[user(json!("<command-name>/model</command-name><command-args>opus</command-args>"), true)]);
    show("ordinary prompt mentioning tag", &[user(json!("please parse <command-name>/x</command-name> here"), false)]);
    show("leading whitespace", &[user(json!("  \n<command-name>/model</command-name><command-args>opus</command-args>"), false)]);
    show("cmd then tool then final", &[
        user(json!("hello"), false),
        asst(json!([{"type":"text","text":"earlier null text"}]), None),
        user(json!("<command-name>/review</command-name><command-args>x</command-args>"), false),
        asst(json!([{"type":"tool_use","id":"t1","name":"Bash","input":{"description":"ls it"}}]), Some("tool_use")),
        user(json!([{"type":"tool_result","tool_use_id":"t1","content":"ok"}]), false),
        asst(json!([{"type":"text","text":"done"}]), Some("end_turn")),
    ]);
    show("answered then cmd at tail", &[user(json!("q"), false), asst(json!([{"type":"text","text":"a"}]), Some("end_turn")), user(json!("<command-name>/clear</command-name><command-args></command-args>"), false)]);
    show("cmd then local stdout at tail", &[user(json!("q"), false), asst(json!([{"type":"text","text":"a"}]), Some("end_turn")), user(json!("<command-name>/model</command-name><command-args>opus</command-args>"), false), user(json!("<local-command-stdout>Set model</local-command-stdout>"), false)]);
    show("message-only", &[user(json!("<command-message>clear</command-message>"), false)]);
    show("other services", &[user(json!("<task-notification>x</task-notification>"), false), user(json!("<bash-input>ls</bash-input>"), false), user(json!("This session is being continued from a previous conversation ..."), false)]);
}
