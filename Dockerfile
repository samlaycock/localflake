# syntax=docker/dockerfile:1

# Build. The engine's build script downloads DuckDB's ICU extension (time
# zones) for the target platform, so the image never needs the network.
FROM rust:1.98-trixie AS build
ARG TARGETARCH
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
# Link DuckDB's prebuilt library for the pinned release instead of compiling
# DuckDB from source, and ship it beside the binary (rpath $ORIGIN).
RUN version=$(sed -n 's/^const DUCKDB_VERSION: &str = "\(.*\)";$/\1/p' crates/localflake-engine/build.rs) \
    && mkdir -p /opt/libduckdb \
    && curl -fsSL "https://install.duckdb.org/$version/duckdb-shared-libs-linux-$TARGETARCH.tar.gz" | tar -xz -C /opt/libduckdb
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    DUCKDB_LIB_DIR=/opt/libduckdb DUCKDB_DOWNLOAD_LIB=0 RUSTFLAGS='-C link-arg=-Wl,-rpath,$ORIGIN' \
    cargo build --release --locked -p localflake-server --no-default-features \
    && mkdir -p /out \
    && cp target/release/localflake /opt/libduckdb/libduckdb.so /out/
RUN mkdir /data

# Run: distroless with the C++ runtime DuckDB needs, as a non-root user.
FROM gcr.io/distroless/cc-debian13:nonroot
COPY --from=build /out/localflake /out/libduckdb.so /usr/local/bin/
COPY --from=build --chown=nonroot:nonroot /data /data
ENV LOCALFLAKE_ADDR=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/localflake"]
