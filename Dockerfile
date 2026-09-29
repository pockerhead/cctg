# syntax=docker/dockerfile:1
#
# cctg hub image (TASK-035): a static musl binary on Debian slim, run as a
# non-root user, state in the /data volume, with the speech recognition
# helper `cctg-voice` and its model (TASK-085). The same static binary is the
# Linux client in GitHub Releases (`--target binary`). Build context: the
# repo root; `.dockerignore` lets in only the workspace sources, never `.env`.
#
#   docker build --build-arg CCTG_BUILD_ID=$(git rev-parse HEAD) -t cctg .
#   docker build --build-arg CCTG_BUILD_ID=... --target binary --output out .

ARG RUST_VERSION=1.95
# Voice recognition (TASK-085): the static sherpa-onnx libraries the helper
# links; the version must equal the sherpa-onnx pin in crates/voice/Cargo.toml.
# x86_64 only: the image is built for linux/amd64.
ARG SHERPA_ONNX_VERSION=1.13.8
ARG SHERPA_ONNX_SHA256=e1fdc5b67530e15741ef897fa5ffff297056f3bf0c6d829a27af9225a4c4b5a6
# alphacep/vosk-model-small-streaming-ru (Apache-2.0), int8 transducer.
ARG VOICE_MODEL_REV=e18123ee13f694036a1eea82eb43f9895387cb59

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

# The recognition helper: glibc and libstdc++, as the static sherpa-onnx
# libraries (with onnxruntime) exist only for glibc. Its own cargo
# workspace; the archive is checked before the build takes it.
FROM rust:${RUST_VERSION}-trixie AS voice
ARG SHERPA_ONNX_VERSION
ARG SHERPA_ONNX_SHA256
WORKDIR /src
RUN archive="sherpa-onnx-v${SHERPA_ONNX_VERSION}-linux-x64-static-lib.tar.bz2" \
    && mkdir /sherpa \
    && curl -fsSL -o "/sherpa/${archive}" \
       "https://github.com/k2-fsa/sherpa-onnx/releases/download/v${SHERPA_ONNX_VERSION}/${archive}" \
    && echo "${SHERPA_ONNX_SHA256}  /sherpa/${archive}" | sha256sum -c -
COPY crates/voice crates/voice
RUN --mount=type=cache,id=voice-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=voice-target,target=/src/crates/voice/target \
    SHERPA_ONNX_ARCHIVE_DIR=/sherpa \
    cargo build --release --locked --manifest-path crates/voice/Cargo.toml \
    && cp crates/voice/target/release/cctg-voice /cctg-voice \
    && /cctg-voice --version

# The model, pinned by revision and checked file by file.
FROM rust:${RUST_VERSION}-trixie AS model
ARG VOICE_MODEL_REV
WORKDIR /model
RUN base="https://huggingface.co/alphacep/vosk-model-small-streaming-ru/resolve/${VOICE_MODEL_REV}" \
    && curl -fsSL -o encoder.int8.onnx "${base}/am-onnx/encoder.int8.onnx" \
    && curl -fsSL -o decoder.int8.onnx "${base}/am-onnx/decoder.int8.onnx" \
    && curl -fsSL -o joiner.int8.onnx "${base}/am-onnx/joiner.int8.onnx" \
    && curl -fsSL -o tokens.txt "${base}/lang/tokens.txt" \
    && printf '%s  %s\n' \
       e0db705e94ec35d803b1df4f40cda23d064e1142977c80ab288430b109777a9d encoder.int8.onnx \
       2b0df458692e1d090075c8249001136ef05240dd0d726a6b56552fd46c538b2d decoder.int8.onnx \
       b55784b071ab7512eab4c7c44e4f5478284ef33c83562cc6a249b972515a31e5 joiner.int8.onnx \
       93bbbc0bae6b78c0bbb743d4aa9fded3bb5ff3aac5f0200e3a769a5a05e0fdf6 tokens.txt \
       | sha256sum -c -

FROM debian:trixie-slim
# ca-certificates: the Bot API client verifies Telegram against the system
# roots (reqwest 0.13, rustls-platform-verifier); without them it is not
# even built. Alpine had them in its base, Debian slim does not.
# hadolint ignore=DL3008
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --no-create-home --uid 10001 --shell /usr/sbin/nologin cctg \
    && mkdir /data && chown cctg /data
COPY --from=build /cctg /usr/local/bin/cctg
COPY --from=voice /cctg-voice /usr/local/bin/cctg-voice
COPY --from=model /model /usr/local/share/cctg/voice-ru
# The helper recognizes the fixture: the real engine and model, at build time,
# twice in one process (TASK-086: it answers requests, each the byte count on
# a line and then the bytes, until stdin closes). Its answer lines (took_ms,
# peak_rss_kb) go to the build log; the build fails unless both have the
# words.
RUN --mount=type=bind,source=crates/voice/tests/fixtures/voice-ru.ogg,target=/tmp/voice-ru.ogg \
    size="$(stat -c %s /tmp/voice-ru.ogg)" \
    && heard="$( { printf '%s\n' "${size}"; cat /tmp/voice-ru.ogg; \
                   printf '%s\n' "${size}"; cat /tmp/voice-ru.ogg; } \
                 | cctg-voice /usr/local/share/cctg/voice-ru )" \
    && echo "cctg-voice on the fixture, twice: ${heard}" \
    && test "$(echo "${heard}" | grep -c 'запусти тесты')" -eq 2     && test "$(echo "${heard}" | wc -l)" -eq 2
USER 10001
WORKDIR /data
# Both listeners on every interface of the container; TLS comes from
# CCTG_TLS_CERT and CCTG_TLS_KEY in the env file (docs/remote-hub.md). Voice
# messages are recognized with the helper and model above (TASK-085).
ENV CCTG_STATE_DIR=/data \
    CCTG_AGENT_LISTEN=0.0.0.0:47291 \
    CCTG_HOOK_LISTEN=0.0.0.0:47292 \
    CCTG_VOICE_HELPER=/usr/local/bin/cctg-voice \
    CCTG_VOICE_MODEL=/usr/local/share/cctg/voice-ru
VOLUME /data
EXPOSE 47291 47292
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD ["cctg", "health"]
STOPSIGNAL SIGTERM
ENTRYPOINT ["cctg"]
CMD ["hub"]
