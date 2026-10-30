# Workstation notes

This container is part of the disposable `docker-e2e` stack.

- `knife` is preconfigured for organization `e2e` against
  `http://microwave:8000`; plain `knife node list`, `knife cookbook list`, and
  the other commands work without flags. The config and the superuser key live
  in `/root/work/.chef` (`KNIFE_HOME`).
- The workspace is the pinned `chef_repo_path`, so a bare `knife list` returns
  the server root only when the shell is in `/root/work`; elsewhere use
  `knife node list`, `knife cookbook list`, or an absolute `knife list /nodes`.
- The `microwave-e2e` MCP server exposes node, environment, cookbook, and run
  tools over the same organization.
- Write tools run as the `pivotal` superuser. Destructive calls (`delete_node`,
  `delete_environment`, `delete_cookbook_version`) take effect immediately.
- Everything runs as root. The image has no unprivileged `dev` user; keep the
  box to the disposable e2e stack for that reason.
- MCP is experimental. The bearer token is a fixed e2e value and is not a
  production credential.

Start OpenCode in `/root/work` so it picks up `opencode.json`.
