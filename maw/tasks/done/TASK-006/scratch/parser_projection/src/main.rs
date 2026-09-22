use transcript::parse;

fn record(stop_reason: &str) -> String {
    format!(
        r#"{{"type":"assistant","message":{{"id":"msg_same","stop_reason":"{stop_reason}","content":[{{"type":"text","text":"Visible text"}}]}}}}"#
    )
}

fn main() {
    let intermediate = parse(&record("tool_use"));
    let final_text = parse(&record("end_turn"));
    println!("INTERMEDIATE_TURNS={intermediate:?}");
    println!("FINAL_TURNS={final_text:?}");
    println!("PARSED_EQUAL={}", intermediate == final_text);
}
