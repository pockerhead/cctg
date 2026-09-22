import json, datetime

ts = datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
base = dict(stage='plan-reviewer-2', provider='claude', model='opus', effort='medium')
entries = [
    dict(kind='decision',
         body='Keep PLAN_V2 per-item internally tagged RawBlock with #[serde(other)] Ignored; compiled and tested in scratch/rev2_crate (24 tests, clippy with deny(unwrap_used,expect_used,panic) clean). Alternative rejected: PLAN.md flat all-variants RawBlock (drops a valid text block that has a wrong-typed irrelevant id).',
         refs=['scratch/rev2_crate', 'scratch/rev2_probe.out.txt']),
    dict(kind='decision',
         body='Fixture privacy test checks lowercase substrings "users" + backslash and "users/" plus hand-coded -100 id and bot-token detectors, instead of the PLAN_V2 literal Windows home path, which misses JSON-escaped (doubled backslash) paths in raw fixture text. Alternative: regex dev-dependency, rejected as a new dependency.',
         refs=['scratch/rev2_crate/tests/parse_fixtures.rs']),
    dict(kind='decision',
         body='PLAN_FINAL embeds the verified reference lib.rs and test files verbatim so the implementer does not re-derive them. Alternative: prose-only steps as in PLAN_V2, rejected because its purity and privacy patterns were ambiguous.',
         refs=['PLAN_FINAL.md']),
]
with open('log.jsonl', 'a', encoding='utf-8', newline='\n') as f:
    for e in entries:
        f.write(json.dumps(dict(ts=ts, **base, **e), ensure_ascii=False) + '\n')
