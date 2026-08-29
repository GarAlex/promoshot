# The whole render environment in one box: the MCP server, the CLI it
# shells to, ffmpeg, a software Vulkan (lavapipe) and the fonts that keep
# caption stand-ins real. What a client gets from `docker run -i` is a
# working promoshot-mcp on stdio with zero host setup — the same
# environment the first real-Linux run was proved in.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p promo-cli -p promoshot-mcp

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
      ffmpeg mesa-vulkan-drivers libvulkan1 fontconfig \
      fonts-liberation fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*
# Side by side on purpose: promoshot-mcp finds `promo` next to itself.
COPY --from=build /src/target/release/promo /usr/local/bin/
COPY --from=build /src/target/release/promoshot-mcp /usr/local/bin/
# A complete project baked in, so the image can prove itself:
#   promo_render_still on examples/LinuxSmoke.promo, no mounts needed.
COPY examples /usr/local/share/promoshot/examples
# Mount your projects here; promo_workspace points at it.
ENV PROMOSHOT_WORKSPACE=/projects
WORKDIR /projects
ENTRYPOINT ["promoshot-mcp"]
