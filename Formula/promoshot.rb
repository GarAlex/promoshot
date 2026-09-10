class Promoshot < Formula
  desc "PromoShot's promo CLI and promoshot-mcp server: author and render .promo video projects"
  homepage "https://github.com/GarAlex/promoshot"
  version "0.2.77"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/GarAlex/promoshot/releases/download/v0.2.77/promoshot-v0.2.77-macos-arm64.tar.gz"
      sha256 "32d2d87070d5280f88798908f8ba9b798ae51b04132d123bd837bb90fd30c85b"
    end
  end
  on_linux do
    on_intel do
      url "https://github.com/GarAlex/promoshot/releases/download/v0.2.77/promoshot-v0.2.77-linux-x64.tar.gz"
      sha256 "b279372701b6f05b5754563d2ae17fb1f5c03eb4a73db62ce16eb76956d3897e"
    end
  end

  # Rendering video decodes and encodes through ffmpeg; frames are the GPU's.
  depends_on "ffmpeg" => :recommended

  def install
    bin.install "promo", "promoshot-mcp"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/promo --version")
  end
end
