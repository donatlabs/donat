#!/usr/bin/env bash
# Install the donat skills and prompts for OpenAI Codex CLI.
#
# Codex has no plugin format. What it does have is AGENTS.md, which it reads
# automatically, and ~/.codex/prompts/*.md, which become slash commands. This
# script installs the skills to a known path and the prompts beside them.
#
#   ./install.sh              install
#   ./install.sh --uninstall  remove what this script installed
set -euo pipefail

here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
plugin_root="$(dirname -- "$here")"
codex_home="${CODEX_HOME:-$HOME/.codex}"
skills_dest="$codex_home/donat/skills"
prompts_dest="$codex_home/prompts"

prompts=(
  donat-new-app.md
  donat-add-table.md
  donat-add-command.md
  donat-add-process.md
  donat-review.md
  donat-set-goal.md
)

if [[ "${1:-}" == "--uninstall" ]]; then
  rm -rf -- "$codex_home/donat"
  for p in "${prompts[@]}"; do
    rm -f -- "$prompts_dest/$p"
  done
  echo "Removed $codex_home/donat and ${#prompts[@]} prompts from $prompts_dest."
  exit 0
fi

mkdir -p -- "$skills_dest" "$prompts_dest"

rm -rf -- "$skills_dest"
mkdir -p -- "$skills_dest"
cp -R -- "$plugin_root/skills/." "$skills_dest/"

for p in "${prompts[@]}"; do
  cp -- "$here/prompts/$p" "$prompts_dest/$p"
done

cat <<EOF
Installed:
  skills   -> $skills_dest
  prompts  -> $prompts_dest  (${#prompts[@]} files)

Available in Codex as:
  /donat-new-app  /donat-add-table  /donat-add-command  /donat-add-process
  /donat-review   /donat-set-goal

One manual step remains. Append the project rules to the AGENTS.md of the
repository that builds on donat, so Codex loads them without being asked:

  cat "$here/AGENTS.donat.md" >> /path/to/your-app/AGENTS.md

Re-run this script after updating the plugin. Uninstall with --uninstall.
EOF
