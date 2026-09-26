"""Turns literal newlines the shell heredoc produced back into \\n escapes."""
import re

fixes = {
    'crates/cctg/tests/status_e2e.rs': [
        ('''        format!(
            "CCTG_HUB_SECRET={SECRET}
CCTG_HUB_HOOK_ADDR={addr}
CCTG_HOST={HOST}
"
        ),''', '''        format!("CCTG_HUB_SECRET={SECRET}\\nCCTG_HUB_HOOK_ADDR={addr}\\nCCTG_HOST={HOST}\\n"),'''),
        ('''        shown(
            &format!(
                "💤 Ждёт вас
{NUMBERS}"
            ),
            &[],
        ),''', '''        shown(&format!("💤 Ждёт вас\\n{NUMBERS}"), &[]),'''),
        ('''        shown(
            &format!(
                "🗜 Сжимаю контекст (вручную)…
{NUMBERS}"
            ),
            &[],
        ),''', '''        shown(&format!("🗜 Сжимаю контекст (вручную)…\\n{NUMBERS}"), &[]),'''),
    ],
    'crates/cctg/src/hub/status.rs': [
        ('''            "🗜 Сжимаю контекст (авто)… 1 мин
Opus 5.5 · high · ctx 50% · 5h 3%"''', '''            "🗜 Сжимаю контекст (авто)… 1 мин\\nOpus 5.5 · high · ctx 50% · 5h 3%"'''),
    ],
}
for path, pairs in fixes.items():
    s = open(path, encoding='utf-8').read()
    for a, b in pairs:
        n = s.count(a)
        assert n >= 1, (path, a)
        s = s.replace(a, b)
    open(path, 'w', encoding='utf-8', newline='').write(s)
