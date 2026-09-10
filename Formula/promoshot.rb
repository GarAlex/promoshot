class Promoshot < Formula
  desc "PromoShot's promo CLI and promoshot-mcp server: author and render .promo video projects"
  homepage "https://github.com/GarAlex/promoshot"
  version "0.2.75"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/GarAlex/promoshot/releases/download/v0.2.75/promoshot-v0.2.75-macos-arm64.tar.gz"
      sha256 "d469ef78d75049d2329cf0d8b666a0f7f33e3cad63f202a51fff4ec2a8078a2b"
    end
  end
  on_linux do
    on_intel do
      url "https://github.com/GarAlex/promoshot/releases/download/v0.2.75/promoshot-v0.2.75-linux-x64.tar.gz"
      sha256 "64a52954e10fb10a549a52136efddd58c99e7a4086fc02dfe3cf1b9386244e0e"
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
