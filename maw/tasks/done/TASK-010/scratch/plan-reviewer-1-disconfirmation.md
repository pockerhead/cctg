# Disconfirmation case

Before evaluating the plan, test this concrete counterexample against the reference implementation:

`POST /v1/hook HTTP/1.1\r\nAuthorization: Bearer <valid>\r\nContent-Length : 0\r\n\r\n`

RFC 9112 section 5.1 requires a server to reject whitespace between a field name and the colon with 400. If the hand-written parser trims the field name and accepts this request (or merely falls through to 411), the plan's claim that every dangerous HTTP parsing branch is covered and RFC-compliant is wrong.
