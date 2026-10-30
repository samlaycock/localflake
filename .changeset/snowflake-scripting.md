---
"localflake": minor
---

Support Snowflake Scripting anonymous blocks and `EXECUTE IMMEDIATE`: typed variables (`DECLARE`, `LET`, `:=`), `IF`, `CASE`, `FOR`, `WHILE`, `REPEAT` and `LOOP` with labelled `BREAK`/`CONTINUE`, `RETURN`, SQL statements with `:variable` binds and `SELECT ... INTO`, `SQLROWCOUNT`/`SQLFOUND`/`SQLNOTFOUND`/`ACTIVITY_COUNT`, declared exceptions with `RAISE`, `EXIT` and `CONTINUE` handlers (`SQLCODE`, `SQLERRM`, `SQLSTATE`), and Snowflake's uncaught-exception errors. Blocks sent by drivers are kept whole rather than split at their semicolons, and cancellation and statement timeouts stop a running block.
