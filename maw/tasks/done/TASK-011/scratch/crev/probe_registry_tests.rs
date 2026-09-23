    #[test]
    fn crev_clear_in_real_hook_order_jumps_topic() {
        // Real /clear order: SessionEnd(old) first, then SessionStart(new, same claude pid).
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None)); // #1
        registry.apply_hook(&start(B, CWD, Some(11), None)); // #2
        settle(&mut registry, &mut topic);
        registry.apply_hook(&end(A)); // #1 free
        registry.apply_hook(&end(B)); // /clear in B's process: SessionEnd first
        registry.apply_hook(&start_from(C, CWD, Some(11), None, "clear"));
        let b = slot_of(&registry, B).unwrap();
        let c = slot_of(&registry, C).unwrap();
        println!("B ordinal {} C ordinal {}", registry.slots[b.0].ordinal, registry.slots[c.0].ordinal);
        assert_eq!(c, b, "clear jumped from #{} to #{}", registry.slots[b.0].ordinal, registry.slots[c.0].ordinal);
    }

    #[test]
    fn crev_nested_resume_of_other_toplevel_loses_its_slot_forever() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None)); // parent P=A #1
        registry.apply_hook(&start(B, r"C:\Work\Other", Some(11), None)); // B own slot
        settle(&mut registry, &mut topic);
        let b_slot = slot_of(&registry, B);
        registry.apply_hook(&end(B));
        // Inside A's Bash: claude -p --resume B
        registry.apply_hook(&start_from(B, r"C:\Work\Other", Some(20), Some(10), "resume"));
        registry.apply_hook(&end(B));
        // Later a plain interactive resume of B.
        registry.apply_hook(&start_from(B, r"C:\Work\Other", Some(30), None, "resume"));
        println!("kind {:?} slot {:?} was {:?}", registry.sessions[B].kind, slot_of(&registry, B), b_slot);
        assert_eq!(slot_of(&registry, B), b_slot);
    }
}
