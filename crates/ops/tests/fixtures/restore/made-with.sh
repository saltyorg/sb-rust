#!/bin/sh
# How restore's fixtures were made (ops spec, Testing Strategy): OpenSSL
# 3's `enc -aes-256-cbc -pbkdf2`, as Saltbox's backup role writes its
# uploads (a random salt, so `Salted__` and the salt lead the file;
# PBKDF2-HMAC-SHA256, 10000 iterations, the defaults), over synthetic
# content. Made once with OpenSSL 3.0.2 and committed: each run picks a
# new salt, so a rerun gives other bytes for the same plaintexts. Why not
# -S for a fixed salt: OpenSSL 3 then leaves the `Salted__` header out,
# which the backup role's files always have.
set -eu
cd "$(dirname "$0")"
password='synthetic-restore-pass'
printf 'settings:\n  synthetic: true\n' > settings.yml
: > empty.yml
i=1
: > long.conf
while [ "$i" -le 200 ]; do
  printf 'line %03d of a synthetic rclone-style config\n' "$i" >> long.conf
  i=$((i + 1))
done
for plaintext in settings.yml empty.yml long.conf; do
  openssl enc -aes-256-cbc -pbkdf2 -k "$password" -in "$plaintext" -out "$plaintext.enc"
done
