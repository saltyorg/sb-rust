# Saltbox schemas, saphyr scalars, public suffix and tz crates

Verified 2026-11-03 for the `config` module (config plan, Task 1).

**Software:** Saltbox's `schema/*.schema.yml` at
https://github.com/saltyorg/Saltbox commit
6830b26bf004078f4d047fcf2a6abf7fc7a78131 (2026-11-01, GPL-3.0); sb-go's
`saltbox/yaml_schema.go` and `saltbox/custom_validators.go` (read);
saphyr 0.3.3 and saphyr-parser 0.3.3; regex 1.13.1; psl 2.1.246;
jiff 0.2.39.

**Method:** a fresh clone of the public repository (never a live
checkout); a script walking every rule node (a mapping with `type`) of the
six schemas and counting keys, types, validators and formats; reading
sb-go's `SchemaRule`; a scratch crate loading YAML with saphyr and walking
saphyr-parser's events; jiff lookups with `TZDIR` pointed at fixture
directories. The pinned copies go into sb-config's test fixtures with a
provenance note when the crate is created (Task 4). gitleaks finds no
leaks in them. The only email address in them is the placeholder that
accounts.schema.yml's `user.email` forbids with `not_equals`.

## Inventory

Six schemas: accounts, adv_settings, backup_config, hetzner_vlan, motd,
settings.

- **Rule keys** (number of rule nodes using each): `type` 206, `required`
  186, `example` 72, `properties` 51, `items` 14, `description` 13,
  `pattern` 9, `required_with` 6, `not_equals` 3, `min_length` 2,
  `format` 2, `custom_validator` 2, `additionalProperties` 1,
  `required_when_true` 1, `validate_when_true` 1. No `max_length`, though
  sb-go supports it.
- **Types:** object 52, string 57, boolean 23, ansible_bool 22, array 14,
  number 14, url 11, subdomain 3, directory_path 3, hostname 2, password
  1, ssh_key_or_url 1, timezone 1, cron_time 1, rclone_template 1. No
  `float`.
- **`format`:** `email` only (2 uses).
- **`custom_validator`:** `validate_cloudflare_config` and
  `validate_dockerhub_config`, once each. `validate_rclone_remote` isn't
  used, which confirms the spec's decision 6.
- **`additionalProperties`:** one use, motd's `systemd.display_names`: a
  rule (`type: string`) on an object with no `properties`. No `true` or
  `false` form.
- **`pattern`:** nine uses, all in motd, all the same:
  `^#([A-Fa-f0-9]{6}|[A-Fa-f0-9]{3})$` (a hex colour). sb-go's
  `SchemaRule` has neither `pattern` nor `additionalProperties`, so its
  YAML decoder ignores both keys.

## The patterns under Rust's regex

`^#([A-Fa-f0-9]{6}|[A-Fa-f0-9]{3})$`:

- **Compiles** under regex 1.13.1 and matches `#1a2B3c` and `#abc`, not
  `abc`, `#abcd` or `#abc\n`.
- **Same meaning as ECMA-262:** ASCII character classes only (no `\d`,
  `\w` or `\b`, whose Unicode reading in Rust differs); without the
  multi-line flag `$` matches only at the end of the text in both, so a
  trailing newline doesn't match.
- **Anchored:** `^…$`, so the unanchored search the spec prescribes gives
  the same result as a full match.

## saphyr keeps what scalar resolution needs

- With a `YamlLoader<MarkedYaml>` set to `early_parse(false)`, every scalar is
  `YamlData::Representation(text, style, tag)`. For plain scalars the text
  is the source text: `017`, `0x1F`, `1_000`, `yes`,
  `99999999999999999999` arrive unchanged with `ScalarStyle::Plain`.
  Quoted scalars arrive unescaped with their style (`"q\x41"` → `qA`,
  `DoubleQuoted`), and they are always strings anyway. So sb can resolve
  plain scalars by yaml.v3's rules itself. An explicit tag (`!!str 017`)
  arrives in `Representation`'s tag; yaml.v3 honours such tags, so sb's
  resolver must too.
- The loader rejects a duplicate key by default and stops there
  (`allow_duplicate_keys`, default `false`). Since sb reports duplicates
  from its own event pass first, with every position, the loader that
  follows is set to `allow_duplicate_keys(true)` or isn't reached for a
  file with duplicates (Task 5 decides which).
- saphyr-parser's `Parser` iterates `(Event, Span)`; every `Scalar`,
  `MappingStart` and `MappingEnd` event carries its start marker, enough
  for the duplicate-key pass. Lines are 1-based and columns 0-based,
  counted in characters (not bytes), as yaml.v3 counts them; sb adds 1 to
  columns to report yaml.v3's 1-based columns.

## Crate choices

- **Public suffix: `psl` 2.1.x.** It is generated from the full public
  suffix list, ICANN and private sections (as Go's
  `golang.org/x/net/publicsuffix`, which sb-go uses for
  `EffectiveTLDPlusOne`). It is compiled in, so lookups need no file or
  network. It comes from https://github.com/addr-rs/psl, and its patch
  releases track the list (2.1.246 at the time). The alternative,
  `publicsuffix`, parses a list at run time, which sb would have to
  vendor and keep updated, or fetch. `psl::domain` gives `example.co.uk` for
  `www.example.co.uk` (ICANN) and `foo.github.io` for `foo.github.io`
  (private section). Cost: about
  640 KiB of code (see [musl-c-dependencies.md](musl-c-dependencies.md)).
- **Timezone: `jiff` 0.2** with `default-features = false` and features
  `std`, `tz-system`, `tzdb-zoneinfo`: it reads the system's zoneinfo
  directory (no embedded database), as Go's `time.LoadLocation` does
  without `time/tzdata`, with the differences listed below.
  `TimeZone::get` uses a database from `TZDIR`
  when that directory holds zone files. An empty or missing `TZDIR` falls
  back to the standard system directories, so tests need a fixture
  directory that is not empty. With such a fixture holding only `Etc/UTC`
  and `Fake_Zone`, `Fake_Zone` is found and `Europe/Copenhagen` is not.
  sb-go's special names (`auto` in any case, which its validator accepts
  before `LoadLocation`; Go's `UTC`, `Local` and the empty string) are
  sb's to handle (Task 7).

### jiff's lookups differ from Go's `LoadLocation`

Read in jiff 0.2.39's `src/tz/db/zoneinfo/enabled.rs` (identical in 0.2.38) and Go's
`time.LoadLocation` documentation (it reads the file named by the name,
under `ZONEINFO` or the system directories):

1. **Case:** jiff falls back to an ASCII case-insensitive match
   (`cmp_ignore_ascii_case`), so `europe/copenhagen` resolves. Go on Linux
   opens the file by its exact name and rejects it.
2. **`posix/` and `right/`:** jiff skips these top-level directories when
   listing zones, so `posix/Europe/Berlin` doesn't resolve. Go accepts it
   where the file exists.
3. **The environment variable:** jiff honours `TZDIR`; Go honours
   `ZONEINFO`, not `TZDIR`.

The spec says names are checked "as Go's `LoadLocation`", so these are
for the maintainer at Checkpoint A: list them as differences, or have sb
make up for them (a name accepted only when it equals the resolved
zone's name exactly; `posix/` and `right/` names handled), for example
by resolving the file path as Go does and parsing it with jiff's
`TimeZone::tzif`.
