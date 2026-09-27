# The whole render environment in one box: the MCP server, the CLI it
# shells to, ffmpeg, a software Vulkan (lavapipe) and the fonts that keep
# caption stand-ins real. What a client gets from `docker run -i` is a
# working promoshot-mcp on stdio with zero host setup — the same
# environment the first real-Linux run was proved in.

# Pinned with the CI gate's toolchain, so the image is built by the compiler
# the tests ran on.
FROM rust:1.96-bookworm AS build
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
# Every recipe's example baked in, so the image can prove itself with no
# mounts — start with the one that teaches the product:
#   promo_render_still on /usr/local/share/promoshot/examples/ProductCard.promo
COPY examples /usr/local/share/promoshot/examples
# Mount your projects here; promo_workspace points at it.
LABEL io.modelcontextprotocol.server.name="io.github.GarAlex/promoshot"
ENV PROMOSHOT_WORKSPACE=/projects
# Not root: a decoder bug in a project someone else made should not run as
# root, nor write root-owned files into the host's mounted folder. UID 1000
# is the first user on most Linux hosts; elsewhere pass `--user $(id -u)`.
RUN useradd --uid 1000 --create-home --shell /usr/sbin/nologin promoshot \
    && mkdir -p /projects && chown promoshot:promoshot /projects
USER promoshot
WORKDIR /projects
ENTRYPOINT ["promoshot-mcp"]
