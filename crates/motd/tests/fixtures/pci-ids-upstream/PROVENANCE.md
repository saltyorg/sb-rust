# pci.ids (pinned)

Copied unchanged from pciutils' release v3.15.0, for sb-motd's test that
the byte parser gives the reference parser's names on the whole upstream
database (`gpu.rs`, `the_parser_matches_the_text_parser_on_upstream_pci_ids`).
No test reads a host's `/usr/share/misc/pci.ids`.

- Source: https://raw.githubusercontent.com/pciutils/pciutils/v3.15.0/pci.ids
- Release: pciutils v3.15.0, tag commit
  b424ac8b498317965bfd3ab33ae21b158a7f1dd2 (2026-04-05)
- The file's header: `Version: 2026.04.05`, `Date: 2026-04-05 21:15:31`
- Git blob: bc13213d6427951c149940b1af4401c7a7bff36b, the tag's
  `pci.ids`; SHA-256
  f2b4082bec8b9c7734997fd16f6831c2dd68c21a7ab749895037f804a362b47b
- Licence: GPL-2.0-or-later or BSD-3-Clause, the file's own header (the
  PCI ID Project, https://pci-ids.ucw.cz/)

The pin moves by hand: copy a later release's `pci.ids` unchanged,
update this file, and update the test's counts from the new copy.
