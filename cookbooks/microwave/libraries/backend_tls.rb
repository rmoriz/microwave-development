# Path naming for the backend TLS material the server, database and search
# recipes share. Issuance itself happens in the recipes.
module Microwave
  module TLS
    DIR = '/etc/microwave/tls'.freeze

    def self.ca_dir
      DIR
    end

    def self.ca_cert
      File.join(DIR, 'ca.crt')
    end

    def self.ca_key
      File.join(DIR, 'ca.key')
    end

    def self.cert(hostname)
      File.join(DIR, "#{hostname}.crt")
    end

    def self.key(hostname)
      File.join(DIR, "#{hostname}.key")
    end
  end
end
