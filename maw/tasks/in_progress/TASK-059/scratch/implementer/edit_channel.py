# TASK-059 edits of crates/cctg/src/channel.rs (run once from the repo root).
p = 'crates/cctg/src/channel.rs'
s = open(p, encoding='utf-8').read()


def rep(old, new, count=1):
    global s
    assert s.count(old) == count, (s.count(old), old[:70])
    s = s.replace(old, new)


rep('''/// A `send_file` call waiting for the agent loop; its answer is
/// [`tool_answer`] for `id`.
#[derive(Debug, Clone, PartialEq)]
pub struct FileCall {
    pub id: Value,
    pub path: String,
    pub caption: Option<String>,
}''', '''/// A `send_file` call waiting for the agent loop; its answer is
/// [`tool_answer`] for `id`. `paths`: one file (`path`), or 2 to
/// [`MAX_ALBUM`] of them (`paths`, TASK-059).
#[derive(Debug, Clone, PartialEq)]
pub struct FileCall {
    pub id: Value,
    pub paths: Vec<String>,
    pub caption: Option<String>,
}''')
rep('''use crate::wire::{AgentMsg, Behavior, HubMsg, PermissionRequest};''',
    '''use crate::wire::{AgentMsg, Behavior, HubMsg, MAX_ALBUM, PermissionRequest};''')
rep('''    fn send_file(&mut self, id: &Value, arguments: Option<&Value>) -> Option<Vec<u8>> {
        let answer = |text: &str| Some(tool_answer(id, text, true));
        let path = arguments
            .and_then(|arguments| arguments.get("path"))
            .and_then(Value::as_str)
            .filter(|path| !path.trim().is_empty());
        let Some(path) = path else {
            return answer("send_file needs a non-empty `path` string");
        };''', '''    fn send_file(&mut self, id: &Value, arguments: Option<&Value>) -> Option<Vec<u8>> {
        let answer = |text: &str| Some(tool_answer(id, text, true));
        let named = |value: &Value| {
            value
                .as_str()
                .filter(|path| !path.trim().is_empty())
                .map(str::to_owned)
        };
        let field = |name: &str| {
            arguments
                .and_then(|arguments| arguments.get(name))
                .filter(|value| !value.is_null())
        };
        let paths = match (field("path"), field("paths")) {
            (Some(_), Some(_)) => return answer("send_file takes `path` or `paths`, not both"),
            (None, Some(list)) => match list.as_array().map(|list| {
                list.iter().map(named).collect::<Option<Vec<String>>>()
            }) {
                Some(Some(paths)) if (2..=MAX_ALBUM).contains(&paths.len()) => paths,
                _ => return answer(BAD_PATHS),
            },
            (path, None) => match path.and_then(named) {
                Some(path) => vec![path],
                None => return answer("send_file needs a non-empty `path` string"),
            },
        };''')
rep('''        self.file_calls.push(FileCall {
            id: id.clone(),
            path: path.to_owned(),
            caption,
        });''', '''        self.file_calls.push(FileCall {
            id: id.clone(),
            paths,
            caption,
        });''')
rep('''fn send_file_tool() -> Value {
    json!({''', '''const BAD_PATHS: &str =
    "`paths` must be a list of 2 to 10 non-empty path strings; for one file use `path`";

/// Exactly one of `path` and `paths` is checked by [`Server::send_file`]:
/// the Messages API takes no `oneOf` at the top of a tool's input schema.
fn send_file_tool() -> Value {
    json!({''')
rep('''            (a screenshot, a report, a build artifact), not for text: what you write reaches \\
            the topic anyway. Answers once Telegram took the file.",''', '''            (a screenshot, a report, a build artifact), not for text: what you write reaches \\
            the topic anyway. Give either `path` (one file) or `paths` (2 to 10 files, sent \\
            together as one album: pictures as a photo album, the rest as a document album, \\
            pictures first). Answers once Telegram took the files, saying what went and what \\
            did not.",''')
rep('''                "path": {
                    "type": "string",
                    "description": "The file: an absolute path, or one relative to the session's working folder.",
                },''', '''                "path": {
                    "type": "string",
                    "description": "One file: an absolute path, or one relative to the session's working folder. Leave it out when you give `paths`.",
                },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 2,
                    "maxItems": MAX_ALBUM,
                    "description": "Several files, 2 to 10, each like `path`, sent as one album in this order. Leave it out when you give `path`.",
                },''')
rep('''Without it the file name is shown.",''', '''Without it the file name is shown. With `paths` it goes on the first file of the album.",''')
rep('''            "required": ["path"],
            "additionalProperties": false,
        },
    })
}

fn reply_tool''', '''            "additionalProperties": false,
        },
    })
}

fn reply_tool''')
# tests
rep('''                FileCall {
                    id: json!(4),
                    path: "C:/x/shot.png".into(),''', '''                FileCall {
                    id: json!(4),
                    paths: vec!["C:/x/shot.png".into()],''')
rep('''                FileCall {
                    id: json!(5),
                    path: "rel.txt".into(),''', '''                FileCall {
                    id: json!(5),
                    paths: vec!["rel.txt".into()],''')
rep('''        assert_eq!(tools[1]["inputSchema"]["required"], json!(["path"]));''', '''        // `path` or `paths` (TASK-059): the server checks that one is given.
        let schema = &tools[1]["inputSchema"];
        assert!(schema.get("required").is_none() && schema.get("oneOf").is_none());
        assert_eq!(schema["properties"]["path"]["type"], "string");
        assert_eq!(
            (
                &schema["properties"]["paths"]["type"],
                &schema["properties"]["paths"]["minItems"],
                &schema["properties"]["paths"]["maxItems"]
            ),
            (&json!("array"), &json!(2), &json!(10))
        );''')
rep('''        assert!(description.contains("as a photo") && description.contains("50 MB"));''', '''        assert!(description.contains("as a photo") && description.contains("50 MB"));
        assert!(description.contains("`paths`") && description.contains("album"));''')
rep('''            json!({ "path": "a", "caption": 1 }),
        ] {''', '''            json!({ "path": "a", "caption": 1 }),
            json!({ "paths": ["a"] }),
            json!({ "paths": ["a", " "] }),
            json!({ "paths": ["a", 2] }),
            json!({ "paths": "a" }),
            json!({ "paths": vec!["a"; 11] }),
            json!({ "path": "a", "paths": ["b", "c"] }),
        ] {''')
rep('''        assert!(server.take_file_calls().is_empty());
        assert!(rx.try_recv().is_err(), "nothing reached the hub");''', '''        assert!(server.take_file_calls().is_empty());
        assert!(rx.try_recv().is_err(), "nothing reached the hub");
        // Several files (TASK-059); a null `path` counts as none.
        let album = call(
            9,
            json!({ "path": null, "paths": ["a.png", "b.txt"], "caption": "two" }),
        );
        assert!(server.on_line(album.as_bytes()).is_empty());
        assert_eq!(
            server.take_file_calls(),
            [FileCall {
                id: json!(9),
                paths: vec!["a.png".into(), "b.txt".into()],
                caption: Some("two".into()),
            }]
        );''')
open(p, 'w', encoding='utf-8', newline='').write(s)
print('ok')
