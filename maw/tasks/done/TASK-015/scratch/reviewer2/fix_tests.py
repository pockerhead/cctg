# -*- coding: utf-8 -*-
# Reviewer-2 unit tests in registry.rs and subagents.rs of ws/.
import io, os
here = os.path.dirname(os.path.abspath(__file__))
hub = os.path.join(here, 'ws', 'crates', 'cctg', 'src', 'hub')
s = ''


def sub(old, new, count=1):
    global s
    assert s.count(old) == count, old[:100]
    s = s.replace(old, new)


p = os.path.join(hub, 'registry.rs')
s = io.open(p, encoding='utf-8', newline='').read()
s = s.replace('registry.block_work()', 'registry.block_work(usize::MAX)')

sub('''    #[test]
    fn a_reply_finds_only_its_own_sessions_subagent_block() {''', '''    #[test]
    fn a_legacy_subagent_record_is_dropped_on_load() {
        let dir = TempDir::new("registry-legacy-subagent");
        let store = RegistryStore::open(dir.path()).unwrap();
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        registry.confirm_subagent("a1", A, "↳ Explore a1".into());
        registry.confirm_subagent("a2", A, "↳ Explore a2".into());
        // TASK-011 wrote `{ parent_session, slot }` for every typed hook.
        let mut json: serde_json::Value =
            serde_json::from_slice(&RegistryStore::encode(&registry)).unwrap();
        json["subagents"]["a2"]
            .as_object_mut()
            .unwrap()
            .remove("block");
        store.save(&serde_json::to_vec(&json).unwrap()).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.subagents.keys().collect::<Vec<_>>(), ["a1"]);
    }

    #[test]
    fn an_unclear_first_send_is_never_repeated() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        registry.confirm_subagent("a1", A, "↳ Explore a1".into());
        let key = BlockKey::Agent("a1".into());
        assert_eq!(registry.block_work(usize::MAX).len(), 1);
        registry.block_send_unclear(&key);
        // Neither its result nor its session's end sends it again.
        registry.show_block(&key, "↳ Explore a1\\ndone".into(), false);
        assert!(registry.block_work(usize::MAX).is_empty());
        registry.retry_failed();
        registry.lose_blocks(&[A.to_owned()]);
        assert!(registry.block_work(usize::MAX).is_empty());
        let block = &registry.subagents["a1"].block;
        assert!(block.sending && block.pending.is_none() && block.message_id.is_none());
    }

    #[test]
    fn block_work_hands_out_at_most_its_limit() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        for agent in ["a1", "a2", "a3"] {
            registry.confirm_subagent(agent, A, format!("↳ Explore {agent}"));
        }
        assert_eq!(registry.block_work(2).len(), 2);
        assert_eq!(registry.block_work(2).len(), 1);
        assert!(registry.block_work(2).is_empty());
    }

    #[test]
    fn subagent_records_are_bounded_oldest_settled_first() {
        let mut registry = Registry::default();
        let mut topic = 100;
        let mut message = 500;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        let id = |i: usize| format!("a{i}");
        for i in 0..MAX_SUBAGENTS {
            assert!(registry.confirm_subagent(&id(i), A, format!("↳ Explore {}", id(i))));
        }
        settle_blocks(&mut registry, &mut message);
        // Every block still works: a new one is refused, none is dropped.
        assert!(!registry.confirm_subagent("new", A, "↳ Explore new".into()));
        // a5 and a3 finished (in that order of confirmation: a3 is older).
        for i in [5, 3] {
            let key = BlockKey::Agent(id(i));
            registry.show_block(&key, "done".into(), false);
        }
        settle_blocks(&mut registry, &mut message);
        assert!(registry.confirm_subagent("new", A, "↳ Explore new".into()));
        assert_eq!(registry.subagents.len(), MAX_SUBAGENTS);
        assert!(!registry.subagents.contains_key("a3"));
        assert!(registry.subagents.contains_key("a5"));
    }

    #[test]
    fn a_nested_answer_is_kept_in_the_registry_until_the_run_ends() {
        let dir = TempDir::new("registry-nested-answer");
        let store = RegistryStore::open(dir.path()).unwrap();
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, CWD, Some(10), None));
        registry.apply_hook(&start(N, CWD, Some(20), Some(10)));
        assert!(!registry.set_nested_answer(A, "top-level answers are turn answers"));
        assert!(registry.set_nested_answer(N, "first"));
        assert!(registry.set_nested_answer(N, "last"));
        store.save(&RegistryStore::encode(&registry)).unwrap();
        let mut registry = store.load().unwrap();
        assert_eq!(registry.take_nested_answer(N).as_deref(), Some("last"));
        assert_eq!(registry.take_nested_answer(N), None);
        // A run lost with its parent keeps no answer.
        registry.set_nested_answer(N, "late");
        registry.lose_blocks(&[A.to_owned()]);
        assert_eq!(registry.take_nested_answer(N), None);
    }

    #[test]
    fn a_reply_finds_only_its_own_sessions_subagent_block() {''')
io.open(p, 'w', encoding='utf-8', newline='\n').write(s)

p = os.path.join(hub, 'subagents.rs')
s = io.open(p, encoding='utf-8', newline='').read()
sub('''    #[test]
    fn agent_ids_are_plain() {''', '''    #[test]
    fn an_index_keeps_the_newest_calls_and_short_fields() {
        let mut text = String::new();
        for i in 0..MAX_INDEX_ENTRIES + 5 {
            let call = serde_json::json!({
                "type": "assistant",
                "message": { "role": "assistant", "content": [{
                    "type": "tool_use", "id": format!("t{i}"), "name": "Agent",
                    "input": { "description": "d".repeat(5000), "subagent_type": "Explore" },
                }]},
            });
            let result = serde_json::json!({
                "type": "user",
                "message": { "role": "user", "content": [{
                    "type": "tool_result", "tool_use_id": format!("t{i}"), "content": "ok",
                }]},
                "toolUseResult": { "agentId": format!("a{i}") },
            });
            text.push_str(&format!("{call}\\n{result}\\n"));
        }
        let mut index = AgentIndex::default();
        index.merge(scan_text(&text));
        assert_eq!(index.calls.len(), MAX_INDEX_ENTRIES);
        assert_eq!(index.links.len(), MAX_INDEX_ENTRIES);
        assert_eq!(index.call("a0"), None);
        let newest = index.call(&format!("a{}", MAX_INDEX_ENTRIES + 4)).unwrap();
        let description = newest.description.as_deref().unwrap();
        assert_eq!(telegram_len(description), MAX_CALL_FIELD);
    }

    #[test]
    fn agent_ids_are_plain() {''')
io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')
