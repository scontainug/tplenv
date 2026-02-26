#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
  cat <<USAGE
Install tplenv shell completion.

Usage:
  $0 [--shell bash|zsh]

If --shell is omitted, the script tries to detect your current shell.
USAGE
}

shell_name=""
if [[ ${1:-} == "--help" || ${1:-} == "-h" ]]; then
  usage
  exit 0
fi
if [[ ${1:-} == "--shell" ]]; then
  shell_name="${2:-}"
  if [[ -z "$shell_name" ]]; then
    echo "Missing shell name after --shell" >&2
    exit 1
  fi
fi

if [[ -z "$shell_name" ]]; then
  shell_name="$(basename "${SHELL:-}")"
fi

case "$shell_name" in
  bash)
    target_dir="${XDG_DATA_HOME:-$HOME/.local/share}/bash-completion/completions"
    mkdir -p "$target_dir"
    cp "$ROOT_DIR/completions/tplenv.bash" "$target_dir/tplenv"
    echo "Installed bash completion: $target_dir/tplenv"
    echo "Open a new shell, or run: source $target_dir/tplenv"
    ;;
  zsh)
    target_dir="$HOME/.zsh/completions"
    mkdir -p "$target_dir"
    cp "$ROOT_DIR/completions/_tplenv" "$target_dir/_tplenv"

    zshrc="$HOME/.zshrc"
    line='fpath=(~/.zsh/completions $fpath)'
    init='autoload -Uz compinit && compinit'
    if [[ -f "$zshrc" ]]; then
      grep -Fqx "$line" "$zshrc" || echo "$line" >> "$zshrc"
      grep -Fqx "$init" "$zshrc" || echo "$init" >> "$zshrc"
    else
      printf '%s\n%s\n' "$line" "$init" > "$zshrc"
    fi

    echo "Installed zsh completion: $target_dir/_tplenv"
    echo "Open a new shell, or run: fpath=(~/.zsh/completions \$fpath); autoload -Uz compinit && compinit"
    ;;
  *)
    echo "Unsupported shell: $shell_name" >&2
    echo "Use --shell bash or --shell zsh" >&2
    exit 1
    ;;
esac
