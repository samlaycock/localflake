---
"localflake": minor
---

Support Snowflake's `TIMEZONE` parameter at every level. `LOCALFLAKE_TIMEZONE` (`--timezone`) sets the account default (still `America/Los_Angeles` if unset). `ALTER SESSION SET`/`UNSET TIMEZONE` and `TIMEZONE` at login set it for the session, and `TIMEZONE` in a query request's `parameters` applies to that query only. `SHOW PARAMETERS LIKE 'TIMEZONE'` (`IN SESSION` or `IN ACCOUNT`) reports the level it was set at, and unknown zone names fail with error 002043 without changing anything.

Also fix two `TIMESTAMP_TZ` bugs. Comparisons now match by instant, as Snowflake does: the same instant at different offsets is equal, and text or `TIMESTAMP_NTZ` on the other side takes the time zone in effect. This covers `=`, `<>`, `<`, `>`, `BETWEEN`, `IN` lists and subqueries, `IS [NOT] DISTINCT FROM`, `CASE x WHEN`, `DECODE` and scalar subqueries. `TO_VARCHAR` of `DATE_TRUNC`/`DATEADD` on a `TIMESTAMP_TZ` keeps its offset.
