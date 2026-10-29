---
"localflake": minor
---

Support materialized views and dynamic tables, emulated as views that are always up to date: `CREATE`, `DROP`, `ALTER ... RENAME TO | SET COMMENT | UNSET COMMENT` (refresh, suspend, resume and the like are accepted), `DESCRIBE`, `SHOW MATERIALIZED VIEWS`, `SHOW DYNAMIC TABLES` (with the target lag, warehouse and refresh mode given), and dynamic tables listed as tables by `SHOW TABLES`, `SHOW OBJECTS` (`is_dynamic`) and `INFORMATION_SCHEMA.TABLES`. `ALTER VIEW` (rename, comments, and accepted no-ops such as `SET SECURE`) now works too.
