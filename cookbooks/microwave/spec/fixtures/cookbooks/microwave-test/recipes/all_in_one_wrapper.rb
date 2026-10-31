# A wrapper with its own split layout: it decides where the backends live and
# only then pulls in the all-in-one default, which must not stomp the hosts.
node.default['microwave']['database_host'] = 'db.wrapper.example.com'
node.default['microwave']['search_host'] = 'search.wrapper.example.com'

include_recipe 'microwave::default'
