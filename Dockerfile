# Build the donat engine binary, then ship a slim runtime image.
# Published to ghcr.io/donatlabs/donat by .github/workflows/release.yml.
# Build context is the repository root.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p donat-server --bin donat

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/donat /usr/local/bin/donat

# The engine's own `donat.*` schema — cron state, the event log, command claims,
# the durable Process journals. The serving process still runs no DDL; these are
# for the `migrate` subcommand, which is the only thing in this image that does.
#
# They ship here because an application that embeds or deploys this engine has
# to apply them, and the alternative was for every such repository to vendor a
# copy that nothing checks against the binary it runs beside. A copy that can
# disagree with its engine is a copy that eventually does.
COPY migrations/ /usr/share/donat/migrations/

EXPOSE 8080
ENTRYPOINT ["donat"]
