# Each mutation breaks one rule of the reference; the named lib test must
# fail. Usage: python mutations.py <reference workspace root>
# Runs cargo with the shared target dir (CARGO_TARGET_DIR from the env).
import subprocess, sys, os

ws = sys.argv[1]
MUTATIONS = [
    # The hook echoes the call's own input (questions and all) next to the answers.
    ('crates/cctg/src/hook.rs',
     '    let mut updated = tool_input.clone();\n    updated\n        .as_object_mut()?',
     '    let mut updated = json!({});\n    updated\n        .as_object_mut()?',
     'hook::build_tests::answers_are_claude_code_pre_tool_use_output'),
    # The PermissionRequest hook of a question never asks the hub.
    ('crates/cctg/src/hook.rs',
     '    if input.tool_name == QUESTION_TOOL {\n        return Err(Skip("questions are asked by their PreToolUse hook"));',
     '    if input.tool_name == "never" {\n        return Err(Skip("questions are asked by their PreToolUse hook"));',
     'hook::build_tests::questions_carry_their_capped_texts_and_keep_the_input'),
    # A channel permission_request of a question gets no Allow/Deny.
    ('crates/cctg/src/hub/slots.rs',
     '        if request.tool_name == QUESTION_TOOL {\n            debug!(',
     '        if request.tool_name == "never" {\n            debug!(',
     'hub::slots::tests::permission_requests_of_a_question_get_no_buttons'),
    # A text after ✏️ Другое is the answer, not a message for the session.
    ('crates/cctg/src/hub/slots.rs',
     '            && self.answer_question(thread_id, input.reply_to, text)',
     '            && (text.is_empty() && self.answer_question(thread_id, input.reply_to, text))',
     'hub::slots::tests::a_question_is_answered_with_buttons_and_own_text'),
    # An unanswered question goes to the terminal when its time is up.
    ('crates/cctg/src/hub/slots.rs',
     '                } else if waiter.is_some_and(|waiter| now >= waiter.until) {',
     '                } else if waiter.is_some_and(|waiter| now >= waiter.until + Duration::from_secs(3600)) {',
     'hub::slots::tests::an_unanswered_question_goes_to_the_terminal'),
    # multiSelect labels are joined with a comma and a space.
    ('crates/cctg/src/hub/questions.rs',
     '                let answer = self.picked_labels().join(", ");',
     '                let answer = self.picked_labels().join(",");',
     'hub::questions::tests::a_multi_select_ticks_and_needs_one'),
    # A button of an earlier question answers nothing.
    ('crates/cctg/src/hub/questions.rs',
     '        if !self.is_open() || question != self.step {',
     '        if !self.is_open() {',
     'hub::questions::tests::a_single_choice_answers_and_moves_on'),
]
failed = 0
for path, old, new, test in MUTATIONS:
    full = os.path.join(ws, path)
    src = open(full, encoding='utf-8').read()
    assert src.count(old) == 1, (path, old)
    open(full, 'w', encoding='utf-8', newline='\n').write(src.replace(old, new))
    try:
        r = subprocess.run(['cargo', 'test', '-j', '1', '-p', 'cctg', '--lib', '--', '--exact', test],
                           cwd=ws, capture_output=True, text=True, encoding='utf-8', errors='replace')
        caught = r.returncode != 0
        print(('CAUGHT ' if caught else 'MISSED ') + test)
        failed += 0 if caught else 1
    finally:
        open(full, 'w', encoding='utf-8', newline='\n').write(src)
sys.exit(failed)
