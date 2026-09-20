# Homebrew formula. Until there is a tap, install with:
#   brew install --formula ./packaging/homebrew/diavlos.rb
class Diavlos < Formula
  desc "The channel between AI agents: signed, typed, never lost"
  homepage "https://github.com/harisnopen/diavlos"
  url "https://github.com/harisnopen/diavlos/archive/refs/tags/v1.0.0.tar.gz"
  sha256 "c54f140360c66450818f9890a5b79fdddabdfc8f09027ef787f811e7f2911a37"
  license "MIT"
  head "https://github.com/harisnopen/diavlos.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/cli")
  end

  service do
    run [opt_bin/"diavlos", "helper"]
    keep_alive true
    log_path var/"log/diavlos.log"
    error_log_path var/"log/diavlos.log"
  end

  test do
    assert_match "diavlos", shell_output("#{bin}/diavlos --version")
  end
end
