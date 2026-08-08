# Build the standalone lending-golang example.
#
# This binary embeds the Donat Rust engine as a WebAssembly module (wazero,
# CGO_ENABLED=0) and serves GraphQL from the same Go process. No Rust binary
# is needed at runtime; the wasm is embedded via //go:embed in the SDK package.
#
# Build context is the REPOSITORY ROOT (see docker-compose.yml) because the
# module uses a `replace github.com/donatlabs/donat/sdk/go => ../../sdk/go`
# directive: Docker needs to see both trees at build time.
#
# Once the SDK module is published/tagged to pkg.go.dev this Dockerfile can
# be simplified to build from the example directory alone.

FROM golang:1.22-bookworm AS build

WORKDIR /src
# Copy only what the build needs: the SDK and this example.
COPY sdk/go ./sdk/go
COPY examples/lending-golang ./examples/lending-golang

WORKDIR /src/examples/lending-golang
# The wasm core is a build output kept out of the branches, so it is not in a
# fresh checkout — `//go:embed` would fail here with "no matching files found",
# which says nothing about the cause. Build it first:
#
#     make wasm-core && docker compose up --build
#
RUN test -f ../../sdk/go/donat/wasm/core.wasm || { \
      echo "the wasm core is missing: run 'make wasm-core' in the repository root first" >&2; \
      exit 1; \
    }
# CGO_ENABLED=0 → pure-Go, static binary (no libc dependency).
RUN CGO_ENABLED=0 go build -o /lending .

# Distroless "static" has no shell, no libc, no package manager — smallest
# possible attack surface for a fully-static Go binary.
FROM gcr.io/distroless/static-debian12

COPY --from=build /lending /lending
EXPOSE 8080
ENTRYPOINT ["/lending"]
