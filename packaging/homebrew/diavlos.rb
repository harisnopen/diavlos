# Build-from-source formula. The tap does not use this one: the release
# workflow generates a formula that installs the signed binaries, and
# attaches it to each release as `diavlos.rb`. See docs/PUBLISHING.md.
#
# This is here for building a tag from source, with no release:
#   brew install --formula ./packaging/homebrew/diavlos.rb
#
# The sha256 below belongs to one exact tag archive. Recompute it for any
# other tag:
#   curl -fsSL https://github.com/harisnopen/diavlos/archive/refs/tags/vX.Y.Z.tar.gz | shasum -a 256
class Diavlos < Formula
  desc "The channel between AI agents: signed, typed, never lost"
  homepage "https://diavlos.sh"
  url "https://github.com/harisnopen/diavlos/archive/refs/tags/v1.0.0.tar.gz"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
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
