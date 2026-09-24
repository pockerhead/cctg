import os, sys
sys.path.insert(0, os.path.dirname(__file__))
from ed import edit
WS = os.path.join(os.path.dirname(__file__), '..', 'ws')
edit(os.path.join(WS, 'crates/cctg/src/wire.rs'), [
(r'''                        StreamItem::Result {
                            id: "t1".into(),
                            error: Some("boom".into()),
                        },
                    ],
                }],
                missing: false,
                more: true,
                reset: false,''', r'''                        StreamItem::Result {
                            id: "t1".into(),
                            error: Some("boom".into()),
                        },
                        StreamItem::TurnEnd,
                    ],
                }],
                missing: false,
                more: true,
                reset: false,'''),
(r'''                    items: vec![StreamItem::Other, StreamItem::Channel { message_id: 3 }],
                }],
                missing: false,
                more: false,
            })''', r'''                    items: vec![StreamItem::Other, StreamItem::Channel { message_id: 3 }],
                }],
                missing: false,
                more: false,
                reset: false,
            })'''),
])
