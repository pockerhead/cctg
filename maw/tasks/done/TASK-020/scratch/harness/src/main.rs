fn main() {
    let cases = [
        "<command-message>maw-context</command-message>\n<command-name>/maw-context</command-name>\n<command-args>--review</command-args>",
        "<command-name>/model</command-name>\n<command-message>model</command-message>",
        "<command-name>/x</command-name><command-message>x</command-message><command-args>a </command-args> b</command-args>",
    ];
    for c in cases {
        let content = serde_json::Value::String(c.to_owned());
        let line = format!(
            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":{content}}}}}\n"
        );
        let t = transcript::parse(&line);
        println!(
            "brief={:?} | full={:?}",
            transcript::render_brief(&t),
            transcript::render_full(&t)
        );
    }
}
