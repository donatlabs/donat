# Build the dist-api engine binary, then ship a slim runtime image.
# Published to ghcr.io/pantyukhov/dist-api by .github/workflows/release.yml.
# Build context is the repository root.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p dist-server --bin dist-api

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/dist-api /usr/local/bin/dist-api
EXPOSE 8080
ENTRYPOINT ["dist-api"]
