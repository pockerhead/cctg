import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ed import edit
here = os.path.dirname(os.path.abspath(__file__))
WS = os.path.join(here, '..', 'ws')
P = os.path.join(WS, 'crates/cctg/src/hub/slots.rs')
edit(P, [
# Fake: stream sends that fail.
('''        /// Every reaction is refused (no such reaction in the chat).
        react_error: bool,
    }''', '''        /// Every reaction is refused (no such reaction in the chat).
        react_error: bool,
        /// The next stream messages fail with a 502.
        stream_errors: Mutex<usize>,
    }'''),
('''                Op::React { .. } if self.react_error => error("Bad Request: REACTION_INVALID"),''',
 '''                Op::React { .. } if self.react_error => error("Bad Request: REACTION_INVALID"),
                Op::Stream { .. } if self.take_stream_error() => Err(ApiError::Telegram {
                    code: 502,
                    description: "Bad Gateway".to_owned(),
                }),'''),
# Reader answers from the rig's projects root.
('''            let (to_agent, mut from_hub) = mpsc::channel(16);
            let (kept, kept_rx) = mpsc::unbounded_channel();
            let agents = self.agents.clone();
            tokio::spawn(async move {''', '''            let (to_agent, mut from_hub) = mpsc::channel(16);
            let (kept, kept_rx) = mpsc::unbounded_channel();
            let agents = self.agents.clone();
            let root = self.dir.path().join("projects");
            tokio::spawn(async move {'''),
('''                    let chunk = crate::tail::read_chunk(&session_id, &path, from);''',
 '''                    let chunk = crate::tail::read_chunk(Some(&root), &session_id, &path, from);'''),
# The old hold test: the end of the turn is in the file.
('''        append(&path, &tool_call("t1", "last step"));
        append(&path, &tool_result("t1", None));
        rig.hook(stop(A, Some("done"))).await;
        assert_eq!(
            stream_texts(&rig, 100, 2).await,
            ["• Bash: last step ✓", "done"]
        );''', '''        append(&path, &tool_call("t1", "last step"));
        append(&path, &tool_result("t1", None));
        append(&path, &answer_record("done"));
        rig.hook(stop(A, Some("done"))).await;
        assert_eq!(
            stream_texts(&rig, 100, 2).await,
            ["• Bash: last step ✓", "done"]
        );'''),
('''        slots.registry.dirty = false;
        slots.on_chunk(A, 0, 0, &[], false, false);
        assert!(!slots.registry.dirty, "an empty read wrote the registry");''', '''        slots.registry.dirty = false;
        let empty = Chunk {
            from: 0,
            to: 0,
            lines: &[],
            missing: false,
            more: false,
            reset: false,
        };
        slots.on_chunk(1, A, &empty);
        assert!(!slots.registry.dirty, "an empty read wrote the registry");'''),
])
# helpers + new tests appended at the end of the test module
s = open(P, encoding='utf-8').read()
assert s.endswith('\n}\n')
extra = open(os.path.join(here, 'slots_new_tests.rs'), encoding='utf-8').read()
s = s[:-2] + extra + '}\n'
# Fake helper method
anchor = '''    impl Transport for Fake {'''
assert s.count(anchor) == 1
s = s.replace(anchor, '''    impl Fake {
        fn take_stream_error(&self) -> bool {
            let mut left = self.stream_errors.lock().unwrap();
            let fail = *left > 0;
            *left = left.saturating_sub(1);
            fail
        }
    }

''' + anchor)
open(P, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')
