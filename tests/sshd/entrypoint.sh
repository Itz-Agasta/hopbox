#!/bin/sh
set -eu
install -d -m 0700 -o hopbox -g hopbox /home/hopbox/.ssh
printf '%s\n' "$AUTHORIZED_KEY" > /home/hopbox/.ssh/authorized_keys
chown hopbox:hopbox /home/hopbox/.ssh/authorized_keys
chmod 0600 /home/hopbox/.ssh/authorized_keys
ssh-keygen -A >/dev/null
exec /usr/sbin/sshd -D -e
