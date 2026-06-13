# Build the donat engine binary, then ship a slim runtime image.
# Published to ghcr.io/donatlabs/donat by .github/workflows/release.yml.
# Build context is the repository root.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p donat-server --bin donat

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/donat /usr/local/bin/donat
EXPOSE 8080
ENTRYPOINT ["donat"]
