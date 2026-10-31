---
"localflake": patch
---

Fix `TRY_CAST(... AS BOOLEAN)`, which failed with "TRY can not be used in combination with a volatile function". This also fixes `COPY INTO` with `MATCH_BY_COLUMN_NAME` into tables with `BOOLEAN` columns.
