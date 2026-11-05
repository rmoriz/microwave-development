import json
import os
import pathlib
import re
import subprocess
import unittest

import yaml

ROOT = pathlib.Path(__file__).resolve().parents[2]
STACK = ROOT / "docker-e2e"
OVERLAY = ["-f", "docker-compose.yml", "-f", "docker-compose.build.yml"]


def compose_config(extra=(), env=None):
    result = subprocess.run(
        ["docker", "compose", *extra, "config", "--format", "json"],
        cwd=STACK,
        env=env,
        capture_output=True,
        text=True,
        check=True,
    )
    return json.loads(result.stdout)


class ComposeTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if subprocess.run(
            ["docker", "compose", "version"], capture_output=True
        ).returncode != 0:
            raise unittest.SkipTest("docker compose is unavailable")

    def base(self, env=None):
        return compose_config(env=env)

    def overlay(self, env=None):
        return compose_config(extra=OVERLAY, env=env)

    def test_pull_is_the_default(self):
        service = self.base()["services"]["microwave"]
        self.assertEqual(service["image"], "ghcr.io/rmoriz/microwave-server:latest")
        self.assertNotIn("build", service)

    def test_overlay_builds_locally(self):
        service = self.overlay()["services"]["microwave"]
        self.assertEqual(service["image"], "microwave-server:e2e")
        self.assertEqual(pathlib.Path(service["build"]["context"]), ROOT)
        self.assertTrue(service["build"]["dockerfile"].endswith("Dockerfile"))
        self.assertEqual(str(service["build"]["args"]["RUST_VERSION"]), "1")

    def test_pinned_image_wins_in_both_modes(self):
        pinned = "ghcr.io/rmoriz/microwave-server:0.3.1"
        env = {**os.environ, "MW_IMAGE": pinned}
        self.assertEqual(self.base(env)["services"]["microwave"]["image"], pinned)
        self.assertEqual(self.overlay(env)["services"]["microwave"]["image"], pinned)

    def test_project_and_workstation_stay(self):
        config = self.base()
        self.assertEqual(config["name"], "microwave-e2e")
        self.assertIn("build", config["services"]["workstation"])


if __name__ == "__main__":
    unittest.main()
