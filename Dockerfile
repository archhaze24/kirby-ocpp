# Runtime image from PRE-BUILT static musl binaries (see docs/distribution.md).
# Build context layout (release.yml fills it from the release artifacts):
#   dist/linux-amd64/{kirby-ocpp,kirby-csms}   x86_64-unknown-linux-musl
#   dist/linux-arm64/{kirby-ocpp,kirby-csms}   aarch64-unknown-linux-musl
#   docker buildx build --platform linux/amd64,linux/arm64 -t IMAGE .
# To compile from a source checkout instead, use Dockerfile.dev.
FROM alpine:3.24

ARG TARGETARCH
ARG VERSION=dev
ARG REVISION=unknown

LABEL org.opencontainers.image.title="kirby-ocpp" \
      org.opencontainers.image.description="OCPP 1.6J charge point emulator (kirby-ocpp) and central system (kirby-csms)" \
      org.opencontainers.image.source="https://github.com/archhaze24/kirby-ocpp" \
      org.opencontainers.image.url="https://github.com/archhaze24/kirby-ocpp" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}"

# wss:// uses the system root store.
RUN apk add --no-cache ca-certificates \
 && adduser -D -u 10001 kirby-ocpp \
 && mkdir /data \
 && chown kirby-ocpp:kirby-ocpp /data

COPY dist/linux-${TARGETARCH}/kirby-ocpp dist/linux-${TARGETARCH}/kirby-csms /usr/local/bin/
COPY docker/entrypoint.sh /usr/local/bin/docker-entrypoint.sh

ENV KIRBY_OCPP_STATE_DIR=/data
USER kirby-ocpp
WORKDIR /data
VOLUME /data
EXPOSE 9000

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
CMD ["--help"]
