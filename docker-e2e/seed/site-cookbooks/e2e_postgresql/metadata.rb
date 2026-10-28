name 'e2e_postgresql'
maintainer 'Microwave E2E'
license 'Apache-2.0'
description 'Installs the newest PostgreSQL through the resource-only postgresql cookbook'
version '0.1.0'
chef_version '>= 18.0'

# Unversioned on purpose: the seed pins the version in the Berksfile, and an
# exact constraint here would break an override of POSTGRES_VERSION.
depends 'postgresql'
