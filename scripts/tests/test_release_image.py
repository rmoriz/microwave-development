import pathlib
import unittest

import yaml

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


class WorkflowTest(unittest.TestCase):
    def setUp(self):
        path = ROOT / ".github" / "workflows" / "release.yml"
        self.workflow = yaml.safe_load(path.read_text())
        if True in self.workflow:
            self.workflow["on"] = self.workflow.pop(True)

    def test_trigger_is_tag_only(self):
        self.assertNotIn("workflow_dispatch", self.workflow["on"])
        self.assertEqual(self.workflow["on"]["push"]["tags"], ["v*"])

    def test_meta_job_resolves_the_tag(self):
        meta = self.workflow["jobs"]["meta"]
        self.assertIn("scripts/release-image-tags.sh", yaml.safe_dump(meta))
        self.assertEqual(set(meta["outputs"]), {"version", "major_minor", "stable"})

    def test_image_jobs_may_write_packages(self):
        for job in ("build", "manifest"):
            self.assertEqual(self.workflow["jobs"][job]["permissions"]["packages"], "write")

    def test_build_order(self):
        names = [step.get("name", "") for step in self.workflow["jobs"]["build"]["steps"]]
        self.assertLess(names.index("Upload the archive"), names.index("Stage the image context"))
        self.assertLess(names.index("Stage the image context"), names.index("Build the architecture image"))
        self.assertLess(names.index("Build the architecture image"), names.index("Smoke-test the image"))
        self.assertLess(names.index("Smoke-test the image"), names.index("Log in to GHCR"))
        self.assertLess(names.index("Log in to GHCR"), names.index("Push the architecture image"))

    def test_publish_waits_for_the_manifest(self):
        self.assertEqual(set(self.workflow["jobs"]["publish"]["needs"]), {"build", "manifest"})


if __name__ == "__main__":
    unittest.main()
