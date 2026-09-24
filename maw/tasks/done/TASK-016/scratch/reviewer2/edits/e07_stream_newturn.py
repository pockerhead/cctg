import os, sys
sys.path.insert(0, os.path.dirname(__file__))
from ed import edit
WS = os.path.join(os.path.dirname(__file__), '..', 'ws')
edit(os.path.join(WS, 'crates/cctg/src/hub/stream.rs'), [
(r'''    /// A turn ended here: a held answer may go now.
    TurnEnd,
}''', r'''    /// A turn ended here: a held answer may go now.
    TurnEnd,
    /// A prompt typed in the terminal starts a turn.
    NewTurn,
}'''),
(r'''            StreamItem::Prompt { text } => {
                flush(calls, &mut steps);
                steps.push(Step::Send {''', r'''            StreamItem::Prompt { text } => {
                flush(calls, &mut steps);
                steps.push(Step::NewTurn);
                steps.push(Step::Send {'''),
])
