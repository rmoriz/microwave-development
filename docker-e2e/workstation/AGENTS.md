# Workstation notes

This container is part of the disposable `docker-e2e` stack.

- `knife` is preconfigured for organization `e2e` against
  `http://microwave:8000`; plain `knife node list`, `knife cookbook list`, and
  the other commands work without flags.
- The `microwave-e2e` MCP server exposes node, environment, cookbook, and run
  tools over the same organization.
- Write tools run as the `pivotal` superuser. Destructive calls (`delete_node`,
  `delete_environment`, `delete_cookbook_version`) take effect immediately.
- MCP is experimental. The bearer token is a fixed e2e value and is not a
  production credential.

Start OpenCode in `/work` so it picks up `opencode.json`.
