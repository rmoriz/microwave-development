#!/usr/bin/env python3
"""Render docs/remaining-work.md and the marked README block from docs/claims.toml.

Without flags the two files are rewritten. With ``--check`` nothing is written
and the script exits 1 when either file differs from what the inventory says.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import claims_lib

ROOT = Path(__file__).resolve().parents[1]
INVENTORY = ROOT / "docs" / "claims.toml"
DOC = ROOT / "docs" / "remaining-work.md"
README = ROOT / "README.md"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="report drift instead of writing")
    args = parser.parse_args(argv)

    try:
        claims = claims_lib.load_claims(INVENTORY)
    except (OSError, ValueError) as error:
        print(f"{INVENTORY.relative_to(ROOT)}: {error}", file=sys.stderr)
        return 1

    doc = claims_lib.render_doc(claims)
    try:
        readme = claims_lib.replace_block(README.read_text(encoding="utf-8"), claims_lib.render_block(claims))
    except (OSError, ValueError) as error:
        print(f"{README.relative_to(ROOT)}: {error}", file=sys.stderr)
        return 1

    targets = {DOC: doc, README: readme}
    if not args.check:
        for path, text in targets.items():
            path.write_text(text, encoding="utf-8")
        return 0

    stale = [
        path.relative_to(ROOT)
        for path, text in targets.items()
        if not path.exists() or path.read_text(encoding="utf-8") != text
    ]
    for path in stale:
        print(f"{path}: stale, run scripts/render-remaining-work.py", file=sys.stderr)
    return 1 if stale else 0


if __name__ == "__main__":
    sys.exit(main())
