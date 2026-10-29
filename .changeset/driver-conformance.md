---
"localflake": patch
---

Fix driver-facing bugs found by the new shared driver conformance suite: `?` binds inside casts, text with a UTC offset inserted into `TIMESTAMP_LTZ` columns, `TIMESTAMP_LTZ` and `TIMESTAMP_TZ` binds (types and offsets), `CREATE OR REPLACE SCHEMA` on an existing schema, the error code for objects that already exist (002002), async query failures as reported to the Node.js driver, and server logging, which was silent by default.
