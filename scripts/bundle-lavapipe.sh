#!/usr/bin/env bash
# Collects Mesa's lavapipe (the CPU Vulkan driver) into a folder the Linux
# release carries beside `promo`, so a machine whose own driver went away —
# a redeploy that dropped mesa-vulkan-drivers — still renders. `promo`
# finds `lavapipe/lvp_icd.json` next to itself when the system's drivers
# give no adapter (or only SwiftShader); see promo-gpu/src/adapter.rs.
#
# Run on the distribution the release is built on (ubuntu-24.04: glibc
# 2.39, the release's floor), with the packages installed:
#   sudo apt-get install -y mesa-vulkan-drivers libvulkan1 patchelf
#   scripts/bundle-lavapipe.sh dist/lavapipe
#
# What goes in: libvulkan_lvp.so, every library it needs that a glibc
# system cannot be counted on to have (LLVM, libdrm, the xcb/wayland
# client libraries it links for presenting, ICU, libxml2, …), the Vulkan
# loader for a machine with none, a manifest pointing at the driver by a
# relative path, and each package's copyright file with a NOTICES.txt.
# glibc itself, libstdc++ and libgcc_s stay the system's: any glibc 2.39
# machine has them, and two copies of libstdc++ in one process is the
# classic way to crash.
set -euo pipefail

out=${1:?usage: bundle-lavapipe.sh <out-dir>}
arch=$(uname -m)
triple="${arch}-linux-gnu"
lvp=$(ls /usr/lib/"$triple"/libvulkan_lvp.so 2>/dev/null || true)
if [ -z "$lvp" ]; then
  echo "no libvulkan_lvp.so: apt-get install mesa-vulkan-drivers" >&2
  exit 1
fi
loader=/usr/lib/"$triple"/libvulkan.so.1
[ -e "$loader" ] || { echo "no $loader: apt-get install libvulkan1" >&2; exit 1; }
command -v patchelf >/dev/null || { echo "apt-get install patchelf" >&2; exit 1; }

rm -rf "$out"
mkdir -p "$out/licenses"

# Libraries every glibc machine has, by soname prefix.
system_lib() {
  case "$1" in
    linux-vdso.so*|ld-linux*|libc.so*|libm.so*|libdl.so*|libpthread.so*|librt.so*|\
    libresolv.so*|libutil.so*|libstdc++.so*|libgcc_s.so*) return 0 ;;
  esac
  return 1
}

declare -a files=("$lvp" "$loader")
while read -r name _ path _; do
  [ -n "${path:-}" ] || continue
  system_lib "$name" && continue
  files+=("$path")
done < <(ldd "$lvp" | awk '/=>/ {print $1, $2, $3, $4}')

for f in "${files[@]}"; do
  base=$(basename "$f")
  cp -L "$f" "$out/$base"
  chmod 644 "$out/$base"
  # Each finds its neighbours in the bundle first, whoever loads it.
  patchelf --set-rpath '$ORIGIN' "$out/$base"
done

api=$(sed -n 's/.*"api_version" *: *"\([^"]*\)".*/\1/p' /usr/share/vulkan/icd.d/lvp_icd*.json | head -1)
cat > "$out/lvp_icd.json" <<JSON
{
    "file_format_version": "1.0.1",
    "ICD": {
        "library_path": "./libvulkan_lvp.so",
        "api_version": "${api:-1.3.0}"
    }
}
JSON

# Licences: each file's Debian package, its version and its copyright file.
{
  echo "Third-party libraries in this folder (bundled by scripts/bundle-lavapipe.sh"
  echo "from $(. /etc/os-release && echo "$PRETTY_NAME") packages; sources: apt-get source <package>)."
  echo
  printf '%-34s %-28s %s\n' FILE PACKAGE VERSION
} > "$out/NOTICES.txt"
for f in "${files[@]}"; do
  real=$(readlink -f "$f")
  pkg=$(dpkg -S "$real" 2>/dev/null || dpkg -S "${real#/usr}" 2>/dev/null || dpkg -S "$f" 2>/dev/null || true)
  pkg=${pkg%%:*}
  pkg=${pkg%%,*}
  version=$(dpkg-query -W -f='${Version}' "$pkg" 2>/dev/null || echo "?")
  printf '%-34s %-28s %s\n' "$(basename "$f")" "${pkg:-?}" "$version" >> "$out/NOTICES.txt"
  if [ -n "$pkg" ] && [ -f "/usr/share/doc/$pkg/copyright" ]; then
    cp "/usr/share/doc/$pkg/copyright" "$out/licenses/$pkg.copyright"
  fi
done
cat >> "$out/NOTICES.txt" <<'TXT'

Licences in brief (the full texts are in licenses/<package>.copyright):
  Mesa (libvulkan_lvp.so): MIT. libdrm: MIT. Vulkan loader: Apache-2.0.
  LLVM: Apache-2.0 WITH LLVM-exception. ICU: Unicode (ICU) licence.
  libxml2, libexpat, libffi, libxcb*, libX11-xcb, libXau, libXdmcp,
  libxshmfence, libwayland-client, ncurses (libtinfo): MIT/X11-style.
  libedit, libbsd, libmd: BSD-style. zstd: BSD-3-Clause (dual-licensed
  with GPL-2.0; used here under BSD). zlib: Zlib. liblzma: public domain.
Some of these source packages also hold GPL-licensed build scripts, test
suites or tools; none of those are in this folder. Corresponding sources:
`apt-get source <package>` on Ubuntu, at the versions listed above.
Set PROMO_LAVAPIPE=off to never use this folder.
TXT

du -sh "$out"
