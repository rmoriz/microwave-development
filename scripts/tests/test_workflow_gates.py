"""Guard the CI cost rules: documentation-only changes pay for no Rust build.

`cookbooks/**`, `docs/**` and `website/**` are not Rust, so a change to them
must not compile or test the workspace. The workflows express that by keeping
those paths out of the `rust` filter and by gating every job that runs `cargo`
on that filter's output.

The tests read the workflow YAML instead of running GitHub, so they catch a
gate that a later edit drops long before a run does.
"""

import pathlib
import unittest

import yaml

WORKFLOWS = pathlib.Path(__file__).resolve().parents[2] / ".github" / "workflows"

# Documentation-ish trees. A change under one of them alone is not a Rust build.
NON_RUST = ("cookbooks/", "docs/", "website/")

# Jobs that build or test the workspace, and the output that may gate them.
RUST_GATE = "needs.changes.outputs.rust == 'true'"


def load(name):
    return yaml.safe_load((WORKFLOWS / name).read_text())


def triggers(workflow):
    """`on:` is keyed `True` by YAML 1.1 parsers."""
    return workflow.get("on", workflow.get(True, {}))


def rust_filter(workflow):
    changes = workflow["jobs"]["changes"]
    for step in changes["steps"]:
        if step.get("with", {}).get("filters"):
            return yaml.safe_load(step["with"]["filters"])["rust"]
    raise AssertionError("changes job without a paths filter")


def runs_cargo(job):
    text = yaml.safe_dump(job)
    return "cargo " in text or "cargo\\x20" in text


class CiGateTest(unittest.TestCase):
    def test_ci_rust_filter_excludes_documentation_trees(self):
        patterns = rust_filter(load("ci.yml"))
        for path in patterns:
            self.assertFalse(
                path.startswith(NON_RUST),
                f"{path} is documentation, not Rust",
            )

    def test_postgres_rust_filter_excludes_documentation_trees(self):
        patterns = rust_filter(load("postgres.yml"))
        for path in patterns:
            self.assertFalse(
                path.startswith(NON_RUST),
                f"{path} is documentation, not Rust",
            )

    def test_every_cargo_job_is_gated_on_the_rust_filter(self):
        for name in ("ci.yml", "postgres.yml"):
            jobs = load(name)["jobs"]
            self.assertTrue(jobs, f"{name} has no jobs")
            for job_name, job in jobs.items():
                if job_name == "changes" or not runs_cargo(job):
                    continue
                self.assertEqual(
                    job.get("if"),
                    RUST_GATE,
                    f"{name}:{job_name} runs cargo without the rust gate",
                )

    def test_gated_jobs_declare_the_dependency(self):
        for name in ("ci.yml", "postgres.yml"):
            for job_name, job in load(name)["jobs"].items():
                if not job.get("if"):
                    continue
                self.assertIn(
                    "changes",
                    job.get("needs", []),
                    f"{name}:{job_name} gates on an output it does not need",
                )

    def test_every_branch_triggered_cargo_workflow_is_gated(self):
        for path in sorted(WORKFLOWS.glob("*.yml")):
            workflow = load(path.name)
            if not runs_cargo(workflow):
                continue
            trig = triggers(workflow)
            events = [
                event
                for event in ("push", "pull_request")
                if event in trig and "tags" not in (trig[event] or {})
            ]
            if not events:
                continue
            self.assertIn(
                "changes",
                workflow.get("jobs", {}),
                f"{path.name} runs cargo on {events} without a gate",
            )

    def test_no_site_workflow_builds_rust(self):
        for name in ("cookbook.yml", "pages.yml"):
            self.assertFalse(
                runs_cargo(load(name)),
                f"{name} builds Rust on a documentation-only change",
            )


if __name__ == "__main__":
    unittest.main()
