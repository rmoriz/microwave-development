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
        pinned = "ghcr.io/rmoriz/microwave-server:0.5.0"
        env = {**os.environ, "MW_IMAGE": pinned}
        self.assertEqual(self.base(env)["services"]["microwave"]["image"], pinned)
        self.assertEqual(self.overlay(env)["services"]["microwave"]["image"], pinned)

    def test_project_and_workstation_stay(self):
        config = self.base()
        self.assertEqual(config["name"], "microwave-e2e")
        self.assertIn("build", config["services"]["workstation"])

    def test_overlay_only_changes_the_image_source(self):
        base = self.base()["services"]
        overlay = self.overlay()["services"]
        for config in (base, overlay):
            config["microwave"].pop("image")
            config["microwave"].pop("build", None)
        self.assertEqual(base, overlay)


class DocsPageTest(unittest.TestCase):
    def setUp(self):
        self.compose_text = (ROOT / "docker-e2e" / "docker-compose.yml").read_text()
        self.page = (ROOT / "docs" / "docker-e2e.md").read_text()

    def compose_default(self, variable):
        match = re.search(r"\$\{" + variable + r":-([^}]+)\}", self.compose_text)
        self.assertIsNotNone(match, variable)
        return match.group(1)

    def row(self, marker):
        for line in self.page.splitlines():
            if line.startswith("|") and marker in line:
                return line
        self.fail(f"no table row for {marker}")

    def test_page_names_every_service_and_no_other(self):
        compose = yaml.safe_load(self.compose_text)
        for service in compose["services"]:
            self.assertIn(f"microwave-e2e-{service}-1", self.page)
        names = set(re.findall(r"microwave-e2e-([a-z0-9-]+)-1", self.page))
        self.assertEqual(names, set(compose["services"]))

    def test_page_quick_start(self):
        self.assertIn(
            "git clone https://github.com/rmoriz/microwave-development.git",
            self.page,
        )
        self.assertIn("cd docker-e2e", self.page)
        self.assertIn("docker compose up -d", self.page)

    def test_page_ports_come_from_the_compose_defaults(self):
        api = self.compose_default("MW_E2E_PORT")
        console = self.compose_default("MW_CONSOLE_PORT")
        self.assertIn(f":{api}", self.page)
        self.assertIn(f"127.0.0.1:{console}", self.page)
        self.assertEqual(
            set(re.findall(r"(?<![\d.])(\d{5})(?!\d)", self.page)),
            {api, console},
        )

    def test_page_logins_come_from_the_compose_defaults(self):
        console = self.row("Console at")
        self.assertIn(self.compose_default("MW_CONSOLE_USER"), console)
        self.assertIn(self.compose_default("MW_CONSOLE_PASSWORD"), console)
        mcp = self.row("MCP endpoint")
        self.assertIn(self.compose_default("MW_MCP_TOKEN"), mcp)
        self.assertIn("pivotal", self.page)
        self.assertIn("/var/lib/microwave/pivotal.password", self.page)

    def test_page_org_comes_from_the_compose_default(self):
        org = self.compose_default("MW_ORG")
        self.assertIn(f"organizations/{org}/mcp", self.page)

    def test_page_allowed_hosts_match_the_server_config(self):
        server = (ROOT / "docker-e2e" / "microwave" / "microwave.toml").read_text()
        match = re.search(r"allowed_hosts = \[(.*?)\]", server)
        self.assertIsNotNone(match)
        for host in re.findall(r'"([^"]+)"', match.group(1)):
            self.assertIn(host, self.page)

    def test_page_paths(self):
        self.assertIn(
            "docker compose -f docker-compose.yml -f docker-compose.build.yml up -d --build",
            self.page,
        )
        self.assertIn("docker compose down -v", self.page)

    def test_page_is_wired_into_the_site(self):
        config = (ROOT / "website" / "docs" / "docusaurus.config.ts").read_text()
        sidebar = (ROOT / "website" / "docs" / "sidebars.ts").read_text()
        self.assertIn("'docker-e2e.md'", config)
        self.assertIn("id: 'docker-e2e'", sidebar)


if __name__ == "__main__":
    unittest.main()
