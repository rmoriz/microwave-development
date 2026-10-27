#
# Install nginx with the resource-only nginx cookbook and run it as a service.
#
# The node containers boot systemd as PID 1, so the service resource works on
# Debian, Ubuntu, and Fedora alike.
#
nginx_install "nginx" do
  source "distro"
end

nginx_service "nginx" do
  action %i(enable start)
end
