---
"localflake": minor
---

Support stored procedures written in Snowflake Scripting: `CREATE [OR REPLACE] PROCEDURE` with scalar or `RETURNS TABLE` results, default, named and `OUT` arguments, overloads, `STRICT` and `NOT NULL` returns; `CALL` (including `CALL ... INTO :variable` and recursive calls from blocks), `DROP PROCEDURE`, `DESCRIBE PROCEDURE`, `SHOW PROCEDURES` and `INFORMATION_SCHEMA.PROCEDURES`. Procedures are stored in each database's hidden `__localflake` schema, so they persist, clone and drop with the database. Snowflake Scripting also gains cursors (`OPEN`, `FETCH`, `CLOSE`, `FOR row IN cursor`), `RESULTSET`s and `RETURN TABLE(...)`, and `SELECT ... INTO` accepts variables without colons.
