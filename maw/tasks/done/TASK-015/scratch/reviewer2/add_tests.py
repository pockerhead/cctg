# -*- coding: utf-8 -*-
# Inserts the reviewer-2 slot tests and the Fake `unclear_sends` field into ws/.
import io, os
here = os.path.dirname(os.path.abspath(__file__))
p = os.path.join(here, 'ws', 'crates', 'cctg', 'src', 'hub', 'slots.rs')
s = io.open(p, encoding='utf-8', newline='').read()
anchor = '''        assert_eq!(shown(&all)[&700], format!("↳ Explore {S1}\\nLate."));
    }
'''
assert s.count(anchor) == 1
add = io.open(os.path.join(here, 'r2tests.rs'), encoding='utf-8').read()
s = s.replace(anchor, anchor + add)
old = '''        /// Only sends never return; topic calls still answer.
        stall_sends: bool,
    }'''
assert s.count(old) == 1
s = s.replace(old, '''        /// Only sends never return; topic calls still answer.
        stall_sends: bool,
        /// The next sends reach Telegram but the answer cannot be read.
        unclear_sends: Mutex<usize>,
    }''')
old = '''                Op::Send { .. } => match self.send_errors.lock().unwrap().pop() {'''
assert s.count(old) == 1
s = s.replace(old, '''                Op::Send { .. } if self.take_unclear_send() => {
                    Err(ApiError::Decode(
                        serde_json::from_str::<i64>("x").unwrap_err(),
                    ))
                }
''' + old)
s = s.replace('''    impl Fake {
        fn ops(&self) -> Vec<Op> {''', '''    impl Fake {
        fn take_unclear_send(&self) -> bool {
            let mut unclear = self.unclear_sends.lock().unwrap();
            let take = *unclear > 0;
            if take {
                *unclear -= 1;
            }
            take
        }

        fn ops(&self) -> Vec<Op> {''')
io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')
