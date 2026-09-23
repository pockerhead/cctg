import os
here = os.path.dirname(os.path.abspath(__file__))
p = os.path.join(here, 'ws/crates/cctg/src/hub/permissions.rs')
s = open(p, encoding='utf-8').read()


def rep(old, new, count=1):
    global s
    assert s.count(old) == count, (old, s.count(old))
    s = s.replace(old, new)


rep('''    /// Failed this many times; due again on the next retry tick.
    Failed(u32),''', '''    /// Failed; due again on the next retry tick.
    Failed,''')
rep('''    pub waits: bool,
    pub edit: Edit,
}''', '''    pub waits: bool,
    pub edit: Edit,
    /// Final edits that failed so far.
    pub edit_failures: u32,
}''')
rep('''            waits: true,
            edit: Edit::None,
        }''', '''            waits: true,
            edit: Edit::None,
            edit_failures: 0,
        }''')
rep('''        let attempts = match prompt.edit {
            Edit::Failed(before) => before + 1,
            _ => 1,
        };
        prompt.edit = if attempts >= MAX_EDIT_ATTEMPTS {
            Edit::Done
        } else {
            Edit::Failed(attempts)
        };
        attempts''', '''        prompt.edit_failures += 1;
        prompt.edit = if prompt.edit_failures >= MAX_EDIT_ATTEMPTS {
            Edit::Done
        } else {
            Edit::Failed
        };
        prompt.edit_failures''')
rep('''            if matches!(prompt.edit, Edit::Failed(_)) {''', '''            if prompt.edit == Edit::Failed {''')

p2 = os.path.join(here, 'ws/crates/cctg/src/hub/slots.rs')
t = open(p2, encoding='utf-8').read()
old = '''    #[tokio::test]
    async fn a_verdict_never_reaches_another_session_on_the_same_pid() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        rig.agents.send(permission(1, "abcde", "p")).await.unwrap();'''
new = '''    #[tokio::test]
    async fn a_verdict_never_reaches_another_session_on_the_same_pid() {
        let mut rig = rig(Fake::default(), message_options());
        // A hook without a pid: the registry cannot tie pid 10 to A.
        rig.hook(hook(
            A,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: None,
                parent_claude_pid: None,
            },
        ))
        .await;
        rig.ops_after(1).await;
        rig.agent_of(1, A, Some(10)).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        rig.agents.send(permission(1, "abcde", "p")).await.unwrap();'''
assert t.count(old) == 1
t = t.replace(old, new)
old = '''        // An agent of another session reports the same claude pid (a reused
        // pid); its SessionStart has not arrived yet.'''
new = '''        // An agent of another session reports the same claude pid (a reused
        // pid); its SessionStart has not arrived yet, so it waits unbound.'''
assert t.count(old) == 1
t = t.replace(old, new)
open(p, 'w', encoding='utf-8', newline='\n').write(s)
open(p2, 'w', encoding='utf-8', newline='\n').write(t)
print('ok')
