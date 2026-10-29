---
"localflake": minor
---

Support cancelling queries: driver abort requests (`/queries/v1/abort-request` by request ID, as sent on Go context cancellation, Node's `statement.cancel()` and client-side timeouts, and `/queries/<id>/abort-request`), `SYSTEM$CANCEL_QUERY` and the `STATEMENT_TIMEOUT_IN_SECONDS` parameter. Cancelled statements fail with Snowflake's 000604 error and timed-out ones with 000630, and a cancelled statement leaves an open transaction usable.
