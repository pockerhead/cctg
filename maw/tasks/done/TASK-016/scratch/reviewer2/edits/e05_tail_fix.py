import os, sys
sys.path.insert(0, os.path.dirname(__file__))
from ed import edit
WS = os.path.join(os.path.dirname(__file__), '..', 'ws')
edit(os.path.join(WS, 'crates/cctg/src/tail.rs'), [
(r'''        assert_eq!(
            texts(&read(&dir, &path, None)),
            (0, end, vec![], false, false, false)
                .clone()
                .with_from(end)
        );''', r'''        assert_eq!(
            texts(&read(&dir, &path, None)),
            (end, end, vec![], false, false, false)
        );'''),
(r'''    trait WithFrom {
        fn with_from(self, from: u64) -> Self;
    }

    impl WithFrom for (u64, u64, Vec<String>, bool, bool, bool) {
        fn with_from(mut self, from: u64) -> Self {
            self.0 = from;
            self
        }
    }

''', ''),
])
