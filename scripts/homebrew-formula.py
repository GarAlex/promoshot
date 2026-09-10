#!/usr/bin/env python3
"""Write the Homebrew formula for a release from the digests GitHub
publishes for its assets — nothing is downloaded to hash.

    python3 scripts/homebrew-formula.py v0.2.77 > Formula/promoshot.rb

`GH_TOKEN` (or `GITHUB_TOKEN`) in the environment raises the API's rate
limit; the release must already carry both tarballs. The release
workflow runs this after the binaries job and commits the result to
main, and into the tap when it has a token for it."""
import json, os, sys, urllib.request

REPO = "GarAlex/promoshot"
ASSETS = {"macos-arm64": ("on_macos", "on_arm"), "linux-x64": ("on_linux", "on_intel")}

def release(tag):
    req = urllib.request.Request(f"https://api.github.com/repos/{REPO}/releases/tags/{tag}",
                                 headers={"Accept": "application/vnd.github+json", "User-Agent": "promoshot-formula"})
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(req) as r:
        return json.load(r)

def formula(tag):
    version = tag.lstrip("v")
    digests = {}
    for asset in release(tag).get("assets", []):
        for target in ASSETS:
            if asset["name"] == f"promoshot-{tag}-{target}.tar.gz":
                digest = (asset.get("digest") or "")
                assert digest.startswith("sha256:"), f"{asset['name']}: no sha256 digest published"
                digests[target] = (asset["browser_download_url"], digest[len("sha256:"):])
    missing = [t for t in ASSETS if t not in digests]
    assert not missing, f"{tag}: assets not on the release yet: {missing}"
    blocks = []
    for target, (outer, inner) in ASSETS.items():
        url, sha = digests[target]
        blocks.append(f"""  {outer} do
    {inner} do
      url "{url}"
      sha256 "{sha}"
    end
  end""")
    body = "\n".join(blocks)
    return f'''class Promoshot < Formula
  desc "PromoShot's promo CLI and promoshot-mcp server: author and render .promo video projects"
  homepage "https://github.com/{REPO}"
  version "{version}"
  license "Apache-2.0"

{body}

  # Rendering video decodes and encodes through ffmpeg; frames are the GPU's.
  depends_on "ffmpeg" => :recommended

  def install
    bin.install "promo", "promoshot-mcp"
  end

  test do
    assert_match version.to_s, shell_output("#{{bin}}/promo --version")
  end
end
'''

if __name__ == "__main__":
    if len(sys.argv) != 2 or not sys.argv[1].startswith("v"):
        sys.exit("usage: homebrew-formula.py v<version>")
    sys.stdout.write(formula(sys.argv[1]))
