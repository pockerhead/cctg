# syntax=docker/dockerfile:1
#
# cctg hub image (TASK-035): a static musl binary on Alpine, run as a
# non-root user, state in the /data volume. The same binary is the Linux
# client in GitHub Releases (`--target binary`). Build context: the repo root;
# `.dockerignore` lets in only the workspace sources, never `.env`.
#
#   docker build --build-arg CCTG_BUILD_ID=$(git rev-parse HEAD) -t cctg .
#   docker build --build-arg CCTG_BUILD_ID=... --target binary --output out .

ARG RUST_VERSION=1.95

FROM rust:${RUST_VERSION}-alpine AS build
# aws-lc-sys (rustls' crypto) needs only a C compiler outside FIPS mode
# (aws-lc-rs docs, requirements/linux), plus the kernel headers: its
# crypto/rand_extra/urandom.c includes <linux/random.h>, which musl-dev does
# not ship. The Rust image tag pins Alpine.
# hadolint ignore=DL3018
RUN apk add --no-cache musl-dev gcc linux-headers
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
# The commit this image is built from: the hub's build, compared with the
# clients' (no .git in the context). Empty: the binary's hash stands in.
ARG CCTG_BUILD_ID=
# The release tag of a tag build (TASK-050): its hub offers the clients that
# release's binaries. Empty: no download, clients update from their disk.
ARG CCTG_RELEASE=
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    CCTG_BUILD_ID="${CCTG_BUILD_ID}" CCTG_RELEASE="${CCTG_RELEASE}" \
    cargo build --release --locked -p cctg \
    && cp target/release/cctg /cctg \
    && /cctg --version

# `--target binary --output <dir>`: just the static Linux binary.
FROM scratch AS binary
COPY --from=build /cctg /cctg

FROM alpine:3.21
RUN adduser -D -H -u 10001 cctg && mkdir /data && chown cctg /data
COPY --from=build /cctg /usr/local/bin/cctg
USER 10001
WORKDIR /data
# Both listeners on every interface of the container; TLS comes from
# CCTG_TLS_CERT and CCTG_TLS_KEY in the env file (docs/remote-hub.md).
ENV CCTG_STATE_DIR=/data \
    CCTG_AGENT_LISTEN=0.0.0.0:47291 \
    CCTG_HOOK_LISTEN=0.0.0.0:47292
VOLUME /data
EXPOSE 47291 47292
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD ["cctg", "health"]
STOPSIGNAL SIGTERM
ENTRYPOINT ["cctg"]
CMD ["hub"]
