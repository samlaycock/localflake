---
"localflake": minor
---

The web UI is rebuilt as a React app with shadcn/ui components. Everything works as before (worksheets, autocomplete, the object explorer, saved queries, the query history and sessions), with a few changes:

- The query history and sessions have their own addresses, `/__localflake/ui/history` and `/__localflake/ui/sessions`, and each statement's detail has one too (`/__localflake/ui/history/<query id>`). The history's filters are kept in the URL. Old `#history` and `#sessions` links still work.
- An error's query ID links to its detail in the history.
- The explorer, the editor and the history's detail can be resized, and there's a light, dark and system theme switch.
- Worksheets and saved queries carry over from the previous UI.
- `LOCALFLAKE_UI_DIR` is gone: debug builds read the built UI from disk, and `bun run dev` in `crates/localflake-server/ui` serves it with hot reload.
