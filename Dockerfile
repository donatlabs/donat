# Build the donat engine binary, then ship a slim runtime image.
# Published to ghcr.io/donatlabs/donat by .github/workflows/release.yml.
# Build context is the repository root.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p donat-server --bin donat

# The admin panel, built here so the image is one container and one process:
# the engine serves these files itself (`DONAT_ADMIN_DIR`), which is what puts
# the panel, the identity provider proxy and the API on one origin without a
# reverse proxy in front of anything. See
# `knowledgebase/platform/decisions/001-*`, amended.
#
# `VITE_*` is inlined at build time, so the two settings that are a
# deployment's own are build arguments. The defaults suit the common case: an
# engine on the same origin, and the role name most deployments use. A
# deployment that calls its operator something else builds this image itself —
# one `--build-arg` — or leaves `DONAT_ADMIN_DIR` empty and serves the panel
# however it likes.
FROM node:22-bookworm-slim AS panel
WORKDIR /app
COPY apps/admin/package.json apps/admin/package-lock.json ./
RUN npm ci
COPY apps/admin/ ./
ARG VITE_DONAT_GRAPHQL_URL=/v1/graphql
ARG VITE_DONAT_ROLE=admin
ARG VITE_DONAT_IDP_BASE=/auth/v1
ARG VITE_DONAT_IDP_REGISTRATION=false
RUN npm run build

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

# The panel's built files. Serving them is opt-in by directory, and this image
# opts in — set `DONAT_ADMIN_DIR=` (empty) and the engine mounts nothing, which
# is exactly what it did before this existed.
COPY --from=panel /app/dist /usr/share/donat/admin/
ENV DONAT_ADMIN_DIR=/usr/share/donat/admin

# Not root.
#
# The engine reads its metadata, talks to a database and answers on 8080 —
# nothing it does needs the machine. Running as root anyway means a bug here is
# a bug with every capability the container has, and 8080 is unprivileged, so
# there is nothing to give up.
RUN useradd --system --uid 10001 --create-home --home-dir /var/lib/donat donat
USER 10001:10001

EXPOSE 8080
ENTRYPOINT ["donat"]
