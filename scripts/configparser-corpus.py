#!/usr/bin/env python3
r"""Freeze how Saltbox's reader reads sb-config's fact-file texts.

Saltbox's module reads a fact file with `configparser` and its own
settings (`saltbox_facts.py`'s `create_config_parser`, as sb-go's
`readSaltboxPythonFacts`), from a UTF-8 text stream with universal
newlines (its `open(path, encoding="utf-8")`). For each text, this
records each section's own keys (its `_sections`, no `DEFAULT`
inherited) and `DEFAULT` as its own entry, or null where Python refuses
the text. sb-config's tests (`crates/config/src/facts/document/tests.rs`,
`saltbox_reads_all`) compare sb's reading with this file and start no
Python themselves.

Run it on the Python minor Saltbox's master pins (its `.python-version`
on GitHub), in a container, from the repository's root:

    docker run --rm -v "$PWD:/src:ro" -w /src python:<version>-slim \
        python3 -I scripts/configparser-corpus.py --check

--check reads the texts the file holds and fails, printing the first
difference, unless this Python reads each as the file says; it refuses
another Python minor than the file's. To freeze the texts sb-config's
tests compare now (after a change to them, or to the corpus), let the
tests write them, then read them:

    SB_CONFIGPARSER_TEXTS=<empty dir> cargo test -p sb-config --lib \
        facts::document::tests
    docker run --rm --user "$(id -u):$(id -g)" -v "$PWD:/src" \
        -v "<dir>:/texts:ro" -w /src python:<version>-slim \
        python3 -I scripts/configparser-corpus.py --texts /texts

Each JSON file in the directory is a list of texts. The output, one text
a line in text order, is FIXTURE, by default
crates/config/tests/fixtures/configparser/reads.json, with the Python
version that wrote it.
"""

from __future__ import annotations

import argparse
import configparser
import io
import json
import platform
import sys
from pathlib import Path
from typing import TypedDict

DEFAULT_FIXTURE = (
    Path(__file__).resolve().parent.parent
    / "crates/config/tests/fixtures/configparser/reads.json"
)
GENERATOR = "scripts/configparser-corpus.py"

Sections = dict[str, dict[str, str]]


class Read(TypedDict):
    """A text and how Saltbox's reader reads it."""

    text: str
    sections: Sections | None


def read(text: str) -> Sections | None:
    """`text` as Saltbox's reader reads it, or None where it refuses it."""
    parser = configparser.ConfigParser(
        interpolation=None,
        comment_prefixes=("#",),
        inline_comment_prefixes=None,
        default_section="DEFAULT",
        delimiters=("=",),
        empty_lines_in_values=False,
    )
    # Keys keep their case, as Saltbox's parser's do.
    parser.optionxform = str  # type: ignore[assignment,method-assign]
    stream = io.TextIOWrapper(io.BytesIO(text.encode("utf-8")), encoding="utf-8")
    try:
        parser.read_file(stream)
    except configparser.Error:
        return None
    # `_sections` holds each section's own keys, as Saltbox's module reads
    # them; `parser[section]` would add DEFAULT's.
    own: dict[str, dict[str, str]] = parser._sections  # type: ignore[attr-defined]
    sections = {name: dict(own[name]) for name in parser.sections()}
    sections["DEFAULT"] = dict(parser.defaults())
    return sections


def minor(version: str) -> str:
    """A version's major.minor."""
    return ".".join(version.split(".")[:2])


def render(reads: list[Read]) -> str:
    """The fixture: its header, then one read a line, in text order.

    Joined by hand, not one json.dumps, so that a changed read is a
    changed line in a diff.
    """
    header = json.dumps({"generator": GENERATOR, "python": platform.python_version()})
    lines = [json.dumps(entry, ensure_ascii=False, sort_keys=True) for entry in reads]
    return header[:-1] + ', "reads": [\n' + ",\n".join(lines) + "\n]}\n"


def texts_in(directory: Path) -> list[str]:
    """Every text in the directory's JSON files, each once, in order."""
    texts: set[str] = set()
    for path in sorted(directory.glob("*.json")):
        batch = json.loads(path.read_text(encoding="utf-8"))
        if not isinstance(batch, list) or not all(isinstance(t, str) for t in batch):
            raise SystemExit(f"{path}: not a JSON list of texts")
        texts.update(batch)
    if not texts:
        raise SystemExit(f"{directory}: no texts")
    return sorted(texts)


def check(fixture: Path) -> int:
    """0 when this Python reads every text as the fixture says, 1 when it
    reads one otherwise, 2 when it is another minor than the fixture's."""
    frozen = json.loads(fixture.read_text(encoding="utf-8"))
    if minor(frozen["python"]) != minor(platform.python_version()):
        print(
            f"{fixture} was written by Python {frozen['python']}; "
            f"this is {platform.python_version()}: run the minor it names",
            file=sys.stderr,
        )
        return 2
    for entry in frozen["reads"]:
        fresh = read(entry["text"])
        if fresh != entry["sections"]:
            print(
                f"{fixture}: {entry['text']!r} reads as {fresh!r}, "
                f"frozen as {entry['sections']!r}",
                file=sys.stderr,
            )
            return 1
    print(f"{fixture}: {len(frozen['reads'])} texts read as frozen")
    return 0


def main() -> int:
    """The command line: --check, or --texts DIR to freeze them."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true", help="check the fixture")
    mode.add_argument("--texts", type=Path, help="freeze the texts in this directory")
    parser.add_argument("fixture", nargs="?", type=Path, default=DEFAULT_FIXTURE)
    args = parser.parse_args()
    if args.check:
        return check(args.fixture)
    reads: list[Read] = [
        {"text": text, "sections": read(text)} for text in texts_in(args.texts)
    ]
    args.fixture.parent.mkdir(parents=True, exist_ok=True)
    args.fixture.write_text(render(reads), encoding="utf-8")
    print(f"{args.fixture}: {len(reads)} texts")
    return 0


if __name__ == "__main__":
    sys.exit(main())
