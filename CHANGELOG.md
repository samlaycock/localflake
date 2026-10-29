# localflake

## 0.1.0

### Minor Changes

- [#1](https://github.com/samlaycock/localflake/pull/1) [`c8c95c9`](https://github.com/samlaycock/localflake/commit/c8c95c903ec208b2acb1b5d513bd96bbf29aa4e0) Thanks [@samlaycock](https://github.com/samlaycock)! - First release of localflake, a local Snowflake emulator on DuckDB, published as a Docker image (`ghcr.io/samlaycock/localflake`) for linux/amd64 and linux/arm64. It speaks the protocol of Snowflake's official drivers and covers Snowflake SQL, semi-structured data, transactions, stages, `PUT`/`GET` and `COPY INTO`.

### Patch Changes

- [`dbd2f0c`](https://github.com/samlaycock/localflake/commit/dbd2f0cf9efca75838374614edff48afbe2f4848) Thanks [@samlaycock](https://github.com/samlaycock)! - Fix driver-facing bugs found by the new shared driver conformance suite: `?` binds inside casts, text with a UTC offset inserted into `TIMESTAMP_LTZ` columns, `TIMESTAMP_LTZ` and `TIMESTAMP_TZ` binds (types and offsets), `CREATE OR REPLACE SCHEMA` on an existing schema, the error code for objects that already exist (002002), async query failures as reported to the Node.js driver, and server logging, which was silent by default.
