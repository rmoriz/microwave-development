# The certificate guards are one shell string each, so ChefSpec can only answer
# for the whole string and never evaluates the `&&` chain: the registry hands
# back the last registered stub that matches. Register one stub per fragment so
# a spec can flip a single condition, and keep the falsy fragments last so they
# actually decide instead of being shadowed.
RSpec.shared_context 'opensearch certificate guards' do
  def stub_certificate_guards(verify: true, pair: true, san: true)
    fragments = {
      verify: [%r{openssl verify -CAfile /etc/microwave/tls/ca\.crt}, verify],
      pair: [/\[ "\$\(openssl x509 -in .* -pubkey -noout\)" = "\$\(openssl pkey -in/, pair],
      san: [/grep -qE "DNS:/, san],
    }

    fragments.sort_by { |_name, (_pattern, value)| value ? 0 : 1 }.each do |_name, (pattern, value)|
      stub_command(pattern).and_return(value)
    end

    # The readiness probe is only skipped while the engine answers, and a fresh
    # node does not.
    stub_command(%r{curl -fsS --max-time 5 http://127\.0\.0\.1:9210/}).and_return(false)
  end
end
