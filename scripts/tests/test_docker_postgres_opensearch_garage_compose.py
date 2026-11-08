import json
import os
import pathlib
import re
import subprocess
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
STACK = ROOT / "examples" / "docker-postgres-opensearch-garage"


def compose_config(env=None):
    result = subprocess.run(
        ["docker", "compose", "config", "--format", "json"],
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
        cls.config = compose_config()

    def test_own_project_name(self):
        self.assertEqual(self.config["name"], "microwave-e2e-backends")

    def test_pinned_backend_images(self):
        services = self.config["services"]
        self.assertEqual(services["postgres"]["image"], "postgres:19beta2-alpine")
        self.assertEqual(
            services["opensearch"]["image"], "opensearchproject/opensearch:3.9.0"
        )
        self.assertEqual(services["garage"]["image"], "dxflrs/garage:v2.5.0")

    def test_backends_publish_no_host_port(self):
        for name in ("postgres", "opensearch", "garage", "garage-bootstrap"):
            self.assertNotIn("ports", self.config["services"][name], name)

    def test_distinct_host_port_defaults(self):
        compose = (STACK / "docker-compose.yml").read_text()
        self.assertRegex(compose, r"MW_E2E_PORT:-18082")
        self.assertRegex(compose, r"MW_CONSOLE_PORT:-18083")

    def test_postgres_healthcheck_uses_tcp(self):
        check = self.config["services"]["postgres"]["healthcheck"]["test"]
        self.assertIn("-h 127.0.0.1", " ".join(check))

    def test_server_waits_for_backends_and_bootstrap(self):
        depends = self.config["services"]["microwave"]["depends_on"]
        self.assertEqual(depends["garage-bootstrap"]["condition"],
                         "service_completed_successfully")
        for name in ("postgres", "opensearch", "garage"):
            self.assertEqual(depends[name]["condition"], "service_healthy")

    def test_bucket_and_credentials_reach_the_server(self):
        env = self.config["services"]["microwave"]["environment"]
        self.assertEqual(env["MW_OBJECT_STORE__BUCKET"], "chef-cookbooks")
        self.assertEqual(
            env["MW_OBJECT_STORE__ACCESS_KEY"], "GK60459373BB628A8B007A69FB"
        )
        self.assertEqual(
            env["MW_OBJECT_STORE__SECRET_KEY"],
            "72ab6d386bc67fe916f63773bf5b5d1498eb252f3829e774dc11c673f16e96e6",
        )

    def test_endpoint_is_the_compose_service(self):
        server = (STACK / "microwave" / "microwave.toml").read_text()
        self.assertIn('endpoint = "http://garage:3900"', server)
        self.assertNotIn("host.docker.internal", server)

    def test_copied_files_name_this_example(self):
        for path in (STACK / "docker-compose.yml",
                     STACK / "workstation" / "AGENTS.md",
                     STACK / "seed" / "seed.sh"):
            self.assertNotIn("examples/docker-e2e", path.read_text(), str(path))


class OverrideTest(unittest.TestCase):
    def test_bucket_override_reaches_the_server(self):
        if subprocess.run(
            ["docker", "compose", "version"], capture_output=True
        ).returncode != 0:
            raise unittest.SkipTest("docker compose is unavailable")
        env = {**os.environ, "MW_S3_BUCKET": "other-bucket"}
        config = compose_config(env=env)
        self.assertEqual(
            config["services"]["microwave"]["environment"][
                "MW_OBJECT_STORE__BUCKET"
            ],
            "other-bucket",
        )


if __name__ == "__main__":
    unittest.main()
