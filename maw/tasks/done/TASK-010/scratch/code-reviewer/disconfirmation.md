# code-reviewer disconfirmation (written before evaluation)

Counter-example: an agent sends a first line that is valid JSON with v=1 and type=hello but whose
`secret` (or another field) makes serde fail, e.g. {"v":1,"type":"hello","secret":"<real secret>","x":..} with a
wrong-typed field, or {"v":1,"type":"register","session_id":"<secret>" ...} with a type error. If WireError::Malformed
carries serde_json::Error text (serde quotes string values: `invalid type: string "..."`), the secret reaches
the hub log. Second: HTTP ingress where head and body arrive in one read with trailing bytes beyond Content-Length
(pipelining) and the body slice is taken as buf[head_end..] instead of exactly CL bytes.
