# PCTX proposals — TASK-027

## 2026-09-24 (implementer): transcript domain, markdown API

Add to `domains/transcript.md` invariants: `transcript::split_markdown_for_telegram` (TASK-027, `src/markdown.rs`) cuts model markdown and converts each slice to Telegram HTML; every `HtmlChunk { text, html }` has closed tags and `telegram_len(html) <= 4096`, `text` is the plain fallback. Model text (turn answers, replies, stream Note and thinking) goes through it; prompts, tool lines and service texts stay on `split_for_telegram`. Why: later tasks touching outbound text should pick the right splitter.

## 2026-09-24 (implementer): hub domain, formatted sends

Add to `domains/hub.md`: `Op::Send`/`Op::Stream` carry `html: Option<String>` next to `text`; the scheduler sends `html` with `parse_mode: HTML` and, on `400 can't parse entities`, drops it and requeues the job at the head of its lane once as plain `text`. Why: a new outbound path must set `html: None` for service text, and must not add its own markup retry.
