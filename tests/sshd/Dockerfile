# Throwaway sshd for hopbox integration tests. No systemd, so jobs run uncapped.
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends openssh-server rsync util-linux bash procps socat \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --uid 1001 --shell /bin/bash hopbox \
 && mkdir -p /run/sshd
COPY entrypoint.sh /entrypoint.sh
RUN chmod 0755 /entrypoint.sh
EXPOSE 22
ENTRYPOINT ["/entrypoint.sh"]
