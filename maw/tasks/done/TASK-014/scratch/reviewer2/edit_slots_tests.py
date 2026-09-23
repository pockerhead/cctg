import os
import re
here = os.path.dirname(os.path.abspath(__file__))
p = os.path.join(here, 'ws/crates/cctg/src/hub/slots.rs')
s = open(p, encoding='utf-8').read()


def cut_test(name):
    """Removes one `#[tokio::test] async fn <name>` with its body."""
    global s
    start = s.index('    #[tokio::test]\n    async fn %s()' % name)
    end = s.index('\n    }\n', start) + len('\n    }\n')
    if s[end:end + 1] == '\n':
        end += 1
    s = s[:start] + s[end:]


def rep(old, new, count=1):
    global s
    assert s.count(old) == count, (old, s.count(old))
    s = s.replace(old, new)


# Pinned the opposite of the orchestrator decision (prompt kept after its
# SessionEnd): replaced by the three SessionEnd tests.
cut_test('a_prompt_goes_to_the_slot_of_its_session_after_the_slot_moved_on')
# Drove the removed `decide`; the away/return path is
# `the_first_press_stays_the_answer_while_the_agent_is_away`.
cut_test('a_press_while_the_agent_is_away_waits_for_its_process_to_return')

rep('''        rig.agent_acking(2, A, Some(10)).await;
        assert_eq!(
            verdicts(&received(&mut rig, 1).await),
            [("abcde".to_owned(), Behavior::Allow)],
            "the same verdict again"
        );
    }''', '''        rig.agent_acking(2, A, Some(10)).await;
        let again = received(&mut rig, 1).await;
        assert_eq!(
            verdicts(&again),
            [("abcde".to_owned(), Behavior::Allow)],
            "the same verdict again"
        );
        let [HubMsg::PermissionVerdict {
            verdict_id: Some(verdict_id),
            ..
        }] = again.as_slice()
        else {
            panic!("{again:?}");
        };
        assert!(
            edits_of(&rig.fake.ops(), message_id).is_empty(),
            "not decided before the ack"
        );
        rig.agents
            .send(AgentEvent::Message {
                conn: 2,
                msg: AgentMsg::PermissionAck {
                    verdict_id: *verdict_id,
                },
            })
            .await
            .unwrap();
        settled(&rig, |ops| edits_of(ops, message_id).len() == 1).await;
    }

    #[tokio::test]
    async fn an_acking_agent_decides_a_prompt_only_with_its_own_ack() {
        let options = Options {
            retry_every: Duration::from_millis(100),
            ..message_options()
        };
        let mut rig = rig(Fake::default(), options);
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.agent_of(1, A, Some(10)).await;
        rig.hook(start(B, 11)).await;
        settled(&rig, |ops| count(ops, is_create) == 2).await;
        rig.agent_acking(2, B, Some(11)).await;
        settled(&rig, |ops| last_icon(ops, 101) == Some(ICON_ALIVE)).await;
        rig.agents.send(permission(2, "abcde", "p")).await.unwrap();
        let ops = settled(&rig, |ops| {
            prompts(ops).len() == 1 && last_icon(ops, 101) == Some(crate::hub::registry::ICON_WAITING)
        })
        .await;
        let (_, text, message_id) = prompts(&ops).remove(0);
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.control
            .send(press("q1", Some(message_id), "deny:abcde"))
            .unwrap();
        // Unacked, it goes again on every tick with the same id.
        tokio::time::sleep(Duration::from_millis(350)).await;
        let got = received(&mut rig, 1).await;
        let ids: HashSet<Option<u64>> = got
            .iter()
            .map(|msg| match msg {
                HubMsg::PermissionVerdict { verdict_id, .. } => *verdict_id,
                other => panic!("{other:?}"),
            })
            .collect();
        assert!(got.len() >= 2, "{got:?}");
        assert_eq!(ids.len(), 1, "{got:?}");
        let Some(Some(verdict_id)) = ids.into_iter().next() else {
            panic!("an acking agent gets an id");
        };
        assert!(edits_of(&rig.fake.ops(), message_id).is_empty());
        assert_eq!(
            last_icon(&rig.fake.ops(), 101),
            Some(crate::hub::registry::ICON_WAITING)
        );
        // An ack from an agent of another session decides nothing.
        rig.agents
            .send(AgentEvent::Message {
                conn: 1,
                msg: AgentMsg::PermissionAck { verdict_id },
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(edits_of(&rig.fake.ops(), message_id).is_empty());
        rig.agents
            .send(AgentEvent::Message {
                conn: 2,
                msg: AgentMsg::PermissionAck { verdict_id },
            })
            .await
            .unwrap();
        let ops = settled(&rig, |ops| {
            edits_of(ops, message_id).len() == 1 && last_icon(ops, 101) == Some(ICON_ALIVE)
        })
        .await;
        assert_eq!(
            edits_of(&ops, message_id),
            [(
                format!("{text}{}", permissions::DENIED_MARK),
                Some(permissions::no_keyboard())
            )]
        );
        // Decided: nothing goes again, a press only hears "already decided".
        let _ = received(&mut rig, 1).await;
        rig.control
            .send(press("q2", Some(message_id), "allow:abcde"))
            .unwrap();
        let ops = settled(&rig, |ops| answers(ops).len() == 2).await;
        assert_eq!(
            answers(&ops),
            [
                Some(permissions::ANSWER_DENIED),
                Some(permissions::ANSWER_DECIDED)
            ]
        );
        assert!(received(&mut rig, 1).await.is_empty());
        assert!(verdicts(&received(&mut rig, 0).await).is_empty());
    }

    #[tokio::test]
    async fn a_nested_resume_ending_leaves_the_prompt_open() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        rig.agents.send(permission(2, "abcde", "p")).await.unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (_, _, message_id) = prompts(&ops).remove(0);
        tokio::time::sleep(Duration::from_millis(100)).await;
        // The end of a nested `claude -p --resume` of B: another pid.
        rig.hook(hook(
            B,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(99),
            },
        ))
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        rig.control
            .send(press("q", Some(message_id), "allow:abcde"))
            .unwrap();
        settled(&rig, |ops| answers(ops).len() == 1).await;
        assert_eq!(
            verdicts(&received(&mut rig, 1).await),
            [("abcde".to_owned(), Behavior::Allow)]
        );
    }

    #[tokio::test]
    async fn a_turn_boundary_stops_an_unanswered_prompt_from_holding_the_icon() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        // Answered in the terminal: the hub only sees the turn end.
        rig.agents.send(permission(2, "abcde", "1")).await.unwrap();
        settled(&rig, |ops| {
            last_icon(ops, 101) == Some(crate::hub::registry::ICON_WAITING)
        })
        .await;
        rig.hook(hook(
            B,
            HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: None,
            },
        ))
        .await;
        settled(&rig, |ops| last_icon(ops, 101) == Some(ICON_ALIVE)).await;
        rig.agents.send(permission(2, "bcdef", "2")).await.unwrap();
        let ops = settled(&rig, |ops| {
            prompts(ops).len() == 2 && last_icon(ops, 101) == Some(crate::hub::registry::ICON_WAITING)
        })
        .await;
        let second = prompts(&ops)[1].2;
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.control
            .send(press("q", Some(second), "allow:bcdef"))
            .unwrap();
        settled(&rig, |ops| last_icon(ops, 101) == Some(ICON_ALIVE)).await;
    }''')
rep('''    use std::sync::Mutex;

    use super::*;''', '''    use std::collections::HashSet;
    use std::sync::Mutex;

    use super::*;''')
# planner test: a legacy agent gets a verdict without an id
rep('''        let got = received(&mut rig, 1).await;
        assert_eq!(verdicts(&got), [("abcde".to_owned(), Behavior::Allow)]);''', '''        let got = received(&mut rig, 1).await;
        assert_eq!(verdicts(&got), [("abcde".to_owned(), Behavior::Allow)]);
        assert!(
            matches!(
                got.as_slice(),
                [HubMsg::PermissionVerdict {
                    verdict_id: None,
                    ..
                }]
            ),
            "an agent without acks gets the v1 verdict: {got:?}"
        );''')
rep('''    const CLOSED: &str = "Сессия завершилась";''', '''    const CLOSED: &str = "Сессия завершилась";

    #[test]
    fn the_closing_text_is_the_decided_wording() {
        assert_eq!(permissions::CLOSED_TEXT, CLOSED);
    }''')
open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')
