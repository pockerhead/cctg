# Reproduces the reviewed defects on the planner reference: copies
# scratch/planner/ws to scratch/reviewer2/phase1_ws, adds the test-only Fake
# hook for editMessageText errors and the new tests (phase1_tests.rs, written
# against the planner API), runs `hub::slots`. Output: phase1_failing.txt.
import os, re, shutil, subprocess
here = os.path.dirname(os.path.abspath(__file__))
src = os.path.join(here, '..', 'planner', 'ws')
ws = os.path.join(here, 'phase1_ws')
shutil.rmtree(ws, ignore_errors=True)
shutil.copytree(src, ws)
p = os.path.join(ws, 'crates', 'cctg', 'src', 'hub', 'slots.rs')
s = open(p, encoding='utf-8').read()


def rep(old, new):
    global s
    assert s.count(old) == 1, old
    s = s.replace(old, new)


rep('''        send_errors: Mutex<Vec<&'static str>>,
        delete_error: Option<&'static str>,''', '''        send_errors: Mutex<Vec<&'static str>>,
        /// Answers the next `editMessageText` calls (last first).
        message_edit_errors: Mutex<Vec<(i64, &'static str)>>,
        delete_error: Option<&'static str>,''')
rep('''                Op::Delete { .. } => match self.delete_error {''', '''                Op::Edit { .. } => match self.message_edit_errors.lock().unwrap().pop() {
                    Some((code, description)) => Err(ApiError::Telegram {
                        code,
                        description: description.to_owned(),
                    }),
                    None => Ok(Outcome::Done),
                },
                Op::Delete { .. } => match self.delete_error {''')
rep('''                    request_id,
                    behavior,
                } => Some((request_id.clone(), *behavior)),''', '''                    request_id,
                    behavior,
                    ..
                } => Some((request_id.clone(), *behavior)),''')
tests = open(os.path.join(here, 'phase1_tests.rs'), encoding='utf-8').read()
tests = tests.replace('Some(ICON_WAITING)', 'Some(crate::hub::registry::ICON_WAITING)')
anchor = '    #[tokio::test]\n    async fn a_prompt_overtakes_a_full_reply_backlog()'
rep(anchor, tests + anchor)
open(p, 'w', encoding='utf-8', newline='\n').write(s)
env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(os.environ['TEMP'], 'cctg-t014-rev2-target'),
           CARGO_PROFILE_DEV_DEBUG='0')
r = subprocess.run(['cargo', 'test', '-j', '1', '-p', 'cctg', '--lib', 'hub::slots'],
                   cwd=ws, env=env, capture_output=True, text=True, encoding='utf-8', errors='replace')
keep = [l for l in r.stdout.splitlines()
        if l.startswith('test ') and l.endswith('FAILED') or 'panicked at' in l
        or l.startswith('  left') or l.startswith(' right') or l.startswith('assertion')
        or l.startswith('test result')]
out = ['# planner reference + review-2 tests, `cargo test -p cctg --lib hub::slots`, exit %d' % r.returncode] + keep
open(os.path.join(here, 'phase1_failing.txt'), 'w', encoding='utf-8', newline='\n').write('\n'.join(out) + '\n')
print('\n'.join(out))
shutil.rmtree(ws, ignore_errors=True)
