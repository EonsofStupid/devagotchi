# syntax=docker/dockerfile:1.7
# The Devagotchi hub as an image. GitHub builds it from a known commit (CI, `hub image`);
# `docker compose up --build` builds the same thing from a checkout. The one binary
# (D20) is all it holds: no shell, no package manager, running as nonroot. GitHub's build
# also carries the builds the hub hands to joining machines (`/join`, `/dist`): the Linux
# and Windows archives of the same commit, for x86_64 and ARM64, and both installers.
ARG RUST_IMAGE=rust:1.99.1-slim-bookworm@sha256:24707e70a4f1e23d9a7b25202ec43126d28a52294b96aad5c670df1fd1f1d400
ARG RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f

FROM ${RUST_IMAGE} AS build
# The build's name `devagotchi --version` prints (crates/devagotchi/build.rs): GitHub's build
# passes the downloads VERSION (image.yml); a local build is `dev`.
ARG DEVAGOTCHI_BUILD=dev
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p devagotchi --bin devagotchi \
 && mkdir -p /out/state \
 && cp target/release/devagotchi /out/devagotchi

# The builds the hub hands out: empty here, so a build from a checkout serves none. GitHub's
# build replaces this stage with the directory scripts/stage-downloads.sh laid down from the
# same commit's release artifacts (`--build-context downloads=DIR`, image.yml).
FROM scratch AS downloads

FROM ${RUNTIME_IMAGE}
# The commit the image was built from: GitHub's build sets it (image.yml), and update.sh
# fetches the deploy files of that same commit with the image. A local build leaves it
# empty, and update.sh then keeps the deploy files it has.
ARG REVISION=""
LABEL org.opencontainers.image.source="https://github.com/EonsofStupid/devagotchi" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.description="Devagotchi hub: authenticated MCP for AI clients, routed to the machines that link to it"
COPY --from=build /out/devagotchi /usr/local/bin/devagotchi
# Owned by root, read-only to the hub, which serves it when its config names no other
# directory (downloads.rs BUILT_IN).
COPY --from=downloads / /usr/share/devagotchi/downloads/
# The state directory, owned by nonroot so a fresh named volume takes its ownership.
COPY --from=build --chown=65532:65532 --chmod=700 /out/state/ /var/lib/devagotchi/
USER nonroot
EXPOSE 8787
ENTRYPOINT ["/usr/local/bin/devagotchi"]
CMD ["hub", "serve", "--state-dir", "/var/lib/devagotchi", "--config", "/etc/devagotchi/hub.json"]
