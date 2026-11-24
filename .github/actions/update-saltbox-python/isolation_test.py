#!/usr/bin/env python3
"""Exercise the action's child-process isolation, not shell source text.

Ported from sb-go's isolation_test.py (4006309); the action test runs the
prepare mode, sb-go's dry run.
"""

import hashlib
import json
import os
import shutil
import signal

# The tests start the action's scripts and fixture programs, each with a
# fixed argv and no shell (bandit's subprocess findings below).
import subprocess  # nosec B404
import tarfile
import tempfile
import time
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

ACTION = Path(__file__).resolve().parent
# The fixture repository's git, by absolute path, ignoring the user's
# configuration (signing, hooks).
GIT = shutil.which("git") or "/usr/bin/git"
GIT_ENV = os.environ | {"GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1"}


def git(repo: Path, *args: str) -> str:
    return subprocess.check_output(
        [GIT, "-C", str(repo), *args], text=True, env=GIT_ENV
    )  # nosec B603


class IsolationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="sb-action-test-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.workspace = self.root / "workspace"
        self.workspace.mkdir()
        self.caller = self.root / "caller"
        self.caller.mkdir()
        self.target = self.root / "target"
        self.target.mkdir()
        self.env = {"PATH": "/usr/bin:/bin", "HOME": str(self.root / "missing-home")}

    def command(self, kind, executable, *args):
        return [
            "/bin/bash",
            "-c",
            'source "$1"; managed_python_command "$2" "$3" "$4" "$5" "${@:6}"',
            "test",
            str(ACTION / "managed-python.sh"),
            str(self.workspace),
            kind,
            str(self.target),
            str(executable),
            *args,
        ]

    def run_child(self, kind, executable, *args, env=None):
        return subprocess.run(  # nosec B603
            self.command(kind, executable, *args),
            check=False,
            cwd=self.caller,
            env=self.env | (env or {}),
            text=True,
            capture_output=True,
        )

    def test_python_ignores_ambient_settings_and_keeps_network_configuration(self):
        hostile = {
            "UV_CONFIG_FILE": "missing.toml",
            "UV_WORKING_DIR": "/missing",
            "UV_WORKING_DIRECTORY": "/missing",
            "UV_PYTHON": "/missing/python",
            "UV_CACHE_DIR": "/missing/cache",
            "UV_INDEX_URL": "https://invalid.example",
            "UV": "/missing/uv",
            "PIP_INDEX_URL": "https://invalid.example",
            "PYTHONHOME": "/missing",
            "PYTHONPATH": str(self.caller),
            "VIRTUAL_ENV": "/missing",
            "CONDA_PREFIX": "/missing",
            "_CONDA_ROOT": "/missing",
            "__PYVENV_LAUNCHER__": "/missing",
            "XDG_CACHE_HOME": "/missing",
            "TMPDIR": "/missing",
            "TMP": "/missing",
            "TEMP": "/missing",
            "HTTPS_PROXY": "http://proxy.example:3128",
            "https_proxy": "http://proxy.example:3128",
            "NO_PROXY": "localhost",
            "SSL_CERT_FILE": "ca.pem",
            "SSL_CERT_DIR": "certs:/etc/ssl/certs",
            "UV_SYSTEM_CERTS": "true",
            "UV_NATIVE_TLS": "true",
        }
        (self.caller / "json.py").write_text('raise RuntimeError("shadowed json")\n')
        code = "import json,os,sys; print(json.dumps(dict(cwd=os.getcwd(), env=dict(os.environ), isolated=sys.flags.isolated)))"
        result = self.run_child("python", "/usr/bin/python3", "-c", code, env=hostile)
        self.assertEqual(result.returncode, 0, result.stderr)
        got = json.loads(result.stdout)
        self.assertEqual(got["cwd"], str(self.target))
        self.assertEqual(got["env"]["PWD"], str(self.target))
        self.assertEqual(got["isolated"], 1)
        for key in hostile:
            if key.startswith(
                ("UV", "PIP", "PYTHON", "VIRTUAL", "CONDA", "_CONDA", "__PYVENV")
            ) and key not in ("UV_CACHE_DIR", "UV_SYSTEM_CERTS", "UV_NATIVE_TLS"):
                self.assertNotIn(key, got["env"])
        self.assertEqual(got["env"]["HTTPS_PROXY"], hostile["HTTPS_PROXY"])
        self.assertEqual(got["env"]["https_proxy"], hostile["https_proxy"])
        self.assertEqual(got["env"]["SSL_CERT_FILE"], str(self.caller / "ca.pem"))
        self.assertEqual(
            got["env"]["SSL_CERT_DIR"], str(self.caller / "certs") + ":/etc/ssl/certs"
        )
        self.assertEqual(got["env"]["UV_CACHE_DIR"], str(self.workspace / "cache"))
        self.assertEqual(got["env"]["UV_SYSTEM_CERTS"], "true")
        scratch = Path(got["env"]["TMPDIR"])
        self.assertTrue(scratch.is_relative_to(self.workspace / "tmp"))
        self.assertFalse(scratch.exists())

    def test_absolute_inputs_work_from_deleted_caller_directory(self):
        result = subprocess.run(  # nosec B603
            self.command(
                "python", "/usr/bin/python3", "-c", "import os; print(os.getcwd())"
            ),
            check=False,
            cwd=self.caller,
            env=self.env,
            preexec_fn=lambda: os.rmdir(self.caller),
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(self.target))

    def test_uv_receives_no_config_and_explicit_install_directory(self):
        probe = self.root / "uv-probe"
        probe.write_text(
            '#!/usr/bin/python3\nimport json,os,sys\nprint(json.dumps([sys.argv[1:],os.environ.get("UV_PYTHON_INSTALL_DIR")]))\n'
        )
        probe.chmod(0o755)
        result = self.run_child("uv", probe, "python", "find", "3.12.13")
        self.assertEqual(result.returncode, 0, result.stderr)
        args, install_dir = json.loads(result.stdout)
        self.assertEqual(args, ["--no-config", "python", "find", "3.12.13"])
        self.assertEqual(install_dir, str(self.workspace / "python"))

    def test_entrypoint_ignores_ansible_config_and_uses_selected_venv(self):
        bin_dir = self.target / "bin"
        bin_dir.mkdir()
        probe = bin_dir / "ansible"
        probe.write_text(
            '#!/usr/bin/python3\nimport json,os\nfrom pathlib import Path\nprint(json.dumps(dict(env=dict(os.environ), config=Path(os.environ["ANSIBLE_CONFIG"]).read_text())))\n'
        )
        probe.chmod(0o755)
        result = self.run_child(
            "entrypoint",
            probe,
            "--version",
            env={
                "ANSIBLE_CONFIG": "/missing",
                "ANSIBLE_HOME": "/missing",
                "PYTHONPATH": "/missing",
            },
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        got = json.loads(result.stdout)
        self.assertEqual(got["config"], "")
        self.assertNotIn("ANSIBLE_HOME", got["env"])
        self.assertEqual(got["env"]["PYTHONNOUSERSITE"], "1")
        self.assertEqual(got["env"]["PYTHONSAFEPATH"], "1")
        self.assertTrue(got["env"]["PATH"].startswith(str(bin_dir) + ":"))
        self.assertFalse(Path(got["env"]["ANSIBLE_CONFIG"]).exists())

    @unittest.skipUnless(
        os.environ.get("SB_TEST_UV_BINARY"),
        "set SB_TEST_UV_BINARY for real uv acceptance",
    )
    def test_real_uv_ignores_ambient_config_and_storage(self):
        uv = Path(os.environ["SB_TEST_UV_BINARY"]).resolve()
        (self.caller / "uv.toml").write_text("invalid = [")
        (self.caller / ".python-version").write_text("0.0.0")
        (self.caller / ".env").write_text("UV_PYTHON=/missing\n")
        (self.caller / ".venv").mkdir()
        hostile = {
            "UV_CONFIG_FILE": "/missing",
            "UV_WORKING_DIR": "/missing",
            "UV_WORKING_DIRECTORY": "/missing",
            "UV_CACHE_DIR": "/missing",
            "UV_PROJECT_ENVIRONMENT": "/missing",
            "UV_PYTHON": "/missing",
            "PYTHONHOME": "/missing",
            "PYTHONPATH": "/missing",
            "VIRTUAL_ENV": "/missing",
            "XDG_CACHE_HOME": "/missing",
            "TMPDIR": "/missing",
        }
        venv = self.root / "venv"
        result = self.run_child(
            "uv",
            uv,
            "venv",
            "--python",
            shutil.which("python3"),
            "--no-project",
            "--no-python-downloads",
            str(venv),
            env=hostile,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        result = self.run_child(
            "uv", uv, "pip", "check", "--python", str(venv / "bin/python"), env=hostile
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(list((self.workspace / "tmp").iterdir()), [])

    def test_python_preserves_stdin(self):
        result = subprocess.run(  # nosec B603
            self.command("python", "/usr/bin/python3", "-"),
            check=False,
            cwd=self.caller,
            env=self.env,
            input='print("stdin preserved")',
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "stdin preserved\n")

    def test_failure_keeps_stderr_and_removes_scratch(self):
        result = self.run_child(
            "python",
            "/usr/bin/python3",
            "-c",
            'import sys; print("expected failure", file=sys.stderr); sys.exit(17)',
        )
        self.assertEqual(result.returncode, 17, result.stderr)
        self.assertIn("expected failure", result.stderr)
        self.assertEqual(list((self.workspace / "tmp").iterdir()), [])

    def test_termination_removes_scratch(self):
        for sig in (signal.SIGTERM, signal.SIGINT):
            with self.subTest(signal=sig):
                marker = self.root / f"ready-{sig}"
                code = f"import pathlib,time; pathlib.Path({str(marker)!r}).touch(); time.sleep(60)"
                child = subprocess.Popen(  # nosec B603
                    self.command("python", "/usr/bin/python3", "-c", code),
                    cwd=self.caller,
                    env=self.env,
                    start_new_session=True,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.PIPE,
                    text=True,
                )
                try:
                    deadline = time.monotonic() + 5
                    while (
                        not marker.exists()
                        and child.poll() is None
                        and time.monotonic() < deadline
                    ):
                        time.sleep(0.01)
                    self.assertTrue(marker.exists(), "child did not start")
                    os.killpg(child.pid, sig)
                    child.communicate(timeout=5)
                    self.assertNotEqual(child.returncode, 0)
                    self.assertEqual(list((self.workspace / "tmp").iterdir()), [])
                finally:
                    if child.poll() is None:
                        os.killpg(child.pid, signal.SIGKILL)
                        child.wait()
                    if child.stderr is not None:
                        child.stderr.close()


class ActionTests(unittest.TestCase):
    """The prepare mode, end to end, against a fixture Saltbox, a fake curl
    serving a fixture uv archive and its checksum, and a fixture uv."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="sb-action-dry-run-")
        self.addCleanup(self.tmp.cleanup)
        self.root = root = Path(self.tmp.name)
        self.repo = repo = root / "saltbox"
        (repo / "requirements").mkdir(parents=True)
        (repo / ".github").mkdir()
        (repo / ".python-version").write_text("3.12.13\n")
        (repo / ".uv-version").write_text("0.12.7\n")
        (repo / ".github/renovate.json").write_text(
            '{"constraints": {"uv": "0.12.7"}}\n'
        )
        (repo / "requirements/requirements-saltbox.in").write_text("fixture==1.0\n")
        (repo / "requirements/requirements-saltbox.txt").write_text(MASTER_LOCK)
        for args in (
            ["init", "-b", "master"],
            ["add", "."],
            [
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.test",
                "commit",
                "-m",
                "test: create fixture",
            ],
        ):
            git(repo, *args)
        self.original = git(repo, "rev-parse", "HEAD")
        uv = root / "uv"
        uv.write_text(UV_FIXTURE)
        uv.chmod(0o755)
        self.archive = root / "uv.tar.gz"
        with tarfile.open(self.archive, "w:gz") as tar:
            tar.add(uv, arcname="uv-x86_64-unknown-linux-gnu/uv")
        self.bin_dir = root / "bin"
        self.bin_dir.mkdir()
        curl = self.bin_dir / "curl"
        # The archive, or its published checksum for a `.sha256` URL, as
        # uv's releases publish it: "<sha256>  <asset>".
        curl.write_text(
            "#!/bin/bash\n"
            'url=""; out=""\n'
            "while (($#)); do case $1 in --output) out=$2; shift 2;; --write-out) shift 2;; -*) shift;; *) url=$1; shift;; esac; done\n"
            'echo "$url" >> "$TEST_CURL_LOG"\n'
            'if [[ $url == *.sha256 ]]; then printf "%s" "$TEST_UV_SHA256" > "$out"; else cp "$TEST_UV_ARCHIVE" "$out"; fi\n'
            "printf 200\n"
        )
        curl.chmod(0o755)
        digest = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        self.published = f"{digest}  uv-x86_64-unknown-linux-gnu.tar.gz\n"

    def run_update(self, name, extra=None, hostile=False):
        caller = self.root / name
        caller.mkdir()
        if hostile:
            (caller / "uv.toml").write_text("invalid = [")
            (caller / ".python-version").write_text("0.0.0")
        log = self.root / f"calls-{name}.jsonl"
        output = self.root / f"update-{name}"
        env = {
            "PATH": str(self.bin_dir) + ":/usr/bin:/bin",
            "HOME": str(self.root),
            "GITHUB_ACTION_PATH": str(ACTION),
            "UV_VERSION": "0.12.10",
            "PYTHON_MINOR": "3.12",
            "SB_RELEASE": "1.0.0",
            "OUTPUT_DIR": str(output),
            "SALTBOX_CLONE_URL": str(self.repo),
            "TEST_UV_ARCHIVE": str(self.archive),
            "TEST_UV_SHA256": self.published,
            "TEST_CURL_LOG": str(self.root / f"curl-{name}.log"),
            "TEST_UV_LOG": str(log),
            "SSL_CERT_FILE": "ca.pem",
            "TEST_CERT_FILE": str(caller / "ca.pem"),
        }
        if hostile:
            env |= {
                "UV_CONFIG_FILE": "/missing/config",
                "UV_WORKING_DIR": "/missing/cwd",
                "UV_CACHE_DIR": "/missing/cache",
                "PYTHONHOME": "/missing/python",
                "PYTHONPATH": "/missing/path",
                "TMPDIR": "/missing/tmp",
            }
        result = subprocess.run(  # nosec B603
            ["/bin/bash", str(ACTION / "update.sh")],
            check=False,
            cwd=caller,
            env=env | (extra or {}),
            capture_output=True,
            text=True,
            timeout=30,
        )
        calls = (
            [json.loads(line) for line in log.read_text().splitlines()]
            if log.exists()
            else []
        )
        return result, output, calls

    def test_dry_run_is_independent_of_ambient_configuration(self):
        results = []
        for hostile in (False, True):
            result, output, calls = self.run_update(
                "hostile" if hostile else "clean", hostile=hostile
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("+3.12.14", result.stdout)
            self.assertIn("+0.12.10", result.stdout)
            self.assertIn('+{"constraints": {"uv": "0.12.10"}}', result.stdout)
            self.assertNotIn("sb-python-action-", result.stdout)
            self.assertEqual((output / "changed").read_text(), "true\n")
            self.assertEqual((output / "base-commit").read_text(), self.original)
            self.assertEqual(
                (output / "files/.python-version").read_text(), "3.12.14\n"
            )
            self.assertEqual((output / "files/.uv-version").read_text(), "0.12.10\n")
            self.assertEqual(
                (output / "files/.github/renovate.json").read_text(),
                '{"constraints": {"uv": "0.12.10"}}\n',
            )
            self.assertEqual(
                (output / "files/requirements/requirements-saltbox.txt").read_text(),
                "# stable fixture lock\nfixture==1.0\n",
            )
            # The preflight's, the lock's and Saltbox CI's recompile.
            self.assertEqual(
                sum(call["args"][:2] == ["pip", "compile"] for call in calls), 3
            )
            self.assertEqual(git(self.repo, "rev-parse", "HEAD"), self.original)
            results.append(result.stdout)
        self.assertEqual(results[0], results[1])

    def test_uv_archive_is_checked_against_the_published_sha256(self):
        result, _, _ = self.run_update("checked")
        self.assertEqual(result.returncode, 0, result.stderr)
        base = "https://github.com/astral-sh/uv/releases/download/0.12.10/uv-x86_64-unknown-linux-gnu.tar.gz"
        self.assertEqual(
            (self.root / "curl-checked.log").read_text().splitlines(),
            [base, base + ".sha256"],
        )

    def test_checksum_failure_stops_before_uv_runs(self):
        digest = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        other = "0" * 64
        for name, published, message in (
            (
                "mismatch",
                f"{other}  uv-x86_64-unknown-linux-gnu.tar.gz\n",
                f"uv-x86_64-unknown-linux-gnu.tar.gz's SHA-256 is {digest}, not the published {other}",
            ),
            (
                "other-asset",
                f"{digest}  uv-aarch64-unknown-linux-gnu.tar.gz\n",
                "not one SHA-256 line naming it",
            ),
            (
                "two-lines",
                self.published + self.published,
                "not one SHA-256 line naming it",
            ),
            ("empty", "", "not one SHA-256 line naming it"),
            (
                "upper-case",
                f"{digest.upper()}  uv-x86_64-unknown-linux-gnu.tar.gz\n",
                "not one SHA-256 line naming it",
            ),
        ):
            with self.subTest(case=name):
                result, output, calls = self.run_update(
                    name, extra={"TEST_UV_SHA256": published}
                )
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn(message, result.stderr)
                self.assertEqual(calls, [], "uv ran before its archive was checked")
                self.assertFalse(output.exists())
        result, _, _ = self.run_update(
            "binary-mode",
            extra={"TEST_UV_SHA256": f"{digest} *uv-x86_64-unknown-linux-gnu.tar.gz\n"},
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def compiles(self, calls):
        return [call for call in calls if call["args"][:2] == ["pip", "compile"]]

    def test_both_compiles_resolve_behind_the_seven_day_age_floor(self):
        before = datetime.now(timezone.utc).replace(microsecond=0)
        result, _, calls = self.run_update("age-floor")
        after = datetime.now(timezone.utc)
        self.assertEqual(result.returncode, 0, result.stderr)
        compiles = self.compiles(calls)
        self.assertEqual(len(compiles), 3, calls)
        floors = set()
        for call in compiles[:2]:
            args = call["args"]
            self.assertEqual(args.count("--exclude-newer"), 1, args)
            floors.add(args[args.index("--exclude-newer") + 1])
        self.assertEqual(len(floors), 1, floors)
        (floor,) = floors
        self.assertRegex(floor, r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")
        at = datetime.strptime(floor, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
        self.assertLessEqual(before - timedelta(days=7), at)
        self.assertLessEqual(at, after - timedelta(days=7))
        self.assertIn(
            f"published before {floor} (7-day age floor), but for the 2 master's lock pins",
            result.stderr,
        )
        # Saltbox's CI recompiles with the header's command, which has none.
        custom = compiles[1]["args"][
            compiles[1]["args"].index("--custom-compile-command") + 1
        ]
        self.assertNotIn("exclude-newer", custom)
        self.assertNotIn("--exclude-newer", compiles[2]["args"])
        self.assertNotIn("--exclude-newer-package", compiles[2]["args"])

    def test_master_lock_pins_are_kept_and_exempt_from_the_floor(self):
        before = datetime.now(timezone.utc).replace(microsecond=0)
        result, _, calls = self.run_update("pins")
        after = datetime.now(timezone.utc)
        self.assertEqual(result.returncode, 0, result.stderr)
        compiles = self.compiles(calls)
        for call in compiles:
            # Every compile starts from master's lock, whose pins uv keeps;
            # the CI recompile from the lock this run wrote.
            self.assertEqual(
                call["seed"],
                MASTER_LOCK
                if call is not compiles[2]
                else "# stable fixture lock\nfixture==1.0\n",
            )
        for call in compiles[:2]:
            args = call["args"]
            exempt = [
                args[i + 1]
                for i, a in enumerate(args)
                if a == "--exclude-newer-package"
            ]
            self.assertEqual(
                [e.split("=")[0] for e in exempt], ["fixture", "young-pkg"]
            )
            for e in exempt:
                at = datetime.strptime(e.split("=")[1], "%Y-%m-%dT%H:%M:%SZ").replace(
                    tzinfo=timezone.utc
                )
                self.assertLessEqual(before, at)
                self.assertLessEqual(at, after)

    def test_a_lock_saltbox_ci_would_not_reproduce_is_refused(self):
        result, output, calls = self.run_update(
            "late-wheel", extra={"TEST_UV_LATE_WHEEL": "1"}
        )
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(
            "Saltbox's CI, recompiling without the age floor, would not reproduce this lock",
            result.stderr,
        )
        self.assertIn("--hash=sha256:late", result.stderr)
        self.assertEqual(len(self.compiles(calls)), 3)
        self.assertFalse(output.exists())

    def test_a_failed_compile_stops_the_run_after_naming_the_age_floor(self):
        result, output, calls = self.run_update(
            "unresolved", extra={"TEST_UV_COMPILE_FAILS": "1"}
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("(7-day age floor)", result.stderr)
        self.assertIn("no solution found", result.stderr)
        self.assertEqual(len(self.compiles(calls)), 1)
        self.assertFalse(output.exists())


# Master's lock in the fixture Saltbox: two pins, as uv writes them.
MASTER_LOCK = """# This file was autogenerated by uv via the following command:
#    uv pip compile --python-version 3.12.13 --generate-hashes --output-file requirements/requirements-saltbox.txt requirements/requirements-saltbox.in
fixture==0.9 \\
    --hash=sha256:1111111111111111111111111111111111111111111111111111111111111111
    # via -r requirements/requirements-saltbox.in
young-pkg==2.0 \\
    --hash=sha256:2222222222222222222222222222222222222222222222222222222222222222
    # via fixture
"""


UV_FIXTURE = r"""#!/usr/bin/python3
import json, os, pathlib, sys
args = sys.argv[1:]
if not args or args.pop(0) != "--no-config":
    sys.exit("managed uv invocation did not disable ambient configuration")
for key in ["UV_CONFIG_FILE", "UV_WORKING_DIR", "PYTHONHOME", "PYTHONPATH", "UV_VERSION", "PYTHON_MINOR"]:
    if key in os.environ:
        sys.exit("ambient setting reached managed uv: " + key)
if os.environ.get("SSL_CERT_FILE") != os.environ["TEST_CERT_FILE"]:
    sys.exit("relative certificate resolved from the wrong directory")
entry = dict(args=args, cwd=os.getcwd())
if args[:2] == ["pip", "compile"]:
    output = pathlib.Path(args[args.index("--output-file")+1])
    entry["seed"] = output.read_text() if output.exists() else None
with open(os.environ["TEST_UV_LOG"], "a") as log:
    log.write(json.dumps(entry) + "\n")
python_script = '#!/bin/bash\nif [[ ${1:-} == -I ]]; then shift; fi\nif [[ ${1:-} == --version ]]; then echo "Python 3.12.14"; else exec /usr/bin/python3 -I "$@"; fi\n'
def executable(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)
    path.chmod(0o755)
if args == ["--version"]:
    print("uv 0.12.10")
elif args[:2] == ["python", "list"]:
    print(json.dumps([dict(implementation="cpython", os="linux", arch="x86_64", libc="gnu", variant="default", version="3.12.14", version_parts=dict(major=3,minor=12,patch=14))]))
elif args[:2] == ["python", "install"]:
    executable(pathlib.Path(args[args.index("--install-dir")+1]) / "bin/python3", python_script)
elif args[:2] == ["python", "find"]:
    print(pathlib.Path(os.environ["UV_PYTHON_INSTALL_DIR"]) / "bin/python3")
elif args[:2] == ["pip", "compile"]:
    if os.environ.get("TEST_UV_COMPILE_FAILS"):
        sys.exit("no solution found")
    late = os.environ.get("TEST_UV_LATE_WHEEL") and "--exclude-newer" not in args
    pathlib.Path(args[args.index("--output-file")+1]).write_text("# stable fixture lock\nfixture==1.0\n" + ("    --hash=sha256:late\n" if late else ""))
elif args[0] == "venv":
    venv = pathlib.Path(args[-1])
    executable(venv / "bin/python", python_script)
    for name in ["ansible", "certbot", "apprise"]:
        executable(venv / "bin" / name, "#!/bin/sh\nexit 0\n")
elif args[:2] not in (["pip", "sync"], ["pip", "check"]):
    sys.exit("unexpected uv command: " + repr(args))
"""

if __name__ == "__main__":
    unittest.main()
