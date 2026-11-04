import pathlib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]


class DockerfileTest(unittest.TestCase):
    def setUp(self):
        self.text = (ROOT / "Dockerfile").read_text()

    def test_local_is_the_last_stage(self):
        stages = [line for line in self.text.splitlines() if line.startswith("FROM ")]
        self.assertEqual(stages[-1], "FROM runtime AS local")

    def test_release_does_not_use_the_builder(self):
        release = self.text.split("FROM runtime AS release", 1)[1].split(
            "FROM runtime AS local", 1
        )[0]
        self.assertNotIn("--from=builder", release)
        self.assertIn("COPY --chmod=0755 microwave-server", release)

    def test_runtime_contract_is_shared(self):
        runtime = self.text.split("FROM debian:trixie-slim AS runtime", 1)[1].split(
            "FROM runtime AS release", 1
        )[0]
        self.assertIn("USER microwave:microwave", runtime)
        self.assertIn("HEALTHCHECK", runtime)
        self.assertIn("packaging/microwave.toml.example", runtime)
        self.assertIn("org.opencontainers.image.version", runtime)


if __name__ == "__main__":
    unittest.main()
