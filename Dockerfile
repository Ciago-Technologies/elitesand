FROM debian:bookworm-slim

RUN groupadd --system ciagomem && useradd --system --gid ciagomem --home-dir /var/lib/ciagomem ciagomem \
    && mkdir -p /var/lib/ciagomem/data \
    && chown -R ciagomem:ciagomem /var/lib/ciagomem

COPY target/release/ciagomem-server /usr/local/bin/ciagomem-server

RUN chmod 0755 /usr/local/bin/ciagomem-server

USER ciagomem
WORKDIR /var/lib/ciagomem
VOLUME ["/var/lib/ciagomem/data"]
ENTRYPOINT ["/usr/local/bin/ciagomem-server"]
