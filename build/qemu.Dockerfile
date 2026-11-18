# The image check.sh full runs the aarch64 production binary in, on an
# x86_64 host, under QEMU user-mode: qemu-user-static runs it, and Ubuntu
# 22.04's arm64 glibc (libc6-arm64-cross, the same 2.35 the cross image
# links against, ADR-020) and libgcc_s (libgcc-s1-arm64-cross, which
# Rust's unwinder needs) are its loader and libraries, found through
# qemu's -L prefix. No binfmt handler is registered: qemu is called by
# name, so the host's kernel is left as it is.
# https://www.qemu.org/docs/master/user/main.html#command-line-options
FROM ubuntu:22.04@sha256:74b12b11ffa550fba1929ad4d8d16533c34f6612728469671afba1b03b66881c
ARG DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends qemu-user-static libc6-arm64-cross libgcc-s1-arm64-cross \
 && rm -rf /var/lib/apt/lists/*
