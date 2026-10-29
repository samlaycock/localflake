---
"localflake": patch
---

Fix `TO_VARCHAR` with a format on `TIMESTAMP_TZ` values printing `+00` instead of `+00:00` for `TZH:TZM` when a text bind was range-compared with a date or timestamp column (`WHERE D >= ?`).
