# Build from the repository root:
#   docker build -f deploy/Dockerfile -t wb2api:local .
FROM rust:1.99-alpine3.23 AS builder

RUN apk add --no-cache build-base musl-dev linux-headers perl pkgconf
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY assets ./assets
COPY crates ./crates
COPY gateway ./gateway

ENV OPENSSL_STATIC=1 \
    OPENSSL_NO_VENDOR=0 \
    RUSTFLAGS="-C target-feature=+crt-static"
RUN target="$(rustc -vV | awk '/^host: / {print $2}')" \
    && cargo build --locked --release --package wb2api --features vendored-openssl --target "$target" \
    && mkdir -p /out \
    && cp "target/$target/release/wb2api" /out/wb2api \
    && readelf --program-headers /out/wb2api > /out/elf-headers.txt \
    && if grep -q INTERP /out/elf-headers.txt; then \
         printf '%s\n' 'Expected a static musl executable, found an ELF interpreter' >&2; exit 1; \
       fi

FROM alpine:3.23 AS runtime
RUN apk add --no-cache ca-certificates tzdata \
    && addgroup -S -g 10001 wb2api \
    && adduser -S -D -H -u 10001 -G wb2api wb2api \
    && mkdir -p /data/accounts /data/usage \
    && chown -R wb2api:wb2api /data \
    && chmod 0700 /data/accounts /data/usage
COPY --from=builder --chmod=0555 /out/wb2api /usr/local/bin/wb2api
COPY LICENSE NOTICE.md /usr/share/licenses/wb2api/
ENV TZ=Etc/UTC \
    SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
WORKDIR /data
USER 10001:10001
EXPOSE 8788
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/wb2api"]
CMD ["--host", "0.0.0.0", "--port", "8788", "--accounts-dir", "/data/accounts", "--usage-dir", "/data/usage"]
