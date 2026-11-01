# A container without systemd has no init running, and the fixture images ship
# no init script either, so the packaged one cannot start, stop or probe the
# daemon. Those nodes get start-stop-daemon commands instead; a host with
# systemd keeps its unit and only calls the commands when no unit can be used.
module Microwave
  module DirectService
    def self.systemd?
      ::File.exist?('/run/systemd/system')
    end

    # Fills in the command attributes of a service resource. `background` makes
    # start-stop-daemon daemonize a program that does not do it itself; a
    # program that daemonizes keeps writing its own pidfile through `args`.
    # Guards and the bounded wait for the plain HTTP port of a backend; both
    # live here so the two places that declare the wait cannot drift.
    def self.up_command(port)
      "curl -fsS --max-time 5 http://127.0.0.1:#{port}/ >/dev/null 2>&1"
    end

    def self.wait_command(port)
      "for i in $(seq 150); do #{up_command(port)} && exit 0; sleep 2; done; exit 1"
    end

    def self.commands(resource, pidfile:, exec:, chuid:, chdir: '/', args: '', env: nil, background: false)
      start = +''
      start << "env #{env} " if env
      start << '/usr/sbin/start-stop-daemon --start --quiet '
      start << "--pidfile #{pidfile} "
      start << '--background --make-pidfile ' if background
      start << "--chdir #{chdir} --chuid #{chuid} --exec #{exec} -- #{args}"
      stop = '/usr/sbin/start-stop-daemon --stop --quiet ' \
             "--pidfile #{pidfile} --retry TERM/30/KILL/5"

      resource.start_command start
      resource.stop_command stop
      resource.restart_command "#{stop}; #{start}"
      resource.status_command "/usr/sbin/start-stop-daemon --status --quiet --pidfile #{pidfile}"
    end
  end
end
