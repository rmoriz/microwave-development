import tempfile, unittest
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
