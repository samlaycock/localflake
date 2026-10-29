---
"localflake": patch
---

Fix `TO_VARCHAR`/`TO_CHAR` with a format on `TIMESTAMP_TZ` and `TIMESTAMP_LTZ` values: date and time parts now print in the value's own offset instead of UTC, `TZH:TZM` keeps its minutes (`+00:00`, not `+00`), and formats made only of `TZH`/`TZM` no longer fail with a `TIMESTAMP_NS -> DOUBLE` cast error.
