name "e2e_nginx"
maintainer "microwave-server e2e"
maintainer_email "noreply@example.com"
license "Apache-2.0"
description "Driver recipe for the resource-only nginx cookbook"
version "0.1.0"
chef_version ">= 18"

# The role's nginx recipe. nginx 12.x ships resources only (nginx_install,
# nginx_config, nginx_site, nginx_service) and has no recipes of its own, so
# this wrapper is what the role points at.
depends "nginx"
