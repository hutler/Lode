# Homebrew formula of the Lode client, for a tap such as hutler/homebrew-lode:
#   brew install hutler/lode/lode
# `packaging/render-manifests.sh` fills in the placeholders for a release.
class Lode < Formula
  desc "Command-line client of the Lode version control system"
  homepage "https://github.com/hutler/Lode"
  version "{{VERSION}}"
  license "Apache-2.0"

  base = "https://github.com/hutler/Lode/releases/download/lode-v{{VERSION}}"
  on_macos do
    on_arm do
      url "#{base}/lode-{{VERSION}}-aarch64-apple-darwin.tar.gz"
      sha256 "{{SHA256_lode-{{VERSION}}-aarch64-apple-darwin.tar.gz}}"
    end
    on_intel do
      url "#{base}/lode-{{VERSION}}-x86_64-apple-darwin.tar.gz"
      sha256 "{{SHA256_lode-{{VERSION}}-x86_64-apple-darwin.tar.gz}}"
    end
  end
  on_linux do
    on_arm do
      url "#{base}/lode-{{VERSION}}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "{{SHA256_lode-{{VERSION}}-aarch64-unknown-linux-musl.tar.gz}}"
    end
    on_intel do
      url "#{base}/lode-{{VERSION}}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "{{SHA256_lode-{{VERSION}}-x86_64-unknown-linux-musl.tar.gz}}"
    end
  end

  def install
    bin.install "lode"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/lode --version")
  end
end
