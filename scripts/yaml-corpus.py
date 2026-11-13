#!/usr/bin/env python3
"""Generate the YAML corpus's expected results from libyaml (decision 3).

For each case in CORPUS/cases/*.yml, in name order, record what libyaml
does with it, through PyYAML's libyaml binding (`CSafeLoader`: libyaml's
parser, and PyYAML's safe constructor and YAML 1.1 resolver, the parser
Ansible's loader uses when libyaml is there): whether it is accepted as
a single document, and for an accepted case, every scalar value's key path,
style, text as parsed and resolved value, and every empty collection.
Writes CORPUS/expected.json.

Run once, and again only when a case is added or changed:

    scripts/yaml-corpus.py [CORPUS]

CORPUS defaults to the repository's crates/config/tests/yaml-corpus.
Needs PyYAML built with libyaml (`yaml.__with_libyaml__`); the versions
used are recorded in the output and in
docs/platform-notes/yaml-acceptance.md. Type-checking it needs
types-PyYAML beside mypy.
"""

from __future__ import annotations

import datetime
import json
import sys
from pathlib import Path
from typing import Any

import yaml

DEFAULT_CORPUS = (
    Path(__file__).resolve().parent.parent / "crates/config/tests/yaml-corpus"
)

# PyYAML's style marks, as sb's corpus test names saphyr's styles. The
# libyaml binding marks a plain scalar "", the pure-Python parser None.
STYLES = {
    None: "plain",
    "": "plain",
    "'": "single",
    '"': "double",
    "|": "literal",
    ">": "folded",
}


def resolved(value: Any, text: str) -> list[str]:
    """A constructed scalar as [type, text]: comparable across languages,
    and exact for integers past 64 bits. A date or time keeps its source
    `text`, as sb's resolver does."""
    if value is None:
        return ["null", ""]
    if isinstance(value, bool):
        return ["bool", "true" if value else "false"]
    if isinstance(value, int):
        return ["int", str(value)]
    if isinstance(value, float):
        return ["float", repr(value)]
    if isinstance(value, str):
        return ["str", value]
    if isinstance(value, datetime.date):
        return ["timestamp", text]
    return [type(value).__name__, str(value)]


def scalars(
    loader: yaml.CSafeLoader, node: yaml.Node, path: str, out: list[dict[str, Any]]
) -> None:
    """Every scalar value under `node` at key path `path`, in document
    order, appended to `out`, and every empty collection as a `seq` or
    `map` entry. Keys name the path (`a.b`, `a[0]`) and aren't listed
    themselves."""
    if isinstance(node, yaml.ScalarNode):
        try:
            value = resolved(loader.construct_object(node, deep=True), node.value)
        except (yaml.YAMLError, ValueError) as err:
            # A scalar libyaml parses but PyYAML can't construct (a date
            # like 2025-13-45): recorded, not fatal.
            value = ["unconstructible", str(err).splitlines()[0]]
        out.append(
            {
                "path": path,
                "style": STYLES.get(node.style, str(node.style)),
                "text": node.value,
                "value": value,
            }
        )
    elif isinstance(node, yaml.SequenceNode):
        if not node.value:
            out.append({"path": path, "style": "seq", "text": "", "value": ["seq", ""]})
        for index, item in enumerate(node.value):
            scalars(loader, item, f"{path}[{index}]", out)
    elif isinstance(node, yaml.MappingNode):
        if not node.value:
            out.append({"path": path, "style": "map", "text": "", "value": ["map", ""]})
        for key, value in node.value:
            name = key.value if isinstance(key, yaml.ScalarNode) else "<key>"
            scalars(loader, value, name if not path else f"{path}.{name}", out)


def verdict(data: bytes) -> dict[str, Any]:
    """libyaml's verdict on one case: accepted, with its scalars, or the
    error it stopped at (1-based line and column)."""
    loader = yaml.CSafeLoader(data)
    try:
        node = loader.get_single_node()
        out: list[dict[str, Any]] = []
        if node is not None:
            scalars(loader, node, "", out)
        return {"accepted": True, "scalars": out}
    except yaml.MarkedYAMLError as err:
        mark = err.problem_mark
        where = f" at line {mark.line + 1}, column {mark.column + 1}" if mark else ""
        return {"accepted": False, "error": f"{err.problem}{where}"}
    finally:
        loader.dispose()


def main(argv: list[str]) -> int:
    if not yaml.__with_libyaml__:
        print("PyYAML isn't built with libyaml: no CSafeLoader", file=sys.stderr)
        return 2
    corpus = Path(argv[1]) if len(argv) > 1 else DEFAULT_CORPUS
    cases = sorted((corpus / "cases").glob("*.yml"))
    if not cases:
        print(f"no cases in {corpus / 'cases'}", file=sys.stderr)
        return 2
    expected = {
        "generator": "scripts/yaml-corpus.py",
        "pyyaml": yaml.__version__,
        "libyaml": yaml._yaml.get_version_string(),
        "cases": {case.stem: verdict(case.read_bytes()) for case in cases},
    }
    text = json.dumps(expected, indent=1, sort_keys=True, ensure_ascii=False)
    (corpus / "expected.json").write_text(text + "\n", encoding="utf-8")
    print(f"{len(cases)} cases written to {corpus / 'expected.json'}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
