#!/usr/bin/env python3
"""Check docs/claims.toml, its surfaces and targets, links, and generated output.

Exits 1 and prints one line per problem when anything is stale. ``--no-coverage``
skips the rule that every prose file must be owned by a claim, for the partial
inventories of the correction commits.
"""

from __future__ import annotations

import argparse
import sys
from datetime import date
from pathlib import Path

import claims_lib

ROOT = Path(__file__).resolve().parents[1]
INVENTORY = ROOT / "docs" / "claims.toml"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--no-coverage", action="store_true", help="skip the prose coverage rule")
    parser.add_argument("--no-render", action="store_true", help="skip the generated-output rule")
    parser.add_argument("--today", help="date to age manual claims against (YYYY-MM-DD)")
    args = parser.parse_args(argv)

    today = date.fromisoformat(args.today) if args.today else date.today()

    try:
        claims = claims_lib.load_claims(INVENTORY)
    except (OSError, ValueError) as error:
        print(f"{INVENTORY.relative_to(ROOT)}: {error}")
        return 1

    errors = claims_lib.validate(claims, ROOT, today)
    errors += claims_lib.link_errors(claims, ROOT)
    if not args.no_render:
        errors += claims_lib.render_errors(claims, ROOT)
    if not args.no_coverage:
        errors += claims_lib.coverage_gaps(claims, ROOT)

    for error in errors:
        print(error)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
