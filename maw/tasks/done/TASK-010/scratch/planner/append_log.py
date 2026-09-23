import datetime, io, json, os
LOG = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..', 'log.jsonl')
ts = datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
base = dict(stage='planner', provider='claude', model='opus', effort='medium', kind='decision')
entries = [
 ("Hook endpoint is a hand-rolled HTTP/1.1 reader over tokio (POST only, Content-Length only, Transfer-Encoding and repeated Content-Length rejected per RFC 9112 6.3, 8 KiB head, 1 MiB body, 2 s deadline, Connection: close, lingering close). Alternative: add hyper/axum (outside the agreed crate set, far more surface than one endpoint).",
  ["crates/cctg/src/hub/ingress.rs"]),
 ("Hook client is a raw tokio TcpStream POST under one overall timeout, no retry loop. Alternative: reqwest (needs the blocking feature or a client with rustls root setup; start-up cost counts against the 1.5 s SessionEnd budget).",
  ["crates/cctg/src/hook.rs"]),
 ("Event id = 32 hex from two std RandomState-keyed SipHash draws over counter+clock+pid; HookPost::new mints it, re-sending the same HookPost reuses it. Alternative: add rand/uuid crate (outside the agreed set; uniqueness, not secrecy, is required).",
  ["crates/cctg/src/wire.rs"]),
 ("Dedup remembers an event id only after try_send to the hub channel succeeded; a full channel answers 503 without remembering, so a re-send is delivered. Alternative: remember on receipt (a 503 would turn the re-send into a silent drop).",
  ["crates/cctg/src/hub/ingress.rs"]),
 ("Non-loopback listening needs only an explicit CCTG_AGENT_LISTEN/CCTG_HOOK_LISTEN ip:port (host names rejected) plus a startup warning. Alternative: an extra CCTG_ALLOW_REMOTE flag (second knob for the same explicit choice).",
  ["crates/cctg/src/hub/config.rs"]),
 ("Config.hub_secret is Option<Secret> and `cctg hub` fails at start when it is unset, like projects_dir. Alternative: make it required in Config::from_vars (rewrites every existing config test fixture).",
  ["crates/cctg/src/hub/config.rs", "crates/cctg/src/hub/mod.rs"]),
 ("Messages are versioned per line: decode parses a serde_json::Value, checks v, then type against Kinds::KINDS, then fields; errors carry no input. Alternative: #[serde(other)] Unknown variant (cannot round-trip, and serde error texts quote input values, which may be the secret).",
  ["crates/cctg/src/wire.rs"]),
]
with io.open(LOG, 'a', encoding='utf-8', newline='\n') as f:
    for body, refs in entries:
        f.write(json.dumps(dict(ts=ts, **base, body=body, refs=refs), ensure_ascii=False) + '\n')
print('appended', len(entries))
