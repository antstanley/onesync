# Homebrew formula template for onesync.
#
# This file is the source of truth; the canonical copy lives in a separate
# tap repository (e.g. https://github.com/<owner>/homebrew-onesync), in
# `Formula/onesync.rb`. Per-release the workflow either copies this file
# verbatim or runs `brew bump-formula-pr` to update `version`, `url`, and
# `sha256`.
#
# Users install via:
#
#     brew tap <owner>/onesync
#     brew install onesync
#     brew services start onesync
#
class Onesync < Formula
  desc "Two-way macOS ↔ OneDrive sync daemon"
  homepage "https://github.com/<owner>/onesync"
  license "MIT"

  # Bumped per release.
  version "0.1.0"

  on_macos do
    on_arm do
      url "https://github.com/<owner>/onesync/releases/download/v#{version}/onesync-#{version}-macos-universal.tar.gz"
      sha256 "REPLACE_WITH_RELEASE_TARBALL_SHA256"
    end
    on_intel do
      url "https://github.com/<owner>/onesync/releases/download/v#{version}/onesync-#{version}-macos-universal.tar.gz"
      sha256 "REPLACE_WITH_RELEASE_TARBALL_SHA256"
    end
  end

  def install
    bin.install "onesync"
    bin.install "onesyncd"
  end

  # `brew services start onesync` generates a launchd plist pointing at the
  # opt_bin/"onesyncd" target. Mirrors the LaunchAgent that `onesync service
  # install` would otherwise write to ~/Library/LaunchAgents.
  service do
    run [opt_bin/"onesyncd"]
    keep_alive true
    log_path var/"log/onesync.log"
    error_log_path var/"log/onesync.err.log"
    process_type :background
  end

  test do
    # The CLI exits 0 on `--version` and prints the version string.
    assert_match(/^onesync /, shell_output("#{bin}/onesync --version"))
  end
end
