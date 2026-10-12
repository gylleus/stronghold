#!/bin/zsh
# imagegen.sh OUT.png PROMPT.txt [REF.png ...] — one Codex image_gen call; saves the untouched image to OUT.png.
set -e
cd "${0:a:h}"
out=$1; prompt=$2; shift 2
refs=(); for r in "$@"; do refs+=(-i "$r"); done
codex exec --skip-git-repo-check -s workspace-write -C "$PWD" --ephemeral $refs -o "$out.log.txt" \
"Use your built-in image generation tool exactly once to create the image described below.$( [[ $# -gt 0 ]] && echo " The attached image(s) are identity references: keep the character's design, proportions, materials and colours exactly." )
Then copy the generated PNG file unchanged to: $out
Do not edit, crop or post-process it. Reply with just the path.

$(cat "$prompt")" < /dev/null > "$out.codex.txt" 2>&1
ls -la "$out"
