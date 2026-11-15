# Spec: `config` — `sb validate-config` (approved 2026-11-03)

Module of the approved capability map (`config`). The first vertical slice
is `sb version` → `sb docker ps` (done) → `sb validate-config`, built as
`cli-core` → `term-ui` → `docker` + `config`. This spec covers the
module's validation half and builds `sb validate-config`; the facts half
(line-preserving writer, locking, drift detection) and `sb update`'s call
into validation come with their own plans.

**Also in this module, the one Saltbox-user lookup** (the python-env and
ops specs point here): `user::saltbox_user(root: &Path) ->
Result<String, UserError>` (as built in M12: validate-config's
`Settings` carries its HTTP client and service URLs, which this lookup
doesn't need): `user.name` from `<saltbox root>/accounts.yml`, read as
sb-go's `GetSaltboxUser` reads it (host/system.go:69-92): the file
through cli-core's `read_small` and sb-config's YAML entry (ADR-022),
no schema validation, so an unrelated issue elsewhere in the file
doesn't stop `update`. sb-go's texts: `failed to read accounts.yml:
<cause>`, `failed to unmarshal accounts.yml: <cause>`, `user section
not found in accounts.yml`, `user.name not found in accounts.yml`.
Callers add their own wrapping (`error getting saltbox user: <cause>`
in python-env's commands, `get Saltbox user: <cause>` in setup). Its
users: ops (`update`, `branch`, `branch-sandbox`, `setup`), which
passes the name to repo's `fetch_and_reset` as `owner`, and
python-env's commands. *Amended 2026-11-13 by the repo spec (with the
python-env and ops specs).*

## Assumptions

1. **sb-go's command, with the map's decisions.** The files, schema
   rules, validators, network checks and step labels are sb-go's
   (`cmd/validateConfig.go`, `saltbox/`). The capability map's `config`
   row and proto-yaml's verdict ("adopt with changes") change four things:
   every issue is collected and reported with its position (sb-go stops
   at the first schema error, which has no position, and which one comes
   first can change between runs because it walks a Go map); schemas load
   strictly (an unknown schema key, rule type or validator is an error
   instead of being ignored); motd's `pattern` and `additionalProperties`
   are enforced (sb-go ignores them); the output is term-ui's runner.
2. **Saltbox owns the schemas.** They are read at run time from the
   Saltbox checkout (`<saltbox>/schema/*.schema.yml`), as sb-go does;
   nothing is embedded.
3. **It is the first runner command.** term-ui's criteria carried here by
   docker decision 1 are met by it: the failure block and Ctrl-C endings
   and restore in sb's own commands.
4. **It needs root,** as every sb command but `version` and completion
   (cli-core decision 3).
5. **The two network checks stay.** With credentials set, sb-go checks the
   Cloudflare token and the Docker Hub login on every run; so does sb,
   through one `reqwest` client (the stack table's choice over a
   Cloudflare crate).

## Objective

`sb validate-config` tells a Saltbox user, before a run of Saltbox's
playbooks, everything that is wrong with their configuration at once, each
issue with its file, line, column and key path, so they can fix it in one
pass; and confirms that their Cloudflare and Docker Hub credentials work.

### Behaviour

- **Arguments:** none (sb-go: `cobra.NoArgs`); help texts sb-go's
  ("Validate Saltbox configuration files"). No `--verbose/-v` (decision 1).
- **The Saltbox root** is `/srv/git/saltbox`, or `SB_SALTBOX_DIR` when set
  and not empty (tests and debugging, as `SB_CONFIG`; sudo's relaunch
  drops it). It moves cli-core's inventory path too (read at startup for
  the theme), so there is one root.
- **Files**, in sb-go's order:

  | File | Schema | Missing |
  |---|---|---|
  | accounts.yml | schema/accounts.schema.yml | error |
  | adv_settings.yml | schema/adv_settings.schema.yml | error |
  | backup_config.yml | schema/backup_config.schema.yml | error |
  | hetzner_vlan.yml | schema/hetzner_vlan.schema.yml | error |
  | settings.yml | schema/settings.schema.yml | error |
  | motd.yml | schema/motd.schema.yml | skipped, logged |
  | inventories/host_vars/localhost.yml | none: duplicate keys only | skipped, logged |

  A missing required file is that file's issue (`required config file not
  found: <path>`); a missing schema for a present file is its issue too
  (`schema file not found: <path>`). Files and schemas are read up to
  1 MiB each (cli-core's cap for config it reads): a larger one is an
  issue. localhost.yml has no cap: cli-core reads it whole
  (`read_inventory`; hardening decision 41), and only its nesting depth
  is limited, below. *Amended by hardening Task 61.*
- **Steps** (term-ui's runner, titled "sb validate-config"): one parent
  step, "Validating Saltbox configuration" → "Saltbox configuration
  validated" (a failed file makes it read term-ui's "sub-step failed"),
  and under it one step per file, "Validating <file>" → "Validated
  <file>", failing as "Validation of <file>" with that file's issues in
  its failure block (localhost.yml's step as sb-go names it, "Validating
  localhost.yml (no duplicates)": the whole path would be cut off in
  term-ui's label column; its issues and notice keep the path). Every file
  is validated (decision 2), each failed one with its own block; a failed
  credential check shows its own block too, besides any block of
  accounts.yml's own issues. Under accounts.yml, the two credential checks
  run as concurrent sibling steps, "Validating Cloudflare API credentials"
  → "Cloudflare API credentials validated" (failure "Cloudflare API
  credentials validation"), the same for "Docker Hub credentials"; both
  always run to their end and report, through term-ui's new group mode
  that lets every sibling finish (decision 3). A check with no credentials
  set isn't started: an info notice under accounts.yml's step says it was
  skipped ("no Cloudflare credentials set; skipped"). Warnings (sb-go's
  short-password text) are notices under their file's step.
- **One file** (for `sb edit` and `sb motd`; tui and motd specs):
  *Amended 2026-11-13 by the tui and motd specs.*
  - `check_file(settings, file) -> Result<Checked, FileError>` reads one
    entry of `FILES` and its schema as validate-config does (cli-core's
    `read_small` and 1 MiB cap, or `read_inventory` with none for
    localhost.yml, the YAML pre-pass, the parse, duplicate
    keys, the schema rules and the warnings), with no network request.
    `Checked` holds the `Report` (issues with positions, warnings, the
    issue count) and the parsed tree, so a caller that needs the values
    (motd's typed model) reads the file once. `FileError` is a read or
    schema-load failure, worded as validate-config words it.
  - `run::validate_file(step, settings, file, checks)` draws
    `check_file`'s result under the caller's step exactly as `run` does
    for that file. `checks` is `Checks::Run` (validate-config and `sb
    update`: accounts.yml's credential checks as concurrent sibling
    steps) or `Checks::Skip`, which makes no request and adds the notice
    `credential checks skipped; sb validate-config runs them` when the
    file has credentials set. `run` calls it for each file with
    `Checks::Run`, so the paths never differ. It returns the issue count
    for the caller's log. *As built (M48):* `step` is the file's own
    step, which the caller opens with `run::file_labels(file)` (so `sb
    edit` can draw it at the top level); the count comes back on the
    error the step fails with, `run::Issues::count()`, and is in
    `check_file`'s "config file checked" log record. `FileError` also
    names a missing file: `Missing` (required, the issue) or `Skipped`
    (optional, the info notice). The tree comes back as `tree::Tree`,
    which drops level by level, so a deep file's tree can be dropped on
    any stack.
- **When the checks run:** after accounts.yml's own validation, and only for
  a section with no issues of its own (an unknown key or a format error
  inside `cloudflare:` or `dockerhub:` skips that check too). Structure
  errors (a scoped token mixed with a global key, `api` without `email`, no
  `user.domain`, a half-set Docker Hub login) are accounts.yml issues, and
  that check is then skipped. (sb-go starts the checks during the walk,
  loses their results when the walk fails, and reports the structure errors
  from the checks' own steps.)
- **Issues:** `<file>:<line>:<column>: <key path>: <message>`, the file
  relative to the Saltbox root, sorted by position (a value taken from
  an alias at the alias). "is required" points
  at the parent key (a missing top-level key at 1:1), "unknown field" at
  the key. Messages keep sb-go's wording where it has one (`field
  'user.email' must be a valid email address` becomes `accounts.yml:3:10:
  user.email: must be a valid email address`); sb-go's `required_with`
  list prints as `a, b` rather than Go's `[a b]`. Control characters in
  any echoed value are escaped (cli-core's rule for group-writable
  config).
- **Secrets** (term-ui decision 8): `cloudflare.api`,
  `cloudflare.scoped_token`, `dockerhub.token` and every `password`-typed
  field are secrets; so are the credentials the schemas type `string`
  (*amended by hardening Task 60*): accounts.yml's `apprise` (a URL that
  carries a token), backup_config.yml's `backup.restore_service.pass`,
  and motd.yml's `token` (emby, jellyfin, plex), `apikey` (lidarr,
  radarr, readarr, sabnzbd, sonarr) and `password` (nzbget, qbittorrent,
  rtorrent) of each `<app>.instances` item. A test checks the list
  against the pinned schemas. A key with one of their names is a
  secret too where the schema doesn't place it (`user: [{pass: …}]`;
  *amended by hardening Task 46*): no issue, notice, output line or log
  record prints their values (an issue on one names the key, never the
  value), and `not_equals` never prints the value it compared.
- **Parsing:** one YAML document, as Ansible's loader wants (a second
  is refused); an
  empty file is an empty mapping (every required key missing, as sb-go);
  a root that isn't a mapping is one issue. The layouts accepted are
  libyaml's, Ansible's parser: cli-core's YAML pre-pass rewrites the text
  before saphyr reads it, for validate-config and the inventory read
  alike ([ADR-022](../decisions/ADR-022-yaml-pre-pass.md); the hardening
  plan's decision 28). What Ansible's loader refuses, sb refuses before
  its own rules (decision 38): a second document or content after the
  first one's end, a `:` with no key, an anchor defined twice, a
  collection as a mapping key, a tag Ansible's loader has no constructor
  for (`!custom`; it takes the YAML types' tags and its own `!unsafe` and
  `!vault`, which sb reads as their strings, as yaml.v3 does), a control
  character, NEL, LS and PS, a byte order mark past the file's start
  (libyaml reads one there as a column or as text), and the rest listed in
  docs/platform-notes/yaml-acceptance.md. A block scalar at the end of a
  file without a final line break ends without one, as in libyaml and
  yaml.v3 (saphyr adds one; sb takes it off).
  Nesting is limited to 23,000 levels, under the depth Ansible's loader
  takes before it crashes (measured in that note); the limit is on depth,
  never on length, since localhost.yml is as long as its owner makes it.
  A syntax error is that file's issue, with saphyr's
  line and column in the file as written, or the pre-pass's for what
  it refuses. *Amended by hardening Tasks 43, 59 and 61, and the module
  build's byte order mark follow-up.* Merge keys (`<<`) aren't followed
  (saphyr doesn't; yaml.v3 does), so a `<<` key is an unknown field: a
  difference, below.
- **Duplicate keys:** a pass over the parser's events runs first and
  reports every duplicate in the file, each with its key path and its
  position and the first one's (*amended by the module build's
  duplicate-key follow-up*: with every earlier one listed, the report
  of a key repeated N times grew as N²). A file with duplicates is not
  schema-validated further (sb-go's decode fails there too); its
  duplicates are its issues. So too a value that can't be decoded as its
  tag (`!!int abc`) or a mapping key that is a collection: those are the
  file's issues, its rules and credential checks don't run. (sb-go:
  yaml.v3 lists each duplicate with line numbers for the schema-backed
  files; its own walker lists localhost.yml's with paths and counts, no
  positions.)
- **Schema rules**, as sb-go's `SchemaRule`: `type` (object, array,
  string, boolean, number, float, and the type validators below),
  `required`, `required_when_true`, `validate_when_true`, `required_with`,
  `not_equals`, `format` (email, hostname, url), `min_length` /
  `max_length` (bytes), `properties`, `items`, `custom_validator`;
  `description` and `example` are accepted, and no check reads them
  (sb-go uses them to generate examples; so does `sb motd
  --generate-config`, which reads them since motd's M42: `example` is
  kept as written, any YAML value with string keys); plus `pattern` and
  `additionalProperties`.
  Semantics as sb-go, including its quirks, each pinned by a test:
  unknown config keys are errors; `string`, `boolean`, `array` and
  `object` values aren't type-checked, except that a list with items
  where an object with `properties` or `additionalProperties` belongs is
  an issue at the list, `must be an object, got a list` (a credential
  section keeps its own structure issue; *amended by hardening Task
  60*, a difference, below). motd.yml's older bare lists of instances
  are migrated to the current form before its check, when the file
  passes Saltbox's last list-form schema and the rewrite passes the
  current one, with a backup (`run::validate_file_migrating`, motd spec,
  decision 4; the maintainer, 2026-11-15); one left is the same plain
  issue; empty non-required values are
  skipped; `required: true` with a null value passes unless another rule
  rejects null; `required_with` fires only for a present-but-empty key;
  array items look up `required_when_true` and the like on the array's
  parent; an object without `properties` (or `additionalProperties`)
  accepts any keys; Ansible
  booleans are true/yes/on/1 and false/no/off/0 as strings (any case),
  or YAML booleans, and an unquoted `1`/`0` (an integer) is rejected as
  in sb-go. Where yaml.v3 and saphyr resolve scalars differently (octal
  and hex forms, `1_000`, large integers), sb follows yaml.v3's result:
  it resolves plain scalars itself from their source text, which
  saphyr's loader keeps with `early_parse(false)` (quoted scalars
  arrive unescaped, with their style), following yaml.v3's rules
  (domain code, a test at each edge). *Wording amended from Task 1's
  findings, approved at Checkpoint A.*
- **`pattern`:** a Rust `regex` (no lookaround or backreferences), an
  unanchored search as JSON Schema's, on string values only (a non-string
  with a pattern: `must be a string`), checked after `min_length` and
  `max_length`, before the type's own checks. A pattern that doesn't compile
  fails schema loading, naming the schema file, the pattern and its
  position. The plan's first task checks the pinned schemas' patterns:
  that they compile under Rust's syntax, mean the same as their ECMA-262
  reading (`\d` is Unicode in Rust) and anchor themselves. A string that
  doesn't match is an issue, exit 1, from the start (decision 4).
- **`additionalProperties`:** with `properties` and no
  `additionalProperties`, other keys are errors (sb-go's unknown field);
  with neither, any keys are accepted (as sb-go). A rule (as motd's schema
  uses it, and proto-yaml implemented) checks the value of every key outside
  `properties`; `false` forbids such keys; `true` allows them (new: sb-go
  has no way to). As with sb-go's other object rules, a scalar where the
  mapping belongs skips them (type flexibility), pinned by a test; a list
  with items is an issue (Task 60).
- **Strict loading:** an unknown schema key, rule type, format, validator
  name, or a malformed value is an error naming the schema file and the
  line and column (and the key or value at fault, where there is one;
  serde-saphyr reports positions, not key paths). Everywhere, also in a
  user's run (decision 5).
- **Validators.** Type validators (usable as `type`): `ansible_bool`
  (sb-go trims for `required_when_true` but not here: kept),
  `subdomain`, `hostname`, `directory_path`, `url`, `timezone`
  (decision 7, as Go's `LoadLocation`: `auto` in any case first, as
  sb-go; `UTC`, `Local` and empty accepted; a name containing `..` or
  starting with `/` or `\` rejected before any file is opened; else
  the file of exactly that name, `posix/` and `right/` included, under
  `ZONEINFO` when it names a directory, then `/usr/share/zoneinfo/`,
  `/usr/share/lib/zoneinfo/`, `/usr/lib/locale/TZ/` and
  `/etc/zoneinfo`, in that order, each file read up to 10 MB (Go's
  limit) and parsed with jiff's `TimeZone::tzif`. The first place
  holding a valid TZif file of that name wins; a missing file, a
  directory, a larger file or one that isn't TZif moves on to the next;
  the name is invalid only when none has one), `cron_time`,
  `rclone_template` (dropbox/google/sftp/nfs in any case, or an absolute
  path that must exist), `ssh_key_or_url`,
  `password` (its short-password warning only through `type: password`,
  as in sb-go). Custom validators (`custom_validator`), sb-go's registry
  exactly: `validate_ansible_bool`, `validate_subdomain`,
  `validate_hostname`, `validate_directory_path`, `validate_url`,
  `validate_timezone`, `validate_cron_time`, `validate_rclone_template`,
  `validate_ssh_key_or_url`, `validate_password_strength`,
  `validate_whole_number`, `validate_positive_number`,
  `validate_cloudflare_config`, `validate_dockerhub_config`;
  not `validate_rclone_remote` (decision 6).
- **Cloudflare** (`validate_cloudflare_config`): with a global key,
  `GET /user` first; then `GET /zones?name=<zone>` (the zone is
  `user.domain`'s registrable domain under the public suffix list,
  ICANN and private sections, as x/net's), and
  `GET /zones/<id>/settings/ssl`; SSL mode `flexible` or `off` is an
  error. Any zone-list or SSL-settings failure (refused, not found,
  network error, timeout) gives sb-go's guidance text for the credential
  kind after the cause, where the cause is the HTTP status and
  Cloudflare's own error messages (or the transport error), escaped and
  capped. Each request: 10 s, and cloudflare-go's retries (two more
  attempts with backoff on connection errors, 408, 409, 429 and 5xx,
  honouring `Retry-After-Ms` and `Retry-After`, capped at 60 s, and
  `x-should-retry`), so a check can take longer than 10 s. The base URL
  follows `CLOUDFLARE_BASE_URL` when set, as cloudflare-go
  does (tests and debugging; plain `http://` allowed; sudo's relaunch
  drops it); its other environment variables (credentials, custom
  headers) aren't read: a listed difference (sb-go's client adds them
  beside the configured credentials; sudo's env reset usually drops
  them).
- **Docker Hub** (`validate_dockerhub_config`): `POST
  https://hub.docker.com/v2/users/login/` with a properly encoded JSON body
  (sb-go formats it unescaped), 10 s, no retries (sb-go's plain client);
  non-200 is an error with the status and Docker Hub's `message` (its
  `details` when there is none), escaped and capped. The URL follows
  `SB_DOCKERHUB_URL` when set (tests and debugging; sudo drops it).
- **HTTP:** one reqwest client, rustls with the system's roots and
  `HTTPS_PROXY` honoured (as Go's default transport); response bodies
  read up to 1 MiB.
- **Ending:** all valid: exit 0. Any issue: the failed files' blocks,
  exit 1. Ctrl-C / SIGTERM: the runner's interrupted ending, death by the
  signal (`$?` 130 / 143; exec decision 7), terminal restored.
- **Logging** (verbose by default): each file, its issues, each check's
  outcome and timing; never a secret's value.

### Differences from sb-go (the complete list)

- Every issue in every file, with positions, sorted (sb-go: the first
  schema error of the first failing file, without a position).
- Strict schema loading; `pattern` and `additionalProperties` enforced.
- Both credential checks always finish and report; structure errors are
  accounts.yml issues; a check without credentials says it skipped
  (sb-go: lost check results when the walk fails, structure errors in the
  check steps, "validated" for a skipped check).
- term-ui's runner and failure blocks (sb-go: its Bubble Tea tree and
  fang's error badge); no `DEBUG:` lines on stdout (the run log has them).
- Merge keys (`<<`) aren't followed.
- A syntax error has saphyr's position and wording: where its scanner
  stops, at the end of the file for an unclosed flow sequence (yaml.v3
  counts the line differently and words it its own way). A tab libyaml
  refuses has sb's wording at the tab.
- The YAML accepted is libyaml's, so Ansible's (ADR-022): yaml.v3 also
  accepts the tab layouts libyaml refuses (a tab-only line, a tab before
  a top-level key, `-\tvalue`), and refuses a flow collection or quoted
  scalar continued left of its key's column; yaml.v3 and sb-go also read
  what libyaml refuses (a second document, of which they read the
  first; a `:` with no key; an anchor defined twice; a control
  character; junk after an indented first mapping), and yaml.v3 reads a
  tag Ansible's loader has no constructor for (`!custom`) as its string;
  sb refuses them all.
  A few layouts libyaml and saphyr read differently are refused though
  libyaml accepts them (`[?x]`, `*x: 1`, a block scalar's spaces-only
  lines before its end when it has no content; the note lists them). Where libyaml
  and saphyr still differ (saphyr's own refusals:
  docs/platform-notes/yaml-acceptance.md), sb follows saphyr. NEL, LS and PS (U+0085, U+2028, U+2029), line breaks to
  libyaml and not to saphyr, are refused wherever they are, even where
  libyaml reads them as saphyr would; so is a byte order mark past the
  file's first character (a column or text to libyaml).
- Nesting deeper than 23,000 levels is refused (yaml.v3 has no limit),
  and so is flow nesting deeper than 255, saphyr-parser's own limit,
  which libyaml takes.
- A duplicate key reads `<path>: duplicate key (also at line L, column
  C)` (sb-go: yaml.v3's `mapping key "k" already defined at line L` for
  the schema-backed files, its walker's `<path> (appears N times)` for
  localhost.yml).
- A missing motd.yml or localhost.yml is a step with a "not found;
  skipped" notice (sb-go: no step; a line only with `--verbose`).
- Files (localhost.yml excepted, read whole as sb-go reads it), schemas
  and HTTP response bodies read up to 1 MiB each; control
  characters in echoed values and remote text escaped.
- Cloudflare failures show the HTTP status and Cloudflare's own messages
  (sb-go: cloudflare-go's error text) before sb-go's guidance.
- `SB_SALTBOX_DIR`, `SB_DOCKERHUB_URL` and `SB_BACKUPS_DIR` (motd.yml's
  migration backups; tests and debugging; sb-go hard-codes the first
  two and has no migration; sudo's relaunch drops them).
- An old-format motd.yml is migrated before its check (motd spec,
  decision 4); sb-go's validate-config skipped a list where an object
  belongs.
- Docker Hub's login body properly encoded; `CLOUDFLARE_CUSTOM_HEADERS`
  and cloudflare-go's other environment variables not read.
- `required_with`'s list printed as `a, b`.
- `not_equals` takes its value as serde-saphyr resolves it: unquoted
  `yes`, `on` and `y` are booleans, `017` is 17 and a date is a string
  (yaml.v3: the strings, 15, and a `time.Time` that matches a date in the
  config). Saltbox quotes its `not_equals` values.
- A list with items where the schema describes an object's keys
  (`user:\n  - pass: x`) is an issue at the list (sb-go's
  `validateFieldWithTypeFlexibility`, saltbox/yaml_schema.go, walks an
  object only when the value is a `map[string]any` and returns nil
  otherwise, so the list's keys go unchecked there). An empty list passes,
  as in sb-go.
- A nested mapping with a key that isn't a string (`1:`, `true:`) is
  checked like any other (sb-go's yaml.v3 decodes it into a map its rules
  skip, so its keys and values go unchecked there).
- A `type: float` string in Go's hex-float form (`0x1p-2`) isn't a
  float (Go's `ParseFloat` reads it).
- `validate_whole_number`'s "got:" shows a timestamp as written (Go
  prints its `time.Time`); `validate_directory_path` doesn't fail when
  the working directory can't be read (sb-go does, for a relative path).
  No pinned schema uses either.
- The credential checks follow no redirects (Go's client follows up to
  10, re-sending a login's body): neither service redirects, and
  following one would send the credentials on.
- A proxy variable applies to loopback addresses too (Go's transport
  never proxies `localhost` or a loopback IP); only a test or debugging
  base URL is ever loopback.
- A `!!binary` scalar keeps its text (yaml.v3 decodes the base64); no
  Saltbox file uses one.
- Plain scalars resolve as yaml.v3's (YAML 1.2 core, as sb-go), not as
  Ansible's PyYAML resolver (YAML 1.1): `yes`, `no`, `on` and `off` stay
  strings, which the schema rules read as Ansible booleans anyway;
  `12:30` is a string, not 750; `1e3` is a float; an integer past 64
  bits is a float (the YAML corpus's resolution lines).
- `ZONEINFO` naming a zip file (Go reads one) isn't read: sb reads it
  only as a directory; nor is Go's last fallback, the build machine's
  `$GOROOT/lib/time/zoneinfo.zip`, which only a host with a Go
  installation at that path has (decision 7).
- A delay Cloudflare names (`Retry-After-Ms`, `Retry-After` in seconds or
  as a date) waits at most 60 s (maintainer, 2026-11-05;
  cloudflare-go waits as long as the server says, so one answer could
  hold the step for an hour).
- No `--verbose/-v` (decision 1): `-v` is a parse error, exit 2.
- No `validate_rclone_remote` (decision 6): a schema naming it fails
  loading, naming it.

### Other specs

- **cli-core:** `config::INVENTORY` (cli-core spec, theme) moves with
  `SB_SALTBOX_DIR`; that spec gets the amendment with this module's first
  task that touches it.

### Capability map correction

The `config` row's dependencies become: `cli-core`, `term-ui` (for
validate-config, through the runner); `exec`, `host`, `ansible` and
`release` from the facts half and `sb update` on. deny.toml's wrappers
follow (`sb-config` allowed on `sb-cli-core` and `sb-term-ui`; only `sb`
on `sb-config`).

## Tech Stack

- saphyr 0.3.3 `MarkedYaml` for the config files (positions on every
  node; already in sb for the inventory) and saphyr-parser's events for
  the duplicate-key pass, both on cli-core's pre-passed text (ADR-022;
  no dependency); serde-saphyr 2.0 (`deserialize` only) for the
  schema files (typed, `deny_unknown_fields`); `regex` for `pattern`.
- reqwest (rustls, aws-lc-rs; the stack table) for the two checks; `psl`
  for the public suffix list (both sections, compiled in) and `jiff`
  (`TimeZone::tzif` on the zoneinfo file sb finds as Go does; decision
  7; only the features `tzif` needs, checked on docs.rs in Task 7, not
  jiff's own zoneinfo lookup) for `timezone`. Evidence:
  [saltbox-schemas.md](../platform-notes/saltbox-schemas.md),
  [musl-c-dependencies.md](../platform-notes/musl-c-dependencies.md).
  *Amended from Task 1's findings, approved at Checkpoint A.*
- term-ui's runner; cli-core's exit codes, `Shutdown`, run log, secrets.

## Commands

```
Check: scripts/check.sh fast | task | full
Test:  systemd-run --scope -p MemoryMax=8G -p CPUQuota=400% cargo test -p sb-config -p sb
Try:   the production build as root with SB_LOG_DIR set
```

## Project Structure

```
crates/config/            sb-config: schema loading, rules, validators,
  src/                    the duplicate-key pass, the credential checks,
                          validate-config's steps
  tests/fixtures/         synthetic config sets; Saltbox's schemas at a
                          pinned upstream commit
  src/run.rs              `check_file` and `validate_file` (amended
                          2026-11-13 by the tui spec)
crates/sb/src/main.rs     the validate-config command
docs/specs/config.md      this spec
```

## Testing Strategy

- **Unit (most):** every rule, validator and message against synthetic
  YAML, sb-go's own test tables (its validator and schema tests) ported
  case by case; strict loading; duplicate keys with positions; sorting;
  the scalar edges; the secret fields never echoed. Timeouts injectable,
  so timeout tests take milliseconds.
- **Real schemas:** Saltbox's `schema/` copied into the fixtures from the
  public upstream repository at a pinned commit (never from a live
  checkout), with a provenance note (repository URL, commit, GPL-3.0);
  the loader accepts them, and a valid synthetic config set passes them.
  The pin moves by hand with a check.
- **Fixture sets:** a valid synthetic config set (no real hosts, domains
  or tokens) and broken variants through `SB_SALTBOX_DIR`; proto-yaml's
  many-errors case copied in: every issue found.
- **HTTP fakes:** Cloudflare and Docker Hub answered by a local server
  through `CLOUDFLARE_BASE_URL` and `SB_DOCKERHUB_URL`: success, each
  refusal's guidance, flexible SSL, retries, timeouts. No test calls the
  real services.
- **Through the binary** (root-gated, `SB_TEST_ROOT=1`, `SB_LOG_DIR`
  under the test's directory): the runner's look (steps, concurrent
  checks, notices), the failure blocks with exit 1 (term-ui criterion 4),
  Ctrl-C and SIGTERM endings and restore on a PTY with a check held open
  (criterion 6), piped plain output, the run log without any known secret.
- **Parity (by hand, once):** sb and sb-go on the same fixture set, sb-go
  run in a private mount namespace with the fixtures bind-mounted over
  `/srv/git/saltbox` (its paths are fixed), its Cloudflare check pointed
  at the fake through `CLOUDFLARE_BASE_URL`; no Docker Hub credentials
  in that set (sb-go's URL can't be redirected). Only the listed
  differences.
- **On real config (by hand):** the maintainer runs the production build
  on a real host.

## Boundaries

- **Always:** read-only on Saltbox files; tests on fixtures only; secrets
  marked where they're read.
- **Ask first:** writing to any Saltbox file outside the facts half's
  specified saves (tui spec, Editors; *amended 2026-11-13 by the tui
  spec*); calling the
  real Cloudflare or Docker Hub APIs from a test; a term-ui API change
  beyond decision 3's group mode; a difference from sb-go not listed.
- **Never:** read or change a real Saltbox checkout in tests or reviews;
  commit config, domains, tokens or results from a real host.

## Success Criteria

1. Every rule, validator and message has unit tests (sb-go's cases
   included); strict loading fails on an unknown schema key, rule type,
   format or validator and on a pattern that doesn't compile, naming it;
   Saltbox's pinned schemas load.
2. A broken fixture set yields every issue, each with file, line, column
   and key path, sorted; the valid set yields none.
3. Duplicate keys: every one, with path and positions, in schema-backed
   files and localhost.yml; such a file isn't schema-validated further.
4. Missing required file or schema: that file's issue; missing motd.yml or
   localhost.yml: skipped and logged; a file over 1 MiB: an issue, but
   localhost.yml, which is read at any length.
5. The credential checks against fakes: success; each failure's text with
   its guidance; retries (Cloudflare) and none (Docker Hub); a timeout;
   structure errors as accounts.yml issues; skipped with a notice without
   credentials; both finish when one fails.
6. No secret's value (tokens, keys, passwords) in any output, issue,
   notice or log record (a known-secret test through the binary).
7. Through the binary: the runner's steps with the checks concurrent and
   warnings as notices; any issue: each failed file's block, exit 1; all
   valid: exit 0.
8. Ctrl-C / SIGTERM during a held credential check: interrupted, `$?` 130
   / 143, terminal restored (PTY).
9. Piped: plain lines, no escape codes; `--color` and NO_COLOR as
   everywhere.
10. Startup budget kept; binary size measured, with an ADR if past the
    tolerance (reqwest and rustls will be).
11. The maintainer runs it on a real host's config (the slice's goal).
12. `check_file` and `validate_file` give the same issues for a file as
    `run` does (a test over the fixture sets), and neither makes a request
    with `Checks::Skip`. *Amended 2026-11-13 by the tui spec* (numbered 13
    in its wording; 12 is the next free number here).

## Decisions (maintainer, 2026-11-03)

1. **No `--verbose/-v`:** dropped, as term-ui decision 9 already decided
   (no verbose mode; the run log carries the debug detail by default).
   The same holds for docker start/stop/restart.
2. **Every file in one run:** every file is validated and every issue
   shown, each failed file with its own block.
3. **Checks that don't stop each other:** term-ui gains a group mode
   that lets every sibling finish and collects their results (a term-ui
   spec amendment with this module's plan); both credential checks always
   report.
4. **`pattern` misses are errors** from the start.
5. **Strict loading everywhere:** sb updates itself, so a user's sb keeps
   up with Saltbox's schemas; schema drift stays with development builds,
   where it fails loudly.
6. **`validate_rclone_remote` left out** while no Saltbox schema uses it
   (the plan's first task checks the pinned schemas); a schema that
   starts to fails loading, naming it.
7. **Timezones as Go (Checkpoint A, 2026-11-03):** jiff's own lookup
   differs from Go's `LoadLocation` (case-insensitive names, no `posix/`
   or `right/`, `TZDIR` rather than `ZONEINFO`;
   [saltbox-schemas.md](../platform-notes/saltbox-schemas.md)). sb
   resolves the file path as Go does (the rules under Validators) and
   parses it with jiff, so results match sb-go (they agree on every zone
   zic writes; only a TZif file with a malformed footer, which Go
   accepts, is refused); tests pass a fixture zoneinfo directory. Only a
   `ZONEINFO` zip file isn't read (listed under the differences).
8. **musl builds through `cross` (Checkpoint A, 2026-11-03):** aws-lc-sys
   compiles C, so check.sh's production musl build moves to `cross`
   ([ADR-008](../decisions/ADR-008-musl-builds-through-cross.md)), with
   the commit that first links it (plan Task 11). *Superseded by
   [ADR-020](../decisions/ADR-020-glibc-builds-on-the-oldest-release.md)
   (2026-11-13): a glibc build in an `ubuntu:22.04` image, whose GCC
   compiles aws-lc-sys.*
