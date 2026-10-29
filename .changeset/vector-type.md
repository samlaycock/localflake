---
"localflake": minor
---

Support the `VECTOR(INT, n)` and `VECTOR(FLOAT, n)` types: columns, casts from and to `ARRAY`, `DESCRIBE` and `INFORMATION_SCHEMA` metadata, and the `VECTOR_COSINE_SIMILARITY`, `VECTOR_INNER_PRODUCT`, `VECTOR_L1_DISTANCE` and `VECTOR_L2_DISTANCE` functions. Results carry Snowflake's `vector` column metadata, as JSON arrays or Arrow fixed-size lists.
