# The image release binaries are built in (ADR-020): the oldest release
# on the host spec's runtime list, so the binary needs its glibc (2.35)
# at most and loads on every newer one. cross 0.2.5 builds it from
# Cross.toml's `dockerfile` and runs cargo in it with the host's
# toolchain mounted:
# https://github.com/cross-rs/cross/blob/v0.2.5/docs/cross_toml.md#targettargetdockerfile
# Pinned by digest (sb-L8): a tag can be moved to another image, a digest
# can't. It is the multi-architecture index's digest of ubuntu:22.04 as
# pulled on 2026-11-18 (image created 2026-11-10). It is bumped by hand
# (or by Renovate once configured), with build/qemu.Dockerfile's beside it.
# https://docs.docker.com/reference/dockerfile/#from
FROM ubuntu:22.04@sha256:74b12b11ffa550fba1929ad4d8d16533c34f6612728469671afba1b03b66881c
ARG DEBIAN_FRONTEND=noninteractive
# gcc and libc6-dev: the C compiler and glibc headers aws-lc-sys builds
# with, and the linker cargo calls (`cc`).
# gcc-aarch64-linux-gnu and libc6-dev-arm64-cross: the same for arm64,
# with Ubuntu's arm64 glibc (the same 2.35).
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      gcc libc6-dev gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
 && rm -rf /var/lib/apt/lists/*
# cargo's linker and the cc crate's compiler and archiver for arm64, as
# cross's own aarch64 image sets them (cc would also derive them from the
# `aarch64-linux-gnu` prefix; set here so the build doesn't depend on it):
# https://doc.rust-lang.org/cargo/reference/config.html#targettriplelinker
# https://docs.rs/cc/1.10.0/cc/#external-configuration-via-environment-variables
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar
