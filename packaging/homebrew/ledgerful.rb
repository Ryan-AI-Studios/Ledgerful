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
  version "0.2.14"
  license :cannot_represent

  on_macos do
    on_arm do
      url "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.14/ledgerful-aarch64-apple-darwin.tar.gz"
      sha256 "484b4556d9b962f0970071f62df9c1cc15de72e985ad6b088503394430a073f8"
    end
    on_intel do
      url "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.14/ledgerful-x86_64-apple-darwin.tar.gz"
      sha256 "5ba8d587d35e03697e4db64df1605e1f754970a7978354f728a4fced760689c5"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.14/ledgerful-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "2b1ac064a2db39b17c0dda4a88505930f91a8ab69af00cd8c42225c1bda8f9f1"
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
