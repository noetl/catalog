# catalog-server — the internal catalog's HTTP surface over an EHDB-backed store.
#
# ⚠ WHY THIS FILE EXISTS
#
# The crate had 157 passing tests, an acceptance suite driven entirely over HTTP, and
# **no Dockerfile and no release workflow** — so it ran nowhere. A catalog that cannot be
# deployed is not complete, however well tested: "built" and "reachable" are independent
# questions, which is the same lens that found seven store methods with no API caller.

# ---------------------------------------------------------------- builder
# Pinned to the SAME 1.99.0 the workspace's rust-toolchain.toml pins. Naming a different
# version here would create two authorities that can disagree — the class of bug behind
# noetl/server's 2026-08-03 version regression.
FROM docker.io/library/rust:1.99.0-alpine3.22 AS builder
WORKDIR /app

# `git` is required, not optional: ehdb-l0 and ehdb-core are git dependencies pinned to
# noetl/ehdb@v0.4.5. That repo is public and the tag resolves unauthenticated (verified),
# so the build needs no credentials — but it does need a git client.
# `musl-dev` for the static link; nothing else, because the workspace has no
# openssl/native-tls dependency.
RUN apk add --no-cache git musl-dev

# Dependency layer first, so a source-only change does not refetch and rebuild the
# dependency graph. The dummy sources are the smallest thing that makes `cargo build`
# resolve the workspace without the real code.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/catalog-model/Cargo.toml   crates/catalog-model/Cargo.toml
COPY crates/catalog-store/Cargo.toml   crates/catalog-store/Cargo.toml
COPY crates/catalog-extract/Cargo.toml crates/catalog-extract/Cargo.toml
COPY crates/catalog-ingest/Cargo.toml  crates/catalog-ingest/Cargo.toml
COPY crates/catalog-api/Cargo.toml     crates/catalog-api/Cargo.toml
RUN set -eu; \
    for c in catalog-model catalog-store catalog-extract; do \
      mkdir -p "crates/$c/src" && echo '' > "crates/$c/src/lib.rs"; \
    done; \
    mkdir -p crates/catalog-ingest/src crates/catalog-api/src; \
    echo '' > crates/catalog-ingest/src/lib.rs; \
    echo 'fn main() {}' > crates/catalog-ingest/src/main.rs; \
    echo '' > crates/catalog-api/src/lib.rs; \
    echo 'fn main() {}' > crates/catalog-api/src/main.rs; \
    cargo build --release --bin catalog-server; \
    rm -rf crates/*/src

# Now the real sources. Touching them defeats any stale mtime-based caching of the
# dummy build above, which would otherwise ship an empty binary that starts and serves
# nothing — a silent-wrong-answer shape.
COPY . .
RUN set -eu; \
    find crates -name '*.rs' -exec touch {} +; \
    cargo build --release --bin catalog-server; \
    # Assert the binary is real. A 0-byte or missing artifact must fail the BUILD, not
    # surface later as a container that exits immediately.
    test -s target/release/catalog-server

# ---------------------------------------------------------------- runtime
FROM docker.io/library/alpine:3.22.2 AS runtime
WORKDIR /app
RUN apk add --no-cache ca-certificates libgcc \
    # ⚠ `curl` is deliberately installed. noetl's other images have NO http client,
    # and four separate `build_info lines=0` readings during the 2026-08 metric work
    # were that missing binary rather than a missing metric. A container whose health
    # cannot be probed from inside is a container whose health is unknowable.
    curl \
    # `git` so /api/catalog/ingest can read a `git:<repo>@<ref>` source, which is the
    # form that reads the REF rather than a working tree — a stale checkout being the
    # most reliable way to produce a confident zero.
    git

COPY --from=builder /app/target/release/catalog-server ./catalog-server

# The store is on disk and must outlive the container. Mount a volume here or the
# catalog is reset on every restart — which would look exactly like an empty catalog.
RUN mkdir -p /data/catalog
VOLUME ["/data/catalog"]

ENV CATALOG_STORE_ROOT=/data/catalog \
    CATALOG_BIND=0.0.0.0:8091 \
    RUST_LOG=info,catalog_api=debug,catalog_store=debug

EXPOSE 8091

# ⚠ No HEALTHCHECK on purpose: Kubernetes probes are declared in the Deployment, and a
# Dockerfile HEALTHCHECK is ignored there — having one would be a second authority that
# reads as coverage while contributing nothing.

ENTRYPOINT ["./catalog-server"]
