// Code-reviewer probe: Live::barrier() grows `waiting` while a message waits.
use cctg::hub::stream::Live;
fn main() {
    let mut live = Live::new(Some(0), Vec::new());
    live.sent(); // one message waits for Telegram (e.g. bucket empty / retry_after)
    let before = format!("{live:?}").len();
    // idle reads every 300 ms for 60 s: 200 empty chunks, each ends with a barrier
    for _ in 0..200 { live.barrier(100); }
    let after = format!("{live:?}").len();
    let barriers = format!("{live:?}").matches("Barrier").count();
    println!("debug len before={before} after={after} barriers_in_waiting={barriers} unanswered={}", live.unanswered());
    assert!(live.advance().is_none());
}
