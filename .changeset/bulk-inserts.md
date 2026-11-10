---
"localflake": patch
---

Fix bulk inserts through the drivers, which ran out of memory, slowed down with the square of their size, or were rejected for being too large.

`TABLE(FLATTEN(INPUT => PARSE_JSON(?)))` over a large bind no longer runs out of memory: the input was evaluated again for every element it produced and copied onto every row, so time and memory grew with rows × document size. It is now evaluated once, and `THIS` only when the query reads it.

Statements with many placeholders no longer slow down with the square of their length, so a multi-row `VALUES` with thousands of binds takes time in proportion to its rows.

Array binds (`INSERT ... VALUES (?, ...)` with a row of binds per row, the drivers' bulk path) now run as multi-row inserts in one transaction rather than one statement per row. They are much faster, and a failing row leaves none of the statement's rows behind, as in Snowflake.

Requests over 2 MB no longer fail with `413 Payload Too Large`. The limit is now 256 MiB and configurable with `LOCALFLAKE_MAX_REQUEST_BYTES` (`--max-request-bytes`, `0` for no limit); a request over it fails as a query error that names the setting.
