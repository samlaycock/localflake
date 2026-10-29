# syntax=docker/dockerfile:1

# Build. The engine's build script downloads DuckDB's ICU extension (time
# zones) for the target platform, so the image never needs the network.
FROM rust:1.98-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p localflake-server \
    && cp target/release/localflake /usr/local/bin/localflake
RUN mkdir /data

# Run: distroless with the C++ runtime DuckDB needs, as a non-root user.
FROM gcr.io/distroless/cc-debian13:nonroot
COPY --from=build /usr/local/bin/localflake /usr/local/bin/localflake
COPY --from=build --chown=nonroot:nonroot /data /data
ENV LOCALFLAKE_ADDR=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/localflake"]
