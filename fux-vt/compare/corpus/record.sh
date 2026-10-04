#!/usr/bin/env bash
# Records the corpus again: every scenario, or those named. Each runs a
# real program with `fux-vt-compare record` (see the README, "The corpus"),
# on a PTY of 40x120 (or the size the scenario gives) with
# TERM=xterm-256color, and saves NAME.bin and NAME.json here.
#
# Everything a program could show comes from a directory of its own,
# $root, with a fixed name so recordings made again are alike: a HOME with
# a minimal rc file for each program, and a work directory holding copies
# of files from this repository and generated text (a git repository with
# a made-up author, a tree of files, cargo projects). The environment is
# only what the manifest lists, and TERM; times are shown in UTC. GNU ls and fish put the host name
# in their file:// URIs, and ranger shows the owner of each file; they are
# scrubbed (`--scrub`).
#
# Every recording is checked for what is private: the login and host
# names, $HOME, and git's user name and email and the account in this
# repository's remote. A recording that holds any of them is dropped (its
# files are moved to $root/dropped, to see why) and the run fails. By hand, the same check is
#   grep -c -i -a -F -e "$(id -un)" -e "$(hostname -s)" -e "$HOME" corpus/*.bin corpus/*.json
# which must print 0 for each file.
#
# At the end the inventory is made again (corpus/INVENTORY.md), and what
# changed in it is printed, for a person to review.
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

# What no recording may hold, and what scrubs it: the owner of the files a
# file manager shows becomes as many x's, so its columns stay where they
# were.
login=$(id -un)
owner_mask=$(printf '%*s' "${#login}" '' | tr ' ' x)
private=("$login" "$(hostname -s)" "$(hostname)" "$HOME")
for value in "$(git -C "$repo" config user.name || true)" "$(git -C "$repo" config user.email || true)"; do
  if ((${#value} >= 4)); then private+=("$value"); fi
done
account=$(git -C "$repo" remote get-url origin 2>/dev/null | sed -E 's#.*[:/]([^/]+)/[^/]+$#\1#' || true)
if ((${#account} >= 4)); then private+=("$account" "${account//-/_}" "${account//_/-}"); fi

# A clean git, as the recordings' programs see it: HOME's .gitconfig only.
clean() {
  env -i "PATH=$path" "HOME=$root/home" GIT_CONFIG_NOSYSTEM=1 LANG=en_US.UTF-8 "$@"
}

# Commits in the generated repository: made-up author, fixed dates.
commit() {
  local date=$1
  shift
  clean GIT_AUTHOR_DATE="$date" GIT_COMMITTER_DATE="$date" git -C "$root/work/repo" "$@"
}

setup() {
  rm -rf "$root"
  mkdir -p "$root/bin" "$root/home" "$root/work/src" "$root/work/empty" "$root/tmux" "$root/zellij"
  # The rust-analyzer helix starts, and the rustup toolchains it needs.
  for tool in cargo rustc rustdoc rustup; do
    if [[ -x $HOME/.cargo/bin/$tool ]]; then ln -s "$HOME/.cargo/bin/$tool" "$root/bin/$tool"; fi
  done
  if [[ -d $HOME/.rustup ]]; then ln -s "$HOME/.rustup" "$root/home/.rustup"; fi
  if command -v claude >/dev/null; then ln -s "$(command -v claude)" "$root/bin/claude"; fi

  local work=$root/work
  cp "$compare/README.md" "$work/README.md"
  cp "$here/fux-corpus.1" "$here/fux-corpus-tables.1" "$here/fux-corpus-long.7" "$work/"
  cp -R "$repo/fux-vt/src/." "$work/src/"
  printf '[package]\nname = "corpus"\nversion = "0.1.0"\nedition = "2024"\n' > "$work/Cargo.toml"
  printf '#!/bin/sh\necho run\n' > "$work/run.sh"
  chmod +x "$work/run.sh"
  ln -s README.md "$work/latest"
  ln -s missing "$work/broken"
  # Fixed times, so listings show the same dates each time.
  find "$work" -exec touch -h -t 202601020304 {} +

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

  # A cargo workspace of crates that build, with tests (one fails), for
  # cargo's progress and test output: fux-vt itself, and small crates on
  # it. No crate needs the network.
  local ws=$work/tiny
  mkdir -p "$ws/vt" "$ws/app/src"
  for crate in words lines counts; do
    mkdir -p "$ws/$crate/src"
    printf '[package]\nname = "%s"\nversion = "0.1.0"\nedition = "2024"\n' "$crate" > "$ws/$crate/Cargo.toml"
    cat > "$ws/$crate/src/lib.rs" <<EOF
//! The $crate of a text.
pub fn count(text: &str) -> usize {
    text.split_whitespace().filter(|w| !w.is_empty()).count()
}

#[cfg(test)]
mod tests {
    #[test]
    fn counts_the_$crate() {
        assert_eq!(super::count("a b c"), 3);
    }

    #[test]
    fn counts_nothing_in_nothing() {
        assert_eq!(super::count(""), 0);
    }
}
EOF
  done
  cp -R "$repo/fux-vt/src" "$ws/vt/src"
  printf '[package]\nname = "fux-vt"\nversion = "0.1.0"\nedition = "2024"\n\n[dependencies]\nunicode-width = { path = "../width" }\n' \
    > "$ws/vt/Cargo.toml"
  # unicode-width, which fux-vt uses, is not at hand without the network:
  # a stand-in with the function fux-vt calls, widths from a table of none.
  mkdir -p "$ws/width/src"
  printf '[package]\nname = "unicode-width"\nversion = "0.2.0"\nedition = "2024"\n' > "$ws/width/Cargo.toml"
  cat > "$ws/width/src/lib.rs" <<'EOF'
//! A stand-in for unicode-width: every character is one column wide.
pub trait UnicodeWidthChar {
    fn width(self) -> Option<usize>;
}

impl UnicodeWidthChar for char {
    fn width(self) -> Option<usize> {
        if self.is_control() { None } else { Some(1) }
    }
}

pub trait UnicodeWidthStr {
    fn width(&self) -> usize;
}

impl UnicodeWidthStr for str {
    fn width(&self) -> usize {
        self.chars().filter_map(UnicodeWidthChar::width).sum()
    }
}
EOF
  printf '[package]\nname = "app"\nversion = "0.1.0"\nedition = "2024"\n\n[dependencies]\nwords = { path = "../words" }\nlines = { path = "../lines" }\ncounts = { path = "../counts" }\n' \
    > "$ws/app/Cargo.toml"
  cat > "$ws/app/src/main.rs" <<'EOF'
//! Counts the words of its arguments.
fn main() {
    let text: Vec<String> = std::env::args().skip(1).collect();
    let text = text.join(" ");
    println!("{} {} {}", words::count(&text), lines::count(&text), counts::count(&text));
}

#[cfg(test)]
mod tests {
    #[test]
    fn agrees_with_itself() {
        assert_eq!(words::count("x y"), lines::count("x y"));
    }

    #[test]
    fn a_test_that_fails() {
        assert_eq!(words::count("one two"), 3, "two words are not three");
    }
}
EOF
  printf '[workspace]\nmembers = ["app", "words", "lines", "counts", "width", "vt"]\nresolver = "3"\n' > "$ws/Cargo.toml"

  # A C file with mistakes, for clang's diagnostics.
  cat > "$work/bad.c" <<'EOF'
#include <stdio.h>

struct point { int x, y; };

int area(struct point a, struct point b) {
    int w = b.x - a.x
    int h = b.y - a.y;
    return w * h;
}

int main(void) {
    struct point a = {0, 0}, b = {3, 4};
    printf("%s\n", area(a, b));
    return undefined;
}
EOF

  # npm: a package installed from a directory beside it, offline.
  mkdir -p "$work/npm/app" "$work/npm/lib"
  printf '{"name":"lib","version":"1.0.0","main":"index.js"}\n' > "$work/npm/lib/package.json"
  printf 'module.exports = (s) => s.split(" ").length;\n' > "$work/npm/lib/index.js"
  printf '{"name":"app","version":"1.0.0","private":true}\n' > "$work/npm/app/package.json"

  # Text in many scripts and widths, for the editors.
  cat > "$work/unicode.txt" <<'EOF'
Plain ASCII, then text of other widths and scripts.

Wide (CJK): 日本語のテキスト、中文文本，한국어 텍스트。
Fullwidth: ＡＢＣ　ｘｙｚ　１２３
Emoji: 🙂 🚀 👍🏽 👩‍💻 🇵🇱 🏳️‍🌈 ❤️ ✨
Combining: é → é, ä → ä, ﬁ ligature, Z̤͔ͧ̑̓ä͖̭̈̇lͮ̒ͫǧ̗͚̚o̙̔ͮ̇͐̇
Greek: Αλφάβητο, Cyrillic: Кириллица, Hebrew: עברית, Arabic: العربية
Box drawing: ┌─┬─┐ │ │ │ ├─┼─┤ └─┴─┘ ═══ ║ ╔╗╚╝
Symbols: ← ↑ → ↓ ⇒ ∀ ∃ ∈ ∑ √ ∞ ≠ ≤ ≥ ± × ÷ ° € £ ¥
A long line of wide glyphs that wraps: 一二三四五六七八九十一二三四五六七八九十一二三四五六七八九十一二三四五六七八九十一二三四五六七八九十一二三四五六七八九十
	A line that starts with a tab, and one with trailing spaces:
The end.
EOF

  # Two versions of a file, for the editors' diff modes.
  cp "$repo/fux-vt/src/grid.rs" "$work/grid-old.rs"
  sed -e '30,34d' -e '60s/$/ \/\/ changed/' -e '90,92s/self/this/g' "$work/grid-old.rs" > "$work/grid-new.rs"
  printf '\n/// Added at the end.\npub fn added() -> usize {\n    1\n}\n' >> "$work/grid-new.rs"

  # A tree of files for the file managers and ncdu: sizes and names of
  # several kinds, made from fixed data.
  local tree=$work/tree
  mkdir -p "$tree/docs/guides" "$tree/src/parser" "$tree/src/screen" "$tree/media" \
    "$tree/build/cache" "$tree/.hidden"
  cp "$work/README.md" "$work/fux-corpus.1" "$tree/docs/"
  cp "$work/src/parser.rs" "$work/src/grid.rs" "$tree/src/"
  cp "$work/src/parser/tests.rs" "$tree/src/parser/"
  cp "$work/src/screen.rs" "$tree/src/screen/"
  cp "$work/unicode.txt" "$tree/docs/guides/"
  local size
  for size in 1 4 16 64 256; do
    head -c $((size * 1024)) /dev/zero | tr '\0' 'a' > "$tree/build/cache/blob-${size}k.bin"
  done
  head -c 2097152 /dev/zero > "$tree/media/zeros-2m.raw"
  head -c 300000 /dev/zero | tr '\0' 'b' > "$tree/media/b-300k.dat"
  printf 'secret-looking but generated\n' > "$tree/.hidden/config"
  printf 'notes\n' > "$tree/notes.txt"
  ln -s docs/README.md "$tree/readme-link"
  find "$tree" -exec touch -h -t 202601020304 {} +

  # Text with colours in it, for less -R.
  (cd "$work" && clean bat --color=always --style=numbers,header --theme=ansi src/link.rs \
    > colored.txt 2>/dev/null) || cp "$work/README.md" "$work/colored.txt"

  # Git: who commits in the generated repository, and how.
  cat > "$root/home/.gitconfig" <<'EOF'
[user]
	name = Corpus Author
	email = corpus@example.com
[init]
	defaultBranch = main
[advice]
	detachedHead = false
[commit]
	gpgsign = false
EOF
  # A repository with a history of its own: commits on two branches, a
  # merge, a tag, and changes not yet committed.
  local r=$work/repo
  mkdir -p "$r/src"
  clean git -C "$r" init -q
  cp "$repo/fux-vt/src/parser.rs" "$r/src/parser.rs"
  printf '# corpus\n\nA repository made for the corpus.\n' > "$r/README.md"
  commit "2026-01-05T10:00:00+0000" add -A
  commit "2026-01-05T10:00:00+0000" commit -q -m "Add the parser"
  cp "$repo/fux-vt/src/grid.rs" "$r/src/grid.rs"
  commit "2026-01-06T11:00:00+0000" add -A
  commit "2026-01-06T11:00:00+0000" commit -q -m "Add the grid" -m "The grid keeps the rows and their cells."
  commit "2026-01-07T09:00:00+0000" checkout -q -b feature
  sed -i '' -e '40,45d' -e '100s/$/ \/\/ the feature/' "$r/src/parser.rs"
  commit "2026-01-07T09:30:00+0000" commit -q -am "Parser: drop a check"
  cp "$repo/fux-vt/src/link.rs" "$r/src/link.rs"
  commit "2026-01-08T14:00:00+0000" add -A
  commit "2026-01-08T14:00:00+0000" commit -q -m "Add hyperlinks"
  commit "2026-01-08T15:00:00+0000" checkout -q main
  printf '\nBuilt with cargo.\n' >> "$r/README.md"
  commit "2026-01-09T08:00:00+0000" commit -q -am "README: how it is built"
  commit "2026-01-10T12:00:00+0000" merge -q --no-ff feature -m "Merge branch 'feature'"
  commit "2026-01-10T12:00:00+0000" tag -a v0.1.0 -m "v0.1.0"
  cp "$repo/fux-vt/src/screen.rs" "$r/src/screen.rs"
  commit "2026-01-11T16:00:00+0000" add -A
  commit "2026-01-11T16:00:00+0000" commit -q -m "Add the screen"
  commit "2026-01-12T10:00:00+0000" checkout -q -b fix
  sed -i '' -e '10s/$/ \/\/ fixed/' "$r/src/screen.rs"
  commit "2026-01-12T10:30:00+0000" commit -q -am "Screen: a fix on a branch"
  commit "2026-01-12T11:00:00+0000" checkout -q main
  # Changes not committed: a file changed in three places, one staged
  # change, and a file git does not know.
  sed -i '' -e '20,22d' -e '50s/$/ \/\/ edited/' -e '200a\
    // a line added' "$r/src/grid.rs"
  printf '\nStaged.\n' >> "$r/README.md"
  clean git -C "$r" add README.md
  printf 'not tracked\n' > "$r/notes.txt"

  # Line editors: a neutral prompt, history, completion.
  mkdir -p "$root/home/zsh" "$root/home/zsh-menu"
  cat > "$root/home/zsh/.zshrc" <<'EOF'
PS1='%~ %# '
RPS1=
HISTFILE=$HOME/.zsh_history
HISTSIZE=100
SAVEHIST=100
bindkey -e
autoload -Uz compinit && compinit -u -d "$HOME/.zcompdump"
EOF
  # zsh with menu selection, descriptions, colours and a right prompt.
  cat > "$root/home/zsh-menu/.zshrc" <<'EOF'
PS1='%F{blue}%~%f %# '
RPS1='%F{yellow}[%?]%f'
HISTFILE=$HOME/zsh-menu/.zsh_history
HISTSIZE=100
SAVEHIST=100
bindkey -e
autoload -Uz compinit && compinit -u -d "$HOME/.zcompdump-menu"
zstyle ':completion:*' menu select
zstyle ':completion:*:descriptions' format '%B%F{green}-- %d --%f%b'
zstyle ':completion:*' list-colors 'di=34' 'ln=35' 'ex=31'
zstyle ':completion:*' group-name ''
EOF
  printf '%s\n' ': 1767225600:0;cargo build --release' ': 1767225601:0;git status' \
    ': 1767225602:0;ls -la src' ': 1767225603:0;echo hello world' > "$root/home/zsh-menu/.zsh_history"
  cat > "$root/home/.bashrc" <<'EOF'
PS1='\w \$ '
HISTFILE=$HOME/.bash_history
EOF
  printf '%s\n' 'cargo build --release' 'git status' 'ls -la src' 'echo hello world' \
    'grep -n parse src/parser.rs' > "$root/home/.bash_history"
  printf "PS1='\$ '\n" > "$root/home/.shrc"
  cat > "$root/home/.vimrc" <<'EOF'
set nocompatible
syntax on
filetype plugin indent on
set number ruler laststatus=2 showcmd hlsearch incsearch
set ttimeoutlen=50
EOF

  # fish: no greeting, a neutral prompt, and a history to suggest from.
  mkdir -p "$root/home/.config/fish" "$root/home/.local/share/fish"
  cat > "$root/home/.config/fish/config.fish" <<'EOF'
set -g fish_greeting
function fish_prompt
    printf '%s> ' (prompt_pwd)
end
EOF
  {
    local when=1767225600 cmd
    for cmd in 'cargo build --release' 'git status' 'ls -la src' 'echo hello world' \
      'grep -n parse src/parser.rs'; do
      printf -- '- cmd: %s\n  when: %s\n' "$cmd" "$when"
      when=$((when + 1))
    done
  } > "$root/home/.local/share/fish/fish_history"

  # neovim: line numbers, no swap or shada files, diagnostics shown as
  # text too, and a command that sets made-up diagnostics on the file
  # (no language server is installed): :Diag.
  mkdir -p "$root/home/.config/nvim"
  cat > "$root/home/.config/nvim/init.lua" <<'EOF'
vim.o.number = true
vim.o.swapfile = false
vim.o.shada = ''
vim.o.ttimeoutlen = 50
vim.diagnostic.config({ virtual_text = true, underline = true, signs = true })
vim.api.nvim_create_user_command('Diag', function()
  local ns = vim.api.nvim_create_namespace('corpus')
  local s = vim.diagnostic.severity
  vim.diagnostic.set(ns, 0, {
    { lnum = 9, col = 12, end_col = 18, severity = s.WARN, message = 'unused variable: `unused`' },
    { lnum = 13, col = 4, end_col = 11, severity = s.HINT, message = 'method `most` is never used' },
    { lnum = 25, col = 18, end_col = 32, severity = s.ERROR, message = 'cannot find value `undefined_name` in this scope' },
    { lnum = 26, col = 20, end_col = 26, severity = s.ERROR, message = 'mismatched types: expected `u8`, found `&str`' },
    { lnum = 19, col = 8, end_col = 15, severity = s.INFO, message = 'counter is built here' },
  })
end, {})
EOF

  # micro: its defaults, with no backups, cursor or undo saved.
  mkdir -p "$root/home/.config/micro"
  printf '{"backup": false, "savecursor": false, "saveundo": false}\n' > "$root/home/.config/micro/settings.json"

  # htop: no user column (and only the processes the scenario starts).
  # The monitors update every 3 seconds, so a step of theirs can end:
  # the recorder ends a step once the program has been quiet a while.
  mkdir -p "$root/home/.config/htop"
  cat > "$root/home/.config/htop/htoprc" <<'EOF'
htop_version=3.5.3
config_reader_min_version=3
fields=0 17 18 38 39 46 47 49 1
hide_kernel_threads=1
hide_userland_threads=1
show_program_path=0
highlight_base_name=1
tree_view=0
header_layout=two_50_50
column_meters_0=LeftCPUs2 Memory Swap
column_meter_modes_0=1 1 1
column_meters_1=RightCPUs2 Tasks LoadAverage Uptime
column_meter_modes_1=1 2 2 2
delay=30
screen:Main=PID PRIORITY NICE M_VIRT M_RESIDENT PERCENT_CPU PERCENT_MEM TIME Command
.sort_key=PID
.tree_sort_key=PID
.sort_direction=1
.tree_sort_direction=1
.tree_view=0
.tree_view_always_by_pid=0
.all_branches_collapsed=0
EOF

  # btop: no process list (it would show every process and its user),
  # no disks (their names are the user's), the loopback interface.
  mkdir -p "$root/home/.config/btop"
  cat > "$root/home/.config/btop/btop.conf" <<'EOF'
color_theme = "Default"
theme_background = True
truecolor = True
shown_boxes = "cpu mem net"
update_ms = 3000
show_disks = False
show_battery = False
net_iface = "lo0"
net_auto = False
show_uptime = True
check_temp = False
EOF

  # lazygit: no popups at the start, no update check, no tips.
  mkdir -p "$root/home/.config/lazygit"
  cat > "$root/home/.config/lazygit/config.yml" <<'EOF'
disableStartupPopups: true
update:
  method: never
gui:
  showRandomTip: false
  nerdFontsVersion: ""
notARepository: skip
EOF

  # Midnight Commander: no subshell (-u), no hints, no confirmation to quit,
# no title (it would name the user and host).
  mkdir -p "$root/home/.config/mc"
  cat > "$root/home/.config/mc/ini" <<'EOF'
[Midnight-Commander]
confirm_exit=false
use_internal_edit=true
skin=default

[Layout]
message_visible=false
xterm_title=false
EOF

  # ranger: no user and host in the title, no image previews.
  mkdir -p "$root/home/.config/ranger"
  printf 'set hostname_in_titlebar false\nset preview_images false\nset show_hidden false\n' \
    > "$root/home/.config/ranger/rc.conf"

  # zellij: no tips or release notes, sh in its panes, nothing kept.
  mkdir -p "$root/home/.config/zellij"
  cat > "$root/home/.config/zellij/config.kdl" <<'EOF'
show_startup_tips false
show_release_notes false
default_shell "/bin/sh"
session_serialization false
mouse_mode false
copy_on_select false
EOF

  # Claude Code: onboarding done and the project trusted, for the main
  # screen (claude-main and the scenarios after it).
  mkdir -p "$work/project"
}

# Claude Code's later starts: see claude-main.
claude_trusted() {
  printf '{"hasCompletedOnboarding":true,"theme":"dark","projects":{"%s":{"hasTrustDialogAccepted":true}}}\n' \
    "$(cd "$root/work/project" && pwd -P)" > "$root/home/.claude.json"
}

# Fails, and deletes the recording, if it holds anything private.
check() {
  local name=$1 file hits found=0
  local args=()
  for value in "${private[@]}"; do args+=(-e "$value"); done
  for file in "$here/$name.bin" "$here/$name.json"; do
    hits=$(grep -c -i -a -F "${args[@]}" "$file" || true)
    if ((hits > 0)); then
      echo "record.sh: $(basename "$file") holds private text ($hits lines): dropped" >&2
      found=1
    fi
  done
  if ((found)); then
    mkdir -p "$root/dropped"
    mv "$here/$name.bin" "$here/$name.json" "$root/dropped/"
    dropped+=("$name")
  fi
}

# record NAME VERSION DIR [record options] -- PROGRAM ARGS...
record() {
  local name=$1 version=$2 dir=$3
  shift 3
  echo "== $name ($version)" >&2
  "$bin" record --size 40x120 --keys "$here/keys/$name.keys" --out "$here/$name" \
    --program "$name" --version "$version" --dir "$dir" \
    --env "PATH=$path" --env "HOME=$root/home" --env LANG=en_US.UTF-8 \
    --env COLORTERM=truecolor --env TZ=UTC "$@"
  check "$name"
}

first() { "$@" 2>&1 | head -1; }

# A tmux of its own, and the commands to end it.
tmux_record() {
  local name=$1 size=$2
  shift 2
  record "$name" "$(first tmux -V)" "$root/work" --size "$size" --env SHELL=/bin/sh \
    --env "ENV=$root/home/.shrc" --env "TMUX_TMPDIR=$root/tmux" \
    -- tmux -L fux-corpus -f /dev/null new-session -s corpus ';' set-option -g status-right corpus "$@"
  TMUX_TMPDIR=$root/tmux tmux -L fux-corpus kill-server 2>/dev/null || true
}

# Background processes for the monitors to show, and nothing else.
watched=()
watch_start() {
  sleep 600 &
  watched+=($!)
  sleep 601 &
  watched+=($!)
  yes >/dev/null &
  watched+=($!)
}
watch_stop() {
  kill "${watched[@]}" 2>/dev/null || true
  wait "${watched[@]}" 2>/dev/null || true
  watched=()
}
watched_list() {
  local IFS=,
  echo "${watched[*]}"
}

scenario() {
  local work=$root/work
  local nvim_version emacs_version
  case $1 in
    vim) record vim "$(first vim --version)" "$work" -- vim src/parser.rs ;;
    vim-diff) record vim-diff "$(first vim --version)" "$work" -- vim -d grid-old.rs grid-new.rs ;;
    vim-insert) record vim-insert "$(first vim --version)" "$work" -- vim src/screen.rs ;;
    vim-help) record vim-help "$(first vim --version)" "$work" -- vim +help ;;
    vim-unicode) record vim-unicode "$(first vim --version)" "$work" -- vim unicode.txt ;;
    vim-small) record vim-small "$(first vim --version)" "$work" --size 24x80 -- vim src/link.rs ;;
    vim-resize) record vim-resize "$(first vim --version)" "$work" -- vim src/grid.rs ;;
    vim-terminal)
      record vim-terminal "$(first vim --version)" "$work" --env SHELL=/bin/sh \
        --env "ENV=$root/home/.shrc" -- vim ;;
    nvim-*)
      nvim_version=$(first nvim --version)
      case $1 in
        nvim-scroll) record "$1" "$nvim_version" "$work" -- nvim src/parser.rs ;;
        nvim-search) record "$1" "$nvim_version" "$work" -- nvim src/grid.rs ;;
        nvim-visual) record "$1" "$nvim_version" "$work" -- nvim src/link.rs ;;
        nvim-split) record "$1" "$nvim_version" "$work" -- nvim src/screen.rs ;;
        nvim-diagnostics)
          record "$1" "$nvim_version" "$work/hello" \
            --note "no language server: the diagnostics are set by :Diag, in HOME's init.lua" \
            -- nvim src/main.rs ;;
        nvim-insert) record "$1" "$nvim_version" "$work" -- nvim src/lib.rs ;;
        nvim-terminal)
          record "$1" "$nvim_version" "$work" --env SHELL=/bin/sh --env "ENV=$root/home/.shrc" -- nvim ;;
        nvim-help) record "$1" "$nvim_version" "$work" -- nvim +help ;;
        nvim-diff) record "$1" "$nvim_version" "$work" -- nvim -d grid-old.rs grid-new.rs ;;
        nvim-tabs) record "$1" "$nvim_version" "$work" -- nvim -p src/lib.rs src/link.rs README.md ;;
        nvim-unicode) record "$1" "$nvim_version" "$work" -- nvim unicode.txt ;;
        nvim-small) record "$1" "$nvim_version" "$work" --size 10x40 -- nvim src/link.rs ;;
        nvim-wide) record "$1" "$nvim_version" "$work" --size 50x200 -- nvim -O src/grid.rs src/screen.rs ;;
        nvim-resize) record "$1" "$nvim_version" "$work" -- nvim src/parser.rs ;;
        nvim-netrw) record "$1" "$nvim_version" "$work" -- nvim src ;;
        *) echo "record.sh: no scenario $1" >&2; return 2 ;;
      esac ;;
    helix)
      # helix sends the same with TERM_PROGRAM=ghostty (tried): it goes by
      # terminfo and its own queries.
      record helix "$(first hx --version)" "$work/hello" \
        --note "rust-analyzer $(first rust-analyzer --version) gives the diagnostics" \
        -- hx src/main.rs ;;
    helix-picker) record helix-picker "$(first hx --version)" "$work" -- hx . ;;
    helix-select) record helix-select "$(first hx --version)" "$work" -- hx README.md ;;
    helix-small) record helix-small "$(first hx --version)" "$work" --size 24x80 -- hx fux-corpus.1 ;;
    helix-resize) record helix-resize "$(first hx --version)" "$work" -- hx README.md ;;
    helix-unicode) record helix-unicode "$(first hx --version)" "$work" -- hx unicode.txt ;;
    emacs-*)
      emacs_version=$(first emacs --version)
      case $1 in
        emacs-scroll) record "$1" "$emacs_version" "$work" -- emacs -nw -Q src/parser.rs ;;
        emacs-split) record "$1" "$emacs_version" "$work" -- emacs -nw -Q src/grid.rs ;;
        emacs-dired)
          # ls-lisp lists the files itself, without owners or groups.
          record "$1" "$emacs_version" "$work" -- emacs -nw -Q \
            --eval "(progn (require 'ls-lisp) (setq ls-lisp-use-insert-directory-program nil ls-lisp-verbosity nil))" \
            src ;;
        emacs-mx) record "$1" "$emacs_version" "$work" -- emacs -nw -Q README.md ;;
        emacs-resize) record "$1" "$emacs_version" "$work" -- emacs -nw -Q src/link.rs ;;
        *) echo "record.sh: no scenario $1" >&2; return 2 ;;
      esac
      # What emacs saves on its own, which the scenarios after would show.
      find "$work" -name '#*#' -delete ;;
    micro-edit) record micro-edit "micro $(micro --version | sed -n 's/^Version: //p')" "$work" -- micro README.md ;;
    micro-split) record micro-split "micro $(micro --version | sed -n 's/^Version: //p')" "$work" -- micro src/link.rs ;;
    micro-small)
      record micro-small "micro $(micro --version | sed -n 's/^Version: //p')" "$work" --size 24x80 \
        -- micro unicode.txt ;;
    pico) record pico "UW PICO 5.09 (/usr/bin/nano)" "$work" --size 24x80 -- nano README.md ;;
    htop | htop-tree | htop-small)
      watch_start
      local size=40x120
      if [[ $1 == htop-small ]]; then size=24x80; fi
      record "$1" "$(first htop --version)" "$work" --size "$size" \
        --note "htop -p: only the three processes the scenario starts" -- htop -p "$(watched_list)"
      watch_stop ;;
    btop | btop-small)
      local size=40x120
      if [[ $1 == btop-small ]]; then size=24x80; fi
      record "$1" "$(first btop --version | sed $'s/\e\\[[0-9;]*m//g')" "$work" --size "$size" \
        --note "no process list, disks or battery (HOME's btop.conf)" -- btop ;;
    top)
      watch_start
      local pids=() pid
      for pid in "${watched[@]}"; do pids+=(-pid "$pid"); done
      record top "top (macOS)" "$work" --note "top -pid: only the three processes the scenario starts" \
        -- top -s 3 -o pid "${pids[@]}"
      watch_stop ;;
    lazygit | lazygit-stage | lazygit-small)
      rm -rf "$work/repo-$1"
      clean git clone -q "$work/repo" "$work/repo-$1"
      cp "$work/repo/src/grid.rs" "$work/repo-$1/src/grid.rs"
      printf 'not tracked\n' > "$work/repo-$1/notes.txt"
      local size=40x120
      if [[ $1 == lazygit-small ]]; then size=24x80; fi
      record "$1" "lazygit $(lazygit --version | sed -E 's/.*, version=([^,]+),.*/\1/')" "$work/repo-$1" \
        --size "$size" --env "XDG_CONFIG_HOME=$root/home/.config" --env GIT_CONFIG_NOSYSTEM=1 \
        -- lazygit ;;
    tig) record tig "$(first tig --version)" "$work/repo" --env GIT_CONFIG_NOSYSTEM=1 -- tig --all ;;
    tig-tree) record tig-tree "$(first tig --version)" "$work/repo" --env GIT_CONFIG_NOSYSTEM=1 -- tig ;;
    tig-blame)
      record tig-blame "$(first tig --version)" "$work/repo" --env GIT_CONFIG_NOSYSTEM=1 \
        -- tig blame src/parser.rs ;;
    git-graph)
      record git-graph "$(first git --version)" "$work/repo" --env GIT_CONFIG_NOSYSTEM=1 \
        -- git log --graph --all --decorate --color=always \
        --format='%C(yellow)%h%C(reset) %C(auto)%d%C(reset) %s %C(dim)(%an, %ad)%C(reset)' --date=short ;;
    git-diff)
      record git-diff "$(first git --version)" "$work/repo" --env GIT_CONFIG_NOSYSTEM=1 \
        -- git -c color.ui=always diff --stat -p HEAD ;;
    git-add-p)
      rm -rf "$work/repo-add"
      clean git clone -q "$work/repo" "$work/repo-add"
      cp "$work/repo/src/grid.rs" "$work/repo-add/src/grid.rs"
      record git-add-p "$(first git --version)" "$work/repo-add" --env GIT_CONFIG_NOSYSTEM=1 \
        -- git -c color.ui=always add -p ;;
    delta-show)
      record delta-show "$(first delta --version), $(first git --version)" "$work/repo" \
        --env GIT_CONFIG_NOSYSTEM=1 \
        -- git -c core.pager='delta --paging=always --line-numbers' show feature~1 ;;
    delta-wide)
      record delta-wide "$(first delta --version), $(first git --version)" "$work/repo" --size 50x200 \
        --env GIT_CONFIG_NOSYSTEM=1 \
        -- git -c core.pager='delta --paging=always --side-by-side --line-numbers' diff HEAD ;;
    bat-diff)
      record bat-diff "$(first bat --version)" "$work/repo" --env GIT_CONFIG_NOSYSTEM=1 \
        -- bat --paging=always --diff src/grid.rs ;;
    bat-page) record bat-page "$(first bat --version)" "$work" -- bat --paging=always src/grid.rs ;;
    bat-markdown)
      record bat-markdown "$(first bat --version)" "$work" --size 24x80 \
        -- bat --paging=never --style=full --line-range=1:60 README.md ;;
    mc | mc-small)
      local size=40x120
      if [[ $1 == mc-small ]]; then size=24x80; fi
      # mc puts the host name in its OSC 7 (the current directory).
      record "$1" "$(first mc --version)" "$work/tree" --size "$size" --env SHELL=/bin/sh \
        --env "XDG_CONFIG_HOME=$root/home/.config" --scrub "$(hostname)=localhost" -- mc -u . ../src ;;
    ranger)
      record ranger "$(first ranger --version | sed 's/^ranger version: //')" "$work/tree" \
        --env "XDG_CONFIG_HOME=$root/home/.config" --env "XDG_DATA_HOME=$root/home/.local/share" \
        --env SHELL=/bin/sh --scrub "$login=$owner_mask" \
        --note "ranger shows the owner of each file: the recorder's login is scrubbed to x's of its length" \
        -- ranger "$work/tree" ;;
    nnn) record nnn "nnn $(first nnn -V)" "$work/tree" --env SHELL=/bin/sh -- nnn ;;
    nnn-detail) record nnn-detail "nnn $(first nnn -V)" "$work/tree" --env SHELL=/bin/sh -- nnn -d -H ;;
    ncdu) record ncdu "$(first ncdu --version)" "$work" -- ncdu --color dark tree ;;
    less) record less "$(first less --version)" "$work" -- less README.md ;;
    less-chop) record less-chop "$(first less --version)" "$work" -- less -S -N src/parser.rs ;;
    less-color) record less-color "$(first less --version)" "$work" -- less -R colored.txt ;;
    less-small) record less-small "$(first less --version)" "$work" --size 10x40 -- less unicode.txt ;;
    man) record man "man (macOS), mandoc" "$work" -- man ./fux-corpus.1 ;;
    man-tables) record man-tables "man (macOS), mandoc" "$work" --size 24x80 -- man ./fux-corpus-tables.1 ;;
    man-long) record man-long "man (macOS), mandoc" "$work" -- man ./fux-corpus-long.7 ;;
    man-wide) record man-wide "man (macOS), mandoc" "$work" --size 50x200 -- man ./fux-corpus-long.7 ;;
    man-small) record man-small "man (macOS), mandoc" "$work" --size 10x40 -- man ./fux-corpus.1 ;;
    fzf)
      record fzf "fzf $(first fzf --version)" "$work" --env 'FZF_DEFAULT_COMMAND=find . -type f' -- fzf ;;
    fzf-height)
      record fzf-height "fzf $(first fzf --version)" "$work" \
        --env 'FZF_DEFAULT_COMMAND=find . -type f' -- fzf --height=40% --layout=reverse --border ;;
    fzf-preview)
      record fzf-preview "fzf $(first fzf --version), $(first bat --version)" "$work" \
        --env 'FZF_DEFAULT_COMMAND=find src -type f' \
        -- fzf --preview 'bat --color=always --style=numbers {}' ;;
    fzf-multi)
      record fzf-multi "fzf $(first fzf --version)" "$work" --env 'FZF_DEFAULT_COMMAND=find . -type f' \
        -- fzf -m --height=50% --border=rounded --info=inline ;;
    fzf-small)
      record fzf-small "fzf $(first fzf --version)" "$work" --size 10x40 \
        --env 'FZF_DEFAULT_COMMAND=find . -type f' -- fzf ;;
    gls)
      record gls "$(first gls --version)" "$work" --scrub "$(hostname)=localhost" \
        -- gls --color=always --hyperlink=always -F . src ;;
    gls-long)
      record gls-long "$(first gls --version)" "$work" --scrub "$(hostname)=localhost" \
        -- gls -l -g -G --color=always --hyperlink=always --time-style=long-iso tree tree/src ;;
    gls-wide)
      record gls-wide "$(first gls --version)" "$work" --size 50x200 --scrub "$(hostname)=localhost" \
        -- gls -R --color=always --hyperlink=always -F -C src tree ;;
    delta-log)
      record delta-log "$(first delta --version), $(first git --version)" "$repo" \
        --env GIT_CONFIG_NOSYSTEM=1 \
        -- git -c core.pager='delta --paging=always' -c color.ui=always log -p -n 3 \
        --format='commit %h%nDate:   %ad%n%n    %s%n' --date=iso a0279cc ;;
    delta-diff)
      record delta-diff "$(first delta --version), $(first git --version)" "$repo" \
        --env GIT_CONFIG_NOSYSTEM=1 \
        -- git -c core.pager='delta --paging=always --side-by-side' diff a0279cc~1 a0279cc ;;
    cargo-build)
      rm -rf "$work/tiny/target"
      record cargo-build "$(first cargo --version)" "$work/tiny" -- cargo build ;;
    cargo-errors) record cargo-errors "$(first cargo --version)" "$work/hello" -- cargo build ;;
    cargo-test)
      record cargo-test "$(first cargo --version)" "$work/tiny" -- cargo test --workspace --exclude fux-vt --no-fail-fast ;;
    clang-errors) record clang-errors "$(first clang --version)" "$work" -- clang -fsyntax-only bad.c ;;
    npm-install)
      rm -rf "$work/npm/app/node_modules" "$work/npm/app/package-lock.json"
      record npm-install "npm $(first npm --version), node $(first node --version)" "$work/npm/app" \
        --note "a package from the directory beside it, offline" \
        -- npm install --offline --no-audit --no-fund --no-update-notifier ../lib ;;
    zsh) record zsh "$(first /bin/zsh --version)" "$work" --env "ZDOTDIR=$root/home/zsh" -- /bin/zsh -i ;;
    zsh-menu | zsh-history | zsh-resize | zsh-small)
      local size=40x120
      if [[ $1 == zsh-small ]]; then size=10x40; fi
      record "$1" "$(first /bin/zsh --version)" "$work" --size "$size" \
        --env "ZDOTDIR=$root/home/zsh-menu" -- /bin/zsh -i ;;
    bash) record bash "$(first bash --version)" "$work" -- bash --noprofile --rcfile "$root/home/.bashrc" -i ;;
    bash-complete | bash-history | bash-small)
      local size=40x120
      if [[ $1 == bash-small ]]; then size=10x40; fi
      record "$1" "$(first bash --version)" "$work" --size "$size" \
        -- bash --noprofile --rcfile "$root/home/.bashrc" -i ;;
    fish | fish-complete | fish-history | fish-small)
      # fish puts the host name in its OSC 7 (the current directory).
      local size=40x120
      if [[ $1 == fish-small ]]; then size=10x40; fi
      record "$1" "$(first fish --version)" "$work" --size "$size" --scrub "$(hostname)=localhost" -- fish ;;
    tmux) tmux_record tmux 40x120 ;;
    tmux-resize) tmux_record tmux-resize 40x120 ;;
    tmux-small) tmux_record tmux-small 24x80 ;;
    tmux-copy) tmux_record tmux-copy 40x120 ;;
    tmux-vim) tmux_record tmux-vim 40x120 ';' set-option -g default-terminal tmux-256color ;;
    zellij | zellij-small)
      local size=40x120
      if [[ $1 == zellij-small ]]; then size=24x80; fi
      record "$1" "$(first zellij --version)" "$work" --size "$size" --env SHELL=/bin/sh \
        --env "ENV=$root/home/.shrc" --env "ZELLIJ_SOCKET_DIR=$root/zellij" \
        --env "XDG_CONFIG_HOME=$root/home/.config" \
        -- zellij --config "$root/home/.config/zellij/config.kdl" --session corpus
      ZELLIJ_SOCKET_DIR=$root/zellij HOME=$root/home zellij kill-all-sessions -y >/dev/null 2>&1 || true ;;
    claude)
      # A first start: no settings, so the theme is asked for.
      rm -f "$root/home/.claude.json"
      record claude "Claude Code $(first claude --version)" "$work/project" \
        --note "a first start: HOME has no Claude Code settings" \
        --env DISABLE_AUTOUPDATER=1 -- claude ;;
    claude-main | claude-ghostty | claude-small | claude-resize)
      # A later start: onboarding done, the directory trusted, so the main
      # screen and its prompt show. fux passes its own environment on to
      # its panes, TERM_PROGRAM included: claude-ghostty is fux run from
      # Ghostty. No prompt is ever sent.
      claude_trusted
      local outer=() size=40x120
      if [[ $1 == claude-ghostty ]]; then outer=(--env TERM_PROGRAM=ghostty); fi
      if [[ $1 == claude-small ]]; then size=24x80; fi
      record "$1" "Claude Code $(first claude --version)" "$work/project" --size "$size" \
        --note "onboarding done and the directory trusted, in HOME's .claude.json" \
        --env DISABLE_AUTOUPDATER=1 "${outer[@]}" -- claude ;;
    *) echo "record.sh: no scenario $1" >&2; return 2 ;;
  esac
}

all=(
  vim vim-diff vim-insert vim-help vim-unicode vim-small vim-resize vim-terminal
  nvim-scroll nvim-search nvim-visual nvim-split nvim-diagnostics nvim-insert nvim-terminal
  nvim-help nvim-diff nvim-tabs nvim-unicode nvim-small nvim-wide nvim-resize nvim-netrw
  helix helix-picker helix-select helix-small helix-resize helix-unicode
  emacs-scroll emacs-split emacs-dired emacs-mx emacs-resize
  micro-edit micro-split micro-small pico
  htop htop-tree htop-small btop btop-small top
  lazygit lazygit-stage lazygit-small tig tig-tree tig-blame
  git-graph git-diff git-add-p delta-log delta-diff delta-show delta-wide bat-diff
  mc mc-small ranger nnn nnn-detail ncdu
  less less-chop less-color less-small bat-page bat-markdown
  man man-tables man-long man-wide man-small
  fzf fzf-height fzf-preview fzf-multi fzf-small gls gls-long gls-wide
  cargo-build cargo-errors cargo-test clang-errors npm-install
  zsh zsh-menu zsh-history zsh-resize zsh-small bash bash-complete bash-history bash-small
  fish fish-complete fish-history fish-small
  tmux tmux-resize tmux-small tmux-copy tmux-vim zellij zellij-small
  claude claude-main claude-ghostty claude-small claude-resize
)
dropped=()
setup
started=$SECONDS
names=("${@:-${all[@]}}")
for name in "${names[@]}"; do
  scenario "$name"
done
echo "recorded ${#names[@]} scenarios in $((SECONDS - started))s" >&2

# The inventory again, and what changed in it.
"$bin" inventory > "$here/INVENTORY.md"
echo "== INVENTORY.md: what changed" >&2
git -C "$repo" diff --stat -- "$here/INVENTORY.md" >&2
git -C "$repo" diff --no-color -U0 -- "$here/INVENTORY.md" | grep -E '^[-+]\| `' >&2 || echo "(no sequence rows changed)" >&2

if ((${#dropped[@]})); then
  echo "record.sh: dropped for holding private text: ${dropped[*]}" >&2
  exit 1
fi
