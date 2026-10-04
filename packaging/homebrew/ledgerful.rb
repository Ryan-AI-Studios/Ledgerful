# Homebrew formula template for Ledgerful (CLI formula, not cask).
# Maintained in-engine; release CI bumps version + sha256 from published
# checksum files via scripts/bump-manifests.{ps1,sh}.
#
# Tap: Ryan-AI-Studios/homebrew-tap
# Install (after tap is seeded): brew install Ryan-AI-Studios/tap/ledgerful
#
# macOS interim: release artifacts are not Apple-codesigned/notarized.
# Homebrew formula installs usually avoid browser quarantine, but if
# Gatekeeper blocks first run on a downloaded binary:
#   xattr -d com.apple.quarantine "$(which ledgerful)"
# Proper fix is codesign+notarize in the release pipeline (upstream of 0051).

class Ledgerful < Formula
  desc "Local-first change intelligence CLI for impact analysis and verification"
  homepage "https://github.com/Ryan-AI-Studios/Ledgerful"
  version "0.2.16"
  license :cannot_represent

  on_macos do
    on_arm do
      url "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.16/ledgerful-aarch64-apple-darwin.tar.gz"
      sha256 "6db77b28495e138dc8df6f05c273af48c81b1d6d832c1d96940b59eda8e125c9"
    end
    on_intel do
      url "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.16/ledgerful-x86_64-apple-darwin.tar.gz"
      sha256 "13f744f19e0c94aac2de49e276c4532fbbf4091f0be2d585080849561a9e54b6"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.16/ledgerful-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "c0f639dfc4c20c0f3d4f7d419154bc0f4ec1a85f20f963d692f54c0a338ba6d6"
    end
  end

  def install
    # Archive tar nests ledgerful-{target}/…; Homebrew stages that directory as
    # buildpath, so the binary is usually a direct child. Nested glob is fallback
    # if staging ever leaves an extra level.
    binary = Pathname.glob(buildpath/"ledgerful").first ||
             Pathname.glob(buildpath/"ledgerful-*/ledgerful").first
    if binary.nil?
      children = begin
        Dir.children(buildpath).sort.join(", ")
      rescue StandardError
        "(unreadable)"
      end
      odie "ledgerful binary not found in archive (buildpath children: #{children})"
    end

    bin.install binary => "ledgerful"
  end

  def caveats
    <<~EOS
      macOS release binaries are not currently Apple-notarized.
      If Gatekeeper reports "developer cannot be verified" on first run:
        xattr -d com.apple.quarantine "$(which ledgerful)"
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/ledgerful --version")
  end
end
