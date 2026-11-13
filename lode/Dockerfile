# syntax=docker/dockerfile:1
# The Lode server, `loded`, with the `lode` client beside it for administration.
# Build from the lode/ directory:  docker build -t loded .
# Data lives in the volume /var/lib/lode. On first start the entrypoint runs `loded init`,
# which prints the admin token once to the container log.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p loded -p lode-cli \
    && install -D -t /out target/release/loded target/release/lode

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --user-group --home-dir /var/lib/lode \
        --shell /usr/sbin/nologin lode \
    && mkdir -p /var/lib/lode \
    && chown lode:lode /var/lib/lode
# Root certificates for outgoing HTTPS (webhooks), from the build image.
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build /out/loded /out/lode /usr/local/bin/
COPY --chmod=0755 packaging/docker/entrypoint.sh /usr/local/bin/loded-entrypoint
USER lode
ENV LODED_DATA=/var/lib/lode
VOLUME /var/lib/lode
EXPOSE 7480
ENTRYPOINT ["loded-entrypoint"]
CMD ["serve"]
