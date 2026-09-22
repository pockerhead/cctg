# Disconfirmation case

Concrete counter-example: a valid `user` record with valid string `message.content`, plus an irrelevant mistyped `"aiTitle": 5`. The proposed shared `RawRecord` uses `ai_title: Option<String>` for both `parse` and `ai_title`; if serde rejects the entire record, `parse` silently loses a valid turn and the plan's field-level tolerance claim is false.

Search target: prepared fixtures and the proposed serde shape in a throwaway compile/run check.

Result: the prepared fixtures do not contain this adversarial shape, but the throwaway program using the plan's exact field shape returns `invalid type: integer 5, expected a string`; the counter-example holds. The same flaw applies to the flat all-variants `RawBlock`: a valid text block with an irrelevant `"id": 5` is rejected wholesale.
