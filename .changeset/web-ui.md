---
"localflake": minor
---

Add a web UI at `http://127.0.0.1:8080/` (`/__localflake/ui/`) for looking inside localflake while you develop. It has SQL worksheets (statement-at-cursor runs, highlighting, name completion, cancellation), a results grid with sorting, filtering and CSV/TSV/JSON export, and an object explorer that can preview, describe, drop and truncate objects, upload files to stages, download them, and load files into tables. It also has saved queries, a live history of every statement from every client with the DuckDB SQL each was translated to, and a list of connected sessions that can end a session to test reconnects. It's embedded in the binary and works offline. Turn it off with `--ui false` (`LOCALFLAKE_UI=false`).
