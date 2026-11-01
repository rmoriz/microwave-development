# A disabled service is marked by a file alone: the unit stays installed, may
# keep running state and data, and only the marker stops `:install` from
# enabling and starting it again.
module Microwave
  module Disabled
    DIR = '/etc/microwave/disabled'.freeze
    UNIT_DIRS = ['/etc/systemd/system', '/lib/systemd/system', '/usr/lib/systemd/system'].freeze

    def self.marker(unit)
      ::File.join(DIR, unit)
    end

    def self.marked?(unit)
      ::File.exist?(marker(unit))
    end

    def self.unit?(unit)
      UNIT_DIRS.any? { |dir| ::File.exist?(::File.join(dir, "#{unit}.service")) }
    end
  end
end
