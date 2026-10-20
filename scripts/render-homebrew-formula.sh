#!/usr/bin/env bash
set -Eeuo pipefail

tag=${1:?usage: render-homebrew-formula.sh vVERSION CHECKSUM_MANIFEST}
manifest=${2:?usage: render-homebrew-formula.sh vVERSION CHECKSUM_MANIFEST}
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
    echo "invalid stable release version: $tag" >&2
    exit 1
}
[ -f "$manifest" ] || { echo "checksum manifest not found: $manifest" >&2; exit 1; }

checksum() {
    local hash
    hash=$(awk -v asset="$1" '$2 == asset || $2 == "*" asset { print $1 }' "$manifest")
    [[ "$hash" =~ ^[0-9a-f]{64}$ ]] || {
        echo "invalid or missing checksum for $1" >&2
        exit 1
    }
    printf '%s' "$hash"
}

# Validate every input before emitting any formula text.
arm_mac=$(checksum bcmr-aarch64-macos.tar.gz)
intel_mac=$(checksum bcmr-x86_64-macos.tar.gz)
arm_linux=$(checksum bcmr-aarch64-linux.tar.gz)
intel_linux=$(checksum bcmr-x86_64-linux.tar.gz)

cat <<EOF
class Bcmr < Formula
  desc "Better Copy Move Remove - A modern, safe file operation tool"
  homepage "https://github.com/Bengerthelorf/bcmr"
  version "${tag#v}"
  license "Apache-2.0"

  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/Bengerthelorf/bcmr/releases/download/$tag/bcmr-aarch64-macos.tar.gz"
      sha256 "$arm_mac"
    else
      url "https://github.com/Bengerthelorf/bcmr/releases/download/$tag/bcmr-x86_64-macos.tar.gz"
      sha256 "$intel_mac"
    end
  elsif OS.linux?
    if Hardware::CPU.arm?
      url "https://github.com/Bengerthelorf/bcmr/releases/download/$tag/bcmr-aarch64-linux.tar.gz"
      sha256 "$arm_linux"
    else
      url "https://github.com/Bengerthelorf/bcmr/releases/download/$tag/bcmr-x86_64-linux.tar.gz"
      sha256 "$intel_linux"
    end
  end

  def install
    bin.install "bcmr"
    generate_completions_from_executable(bin/"bcmr", "completions", shells: [:bash, :zsh, :fish])
  end

  def caveats
    <<~EOS
      To enable bcp, bmv and brm in Zsh, add to ~/.zshrc:
        eval "\$(bcmr init zsh --cmd b)"
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/bcmr --version")
    assert_path_exists bash_completion/"bcmr"
    assert_path_exists zsh_completion/"_bcmr"
    assert_path_exists fish_completion/"bcmr.fish"
  end
end
EOF
