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
  version "0.2.12"
  license :cannot_represent

  on_macos do
    on_arm do
      url "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.12/ledgerful-aarch64-apple-darwin.tar.gz"
      sha256 "a57856bad2e400f3948f2e818b214928dfd402c5b42fc943d665a0ded88b1371"
    end
    on_intel do
      url "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.12/ledgerful-x86_64-apple-darwin.tar.gz"
      sha256 "8e2ab31a05db2f6b8fd87df19d10a89b920a56582782d3016c4f01d247ad9f0a"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.12/ledgerful-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "843d91a399570e2d7e4335e573c6a2e019dfbf727a5f01385f4613e48d93a9d4"
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
