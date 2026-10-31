# Contributing to localflake

Thanks for helping out. Bug reports, Snowflake behaviour that localflake gets
wrong, missing functions and new features are all welcome. For anything large,
please open an issue first so we can agree on the approach.

The most useful bug reports include the SQL, the result localflake gives, and
the result real Snowflake gives (or the documentation page that describes it).

## Layout

| Crate | Role |
|---|---|
| `localflake-server` | Driver protocol over HTTP (axum): login, sessions, `query-request`, response encoding. Also the web UI: the page in `ui/` (plain HTML, CSS and JavaScript, embedded in the binary) and its JSON API in `src/ui.rs` |
| `localflake-engine` | DuckDB sessions, Snowflake databases as attached DuckDB catalogs, result encoding, error code mapping |
| `localflake-sql` | Snowflake SQL → DuckDB: identifier case normalization, parsing (sqlparser-rs), statement classification and rewrites |

Requests flow server → engine → sql → DuckDB, and the Arrow results go back
through the engine's encoder into Snowflake's `rowtype`/`rowset` format.

## Development

You need a recent stable Rust toolchain and a C++ compiler. The driver suites
also need Python with [uv](https://docs.astral.sh/uv/), Node.js and Go, and
Node.js is needed for changesets.

```sh
cargo run -- --addr 127.0.0.1:8080                  # in memory
cargo run -- --data-dir ./data                      # one DuckDB file per database
RUST_LOG=localflake_engine=debug cargo run          # log translated SQL
LOCALFLAKE_UI_DIR=crates/localflake-server/ui cargo run   # serve the web UI's files from disk
```

The web UI's files are embedded at build time. While working on them, set
`LOCALFLAKE_UI_DIR` so a browser reload picks up changes without a rebuild.
The page has no build step or dependencies: keep it that way, so localflake
never needs the network.

Builds compile DuckDB from source by default (the engine's `bundled` feature),
which takes several minutes. To link DuckDB's prebuilt library instead, as CI
and the Docker image do, build without default features; the library for the
pinned release is downloaded automatically, and the rpath tells binaries where
to find it:

```sh
LIB=$PWD/target/duckdb-download/$(rustc -vV | sed -n 's/host: //p')/v2.0.0
RUSTFLAGS="-C link-arg=-Wl,-rpath,$LIB" cargo test --workspace --no-default-features
```

## Testing

```sh
cargo test --workspace                              # unit tests and the conformance corpus, no server
cargo build                                         # the end-to-end suites start target/debug/localflake
(cd tests/python && uv run pytest)                  # official Python connector (Arrow and JSON)
(cd tests/node && npm ci && npm test)               # official Node.js driver (JSON)
(cd tests/go && go test -count=1 ./...)             # gosnowflake (Arrow and JSON)
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Set `LOCALFLAKE_BIN` to test a different binary. CI runs `cargo fmt --check`,
clippy with `-D warnings`, the workspace tests and all three driver suites on
every pull request, and a Docker build check on release pull requests.

The three officially supported drivers share a conformance spec
(`tests/conformance/cases.json`) that each suite runs through its driver, so
they get the same coverage. [`tests/README.md`](tests/README.md) describes the
spec, the driver-specific tests and the known differences between the drivers.
Protocol changes should add a case there.

### Conformance corpus

`crates/localflake-engine/tests/corpus/*.test` holds Snowflake SQL with the
results Snowflake gives, grouped by area (numeric, string, date/time,
semi-structured, DML, metadata, UDFs, ...). Each file runs in a fresh session:

```text
query rowsort types
select region, sum(amount) from sales group by 1
----
VARCHAR|NUMBER(*,2)
east|30.50
```

Cases marked `gap` are known differences: they're reported but don't fail the
run, and a gap that starts passing is flagged so the marker can be removed.
There are currently none. `CORPUS_SLOW=<ms>` lists slow cases.

```sh
cargo test -p localflake-engine --test corpus -- --nocapture   # summary
CORPUS_FILTER=07 CORPUS_SHOW_GAPS=1 cargo test -p localflake-engine --test corpus -- --nocapture
```

The expected results come from Snowflake's documented behaviour. Cases that
real Snowflake disagrees with are bugs in the corpus, so please fix them. SQL
behaviour changes should add or update corpus cases.

## Changesets

Every pull request needs a changeset: a short Markdown file under `.changeset/`
that says what changed and whether it's a `patch`, `minor` or `major` change.
It becomes the changelog entry.

```sh
npm ci
npm run changeset                # describe a change users will notice
npm run changeset -- --empty     # for CI, tests, docs and other invisible changes
```

CI fails if a pull request has no changeset.

## Releases

Releases are managed with [changesets](https://github.com/changesets/changesets).
`package.json` holds the version, and `npm run version` copies it into the
Cargo workspace.

1. Changesets land on `main` with their pull requests.
2. The release workflow keeps a "Release <version>" pull request open. It
   applies the pending changesets: the version bump and `CHANGELOG.md`. CI runs
   on it, including a Docker build check.
3. Merging that pull request does the release:
   - builds the image natively for amd64 and arm64;
   - pushes `ghcr.io/samlaycock/localflake:<version>`, `:<major>.<minor>` (and
     `:<major>` from 1.0) and `:latest`;
   - tags `v<version>` and creates the GitHub release from the changelog.

## License

By contributing, you agree that your contributions will be dual licensed under
the MIT and Apache-2.0 licenses, as described in the
[README](README.md#license).
