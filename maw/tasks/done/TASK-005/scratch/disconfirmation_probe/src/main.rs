use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawRecord {
    #[serde(rename = "type")]
    kind: String,
    message: Option<RawMessage>,
    #[serde(rename = "aiTitle")]
    ai_title: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawMessage {
    content: serde_json::Value,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawBlock {
    #[serde(rename = "type")]
    kind: String,
    text: String,
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum VariantBlock {
    #[serde(rename = "text")]
    Text {
        #[serde(default)]
        text: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        #[serde(default)]
        id: String,
    },
    #[serde(other)]
    Ignored,
}

fn main() {
    let input = r#"{"type":"user","message":{"content":"keep me"},"aiTitle":5}"#;
    println!("record: {:?}", serde_json::from_str::<RawRecord>(input));
    let block = r#"{"type":"text","text":"keep me","id":5}"#;
    println!("block: {:?}", serde_json::from_str::<RawBlock>(block));
    println!(
        "variant block: {:?}",
        serde_json::from_str::<VariantBlock>(block)
    );
}
