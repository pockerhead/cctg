#!/usr/bin/env python3
# Adds a slots test: the /clear hooks reach the hub out of order (new
# SessionStart before the old SessionEnd). Run once.
import os

p = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'ws', 'crates', 'cctg', 'src', 'hub', 'slots.rs')
s = open(p, encoding='utf-8').read()
anchor = '''    #[tokio::test]
    async fn one_slot_lives_through_hook_agent_end_and_the_next_session() {'''
assert s.count(anchor) == 1
test = '''    /// The recorded ops, once they satisfy `ready`.
    async fn settled(rig: &Rig, ready: impl Fn(&[Op]) -> bool) -> Vec<Op> {
        let reached = async {
            loop {
                let ops = rig.fake.ops();
                if ready(&ops) {
                    return ops;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::time::timeout(WAIT, reached).await.expect("ops in time")
    }

    fn last_icon(ops: &[Op], thread: i64) -> Option<&str> {
        ops.iter().rev().find_map(|op| match op {
            Op::EditTopic {
                thread_id,
                icon_custom_emoji_id: Some(icon),
                ..
            } if *thread_id == thread => Some(icon.as_str()),
            _ => None,
        })
    }

    #[tokio::test]
    async fn the_agent_follows_its_claude_process_when_the_new_start_comes_first() {
        let mut rig = rig(Fake::default(), options());
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.agent_of(1, A, Some(10)).await;
        rig.ops_after(2).await;

        // `/clear` hooks run as separate processes: the new start can win.
        rig.hook(hook(
            B,
            HookEvent::SessionStart {
                source: Some("clear".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
        ))
        .await;
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: Some("clear".into()),
                claude_pid: Some(10),
            },
        ))
        .await;
        // B's topic is the last one created; its agent is the old process's.
        let alive_b = |ops: &[Op]| {
            let topic = 99 + count(ops, is_create) as i64;
            topic > 100 && last_icon(ops, topic) == Some(ICON_ALIVE)
        };
        let ops = settled(&rig, alive_b).await;
        let topic_b = 99 + count(&ops, is_create) as i64;

        // The late end of A must not take the pid away from B: a reconnect
        // with the stale env id still lands on B.
        rig.agents
            .send(AgentEvent::Disconnected { conn: 1 })
            .await
            .unwrap();
        settled(&rig, |ops| last_icon(ops, topic_b) != Some(ICON_ALIVE)).await;
        rig.agent_of(2, A, Some(10)).await;
        let ops = settled(&rig, |ops| last_icon(ops, topic_b) == Some(ICON_ALIVE)).await;
        assert_eq!(count(&ops, is_create), 2, "{ops:?}");
    }

'''
s = s.replace(anchor, test + anchor)
open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('added')
