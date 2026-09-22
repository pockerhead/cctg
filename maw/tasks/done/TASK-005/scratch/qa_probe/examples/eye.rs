fn main() {
    let p = std::env::args().nth(1).unwrap_or_default();
    let src = std::fs::read_to_string(p).unwrap_or_default();
    println!("title={:?}", transcript::ai_title(&src));
    for t in transcript::parse(&src).iter().take(25) {
        for b in &t.blocks {
            let s = match b {
                transcript::Block::Text(s) => format!("Text {}", s.chars().take(70).collect::<String>()),
                transcript::Block::ToolUse { name, .. } => format!("ToolUse {name}"),
                transcript::Block::ToolResult { content, is_error, agent_id, .. } => format!("ToolResult err={is_error} agent={agent_id:?} len={}", content.len()),
            };
            println!("{:?} meta={} side={} | {}", t.role, t.is_meta, t.is_sidechain, s.replace('\n', " "));
        }
    }
}
