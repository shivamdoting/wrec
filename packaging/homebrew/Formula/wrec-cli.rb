class WrecCli < Formula
  desc "The most efficient screen recorder for mac, CLI runtime"
  homepage "https://wrec-beta.vercel.app"
  version "0.1.0"
  url "https://github.com/shivamhwp/wrec/releases/download/v#{version}/wrec-cli-aarch64-apple-darwin.tar.gz"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000" # replaced by scripts/update-homebrew.sh
  license "MIT"

  depends_on :macos
  depends_on arch: :arm64

  def install
    libexec.install "wrec", "daemon", "capture-engine"
    bin.write_exec_script libexec/"wrec"
  end

  test do
    system bin/"wrec", "-V"
  end
end
