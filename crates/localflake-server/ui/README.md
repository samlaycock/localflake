# localflake web UI

A React single-page app built with stock [shadcn/ui](https://ui.shadcn.com) components. The
Rust server compiles `dist/` into the binary and serves it under `/__localflake/ui/`: every path
there that isn't one of its files is the app. It talks to localflake through the UI's JSON API
(`/__localflake/api/…`), whose worksheet sessions run SQL like a driver's.

## Working on it

```sh
cargo run                 # localflake on http://localhost:8080 (from the repository root)
cd crates/localflake-server/ui
bun install
bun run dev               # the app with hot reload, at /__localflake/ui/; the API calls go
                          # to localhost:8080 (LOCALFLAKE_URL=http://… for another server)
bun run check             # format check, lint and type check
bun run fix               # format, and apply lint fixes
bun run build             # write dist/: commit it with your change
```

The project is managed by [Vite+](https://viteplus.dev): the scripts call its `vp` command,
which brings Vite, Oxfmt (formatting), Oxlint (linting) and tsrs (type checking). With the
[global `vp`](https://viteplus.dev/guide/global-cli) installed, `vp dev`, `vp check` and
`vp build` work directly.

`dist/` is committed so `cargo install --git` and `cargo build` work without Node, and
localflake never needs the network. CI fails if it doesn't match the source. Debug builds of
localflake read `dist/` from disk, so after `bun run build` a reload shows the change without
rebuilding the binary.

## Conventions

These are shared by the local\* tools (localforce, localflake, localoidc, localmail and
localsaml), so their UIs look and work alike.

- **Stack.** Vite+, React, React Router in data mode (`createBrowserRouter`), Tailwind CSS
  v4 and shadcn/ui, set up with `shadcn init` (the `base-nova` style on Base UI, neutral base
  colour, lucide icons). The Geist font and the icons are bundled, so the UI works offline.
- **Vite+ does the checking.** `vite.config.ts` holds the `lint` and `fmt` settings: Oxlint's
  correctness rules plus the React plugin (hooks and React Compiler rules), type-aware rules
  and type checking, and shadcn's code style for Oxfmt. There's no ESLint, Prettier or `tsc`.
- **Stock components only.** `src/components/ui/` holds components from the public shadcn
  registry, added with `bunx shadcn@latest add <name>` and never edited. Formatting and lint
  skip them. `bun run stock-diff` shows any difference from the registry.
- **One accent colour per product.** `src/index.css` is shadcn's stock theme, as `shadcn init`
  wrote it. `src/theme.css` sets the product's accent (`--primary`, `--ring`, `--chart-1`,
  `--sidebar-primary`) and is the only product-specific styling. Style with theme tokens
  (`bg-muted`, `text-primary`, `border`), not palette colours.
- **App components compose stock ones.** `src/components/` has the header, the theme toggle,
  a copy button and the like, built from stock components and Tailwind classes. The results
  grid is the stock `Table`, rendering only the rows in view.
- **One custom component: the SQL editor.** `src/components/sql-editor.tsx` is a textarea
  over a highlighted copy of its text, with line numbers, the statement at the cursor marked
  and autocomplete. It's styled with theme tokens like the stock components; its syntax
  colours (`src/lib/sql.ts`) are the only palette colours in the app.
- **Custom rendering happens on the server.** Content the stock components can't show is
  rendered by the server and shown in an iframe (localmail's email bodies, for example).

## How localflake's app is put together

- **Worksheets live outside React.** `src/lib/worksheets.ts` holds the worksheets, their
  sessions and their results in a small store, so a query keeps running and its results stay
  while another page shows. The worksheets page stays mounted too (`src/routes/root.tsx`
  hides it), so editors keep their undo history and result grids their scroll, sort and
  filter. Worksheets and saved queries are kept in the browser's storage under the keys the
  previous UI used, so they carry over.
- **Live data is polled.** `src/lib/live.ts` polls the query history and the sessions for the
  header's counts and those pages. The history's filters are in the URL, and a statement's
  detail is a route (`/history/<query id>`) whose loader re-runs when the statement changes.
- **Confirmations and prompts are awaitable.** `confirm()` and `prompt()` in
  `src/lib/dialogs.ts` show a stock dialog through `<DialogHost />`, so an action anywhere (an
  explorer menu, a worksheet tab) can ask before it runs.
