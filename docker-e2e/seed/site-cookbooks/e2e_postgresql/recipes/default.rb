# Debian/Ubuntu: the resource adds the pgdg archive and installs the newest
# major. Fedora is not a supported platform of the cookbook (its repository
# action raises), so use the distro package there.
if platform_family?('debian')
  postgresql_install 'postgresql' do
    version '18'
    action :install
  end

  postgresql_service 'postgresql' do
    action %i(enable start)
  end
else
  package 'postgresql-server'

  execute 'postgresql-setup --initdb' do
    creates '/var/lib/pgsql/data/PG_VERSION'
  end

  service 'postgresql' do
    action %i(enable start)
  end
end
