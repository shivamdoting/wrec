cask "wrec" do
  version "0.1.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000" # replaced by scripts/update-homebrew.sh

  url "https://github.com/shivamhwp/wrec/releases/download/v#{version}/wrec-#{version}.dmg"
  name "Wrec"
  desc "The most efficient screen recorder for mac"
  homepage "https://wrec-beta.vercel.app"

  depends_on macos: ">= :sequoia"
  depends_on arch: :arm64

  app "Wrec.app"

  zap trash: [
    "~/Library/Application Support/Wrec",
    "~/.wrec",
  ]
end
