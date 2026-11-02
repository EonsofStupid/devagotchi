# syntax=docker/dockerfile:1.7
# The Devagotchi hub as an image. GitHub builds it from a known commit (CI, `hub image`);
# `docker compose up --build` builds the same thing from a checkout. The one binary
# (D20) is all it holds: no shell, no package manager, running as nonroot.
ARG RUST_IMAGE=rust:1.99.1-slim-bookworm@sha256:24707e70a4f1e23d9a7b25202ec43126d28a52294b96aad5c670df1fd1f1d400
ARG RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f

FROM ${RUST_IMAGE} AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p devagotchi --bin devagotchi \
 && mkdir -p /out/state \
 && cp target/release/devagotchi /out/devagotchi

FROM ${RUNTIME_IMAGE}
LABEL org.opencontainers.image.source="https://github.com/EonsofStupid/devagotchi" \
      org.opencontainers.image.description="Devagotchi hub: authenticated MCP for AI clients, routed to the machines that link to it"
COPY --from=build /out/devagotchi /usr/local/bin/devagotchi
# The state directory, owned by nonroot so a fresh named volume takes its ownership.
COPY --from=build --chown=65532:65532 --chmod=700 /out/state/ /var/lib/devagotchi/
USER nonroot
EXPOSE 8787
ENTRYPOINT ["/usr/local/bin/devagotchi"]
CMD ["hub", "serve", "--state-dir", "/var/lib/devagotchi", "--config", "/etc/devagotchi/hub.json"]
