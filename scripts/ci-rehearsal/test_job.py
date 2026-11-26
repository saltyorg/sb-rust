"""job.py's own logic: expressions, conditions, the workflow's runners and
steps, the runner's file commands. Run in each rehearsal container on
that image's Python before the job (scripts/ci-rehearsal.sh), or by hand:

    python3 -m unittest discover --start-directory scripts/ci-rehearsal
"""

from __future__ import annotations

import os
import re
import subprocess  # nosec B404 # the tests run git and bash on their own scratch repositories
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any, ClassVar

sys.path.insert(0, str(Path(__file__).resolve().parent))

import job

CI = Path(__file__).resolve().parents[2] / ".github" / "workflows" / "ci.yml"


class Expressions(unittest.TestCase):
    ctx: ClassVar[dict[str, Any]] = {
        "github": {
            "base_ref": "",
            "token": job.NO_TOKEN,
            "server_url": "https://github.com",
        },
        "inputs": {"fallback": "none", "tool": "a@1"},
        "matrix": {"runner": "ubuntu-24.04-arm"},
    }

    def test_literals_and_paths(self) -> None:
        self.assertEqual(job.evaluate("'it''s'", {}), "it's")
        self.assertEqual(job.evaluate("2", {}), 2.0)
        self.assertIs(job.evaluate("true", {}), True)
        self.assertIsNone(job.evaluate("null", {}))
        self.assertEqual(job.evaluate("matrix.runner", self.ctx), "ubuntu-24.04-arm")
        self.assertEqual(job.evaluate("Matrix.RUNNER", self.ctx), "ubuntu-24.04-arm")
        self.assertIsNone(job.evaluate("github.nothing.here", self.ctx))

    def test_equality_ignores_case_and_coerces(self) -> None:
        self.assertIs(
            job.evaluate("runner.os != 'Windows'", {"runner": {"os": "Linux"}}), True
        )
        self.assertIs(job.evaluate("'LINUX' == 'linux'", {}), True)
        self.assertIs(job.evaluate("'1' == 1", {}), True)
        self.assertIs(job.evaluate("null == 0", {}), True)
        self.assertIs(job.evaluate("'a' == 1", {}), False)

    def test_and_or_give_an_operand(self) -> None:
        # install-action's token: '' unless the fallback is cargo-binstall.
        text = "inputs.fallback == 'cargo-binstall' && github.token || ''"
        self.assertEqual(job.evaluate(text, self.ctx), "")
        text = "github.server_url == 'https://github.com' && 'tok' || ''"
        self.assertEqual(job.evaluate(text, self.ctx), "tok")
        self.assertEqual(job.evaluate("'' || 'b'", {}), "b")
        self.assertEqual(job.evaluate("'a' && ''", {}), "")
        self.assertIs(job.evaluate("!(1 == 1)", {}), False)
        self.assertIs(job.evaluate("!''", {}), True)

    def test_contexts_the_rehearsal_lacks_fail(self) -> None:
        for text in ("steps.x.outputs.y", "secrets.TOKEN", "needs.a.result", "vars.X"):
            with self.subTest(text=text), self.assertRaises(job.JobError):
                job.evaluate(text, self.ctx)

    def test_unreadable_expressions_fail(self) -> None:
        for text in ("contains(github.ref, 'x')", "1 ==", "(1", "a b", "'x' $ 1"):
            with self.subTest(text=text), self.assertRaises(job.JobError):
                job.evaluate(text, self.ctx)

    def test_interpolate(self) -> None:
        self.assertEqual(
            job.interpolate("task / ${{ matrix.runner }}", self.ctx),
            "task / ubuntu-24.04-arm",
        )
        self.assertEqual(job.interpolate("${{ github.base_ref }}", self.ctx), "")
        self.assertEqual(job.interpolate(0, self.ctx), "0")
        self.assertEqual(job.interpolate(False, self.ctx), "false")
        self.assertEqual(job.interpolate("${{ 1 == 1 }}", {}), "true")

    def test_conditions(self) -> None:
        self.assertTrue(job.condition(None, {}, "success"))
        self.assertFalse(job.condition(None, {}, "failure"))
        self.assertTrue(job.condition("always()", {}, "failure"))
        self.assertTrue(job.condition("${{ always() }}", {}, "failure"))
        self.assertTrue(job.condition("failure()", {}, "failure"))
        self.assertFalse(job.condition("failure()", {}, "success"))
        # No status function: success() && (...).
        self.assertFalse(
            job.condition(
                "runner.os == 'Linux'", {"runner": {"os": "Linux"}}, "failure"
            )
        )
        self.assertTrue(
            job.condition(
                "runner.os == 'Linux'", {"runner": {"os": "Linux"}}, "success"
            )
        )
        self.assertFalse(
            job.condition(
                "runner.os != 'Linux'", {"runner": {"os": "Linux"}}, "success"
            )
        )


class Workflow(unittest.TestCase):
    def test_platforms(self) -> None:
        self.assertEqual(job.platform("ubuntu-22.04"), ("22.04", "amd64"))
        self.assertEqual(job.platform("ubuntu-26.04-arm"), ("26.04", "arm64"))
        for label in ("ubuntu-latest", "windows-2025", "ubuntu-24.04-armv7"):
            with self.subTest(label=label), self.assertRaises(job.JobError):
                job.platform(label)

    def test_matrix(self) -> None:
        data = {"strategy": {"matrix": {"a": [1, 2], "b": ["x"]}}}
        self.assertEqual(
            job.combinations(data), [{"a": 1, "b": "x"}, {"a": 2, "b": "x"}]
        )
        self.assertEqual(job.combinations({}), [{}])
        with self.assertRaises(job.JobError):
            job.combinations(
                {"strategy": {"matrix": {"a": [1], "include": [{"a": 2}]}}}
            )

    def test_ci_yml_task_runners(self) -> None:
        # The rehearsal's runners are ci.yml's: each one a label it knows.
        found = job.runners(job.load(CI), "task")
        labels = [label for label, _ in found]
        self.assertTrue(labels)
        self.assertEqual(len(set(labels)), len(labels))
        for label in labels:
            job.platform(label)
        self.assertIn("ubuntu-22.04", labels)
        self.assertIn("ubuntu-26.04-arm", labels)

    def test_ci_yml_task_steps_are_ones_the_rehearsal_runs(self) -> None:
        # Every step of ci.yml's task job is a `run` in a shell the
        # rehearsal starts, or an owner/repo@commit action, and its name,
        # env and condition evaluate: a step it can't run fails here, not
        # an hour into a rehearsal.
        data = job.load(CI)
        task = job.job_of(data, "task")
        shell = ((data.get("defaults") or {}).get("run") or {}).get("shell")
        for label, combo in job.runners(data, "task"):
            ctx = {
                "matrix": combo,
                "github": {"base_ref": "", "token": job.NO_TOKEN},
                "runner": {"os": "Linux"},
                "env": {},
            }
            for step in task["steps"]:
                with self.subTest(
                    label=label, step=step.get("name") or step.get("uses")
                ):
                    self.assertTrue(("run" in step) != ("uses" in step))
                    if "uses" in step:
                        _, _, ref = job.uses_parts(step["uses"])
                        self.assertRegex(ref, r"^[0-9a-f]{40}$")
                    else:
                        job.shell_argv(step.get("shell") or shell, Path("/x.sh"))
                    job.interpolate(step.get("name", ""), ctx)
                    for value in (step.get("env") or {}).values():
                        job.interpolate(value, ctx)
                    job.condition(step.get("if"), ctx, "failure")

    def test_jobs_and_steps_the_rehearsal_cannot_run_fail(self) -> None:
        extras: tuple[dict[str, Any], ...] = (
            {"if": "always()"},
            {"needs": "x"},
            {"services": {}},
            {"container": "a"},
        )
        for extra in extras:
            data = {"jobs": {"j": {"runs-on": "ubuntu-24.04", "steps": [], **extra}}}
            with self.subTest(extra=extra), self.assertRaises(job.JobError):
                job.job_of(data, "j")
        data = {
            "jobs": {
                "j": {"runs-on": "ubuntu-24.04", "steps": [{"id": "a", "run": "true"}]}
            }
        }
        with self.assertRaises(job.JobError):
            job.Job(data, "j", "ubuntu-24.04", Path("/src"), "0" * 40)

    def test_checkout_inputs_other_than_a_full_clone_fail(self) -> None:
        run = job.Job(job.load(CI), "task", "ubuntu-24.04", Path("/src"), "0" * 40)
        for given in ({}, {"fetch-depth": 1}, {"fetch-depth": 0, "ref": "x"}):
            step = {"uses": "actions/checkout@" + "0" * 40, "with": given}
            with self.subTest(given=given), self.assertRaises(job.JobError):
                run.action(step, {}, {})

    def test_uses(self) -> None:
        self.assertEqual(job.uses_parts("a/b@v1"), ("a/b", "", "v1"))
        self.assertEqual(
            job.uses_parts("a/b/sub/dir@" + "0" * 40), ("a/b", "sub/dir", "0" * 40)
        )
        for uses in ("./local", "docker://alpine:3", "a@v1"):
            with self.subTest(uses=uses), self.assertRaises(job.JobError):
                job.uses_parts(uses)


class Runner(unittest.TestCase):
    def test_shells(self) -> None:
        script = Path("/t/s.sh")
        self.assertEqual(
            job.shell_argv("bash", script),
            ["bash", "--noprofile", "--norc", "-eo", "pipefail", "/t/s.sh"],
        )
        self.assertEqual(job.shell_argv(None, script)[0], "bash")
        self.assertEqual(job.shell_argv("sh", script), ["sh", "-e", "/t/s.sh"])
        self.assertEqual(
            job.shell_argv("/usr/bin/env -u ENV /bin/sh -eu {0}", script),
            ["/usr/bin/env", "-u", "ENV", "/bin/sh", "-eu", "/t/s.sh"],
        )
        with self.assertRaises(job.JobError):
            job.shell_argv("pwsh", script)

    def test_env_file(self) -> None:
        text = "A=1\nB=x=y\n\nC<<EOF\nline 1\nline 2\nEOF\nD=\n"
        self.assertEqual(
            job.read_env_file(text),
            {"A": "1", "B": "x=y", "C": "line 1\nline 2", "D": ""},
        )
        with self.assertRaises(job.JobError):
            job.read_env_file("C<<EOF\nnever closed\n")
        with self.assertRaises(job.JobError):
            job.read_env_file("no equals sign\n")

    def test_inputs(self) -> None:
        action = {
            "inputs": {
                "tool": {"required": True},
                "fallback": {"default": "cargo-binstall"},
                "token": {
                    "default": "${{ github.server_url == 'https://github.com' && github.token || '' }}"
                },
                "fetch depth": {"default": 1},
            }
        }
        ctx = {"github": {"server_url": "https://github.com", "token": job.NO_TOKEN}}
        env = job.input_env(action, {"Tool": "a@1,b@2", "fallback": "none"}, ctx)
        self.assertEqual(
            env,
            {
                "INPUT_TOOL": "a@1,b@2",
                "INPUT_FALLBACK": "none",
                "INPUT_TOKEN": job.NO_TOKEN,
                "INPUT_FETCH_DEPTH": "1",
            },
        )

    def test_env_layers_and_paths(self) -> None:
        data = job.load(CI)
        run = job.Job(data, "task", "ubuntu-24.04", Path("/src"), "0" * 40)
        run.exported["FROM_STEP"] = "1"
        run.paths += ["/first", "/second"]
        env = run.env_for(
            {"W": "${{ matrix.runner }}"},
            {"J": "${{ env.W }}-job"},
            {"S": "${{ github.base_ref }}"},
        )
        self.assertEqual(env["W"], "ubuntu-24.04")
        self.assertEqual(env["J"], "ubuntu-24.04-job")
        self.assertEqual(env["S"], "")
        self.assertEqual(env["FROM_STEP"], "1")
        self.assertTrue(env["PATH"].startswith("/second:/first:"))
        self.assertEqual(env["RUNNER_ARCH"], "X64")
        self.assertEqual(env["GITHUB_WORKSPACE"], "/home/runner/work/sb-rust/sb-rust")
        arm = job.Job(data, "task", "ubuntu-26.04-arm", Path("/src"), "0" * 40)
        self.assertEqual(arm.runner_env()["RUNNER_ARCH"], "ARM64")
        self.assertEqual(arm.runner_env()["ImageOS"], "ubuntu26")
        with self.assertRaises(job.JobError):
            job.Job(data, "task", "ubuntu-20.04", Path("/src"), "0" * 40)


class PushBase(unittest.TestCase):
    """ci.yml's task step judges a push against the commit it moved main
    from, the push event's `before`, as job.py gives it to the step."""

    def test_before_comes_from_the_event(self) -> None:
        data = job.load(CI)
        run = job.Job(
            data, "task", "ubuntu-24.04", Path("/src"), "a" * 40, before="b" * 40
        )
        ctx = run.contexts({})
        self.assertEqual(ctx["github"]["event"]["before"], "b" * 40)
        self.assertEqual(ctx["github"]["event"]["after"], "a" * 40)
        self.assertEqual(job.interpolate("${{ github.event.before }}", ctx), "b" * 40)
        self.assertEqual(run.event()["before"], "b" * 40)
        # Without one, the checkout's own commit: no change of its own.
        plain = job.Job(data, "task", "ubuntu-24.04", Path("/src"), "a" * 40)
        self.assertEqual(plain.contexts({})["github"]["event"]["before"], "a" * 40)

    def base(self, before: str, base_ref: str = "") -> tuple[str, str]:
        """The base ci.yml's step gives check.sh in a two-commit
        repository, and what it printed, for a push whose `before` is
        `before` (`first`, `second`, `side` and `elsewhere` name commits)."""
        data = job.load(CI)
        step = next(
            s for s in data["jobs"]["task"]["steps"] if s.get("name") == "check.sh task"
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            config = root / "gitconfig"
            config.write_text("")
            git_env = {
                "PATH": os.environ["PATH"],
                "HOME": tmp,
                "GIT_CONFIG_GLOBAL": str(config),
                "GIT_CONFIG_SYSTEM": str(config),
            }
            repo = root / "repo"

            def git(*args: str) -> str:
                out = subprocess.run(  # nosec B603 B607
                    [
                        "git",
                        "-c",
                        "user.name=t",
                        "-c",
                        "user.email=t@example.invalid",
                        *args,
                    ],
                    cwd=repo,
                    env=git_env,
                    check=True,
                    capture_output=True,
                    text=True,
                )
                return out.stdout.strip()

            repo.mkdir()
            git("init", "--quiet")
            git("commit", "--quiet", "--allow-empty", "-m", "first")
            first = git("rev-parse", "HEAD")
            git("commit", "--quiet", "--allow-empty", "-m", "second")
            second = git("rev-parse", "HEAD")
            git("update-ref", "refs/remotes/origin/main", first)
            # A commit of no history of HEAD's, and one the clone hasn't.
            side = git("commit-tree", "-m", "side", git("rev-parse", "HEAD^{tree}"))
            shas = {
                "first": first,
                "second": second,
                "side": side,
                "elsewhere": "c" * 40,
            }
            # A stand-in sudo, first on PATH: it prints check.sh's base.
            bin_dir = root / "bin"
            bin_dir.mkdir()
            sudo = bin_dir / "sudo"
            sudo.write_text(
                '#!/bin/sh\nfor a in "$@"; do last=$a; done\necho "base=$last"\n'
            )
            sudo.chmod(0o755)
            run = job.Job(
                data,
                "task",
                "ubuntu-24.04",
                Path("/src"),
                second,
                before=shas.get(before, before),
            )
            ctx = run.contexts({}, {"github": {"base_ref": base_ref}})
            env = {
                key: job.interpolate(value, ctx) for key, value in step["env"].items()
            }
            env.update(git_env, PATH=f"{bin_dir}:{os.environ['PATH']}")
            # Run as job.py runs a `run` step: its text in a file, given to
            # the step's shell.
            script = root / "step.sh"
            script.write_text(job.interpolate(step["run"], ctx))
            out = subprocess.run(  # nosec B603
                job.shell_argv(step.get("shell"), script),
                cwd=repo,
                env=env,
                check=True,
                capture_output=True,
                text=True,
            )
            printed = out.stdout + out.stderr
            base = next(
                line for line in printed.splitlines() if line.startswith("base=")
            )[5:]
            names = {sha: name for name, sha in shas.items()}
            return names.get(base, base), printed

    def test_a_push_is_judged_against_its_before(self) -> None:
        self.assertEqual(self.base("first")[0], "first")

    def test_a_new_branch_is_judged_against_head_and_says_so(self) -> None:
        base, printed = self.base("0" * 40)
        self.assertEqual(base, "HEAD")
        self.assertIn("the push has no earlier commit", printed)

    def test_a_before_that_isnt_an_ancestor_is_judged_against_head_and_says_so(
        self,
    ) -> None:
        base, printed = self.base("side")
        self.assertEqual(base, "HEAD")
        self.assertIn("isn't an ancestor of HEAD", printed)

    def test_a_before_the_clone_hasnt_is_judged_against_head_and_says_so(self) -> None:
        base, printed = self.base("elsewhere")
        self.assertEqual(base, "HEAD")
        self.assertIn("isn't in the clone", printed)

    def test_the_task_step_leaves_mutation_to_the_local_gate(self) -> None:
        data = job.load(CI)
        step = next(
            s for s in data["jobs"]["task"]["steps"] if s.get("name") == "check.sh task"
        )
        self.assertEqual(step["env"]["SB_GATE_MUTATION"], "off")
        # Kept through sudo, as check.sh runs as root, with GITHUB_ACTIONS,
        # which check.sh asks for beside it.
        kept = re.search(r"--preserve-env=(\S+)", step["run"])
        if kept is None:
            self.fail(step["run"])
        self.assertLessEqual(
            {"SB_GATE_MUTATION", "GITHUB_ACTIONS"}, set(kept.group(1).split(","))
        )

    def test_a_pull_request_keeps_its_merge_base(self) -> None:
        self.assertEqual(self.base("second", base_ref="main")[0], "first")


if __name__ == "__main__":
    unittest.main()
