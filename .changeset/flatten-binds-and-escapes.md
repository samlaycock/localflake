---
"localflake": patch
---

Fix two bugs in statements with `?` binds: `TO_VARCHAR` with a format on `TIMESTAMP_TZ` values printed `+00` instead of `+00:00` for `TZH:TZM` when a bind fed a `FLATTEN` subquery (`... IN (SELECT VALUE FROM TABLE(FLATTEN(INPUT => PARSE_JSON(?))))`), and `ESCAPE '\\'` failed with "Unterminated string literal".
