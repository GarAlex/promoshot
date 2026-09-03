#!/bin/zsh
# Publishes docs/demo-media/ (the 1280-wide videos publish.py makes) to
# the orphan `demo-media` branch — a branch with no history to keep, so
# it is rewritten whole each time and the repository's main stays light.
set -e
HERE="${0:A:h}"; CORE="${HERE:h}"
SRC="$CORE/docs/demo-media"
[ -d "$SRC" ] || { echo "nothing in docs/demo-media"; exit 0 }
WT="$(mktemp -d)/demo-media"
# A fresh orphan every time: drop any local branch of that name first, so
# the checkout below cannot fall through onto main's tree.
git -C "$CORE" branch -D demo-media > /dev/null 2>&1 || true
git -C "$CORE" worktree add --detach "$WT" > /dev/null 2>&1
( cd "$WT" && git checkout -q --orphan demo-media && git rm -rfq . > /dev/null 2>&1 || true
  cp "$SRC"/*.mp4 . && printf '# demo-media\n\nThe demo videos, 1280 wide. Regenerated whole by demos/publish_media.sh; see demo.md on main.\n' > README.md
  git add -A && git commit -q -m "demo videos $(date '+%Y-%m-%d %H:%M')" \
  && git push -q -f github HEAD:demo-media \
  && ( GIT_SSH_COMMAND="ssh -o ConnectTimeout=15 -o BatchMode=yes" git push -q -f origin HEAD:demo-media \
       || echo "origin (the NAS) is unreachable; demo-media is on GitHub, not mirrored" ) )
git -C "$CORE" worktree remove --force "$WT"
echo "demo-media: $(ls "$SRC"/*.mp4 | wc -l | tr -d ' ') videos pushed"
