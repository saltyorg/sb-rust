#!/usr/bin/env python3
"""One job of a workflow, run the way a GitHub-hosted runner runs it.

The CI rehearsal's job runner (scripts/ci-rehearsal.sh), run as root in a
container provisioned as GitHub's Ubuntu image is (provision.sh). It reads
the workflow and runs the job's steps as GitHub's runner does
(https://docs.github.com/actions/reference/workflows-and-actions/workflow-syntax):

- a `run` step as the runner user, with its shell (the workflow's
  `defaults.run.shell`; bash is `bash --noprofile --norc -eo pipefail {0}`),
  its `env` and the variables a runner sets (`CI`, `GITHUB_*`, `RUNNER_*`),
  in `GITHUB_WORKSPACE`; after each step, the variables and PATH entries
  it wrote to `GITHUB_ENV` and `GITHUB_PATH`
  (https://docs.github.com/actions/reference/workflows-and-actions/workflow-commands);
- a `uses` step from the action's repository at the commit the workflow
  pins, downloaded as the runner downloads it: a JavaScript action's main
  script run by Node 24 with its inputs as `INPUT_<NAME>`, a composite
  action's steps in turn; `actions/checkout` alone is replaced, by a
  clone of the repository mounted at `--src`, at `--rev`, with the
  working tree's changes (`--patch`, `--untracked`) committed on top, as
  the push of that tree would be checked out;
- `${{ }}` expressions, with the contexts a push to main has and no
  token (`github.token` is empty), and `if:` conditions, a step after a
  failure running only on `always()` or `failure()`.

Not done: post steps (cache saves and checkout's cleanup), `services`,
containers, Docker actions, `include` and `exclude` in a matrix; a
workflow using one fails here rather than run differently.

Why not nektos/act, the common way to run a workflow locally: it runs
the steps as root in its own images, with no `runner` user and no 0750
home, the conditions the rehearsal exists to show (a checkout `nobody`
can't reach, root only through sudo); its arm64 runs need the host's
binfmt_misc handlers, a change to the host; and its runner images are
its own, not provisioned from GitHub's toolsets
(https://nektosact.com/usage/runners.html).

    job.py runners WORKFLOW JOB       each runner label of JOB's matrix,
                                      with its Ubuntu release and arch
    job.py run WORKFLOW JOB LABEL --src DIR --rev SHA [--before SHA] [--patch FILE]
                                      [--untracked TAR]
"""

from __future__ import annotations

import argparse
import itertools
import json
import math
import os
import pwd
import re
import shlex
import subprocess  # the job's steps are what it runs  # nosec B404
import sys
import tarfile
import time
import urllib.request
import uuid
from pathlib import Path
from typing import Any

import yaml

USER = "runner"
HOME = Path("/home/runner")
NODE = Path("/opt/runner-externals/node24/bin/node")
TOOL_CACHE = "/opt/hostedtoolcache"
REPOSITORY = "saltyorg/sb-rust"
# `github.token`: a rehearsal has none (ci-rehearsal.sh).
NO_TOKEN = ""  # empty: no token at all  # nosec B105
# The PATH of the runner images' /etc/environment for what provision.sh
# installs: pipx's and the user's own bin directories, cargo's, then
# Ubuntu's.
PATH = ":".join(
    [
        f"{HOME}/.local/bin",
        "/opt/pipx_bin",
        f"{HOME}/.cargo/bin",
        "/usr/local/sbin",
        "/usr/local/bin",
        "/usr/sbin",
        "/usr/bin",
        "/sbin",
        "/bin",
        "/usr/games",
        "/usr/local/games",
        "/snap/bin",
    ]
)


class JobError(Exception):
    """The workflow asks for something the rehearsal doesn't do."""


# ---- Expressions --------------------------------------------------------
# https://docs.github.com/actions/reference/workflows-and-actions/expressions

TOKEN = re.compile(
    r"""\s*(?:
      (?P<str>'(?:[^']|'')*')
    | (?P<num>\d+(?:\.\d+)?)
    | (?P<op>==|!=|&&|\|\||!|\(|\)|,)
    | (?P<word>[A-Za-z_][A-Za-z0-9_-]*(?:\.[A-Za-z_][A-Za-z0-9_-]*)*)
    )""",
    re.VERBOSE,
)
STATUS_FUNCTIONS = ("always", "success", "failure", "cancelled")


def tokens(text: str) -> list[tuple[str, str]]:
    out = []
    at = 0
    text = text.rstrip()
    while at < len(text):
        m = TOKEN.match(text, at)
        if not m or m.end() == at:
            raise JobError(f"expression {text!r}: can't read it from {text[at:]!r}")
        kind = m.lastgroup
        if kind is None:  # every alternative is a group: never
            raise JobError(f"expression {text!r}: unreadable")
        out.append((kind, m.group(kind)))
        at = m.end()
    return out


def number(value: Any) -> float:
    """GitHub's coercion to a number for a comparison of two types."""
    if value is None:
        return 0.0
    if isinstance(value, bool):
        return 1.0 if value else 0.0
    if isinstance(value, (int, float)):
        return float(value)
    try:
        return float(value.strip()) if value.strip() else 0.0
    except ValueError:
        return math.nan


def equal(a: Any, b: Any) -> bool:
    if isinstance(a, str) and isinstance(b, str):
        return a.casefold() == b.casefold()
    if type(a) is type(b):
        return bool(a == b)
    return number(a) == number(b)


def truthy(value: Any) -> bool:
    if value is None or value is False or value == "":
        return False
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return value != 0
    return True


def text_of(value: Any) -> str:
    """A value as `${{ }}` puts it into a string."""
    if value is None:
        return ""
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, float) and value.is_integer():
        return str(int(value))
    return str(value)


class Evaluator:
    """A recursive-descent reader of one expression: `||` binds loosest,
    then `&&`, then `==` and `!=`, then `!`."""

    def __init__(self, text: str, contexts: dict[str, Any], status: str) -> None:
        self.text = text
        self.items = tokens(text)
        self.at = 0
        self.contexts = {k.casefold(): v for k, v in contexts.items()}
        self.status = status

    def value(self) -> Any:
        result = self.either()
        if self.at != len(self.items):
            raise JobError(
                f"expression {self.text!r}: unexpected {self.items[self.at][1]!r}"
            )
        return result

    def peek(self) -> str | None:
        return self.items[self.at][1] if self.at < len(self.items) else None

    def take(self) -> tuple[str, str]:
        if self.at >= len(self.items):
            raise JobError(f"expression {self.text!r}: ends too soon")
        item = self.items[self.at]
        self.at += 1
        return item

    def either(self) -> Any:
        left = self.both()
        while self.peek() == "||":
            self.take()
            right = self.both()
            left = left if truthy(left) else right
        return left

    def both(self) -> Any:
        left = self.compare()
        while self.peek() == "&&":
            self.take()
            right = self.compare()
            left = right if truthy(left) else left
        return left

    def compare(self) -> Any:
        left = self.unary()
        while self.peek() in ("==", "!="):
            op = self.take()[1]
            right = self.unary()
            left = equal(left, right) if op == "==" else not equal(left, right)
        return left

    def unary(self) -> Any:
        if self.peek() == "!":
            self.take()
            return not truthy(self.unary())
        return self.primary()

    def primary(self) -> Any:
        kind, text = self.take()
        if kind == "str":
            return text[1:-1].replace("''", "'")
        if kind == "num":
            return float(text)
        if text == "(":
            inner = self.either()
            if self.take()[1] != ")":
                raise JobError(f"expression {self.text!r}: no closing parenthesis")
            return inner
        if kind != "word":
            raise JobError(f"expression {self.text!r}: unexpected {text!r}")
        if text in ("true", "false"):
            return text == "true"
        if text == "null":
            return None
        if self.peek() == "(":
            return self.call(text)
        return self.lookup(text)

    def call(self, name: str) -> Any:
        self.take()
        if self.take()[1] != ")" or name not in STATUS_FUNCTIONS:
            raise JobError(
                f"expression {self.text!r}: the rehearsal has no function {name}(...)"
            )
        return {
            "always": True,
            "success": self.status == "success",
            "failure": self.status == "failure",
            "cancelled": False,
        }[name]

    def lookup(self, path: str) -> Any:
        """A context's value; a key it lacks is null, as on GitHub, but a
        context the rehearsal doesn't have (`steps`, `needs`, `secrets`,
        `vars`, ...) fails rather than read as empty."""
        value: Any = self.contexts
        if path.split(".")[0].casefold() not in self.contexts:
            raise JobError(
                f"expression {self.text!r}: the rehearsal has no {path.split('.')[0]!r} context"
            )
        for part in path.split("."):
            if not isinstance(value, dict):
                return None
            value = {k.casefold(): v for k, v in value.items()}.get(part.casefold())
        return value


def evaluate(text: str, contexts: dict[str, Any], status: str = "success") -> Any:
    return Evaluator(text, contexts, status).value()


EMBEDDED = re.compile(r"\$\{\{(.*?)\}\}", re.DOTALL)


def interpolate(value: Any, contexts: dict[str, Any]) -> str:
    """A workflow value as a string, each `${{ }}` replaced; YAML's own
    scalars as GitHub gives them to a step (`true`, `0`)."""
    if not isinstance(value, str):
        return text_of(value)
    return EMBEDDED.sub(lambda m: text_of(evaluate(m.group(1), contexts)), value)


def condition(value: Any, contexts: dict[str, Any], status: str) -> bool:
    """An `if:`: with no status function in it, `success() && (...)`."""
    if value is None:
        return status == "success"
    if isinstance(value, bool):
        return value and status == "success"
    text = str(value).strip()
    whole = re.fullmatch(r"\$\{\{(.*)\}\}", text, re.DOTALL)
    if whole:
        text = whole.group(1)
    if not re.search(r"\b(" + "|".join(STATUS_FUNCTIONS) + r")\s*\(", text):
        text = f"success() && ({text})"
    return truthy(evaluate(text, contexts, status))


# ---- The workflow ------------------------------------------------------

LABEL = re.compile(r"ubuntu-(?P<release>\d{2}\.\d{2})(?P<arm>-arm)?")


def platform(label: str) -> tuple[str, str]:
    """A GitHub-hosted Ubuntu runner label's release and architecture."""
    m = LABEL.fullmatch(label)
    if not m:
        raise JobError(
            f"runner {label!r}: the rehearsal knows only ubuntu-YY.MM and ubuntu-YY.MM-arm"
        )
    return m["release"], "arm64" if m["arm"] else "amd64"


def load(workflow: Path) -> dict[str, Any]:
    data = yaml.safe_load(workflow.read_text())
    if not isinstance(data, dict) or not isinstance(data.get("jobs"), dict):
        raise JobError(f"{workflow}: no jobs")
    return data


def job_of(data: dict[str, Any], name: str) -> dict[str, Any]:
    job = data["jobs"].get(name)
    if not isinstance(job, dict):
        raise JobError(f"no job {name!r}")
    for key in ("services", "container", "uses", "if", "needs", "outputs"):
        if key in job:
            raise JobError(f"job {name}: the rehearsal doesn't run a job's {key!r}")
    return job


def combinations(job: dict[str, Any]) -> list[dict[str, Any]]:
    """The job's matrix, every combination of its values."""
    matrix = (job.get("strategy") or {}).get("matrix") or {}
    if isinstance(matrix, str) or "include" in matrix or "exclude" in matrix:
        raise JobError("the rehearsal reads a matrix of plain lists only")
    keys = list(matrix)
    return [
        dict(zip(keys, values, strict=True))
        for values in itertools.product(*(matrix[k] for k in keys))
    ]


def runners(data: dict[str, Any], name: str) -> list[tuple[str, dict[str, Any]]]:
    """Each runner label of the job, with its matrix combination."""
    job = job_of(data, name)
    out = []
    for combo in combinations(job):
        label = interpolate(job.get("runs-on"), {"matrix": combo})
        platform(label)
        out.append((label, combo))
    return out


# ---- Running ------------------------------------------------------------


def log(text: str) -> None:
    print(text, flush=True)


def as_runner(argv: list[str]) -> list[str]:
    return [
        "setpriv",
        f"--reuid={USER}",
        f"--regid={USER}",
        "--init-groups",
        "--",
        *argv,
    ]


def shell_argv(shell: str | None, script: Path) -> list[str]:
    """A step's shell as the runner starts it, for its script file."""
    if shell in (None, "bash"):
        return ["bash", "--noprofile", "--norc", "-eo", "pipefail", str(script)]
    if shell == "sh":
        return ["sh", "-e", str(script)]
    if "{0}" in shell:
        return [part.replace("{0}", str(script)) for part in shlex.split(shell)]
    raise JobError(
        f"shell {shell!r}: the rehearsal runs bash, sh and a command with {{0}}"
    )


def read_env_file(text: str) -> dict[str, str]:
    """`GITHUB_ENV`'s `NAME=value` lines and `NAME<<DELIMITER` blocks."""
    out: dict[str, str] = {}
    lines = text.split("\n")
    i = 0
    while i < len(lines):
        line = lines[i]
        i += 1
        if not line.strip():
            continue
        if "<<" in line and ("=" not in line or line.index("<<") < line.index("=")):
            name, delimiter = line.split("<<", 1)
            body = []
            while i < len(lines) and lines[i] != delimiter:
                body.append(lines[i])
                i += 1
            if i == len(lines):
                raise JobError(f"GITHUB_ENV: {name} has no closing {delimiter}")
            i += 1
            out[name] = "\n".join(body)
        elif "=" in line:
            name, value = line.split("=", 1)
            out[name] = value
        else:
            raise JobError(f"GITHUB_ENV: can't read {line!r}")
    return out


def uses_parts(uses: str) -> tuple[str, str, str]:
    """`owner/repo[/path]@ref` as the repository, the path in it, the ref."""
    m = re.fullmatch(
        r"(?P<repo>[\w.-]+/[\w.-]+)(?:/(?P<path>[^@]+))?@(?P<ref>[\w.-]+)", uses
    )
    if not m:
        raise JobError(
            f"uses {uses!r}: the rehearsal runs owner/repo[/path]@ref actions only"
        )
    return m["repo"], m["path"] or "", m["ref"]


def input_env(
    action: dict[str, Any], given: dict[str, Any], contexts: dict[str, Any]
) -> dict[str, str]:
    """An action's inputs as the runner passes them: `INPUT_<NAME>`, from
    the step's `with` or else the input's default."""
    declared = action.get("inputs") or {}
    by_name = {str(k).casefold(): v for k, v in given.items()}
    for name in by_name:
        if name not in {str(k).casefold() for k in declared}:
            log(f"warning: unexpected input {name!r}")
    out = {}
    for name, spec in declared.items():
        spec = spec or {}
        if str(name).casefold() in by_name:
            value = interpolate(by_name[str(name).casefold()], contexts)
        else:
            value = interpolate(spec.get("default", ""), contexts)
        out["INPUT_" + str(name).replace(" ", "_").upper()] = value
    return out


class Job:
    """One job on one runner label."""

    def __init__(
        self,
        data: dict[str, Any],
        name: str,
        label: str,
        src: Path,
        rev: str,
        patch: Path | None = None,
        untracked: Path | None = None,
        before: str | None = None,
    ) -> None:
        self.data = data
        self.name = name
        self.job = job_of(data, name)
        matches = [combo for lab, combo in runners(data, name) if lab == label]
        if not matches:
            raise JobError(f"job {name} has no runner {label!r}")
        self.matrix = matches[0]
        self.label = label
        self.release, self.arch = platform(label)
        self.src = src
        self.rev = rev
        # The push's earlier commit (its event's `before`): the one given,
        # else the checkout's own, so the push is the working tree's
        # changes alone.
        self.before = before or rev
        self.patch = patch
        self.untracked = untracked
        self.work = HOME / "work"
        self.workspace = self.work / REPOSITORY.split("/")[1] / REPOSITORY.split("/")[1]
        self.temp = self.work / "_temp"
        self.actions = self.work / "_actions"
        self.status = "success"
        self.paths: list[str] = []
        self.exported: dict[str, str] = {}
        self.summary: list[tuple[str, str, float]] = []
        self.skipped_posts: list[str] = []
        for index, step in enumerate(self.job.get("steps") or [], 1):
            if "id" in step:
                raise JobError(
                    f"step {index}: the rehearsal keeps no step outputs (`id`, `steps.*`)"
                )

    # The runner's variables (https://docs.github.com/actions/reference/workflows-and-actions/variables).
    def runner_env(self) -> dict[str, str]:
        arch = "X64" if self.arch == "amd64" else "ARM64"
        return {
            "HOME": str(HOME),
            "USER": USER,
            "LOGNAME": USER,
            "SHELL": "/bin/bash",
            "LANG": "C.UTF-8",
            "PATH": PATH,
            "CI": "true",
            "GITHUB_ACTIONS": "true",
            "GITHUB_ACTOR": "rehearsal",
            "GITHUB_API_URL": "https://api.github.com",
            "GITHUB_BASE_REF": "",
            "GITHUB_EVENT_NAME": "push",
            "GITHUB_EVENT_PATH": str(self.work / "_github_workflow" / "event.json"),
            "GITHUB_GRAPHQL_URL": "https://api.github.com/graphql",
            "GITHUB_HEAD_REF": "",
            "GITHUB_JOB": self.name,
            "GITHUB_REF": "refs/heads/main",
            "GITHUB_REF_NAME": "main",
            "GITHUB_REF_TYPE": "branch",
            "GITHUB_REPOSITORY": REPOSITORY,
            "GITHUB_REPOSITORY_OWNER": REPOSITORY.split("/")[0],
            "GITHUB_RUN_ATTEMPT": "1",
            "GITHUB_RUN_ID": "1",
            "GITHUB_RUN_NUMBER": "1",
            "GITHUB_SERVER_URL": "https://github.com",
            "GITHUB_SHA": self.rev,
            "GITHUB_WORKFLOW": str(self.data.get("name", "")),
            "GITHUB_WORKSPACE": str(self.workspace),
            "ImageOS": "ubuntu" + self.release.split(".")[0],
            "ImageVersion": "rehearsal",
            "RUNNER_ARCH": arch,
            "RUNNER_ENVIRONMENT": "github-hosted",
            "RUNNER_NAME": "rehearsal",
            "RUNNER_OS": "Linux",
            "RUNNER_TEMP": str(self.temp),
            "RUNNER_TOOL_CACHE": TOOL_CACHE,
            "AGENT_TOOLSDIRECTORY": TOOL_CACHE,
            "RUNNER_WORKSPACE": str(self.workspace.parent),
            "PIPX_BIN_DIR": "/opt/pipx_bin",
            "PIPX_HOME": "/opt/pipx",
            "XDG_CONFIG_HOME": f"{HOME}/.config",
            "ACCEPT_EULA": "Y",
        }

    def contexts(
        self, env: dict[str, str], extra: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        github = {
            "base_ref": "",
            "head_ref": "",
            "event_name": "push",
            "ref": "refs/heads/main",
            "ref_name": "main",
            "sha": self.rev,
            "event": self.event(),
            "repository": REPOSITORY,
            "repository_owner": REPOSITORY.split("/")[0],
            "server_url": "https://github.com",
            "api_url": "https://api.github.com",
            "token": NO_TOKEN,
            "workspace": str(self.workspace),
            "job": self.name,
            "workflow": self.data.get("name", ""),
            "run_id": "1",
        }
        ctx: dict[str, Any] = {
            "github": github,
            "runner": {
                "os": "Linux",
                "arch": "X64" if self.arch == "amd64" else "ARM64",
                "temp": str(self.temp),
                "tool_cache": TOOL_CACHE,
                "name": "rehearsal",
                "environment": "github-hosted",
            },
            "matrix": self.matrix,
            "env": env,
            "job": {"status": self.status},
        }
        for key, value in (extra or {}).items():
            if key == "github":
                github.update(value)
            else:
                ctx[key] = value
        return ctx

    def event(self) -> dict[str, Any]:
        """The push event's payload, as `github.event` and the event file
        give it: its ref, the commits it moved the branch from (`before`)
        and to (`after`), the repository; a subset of GitHub's
        (https://docs.github.com/en/webhooks/webhook-events-and-payloads#push).
        Not as on GitHub: `after` (and `github.sha`) is `--rev`, and the
        checkout commits the working tree's changes on top of it, so a step
        diffing `before` with `after` would miss them; ci.yml's steps read
        HEAD."""
        return {
            "ref": "refs/heads/main",
            "before": self.before,
            "after": self.rev,
            "repository": {"full_name": REPOSITORY, "default_branch": "main"},
        }

    def chown(self, path: Path) -> None:
        # chown from PATH, as every step finds its tools.
        argv = ["chown", "-R", f"{USER}:{USER}", str(path)]
        subprocess.run(argv, check=True)  # nosec B603

    def prepare(self) -> None:
        for d in (
            self.workspace,
            self.temp,
            self.actions,
            self.work / "_github_workflow",
        ):
            d.mkdir(parents=True, exist_ok=True)
        (self.work / "_github_workflow" / "event.json").write_text(
            json.dumps(self.event()) + "\n"
        )
        self.chown(self.work)

    def env_for(
        self, *layers: dict[str, Any] | None, contexts: dict[str, Any] | None = None
    ) -> dict[str, str]:
        """The runner's variables, those steps exported, then each layer of
        `env:` (workflow, job, step), with the PATH steps added in front."""
        env = self.runner_env()
        env.update(self.exported)
        base = self.contexts(env) if contexts is None else contexts
        for layer in layers:
            for key, value in (layer or {}).items():
                env[str(key)] = interpolate(value, {**base, "env": env})
        if self.paths:
            env["PATH"] = ":".join([*reversed(self.paths), env["PATH"]])
        return env

    def command_files(self, env: dict[str, str]) -> dict[str, Path]:
        """Fresh file-command files for one step, named in `env`."""
        folder = self.temp / "_runner_file_commands"
        folder.mkdir(parents=True, exist_ok=True)
        files = {}
        for var, stem in (
            ("GITHUB_ENV", "set_env"),
            ("GITHUB_PATH", "add_path"),
            ("GITHUB_OUTPUT", "set_output"),
            ("GITHUB_STATE", "save_state"),
            ("GITHUB_STEP_SUMMARY", "step_summary"),
        ):
            path = folder / f"{stem}_{uuid.uuid4()}"
            path.write_text("")
            files[var] = path
            env[var] = str(path)
        self.chown(folder)
        return files

    def take_commands(self, files: dict[str, Path]) -> None:
        self.exported.update(read_env_file(files["GITHUB_ENV"].read_text()))
        for line in files["GITHUB_PATH"].read_text().splitlines():
            if line.strip():
                self.paths.append(line.strip())

    def execute(self, argv: list[str], env: dict[str, str], cwd: Path) -> int:
        files = self.command_files(env)
        status = subprocess.run(  # the workflow's own step  # nosec B603
            as_runner(argv), env=env, cwd=cwd, stdin=subprocess.DEVNULL, check=False
        ).returncode
        self.take_commands(files)
        return status

    def script(
        self, text: str, shell: str | None, env: dict[str, str], cwd: Path
    ) -> int:
        path = self.temp / f"{uuid.uuid4()}.sh"
        path.write_text(text)
        self.chown(path)
        return self.execute(shell_argv(shell, path), env, cwd)

    def fetch(self, repo: str, ref: str) -> Path:
        """An action's repository at `ref`, as the runner downloads it: the
        tarball of that commit, under `_actions/<owner>/<repo>/<ref>`."""
        dest = self.actions / repo / ref
        if dest.exists():
            return dest
        url = f"https://codeload.github.com/{repo}/tar.gz/{ref}"
        log(f"Download action repository '{repo}@{ref}'")
        with urllib.request.urlopen(url, timeout=300) as response:  # an https URL  # nosec B310
            data = response.read()
        dest.mkdir(parents=True)
        # Unpacked by tar, as the runner unpacks it, without the archive's
        # top directory (`<repo>-<sha>/`): its links stay as the action
        # has them (Python's tar filters refuse some, such as
        # setup-python's test data on Python 3.10).
        archive = dest.parent / f"{ref}.tar.gz"
        archive.write_bytes(data)
        argv = ["tar", "-xzf", str(archive), "-C", str(dest), "--strip-components=1"]
        subprocess.run(argv, check=True)  # nosec B603
        archive.unlink()
        self.chown(self.actions)
        return dest

    def checkout(self) -> int:
        """actions/checkout of a push to main, from the mounted repository:
        a clone with every branch and tag (fetch-depth 0), on `main` at the
        commit, the working tree's changes as ci-rehearsal.sh saved them
        (its tracked changes' patch and its untracked files' tar) committed
        on top, `origin/main` at the result, as after the push."""
        git = ["git", "-c", "safe.directory=*"]
        ws = str(self.workspace)

        def run(*args: str) -> None:
            subprocess.run([*git, "-C", ws, *args], check=True)  # git, fixed  # nosec B603

        subprocess.run(  # git, fixed  # nosec B603
            [*git, "clone", "--quiet", "--no-checkout", str(self.src), ws], check=True
        )
        run("checkout", "--quiet", "--force", "-B", "main", self.rev)
        changed = False
        if self.patch and self.patch.stat().st_size:
            run("apply", "--binary", "--whitespace=nowarn", str(self.patch))
            changed = True
        if self.untracked and self.untracked.stat().st_size:
            with tarfile.open(self.untracked) as tar:
                members = tar.getmembers()
                # The tar filter refuses a path out of the workspace.
                tar.extractall(self.workspace, members=members, filter="tar")  # nosec B202
            changed = changed or bool(members)
        if changed:
            run("add", "--all")
            run(
                "-c",
                "user.name=rehearsal",
                "-c",
                "user.email=rehearsal@localhost",
                "commit",
                "--quiet",
                "--no-verify",
                "-m",
                "rehearsal: the working tree's changes",
            )
            log(f"checkout: {self.rev} with the working tree's changes")
        else:
            log(f"checkout: {self.rev}, a clean working tree")
        run("update-ref", "refs/remotes/origin/main", "HEAD")
        run("remote", "set-url", "origin", f"https://github.com/{REPOSITORY}")
        self.chown(self.workspace)
        return 0

    def action(
        self, step: dict[str, Any], env: dict[str, str], contexts: dict[str, Any]
    ) -> int:
        repo, sub, ref = uses_parts(str(step["uses"]))
        if repo == "actions/checkout":
            # What the clone does: every branch and tag, no credentials.
            given = {str(k): text_of(v) for k, v in (step.get("with") or {}).items()}
            if given.get("fetch-depth", "1") != "0" or set(given) - {
                "fetch-depth",
                "persist-credentials",
            }:
                raise JobError(
                    f"checkout with {given}: the rehearsal clones with fetch-depth 0 only"
                )
            return self.checkout()
        root = self.fetch(repo, ref)
        folder = root / sub if sub else root
        meta_file = next(
            (
                folder / n
                for n in ("action.yml", "action.yaml")
                if (folder / n).exists()
            ),
            None,
        )
        if meta_file is None:
            raise JobError(f"{step['uses']}: no action.yml")
        meta = yaml.safe_load(meta_file.read_text())
        runs = meta.get("runs") or {}
        using = str(runs.get("using", ""))
        inputs = input_env(meta, step.get("with") or {}, contexts)
        env = {**env, "GITHUB_ACTION_REPOSITORY": repo, "GITHUB_ACTION_REF": ref}
        if "post" in runs:
            self.skipped_posts.append(
                f"{step.get('name') or step['uses']} ({runs['post']})"
            )
        if re.fullmatch(r"node\d+", using):
            for part in ("pre", "main"):
                if part in runs:
                    status = self.execute(
                        [str(NODE), str(folder / runs[part])],
                        {**env, **inputs},
                        self.workspace,
                    )
                    if status != 0:
                        return status
            return 0
        if using == "composite":
            # The `inputs` context; a composite action's steps get no
            # INPUT_ variables of their own (they map them in `env`).
            inputs_ctx = {
                str(name): inputs["INPUT_" + str(name).replace(" ", "_").upper()]
                for name in meta.get("inputs") or {}
            }
            extra = {
                "inputs": inputs_ctx,
                "github": {
                    "action_path": str(folder),
                    "action_repository": repo,
                    "action_ref": ref,
                },
            }
            return self.composite(runs.get("steps") or [], env, extra, folder)
        raise JobError(
            f"{step['uses']}: the rehearsal doesn't run `using: {using}` actions"
        )

    def composite(
        self,
        steps: list[dict[str, Any]],
        env: dict[str, str],
        extra: dict[str, Any],
        folder: Path,
    ) -> int:
        for step in steps:
            ctx = self.contexts({**env}, extra)
            if not condition(step.get("if"), ctx, "success"):
                continue
            if "run" not in step:
                raise JobError(
                    "a composite action's `uses` step: not run by the rehearsal"
                )
            unread = set(step) - {"run", "shell", "env", "if", "name"}
            if unread:
                raise JobError(
                    f"a composite action's step with {sorted(unread)}: not run by the rehearsal"
                )
            step_env = {**env, "GITHUB_ACTION_PATH": str(folder)}
            for key, value in (step.get("env") or {}).items():
                step_env[str(key)] = interpolate(value, {**ctx, "env": step_env})
            text = interpolate(step["run"], ctx)
            status = self.script(text, step.get("shell"), step_env, self.workspace)
            if status != 0:
                return status
        return 0

    def run(self) -> int:
        # Only in a rehearsal's root (provision.sh made its runner user):
        # the job writes under /home/runner and runs as that user.
        try:
            pwd.getpwnam(USER)
        except KeyError:
            raise JobError(f"no {USER} user: not a rehearsal's root") from None
        self.prepare()
        defaults = ((self.data.get("defaults") or {}).get("run") or {}) | (
            (self.job.get("defaults") or {}).get("run") or {}
        )
        log(
            f"rehearsal: job {self.name} on {self.label} (Ubuntu {self.release}, {self.arch}), {self.rev}"
        )
        start = time.monotonic()
        for index, step in enumerate(self.job.get("steps") or [], 1):
            env = self.env_for(self.data.get("env"), self.job.get("env"))
            ctx = self.contexts(env)
            title = interpolate(
                step.get("name")
                or step.get("uses")
                or step.get("run", "").split("\n")[0],
                ctx,
            )
            if not condition(step.get("if"), ctx, self.status):
                log(
                    f"\n== step {index}: {title}: skipped (if: {step.get('if', 'success()')})"
                )
                self.summary.append((title, "skipped", 0.0))
                continue
            log(f"\n== step {index}: {title}")
            step_env = self.env_for(
                self.data.get("env"), self.job.get("env"), step.get("env")
            )
            ctx = self.contexts(step_env)
            began = time.monotonic()
            cwd = self.workspace / interpolate(
                step.get("working-directory")
                or defaults.get("working-directory")
                or ".",
                ctx,
            )
            if "uses" in step:
                status = self.action(step, step_env, ctx)
            else:
                status = self.script(
                    interpolate(step["run"], ctx),
                    step.get("shell") or defaults.get("shell"),
                    step_env,
                    cwd,
                )
            took = time.monotonic() - began
            if status != 0 and not step.get("continue-on-error"):
                self.status = "failure"
            self.summary.append(
                (title, "passed" if status == 0 else f"failed ({status})", took)
            )
            log(
                f"== step {index}: {title}: {'passed' if status == 0 else f'exit {status}'} ({took:.0f} s)"
            )
        total = time.monotonic() - start
        log("\n== rehearsal summary")
        for title, result, took in self.summary:
            log(f"{took:7.0f} s  {result:12}  {title}")
        if self.skipped_posts:
            log("post steps not run: " + "; ".join(self.skipped_posts))
        limit = self.job.get("timeout-minutes")
        log(
            f"job {self.name} on {self.label}: {self.status} in {total / 60:.1f} min"
            + (f" (timeout-minutes {limit})" if limit else "")
        )
        return 0 if self.status == "success" else 1


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("runners")
    p.add_argument("workflow", type=Path)
    p.add_argument("job")
    p = sub.add_parser("run")
    p.add_argument("workflow", type=Path)
    p.add_argument("job")
    p.add_argument("label")
    p.add_argument("--src", type=Path, required=True)
    p.add_argument("--rev", required=True)
    p.add_argument("--patch", type=Path)
    p.add_argument("--untracked", type=Path)
    p.add_argument("--before", help="the push's earlier commit (default: --rev)")
    args = parser.parse_args(argv)
    try:
        data = load(args.workflow)
        if args.command == "runners":
            for label, _ in runners(data, args.job):
                release, arch = platform(label)
                print(label, release, arch)
            return 0
        os.umask(0o022)
        return Job(
            data,
            args.job,
            args.label,
            args.src,
            args.rev,
            args.patch,
            args.untracked,
            args.before,
        ).run()
    except JobError as err:
        print(f"rehearsal: {err}", file=sys.stderr)
        return 2
    # A clone, an action's download or its unpacking that failed: no
    # verdict on the job.
    except (
        subprocess.CalledProcessError,
        OSError,
        tarfile.TarError,
        yaml.YAMLError,
    ) as err:
        print(f"rehearsal: could not run: {err}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
