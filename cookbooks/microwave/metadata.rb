name 'microwave'
maintainer 'Roland Moriz'
license 'AGPL-3.0'
description 'Installs Microwave Server with PostgreSQL, OpenSearch and Garage on Debian 13'
version '0.1.0'
source_url 'https://github.com/rmoriz/microwave-development'
issues_url 'https://github.com/rmoriz/microwave-development/issues'
chef_version '>= 19.4.46', '< 20'
supports 'debian', '>= 13.0'

# Cookstyle calls the `recipe` declarations redundant, but they are read by
# `knife cookbook metadata` and by spec/metadata_spec.rb; the README documents
# the same seven recipes for humans. Keep both.
# cookstyle:disable Chef/RedundantCode/RecipeMetadata
recipe 'microwave::default', 'Server and backends on one node'
recipe 'microwave::server', 'Server, configuration and service only'
recipe 'microwave::database', 'PostgreSQL 19, its role and its database'
recipe 'microwave::search', 'OpenSearch 3.9 with a bounded heap'
recipe 'microwave::garage', 'Garage 2.5 object store behind nginx TLS'
recipe 'microwave::disable', 'Stops and disables every service until microwave::enable'
recipe 'microwave::enable', 'Enables and starts every service again'
# cookstyle:enable Chef/RedundantCode/RecipeMetadata
