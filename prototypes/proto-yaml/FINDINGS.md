# Prototype: proto-yaml

**Question:** Can the serde-saphyr / saphyr family cover `sb validate-config`:
schema-driven validation, duplicate-key rejection, and line/column errors, on
real Saltbox config?

**Crates tried:** serde-saphyr@2.0.0 (schema files), saphyr@0.3.3 (config
files as `MarkedYaml`), serde@1.0.229.

**What was built:** a validator that loads each `*.schema.yml` into a typed
`Rule` (sb-go's `SchemaRule`). It walks the matching config as a saphyr
`MarkedYaml` tree and collects every issue as `file:line:col: path: message`.
The rules covered are type, required, required_when_true, validate_when_true,
required_with, not_equals, min/max_length, email format, object/array
recursion, number, boolean/ansible_bool, url and additionalProperties.

## Result

- **Parity with sb-go.** On a real Saltbox config set (six schema-backed
  files plus `localhost.yml` duplicates-only), the prototype and sb-go's
  `validate-config` reach the same verdict.
- **The split works well.** sb-go's schema validation runs over dynamic data,
  so typed structs aren't the right model for config files.
  - serde-saphyr is ideal for the schema files (typed, `deny_unknown_fields`,
    excellent errors with a source snippet).
  - saphyr's `MarkedYaml` is ideal for config files (dynamic tree, a position
    on every node).
  - serde-saphyr has no generic `Value` type, so it can't do the second job
    on its own. That's fine: both crates share the same parser.
- **Duplicate keys:** saphyr rejects them by default with a line and column,
  which replaces sb-go's hand-written walker. It stops at the first duplicate
  and doesn't name the key path. sb-go lists every duplicate with its path and
  count, so the real module may want a key-path pass for friendlier output.
- **Errors are much better than sb-go's.** One run reports all 8 problems in
  `fixtures/many-errors.yml`, each with line and column. sb-go stops at the
  first error and reports a dotted path with no position, e.g.
  `field 'user.domain' is required`.
- **Schema drift found (a real sb-go gap).** Strict schema loading revealed
  two keys in `motd.schema.yml` that sb-go's `SchemaRule` doesn't define, so
  Go's YAML decoder silently ignores them:
  - `pattern` (9 uses): regex constraints that are **never enforced** today.
  - `additionalProperties` (`display_names`): value types are never checked.
    sb-go only accepts arbitrary keys there because it skips objects that
    have no `properties`.
- **Rough edges to fix in the module:** sort issues by position, not rule
  order. Point "is required" at the parent key, not the mapping's first
  child. Point "unknown field" at the key, not the value.
- **Not covered by the prototype:** built-in validators hostname, subdomain,
  password, ssh_key_or_url, directory_path, timezone, cron_time and
  rclone_template; the async API validators (cloudflare, dockerhub); and
  `pattern` (needs the `regex` crate). These are plain Rust functions with
  nothing stack-specific about them.

## Verdict

_Technical verdict only: it builds, works and fits the library policy. Pending the maintainer's own review of the look and feel._

**Adopt with changes.** Use serde-saphyr for typed YAML (schemas, sb-owned
config) and saphyr `MarkedYaml` for schema-validated Saltbox config.

## Map impact

- Stack table, YAML row: serde-saphyr **plus** saphyr (`MarkedYaml`) for
  dynamic validation. Add `regex` for `pattern`.
- `config` module: collect all issues with positions, keep strict schema
  loading so drift fails loudly, and implement `pattern` and
  `additionalProperties`.
- Upstream (Saltbox): report that motd `pattern` rules are not enforced by
  sb-go today. This is for you to raise; no consumer changes are made here.
