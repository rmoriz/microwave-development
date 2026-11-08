# Debian/Ubuntu: the resource adds the pgdg archive and installs the newest
# major. Fedora and Amazon Linux are not supported platforms of the cookbook
# (its repository action raises), so use the distro package there; Amazon
# Linux 2023 only ships versioned package names.
if platform_family?('debian')
  postgresql_install 'postgresql' do
    version '18'
    action :install
  end

  postgresql_service 'postgresql' do
    action %i(enable start)
  end
else
  package platform_family?('amazon') ? 'postgresql16-server' : 'postgresql-server'

  execute 'postgresql-setup --initdb' do
    creates '/var/lib/pgsql/data/PG_VERSION'
  end

  service 'postgresql' do
    action %i(enable start)
  end
end
