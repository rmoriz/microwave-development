import tempfile, unittest
from datetime import date
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import claims_lib

GAP = claims_lib.Claim(
    id="compose-postgres",
    summary="The HA compose override names PostgreSQL.",
    area="deployment",
    status="gap",
    surfaces=[claims_lib.Surface("docker-compose.ha.yml", "PostgreSQL")],
    verify=claims_lib.Verify(kind="grep", path="docker-compose.ha.yml", pattern="database.driver"),
)

class RenderTest(unittest.TestCase):
    def test_doc_groups_by_area_and_anchors_ids(self):
        other = claims_lib.Claim("mail-outbox", "No durable outbox.", "mail", "gap",
                                 [claims_lib.Surface("docs/deployment.md", "outbox")],
                                 claims_lib.Verify(kind="grep", path="docs/deployment.md", pattern="outbox"))
        text = claims_lib.render_doc([other, GAP])
        self.assertLess(text.index('<a id="area-deployment">'), text.index('<a id="area-mail">'))
        self.assertIn('<a id="compose-postgres">', text)
        self.assertIn("docker-compose.ha.yml", text)
        self.assertIn("Open gaps: 2", text)

    def test_doc_omits_verified_claims(self):
        verified = claims_lib.Claim("cli-exists", "CLI exists.", "tooling", "verified",
                                    [claims_lib.Surface("README.md", "microwave-ctl")],
                                    claims_lib.Verify(kind="file", path="bin/microwave-ctl"))
        text = claims_lib.render_doc([verified])
        self.assertIn("Open gaps: 0", text)
        self.assertNotIn('<a id="cli-exists">', text)

    def test_block_counts_and_area_links(self):
        block = claims_lib.render_block([GAP])
        self.assertTrue(block.startswith(claims_lib.BLOCK_START))
        self.assertTrue(block.endswith(claims_lib.BLOCK_END))
        self.assertIn("Open gaps: 1", block)
        self.assertIn("remaining-work.md#area-deployment", block)

    def test_replace_block_keeps_surroundings(self):
        readme = "before\n" + claims_lib.BLOCK_START + "\nstale\n" + claims_lib.BLOCK_END + "\nafter\n"
        replaced = claims_lib.replace_block(readme, "NEW")
        self.assertIn("NEW", replaced)
        self.assertNotIn("stale", replaced)
        self.assertTrue(replaced.startswith("before\n"))
        self.assertTrue(replaced.endswith("\nafter\n"))

    def test_replace_block_requires_markers(self):
        with self.assertRaises(ValueError):
            claims_lib.replace_block("no markers\n", "NEW")


class ValidateTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        (self.root / "docs").mkdir()
        (self.root / "docs" / "a.md").write_text("PostgreSQL is available.\n")

    def tearDown(self):
        self.tmp.cleanup()

    def claim(self, **over):
        base = dict(id="x", summary="s", area="docs", status="verified",
                    surfaces=[claims_lib.Surface("docs/a.md", "PostgreSQL")],
                    verify=claims_lib.Verify(kind="grep", path="docs/a.md", pattern="PostgreSQL"))
        base.update(over)
        return claims_lib.Claim(**base)

    def test_valid_claim_passes(self):
        self.assertEqual(claims_lib.validate([self.claim()], self.root, date(2026, 10, 28)), [])

    def test_missing_needle_is_reported(self):
        claim = self.claim(surfaces=[claims_lib.Surface("docs/a.md", "SQLite")])
        self.assertIn("docs/a.md:x:", claims_lib.validate([claim], self.root, date(2026, 10, 28))[0])

    def test_duplicate_id_is_reported(self):
        errors = claims_lib.validate([self.claim(), self.claim()], self.root, date(2026, 10, 28))
        self.assertTrue(any("duplicate" in e for e in errors))

    def test_path_escape_is_reported(self):
        claim = self.claim(verify=claims_lib.Verify(kind="file", path="../outside"))
        self.assertTrue(any("outside" in e for e in claims_lib.validate([claim], self.root, date(2026, 10, 28))))

    def test_manual_claim_expires_after_ninety_days(self):
        claim = self.claim(status="manual",
                           verify=claims_lib.Verify(kind="manual", checked=date(2026, 7, 29), ref="docs/a.md"))
        errors = claims_lib.validate([claim], self.root, date(2026, 10, 28))
        self.assertTrue(any("expired" in e for e in errors))

    def test_fresh_manual_claim_passes(self):
        claim = self.claim(status="manual",
                           verify=claims_lib.Verify(kind="manual", checked=date(2026, 10, 28), ref="docs/a.md"))
        self.assertEqual(claims_lib.validate([claim], self.root, date(2026, 10, 28)), [])

    def test_missing_verify_target_is_reported(self):
        claim = self.claim(verify=claims_lib.Verify(kind="file", path="docs/missing.md"))
        self.assertTrue(any("missing.md" in e for e in claims_lib.validate([claim], self.root, date(2026, 10, 28))))

    def test_uncovered_doc_is_reported(self):
        (self.root / "docs" / "b.md").write_text("x\n")
        self.assertIn("docs/b.md", claims_lib.coverage_gaps([self.claim()], self.root))

    def test_link_to_unknown_id_is_reported(self):
        (self.root / "README.md").write_text("see remaining-work.md#nope\n")
        self.assertIn("nope", claims_lib.link_errors([self.claim()], self.root)[0])

    def test_area_link_passes(self):
        (self.root / "README.md").write_text("see remaining-work.md#area-docs\n")
        self.assertEqual(claims_lib.link_errors([self.claim()], self.root), [])

    def test_render_check_detects_hand_edit(self):
        (self.root / "docs" / "remaining-work.md").write_text("stale\n")
        self.assertTrue(claims_lib.render_errors([self.claim()], self.root))
