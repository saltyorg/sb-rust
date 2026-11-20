# Saltbox schemas (pinned)

Copied unchanged from the public Saltbox repository's `schema/` directory,
for sb-config's tests (config spec, Testing strategy; docker-free, never a
live checkout).

- Repository: https://github.com/saltyorg/Saltbox
- Commit: 6830b26bf004078f4d047fcf2a6abf7fc7a78131 (2026-11-01)
- Licence: GPL-3.0, as the Saltbox repository

The pin moves by hand, with sb-config's schema tests run against the new
copy.

## providers.schema.yml (sb's draft)

Not from Saltbox, which ships no schema for providers.yml yet: sb's
draft for Saltbox's `schema/` (config spec, decision 10; 2026-11-20),
written from Saltbox's `defaults/providers.yml.default` and the
variables `roles/traefik/defaults/main.yml` reads, at commit
3f4f2a80c7d0af8b27e1ce39a55b7745cbbaa35d. Replace it with Saltbox's
copy, unchanged, once it lands there.
