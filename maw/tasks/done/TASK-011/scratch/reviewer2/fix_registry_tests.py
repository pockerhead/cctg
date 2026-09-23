# Reviewer-2 changes to the registry unit tests (run once).
import os
HERE = os.path.dirname(os.path.abspath(__file__))
p = os.path.join(HERE, 'ws', 'crates', 'cctg', 'src', 'hub', 'registry.rs')
s = open(p, encoding='utf-8').read()


def rep(old, new):
    global s
    assert s.count(old) == 1, old[:100]
    s = s.replace(old, new, 1)


# settle answers separators too
rep('''                } => registry.topic_edited(*slot, *thread_id, name.as_deref(), icon.as_deref()),
                TopicJob::Separator { .. } => {}
            }''', '''                } => registry.topic_edited(*slot, *thread_id, name.as_deref(), icon.as_deref()),
                TopicJob::Separator {
                    slot,
                    thread_id,
                    text,
                } => registry.topic_separated(*slot, *thread_id, text),
            }''')

# after a dead one: add queued B + C variant
rep('''    #[test]
    fn concurrent_sessions_get_new_ordinals_and_free_slots_are_reused_first() {''', '''    #[test]
    fn two_starts_after_a_death_take_the_old_slot_and_one_new_ordinal() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        registry.apply_hook(&end(A));
        // B and C both start before any topic call goes out.
        registry.apply_hook(&start(B, CWD, Some(11), None));
        registry.apply_hook(&start(C, CWD, Some(12), None));
        let jobs = settle(&mut registry, &mut topic);
        assert_eq!(creates(&jobs), 1, "{jobs:?}");
        assert_eq!(separators(&jobs), ["── session bbbbbbbb · new ──"]);
        assert_eq!(slot_of(&registry, B), slot_of(&registry, A));
        assert_eq!(registry.slots[slot_of(&registry, C).unwrap().0].ordinal, 2);
        let jobs = settle(&mut registry, &mut topic);
        assert_eq!(creates(&jobs), 0);
        assert!(separators(&jobs).is_empty());
        assert_eq!(registry.slots.len(), 2);
    }

    #[test]
    fn concurrent_sessions_get_new_ordinals_and_free_slots_are_reused_first() {''')

# stale self parent
rep('''    #[test]
    fn a_stale_parent_pid_pointing_at_itself_is_top_level() {
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, CWD, Some(10), None));
        registry.apply_hook(&start_from(A, CWD, Some(10), Some(10), "resume"));
        assert_eq!(registry.sessions[A].kind, SessionKind::TopLevel);
        assert_eq!(registry.slots.len(), 1);
    }''', '''    #[test]
    fn a_parent_pid_that_is_the_session_itself_is_nested_unknown_parent() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        let before = registry.sessions[A].clone();
        let slots_before = registry.slots.clone();
        // A second start of A whose next claude ancestor is A's own process.
        for (pid, parent) in [(20, 10), (10, 10)] {
            let outcome = registry.session_started(
                &start_from(A, r"C:\\Elsewhere", Some(pid), Some(parent), "resume"),
                Some("resume"),
                Some(pid),
                Some(parent),
            );
            assert_eq!(outcome, SlotOrParent::Parent(None));
        }
        // No slot, and A's own record and slot are left alone.
        assert_eq!(registry.sessions[A].kind, SessionKind::TopLevel);
        assert_eq!(registry.sessions[A].slot, before.slot);
        assert_eq!(registry.sessions[A].claude_pid, Some(10));
        assert!(!registry.pids.contains_key("box/20"));
        assert_eq!(registry.slots, slots_before);
        let jobs = settle(&mut registry, &mut topic);
        assert_eq!(creates(&jobs), 0);
        assert!(separators(&jobs).is_empty());
        // A plain restart of A still finds its own slot.
        registry.apply_hook(&end(A));
        registry.apply_hook(&start_from(A, CWD, Some(30), None, "resume"));
        assert_eq!(registry.sessions[A].kind, SessionKind::TopLevel);
        assert_eq!(registry.sessions[A].slot, before.slot);
    }''')

# icons
rep('''        let offered: HashSet<String> = [ICON_ALIVE, ICON_DEAD, ICON_WAITING, ICON_NO_CHANNEL]
            .map(str::to_owned)
            .into();
        let mut checked = Icons::default();
        assert!(checked.keep_valid(&offered).is_empty());
        let mut partial: HashSet<String> = offered.clone();
        partial.remove(ICON_WAITING);
        assert_eq!(checked.keep_valid(&partial), ["waiting"]);
        assert_eq!(checked.waiting, None);
    }''', '''    }

    #[test]
    fn icons_come_from_the_offered_set() {
        let preferred = [ICON_ALIVE, ICON_DEAD, ICON_WAITING, ICON_NO_CHANNEL].map(str::to_owned);
        let (icons, substituted) = Icons::from_offered(preferred.clone()).unwrap();
        assert_eq!(icons, Icons::default());
        assert!(substituted.is_empty());

        // The waiting and dead icons are gone: the smallest spare ids stand in.
        let offered = [
            ICON_ALIVE.to_owned(),
            ICON_NO_CHANNEL.to_owned(),
            "900".to_owned(),
            "800".to_owned(),
            "700".to_owned(),
            String::new(),
        ];
        let (icons, substituted) = Icons::from_offered(offered.clone()).unwrap();
        assert_eq!(substituted, ["dead", "waiting"]);
        assert_eq!(icons.alive.as_deref(), Some(ICON_ALIVE));
        assert_eq!(icons.dead.as_deref(), Some("700"));
        assert_eq!(icons.waiting.as_deref(), Some("800"));
        assert_eq!(icons.no_channel.as_deref(), Some(ICON_NO_CHANNEL));
        let chosen: HashSet<String> = [icons.alive, icons.dead, icons.waiting, icons.no_channel]
            .into_iter()
            .map(Option::unwrap)
            .collect();
        assert_eq!(chosen.len(), 4);
        assert!(chosen.iter().all(|id| offered.contains(id)));

        // Too few distinct usable ids: an error, never an unchecked id.
        assert_eq!(
            Icons::from_offered(["1", "2", "2", ""].map(str::to_owned)),
            Err(IconError::TooFew(2))
        );
        assert_eq!(
            Icons::from_offered(Vec::new()),
            Err(IconError::TooFew(0))
        );
    }''')

# gone topic: add in-flight replacement case + separator pending
rep('''    #[test]
    fn a_failed_call_is_not_repeated_until_retry() {''', '''    #[test]
    fn a_late_gone_report_during_a_replacement_changes_nothing() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        let id = slot_of(&registry, A).unwrap();
        // `/clear`: a separator and a title edit are both due.
        registry.apply_hook(&start_from(B, CWD, Some(10), None, "clear"));
        let jobs = registry.topic_work(&Icons::default(), true);
        assert_eq!(jobs.len(), 1, "one call per slot: {jobs:?}");
        assert_eq!(separators(&jobs).len(), 1);
        registry.topic_invalid(id, 100);
        let jobs = registry.topic_work(&Icons::default(), true);
        assert_eq!(creates(&jobs), 1);
        // Late reports about topic 100 while the replacement is in flight.
        registry.topic_invalid(id, 100);
        registry.topic_edited(id, 100, Some("x"), None);
        registry.topic_separated(id, 100, "x");
        assert!(registry.slots[id.0].busy);
        assert!(registry.topic_work(&Icons::default(), true).is_empty());
        registry.topic_created(id, 200, "y", None);
        let mut total = 1;
        for _ in 0..3 {
            total += creates(&settle(&mut registry, &mut topic));
        }
        assert_eq!(total, 1);
        assert_eq!(registry.slots[id.0].topic_id, Some(200));
    }

    #[test]
    fn a_separator_stays_pending_until_it_is_delivered() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        registry.apply_hook(&end(A));
        settle(&mut registry, &mut topic);
        registry.apply_hook(&start(B, CWD, Some(11), None));
        let id = slot_of(&registry, B).unwrap();
        let text = "── session bbbbbbbb · new ──";
        let jobs = registry.topic_work(&Icons::default(), true);
        assert_eq!(separators(&jobs), [text]);
        assert_eq!(jobs.len(), 1, "the edit waits for the separator: {jobs:?}");
        // In flight, it is still what a save writes.
        assert_eq!(registry.slots[id.0].pending_separator.as_deref(), Some(text));
        assert!(registry.topic_work(&Icons::default(), true).is_empty());
        // Refused: kept, not repeated until the retry.
        registry.topic_failed(id, &Icons::default());
        assert_eq!(registry.slots[id.0].pending_separator.as_deref(), Some(text));
        assert!(registry.topic_work(&Icons::default(), true).is_empty());
        registry.retry_failed();
        let jobs = registry.topic_work(&Icons::default(), true);
        assert_eq!(separators(&jobs), [text]);
        // The scheduler stopped: released, still pending.
        registry.release(id);
        assert_eq!(registry.slots[id.0].pending_separator.as_deref(), Some(text));
        let jobs = registry.topic_work(&Icons::default(), true);
        assert_eq!(separators(&jobs), [text]);
        registry.topic_separated(id, 100, text);
        assert_eq!(registry.slots[id.0].pending_separator, None);
        let jobs = registry.topic_work(&Icons::default(), true);
        assert!(separators(&jobs).is_empty());
        assert!(
            matches!(&jobs[..], [TopicJob::Edit { name: Some(name), .. }] if name == "[box] Project · bbbbbbbb")
        );
    }

    #[test]
    fn a_failed_call_is_not_repeated_until_retry() {''')

# hook-only session: remove adoption
rep('''        // An agent of an unknown session is adopted as top-level.
        assert!(!registry.agent_connected(B, 2));
        let id = registry.adopt(B, "box", CWD);
        assert!(registry.agent_connected(B, 2));
        assert_eq!(registry.slots[id.0].ordinal, 2);
    }''', '''        // An agent of an unknown session is not adopted.
        assert!(!registry.agent_connected(B, 2));
        assert!(!registry.sessions.contains_key(B));
        assert!(registry.topic_work(&Icons::default(), true).is_empty());
    }''')

rep('''    #[test]
    fn prompt_hooks_of_an_unknown_session_register_it_and_ask_for_a_title() {
        let mut registry = Registry::default();
        let followup = registry.apply_hook(&post(
            A,
            CWD,
            HookEvent::UserPromptSubmit { prompt_id: None },
        ));
        assert!(slot_of(&registry, A).is_some());
        assert_eq!(
            followup.read_title,
            Some((A.to_owned(), format!("/t/{A}.jsonl")))
        );''', '''    #[test]
    fn prompt_hooks_of_an_unknown_session_create_nothing() {
        let mut registry = Registry::default();
        for event in [
            HookEvent::UserPromptSubmit { prompt_id: None },
            HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: None,
            },
        ] {
            let followup = registry.apply_hook(&post(A, CWD, event));
            assert_eq!(followup, Followup::default());
        }
        assert!(registry.sessions.is_empty());
        assert!(registry.slots.is_empty());
        assert!(registry.topic_work(&Icons::default(), true).is_empty());
        // Known and top-level: the prompt hook asks for the title once.
        registry.apply_hook(&start(A, CWD, Some(10), None));
        let followup = registry.apply_hook(&post(
            A,
            CWD,
            HookEvent::UserPromptSubmit { prompt_id: None },
        ));
        assert_eq!(
            followup.read_title,
            Some((A.to_owned(), format!("/t/{A}.jsonl")))
        );''')
open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')
