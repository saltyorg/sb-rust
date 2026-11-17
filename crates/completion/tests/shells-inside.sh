#!/usr/bin/env bash
# The inside of shells.sh: runs as root in a throwaway ubuntu:<release>
# container, with the sb under test at /usr/local/bin/sb and results
# written to /out. $1 is the release. Its last line is
# `RESULT: <release>: passed` or the first failure.
#
# Real shells from apt (bash with bash-completion, zsh, fish) and a user,
# `sbuser`, whose login shell is each in turn, driven in tmux with real
# Tab presses. The completion files are the ones sb puts in each shell's
# loading directory (completion spec, "`sb completion`"), and the user's
# own setup is only a key that copies the edited line to ~/captured
# (sb-go's cmd/completion_shell_linux_test.go captures it the same way):
# Ctrl-X, bound in ~/.bashrc, ~/.zshrc and fish's config.fish.
set -euo pipefail
rel=$1
export DEBIAN_FRONTEND=noninteractive
fail() { echo "RESULT: $rel: FAIL: $*"; exit 1; }
note() { echo "NOTE: $*" | tee -a /out/notes.txt; }

apt-get update -qq >/dev/null
apt-get install -y -qq bash-completion zsh fish tmux >/out/packages.log 2>&1
{
  echo "bash $(bash -c 'echo $BASH_VERSION')"
  dpkg-query -W -f='${Package} ${Version}\n' bash-completion zsh fish tmux
} >/out/versions.txt

# The tag cache sb writes, at the default Saltbox root: Saltbox's tags and
# Sandbox's (keyed by the default appdata root's sandbox), readable by
# every user, as sb leaves it (0644).
mkdir -p /srv/git/saltbox
cat >/srv/git/saltbox/cache.json <<'EOF'
{"/srv/git/saltbox": {"commit": "a", "tags": ["plex", "radarr", "sonarr"]},
 "/opt/sandbox": {"commit": "a", "tags": ["plex"]}}
EOF
chmod 0644 /srv/git/saltbox/cache.json

useradd -m -s /bin/bash sbuser
home=$(getent passwd sbuser | cut -d: -f6)
# A file a fallback to file names would complete (`so<Tab>`), so a
# fallback shows.
touch "$home/sofile"
cat >>"$home/.bashrc" <<'EOF'
bind -x '"\C-x": printf "%s\n" "$READLINE_LINE" >>~/captured; READLINE_LINE=; READLINE_POINT=0'
EOF
cat >"$home/.zshrc" <<'EOF'
sb_capture() { print -r -- "$BUFFER" >>~/captured; BUFFER=; }
zle -N sb_capture
bindkey '^X' sb_capture
EOF
mkdir -p "$home/.config/fish"
cat >"$home/.config/fish/config.fish" <<'EOF'
function sb_capture
    commandline >>~/captured
    commandline --replace ''
end
bind \cx sb_capture
EOF
chown -R sbuser:sbuser "$home"

# shellcheck disable=SC2016 # fish expands it
fish_dir=$(fish --no-config -c 'printf %s $__fish_sysconf_dir')/completions
bash_dir=/usr/local/share/bash-completion/completions
zsh_dir=/usr/local/share/zsh/site-functions
mkdir -p "$bash_dir"
note "fish loads from $fish_dir; zsh's clean fpath: $(zsh -d -f -c 'print -l $fpath' | tr '\n' ' ')"

# Verification item 2: which bash file names bash-completion's loader
# finds in /usr/local/share/bash-completion/completions.
for file in sb.bash sb; do
  rm -f "$bash_dir/sb" "$bash_dir/sb.bash"
  sb completion generate bash >"$bash_dir/$file"
  if su sbuser -s /bin/bash -c 'source /usr/share/bash-completion/bash_completion
      complete -p sb >/dev/null 2>&1 && exit 71
      if declare -F _comp_load >/dev/null; then _comp_load sb; else _completion_loader sb; fi
      complete -p sb >/dev/null 2>&1'; then
    note "bash-completion loads $bash_dir/$file"
  else
    note "bash-completion does not load $bash_dir/$file"
  fi
done
rm -f "$bash_dir/sb" "$bash_dir/sb.bash"

# `sb completion install` as root, with a second name, a symlink to sb:
# every shell's file for each name, each calling its own path
# (verification items 2 and 5).
ln -s sb /usr/local/bin/saltbox
sb completion install >/out/install.txt 2>&1 || fail "completion install: $(cat /out/install.txt)"
for name in sb saltbox; do
  for file in "$bash_dir/$name.bash" "$zsh_dir/_$name" "$fish_dir/$name.fish"; do
    grep -qx "Installed .* completion: $file" /out/install.txt || fail "no line for $file"
    [ "$(stat -c %a "$file")" = 644 ] || fail "$file is not 0644"
    head -n 2 "$file" | grep -q "^# sb completion [a-z]* $name\$" || fail "$file has no marker"
  done
  grep -q "/usr/local/bin/$name" "$zsh_dir/_$name" || fail "_$name doesn't call /usr/local/bin/$name"
  grep -q "/usr/local/bin/$name" "$fish_dir/$name.fish" || fail "$name.fish doesn't call /usr/local/bin/$name"
done
for name in sb saltbox; do
  grep -q "_clap_complete_$name $name\$" "$bash_dir/sb.bash" || fail "sb.bash doesn't register $name"
done
note "completion install: $(wc -l </out/install.txt) lines, every file 0644 with its marker"

# One login shell of the user's in tmux; type each case, press Tab once,
# then Ctrl-X: ~/captured gets the line as completion left it.
cases=()
add() { cases+=("$1" "$2"); }
add "sb vers" "sb version"
add "sb install --no-c" "sb install --no-cache"
add "sb install plex --no-c" "sb install plex --no-cache"
add "sb install so" "sb install sonarr"
add "sb install plex p" "sb install plex p"
add "sb install plex,so" "sb install plex,sonarr"
add "sb install plex,radarr,so" "sb install plex,radarr,sonarr"
add "sb install plex, so" "sb install plex, sonarr"
add "sb install sandbox-p" "sb install sandbox-plex"
add "sb install --extra-vars so" "sb install --extra-vars so"
add "sb install --skip-tags so" "sb install --skip-tags so"
add "sb install --extra-vars=so" "sb install --extra-vars=so"
add "sb install --extra-vars \"x=y z\" so" "sb install --extra-vars \"x=y z\" sonarr"
add "sb install \"plex\" p" "sb install \"plex\" p"

failures=0
# drive SHELL LABEL CASE...: a fresh login shell, the cases in order (the
# first Tab is the shell's first completion of sb: verification item 1).
drive() {
  local shell=$1 label=$2
  shift 2
  local n=0 i got want input
  rm -f "$home/captured"
  tmux kill-server 2>/dev/null || true
  tmux new-session -d -s t -x 200 -y 50 "su - sbuser -s $shell"
  sleep 3
  for ((i = 0; i < $#; i += 2)); do
    input=${*:i+1:1}
    want=${*:i+2:1}
    tmux send-keys -t t -l "$input"
    tmux send-keys -t t Tab
    sleep 1.5
    tmux send-keys -t t C-x
    n=$((n + 1))
    for _ in $(seq 50); do
      [ "$(wc -l <"$home/captured" 2>/dev/null || echo 0)" -ge "$n" ] && break
      sleep 0.1
    done
    got=$(sed -n "${n}p" "$home/captured" 2>/dev/null | sed 's/[[:space:]]*$//')
    if [ "$got" = "$want" ]; then
      echo "PASS: $label: [$input] -> [$got]"
    else
      echo "FAIL: $label: [$input] -> [$got], want [$want]"
      failures=$((failures + 1))
    fi
  done
  tmux capture-pane -t t -p >"/out/screen-$label.txt"
  tmux kill-server 2>/dev/null || true
}

# An open quote: bash's readline takes the quote off the word, completes
# it and closes the quote; zsh and fish pass the quote to sb, which
# offers nothing (sb-go's rule for fish's unparsed quote).
quoted() {
  if [ "$1" = bash ]; then
    printf '%s\n' "sb install 'plex, so" "sb install 'plex, sonarr'" \
      "sb install \"plex, so" "sb install \"plex, sonarr\""
  else
    printf '%s\n' "sb install 'plex, so" "sb install 'plex, so" \
      "sb install \"plex, so" "sb install \"plex, so"
  fi
}

for shell in /bin/bash /usr/bin/zsh /usr/bin/fish; do
  name=$(basename "$shell")
  mapfile -t open < <(quoted "$name")
  drive "$shell" "$name" "${cases[@]}" "${open[@]}"
  # A fresh shell whose first Tab is a tag (item 1 for zsh's autoload).
  drive "$shell" "$name-first-tag" "sb install so" "sb install sonarr"
  # The symlink's own file, its first Tab included (item 2).
  drive "$shell" "$name-saltbox" "saltbox vers" "saltbox version" \
    "saltbox install plex,so" "saltbox install plex,sonarr"
done

[ "$failures" -eq 0 ] || fail "$failures case(s) failed"
echo "RESULT: $rel: passed"
