#!/usr/bin/env bash
# Records the corpus again: every scenario, or those named. Each runs a
# real program with `fux-vt-compare record` (see the README, "The corpus"),
# on a PTY of 40x120 with TERM=xterm-256color, and saves NAME.bin and
# NAME.json here.
#
# Everything a program could show comes from a directory of its own,
# $root, with a fixed name so recordings made again are alike: a HOME with
# a minimal rc file for each program, and a work directory holding copies
# of files from this repository and generated text. The environment is
# only what the manifest lists, and TERM. GNU ls puts the host name in its
# file:// URIs; it is scrubbed (`--scrub`).
#
# Before committing a recording, check it holds nothing private:
#   grep -c -i -a -E "$(id -un)|$(hostname -s)|$HOME" corpus/*.bin corpus/*.json
# must print 0 for each file.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
compare=$(dirname "$here")
repo=$(cd "$compare/../.." && pwd)
root=/tmp/fux-corpus
"$compare/run.sh" --cargo build >/dev/null
bin=$compare/target/release/fux-vt-compare

# What each program can find: Homebrew's, the system's, and $root/bin,
# where links to what lives elsewhere go.
path=$root/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin

setup() {
  rm -rf "$root"
  mkdir -p "$root/bin" "$root/home" "$root/work/src" "$root/work/empty" "$root/tmux"
  # The rust-analyzer helix starts, and the rustup toolchains it needs.
  for tool in cargo rustc rustup; do
    if [[ -x $HOME/.cargo/bin/$tool ]]; then ln -s "$HOME/.cargo/bin/$tool" "$root/bin/$tool"; fi
  done
  if [[ -d $HOME/.rustup ]]; then ln -s "$HOME/.rustup" "$root/home/.rustup"; fi
  if command -v claude >/dev/null; then ln -s "$(command -v claude)" "$root/bin/claude"; fi

  local work=$root/work
  cp "$compare/README.md" "$work/README.md"
  cp "$here/fux-corpus.1" "$work/fux-corpus.1"
  cp -R "$repo/fux-vt/src/." "$work/src/"
  printf '[package]\nname = "corpus"\nversion = "0.1.0"\nedition = "2024"\n' > "$work/Cargo.toml"
  printf '#!/bin/sh\necho run\n' > "$work/run.sh"
  chmod +x "$work/run.sh"
  ln -s README.md "$work/latest"
  ln -s missing "$work/broken"

  # A cargo project with errors, for rust-analyzer's diagnostics in helix.
  mkdir -p "$work/hello/src"
  printf '[package]\nname = "hello"\nversion = "0.1.0"\nedition = "2024"\n\n[dependencies]\n' \
    > "$work/hello/Cargo.toml"
  cat > "$work/hello/src/main.rs" <<'EOF'
//! A small program with mistakes in it, for diagnostics.
use std::collections::HashMap;

struct Counter {
    counts: HashMap<String, usize>,
}

impl Counter {
    fn add(&mut self, word: &str) {
        let unused = 1;
        *self.counts.entry(word.to_owned()).or_insert(0) += 1;
    }

    fn most(&self) -> Option<(&String, &usize)> {
        self.counts.iter().max_by_key(|(_, n)| *n)
    }
}

fn main() {
    let mut counter = Counter { counts: HashMap::new() };
    for word in "the quick brown fox jumps over the lazy dog the end".split(' ') {
        counter.add(word);
    }
    let total: usize = counter.counts.values().sum();
    println!("{total} words, most often {:?}", counter.most());
    let missing = undefined_name + 1;
    let wrong: u8 = "text";
}
EOF

  # Line editors: a neutral prompt, history, completion.
  mkdir -p "$root/home/zsh"
  cat > "$root/home/zsh/.zshrc" <<'EOF'
PS1='%~ %# '
RPS1=
HISTFILE=$HOME/.zsh_history
HISTSIZE=100
SAVEHIST=100
bindkey -e
autoload -Uz compinit && compinit -u -d "$HOME/.zcompdump"
EOF
  cat > "$root/home/.bashrc" <<'EOF'
PS1='\w \$ '
HISTFILE=$HOME/.bash_history
EOF
  printf "PS1='\$ '\n" > "$root/home/.shrc"
  cat > "$root/home/.vimrc" <<'EOF'
set nocompatible
syntax on
filetype plugin indent on
set number ruler laststatus=2 showcmd hlsearch incsearch
set ttimeoutlen=50
EOF
}

# record NAME VERSION DIR [record options] -- PROGRAM ARGS...
record() {
  local name=$1 version=$2 dir=$3
  shift 3
  echo "== $name ($version)" >&2
  "$bin" record --size 40x120 --keys "$here/keys/$name.keys" --out "$here/$name" \
    --program "$name" --version "$version" --dir "$dir" \
    --env "PATH=$path" --env "HOME=$root/home" --env LANG=en_US.UTF-8 \
    --env COLORTERM=truecolor "$@"
}

first() { "$@" 2>&1 | head -1; }

scenario() {
  local work=$root/work
  case $1 in
    vim) record vim "$(first vim --version)" "$work" -- vim src/parser.rs ;;
    helix)
      # helix sends the same with TERM_PROGRAM=ghostty (tried): it goes by
      # terminfo and its own queries.
      record helix "$(first hx --version)" "$work/hello" \
        --note "rust-analyzer $(first rust-analyzer --version) gives the diagnostics" \
        -- hx src/main.rs ;;
    less) record less "$(first less --version)" "$work" -- less README.md ;;
    fzf)
      record fzf "fzf $(first fzf --version)" "$work" --env 'FZF_DEFAULT_COMMAND=find . -type f' -- fzf ;;
    fzf-height)
      record fzf-height "fzf $(first fzf --version)" "$work" \
        --env 'FZF_DEFAULT_COMMAND=find . -type f' -- fzf --height=40% --layout=reverse --border ;;
    gls)
      record gls "$(first gls --version)" "$work" --scrub "$(hostname)=localhost" \
        -- gls --color=always --hyperlink=always -F . src ;;
    man) record man "man (macOS), mandoc" "$work" -- man ./fux-corpus.1 ;;
    delta-log)
      record delta-log "$(first delta --version), $(first git --version)" "$repo" \
        --env GIT_CONFIG_NOSYSTEM=1 \
        -- git -c core.pager='delta --paging=always' -c color.ui=always log -p -n 3 \
        --format='commit %h%nDate:   %ad%n%n    %s%n' --date=iso a0279cc ;;
    delta-diff)
      record delta-diff "$(first delta --version), $(first git --version)" "$repo" \
        --env GIT_CONFIG_NOSYSTEM=1 \
        -- git -c core.pager='delta --paging=always --side-by-side' diff a0279cc~1 a0279cc ;;
    zsh) record zsh "$(first /bin/zsh --version)" "$work" --env "ZDOTDIR=$root/home/zsh" -- /bin/zsh -i ;;
    bash)
      record bash "$(first bash --version)" "$work" -- bash --noprofile --rcfile "$root/home/.bashrc" -i ;;
    tmux)
      record tmux "$(first tmux -V)" "$work" --env SHELL=/bin/sh --env "ENV=$root/home/.shrc" \
        --env "TMUX_TMPDIR=$root/tmux" \
        -- tmux -L fux-corpus -f /dev/null new-session -s corpus ';' set-option -g status-right corpus
      TMUX_TMPDIR=$root/tmux tmux -L fux-corpus kill-server 2>/dev/null || true ;;
    claude)
      # A first start: no settings, so the theme is asked for.
      mkdir -p "$work/project"
      record claude "Claude Code $(first claude --version)" "$work/project" \
        --note "a first start: HOME has no Claude Code settings" \
        --env DISABLE_AUTOUPDATER=1 -- claude ;;
    claude-main | claude-ghostty)
      # A later start: onboarding done, the directory trusted, so the main
      # screen and its prompt show. fux passes its own environment on to
      # its panes, TERM_PROGRAM included: claude-ghostty is fux run from
      # Ghostty.
      mkdir -p "$work/project"
      printf '{"hasCompletedOnboarding":true,"theme":"dark","projects":{"%s":{"hasTrustDialogAccepted":true}}}\n' \
        "$(cd "$work/project" && pwd -P)" > "$root/home/.claude.json"
      local outer=()
      if [[ $1 == claude-ghostty ]]; then outer=(--env TERM_PROGRAM=ghostty); fi
      record "$1" "Claude Code $(first claude --version)" "$work/project" \
        --note "onboarding done and the directory trusted, in HOME's .claude.json" \
        --env DISABLE_AUTOUPDATER=1 "${outer[@]}" -- claude ;;
    *) echo "record.sh: no scenario $1" >&2; return 2 ;;
  esac
}

all=(vim helix less fzf fzf-height gls man delta-log delta-diff zsh bash tmux claude claude-main
  claude-ghostty)
setup
for name in "${@:-${all[@]}}"; do
  scenario "$name"
done
