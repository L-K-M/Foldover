#!/usr/bin/env bash
# Check scripts/build.sh --install against a fake macOS toolchain: the
# app replaces the installed copy, a running app is quit and reopened,
# a loaded agent is left for the app to restart, and failures leave
# the installed app alone. Runs on Linux and macOS; touches
# nothing outside a temp dir.
#
# The fakes' bodies are single-quoted on purpose: they expand when the
# fake runs, not here.
# shellcheck disable=SC2016
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

failures=0
fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }

# A throwaway repo holding only build.sh, so builds write nothing here.
repo="$work/repo"
mkdir -p "$repo/scripts"
cp "$here/build.sh" "$repo/scripts/build.sh"
printf '[workspace.package]\nversion = "9.9.9"\n' > "$repo/Cargo.toml"

# Fakes record their calls in $log and read their behavior from $state:
#   app-running      holds the command line of this user's running app;
#                    pgrep and pkill match their pattern against it, and
#                    pkill removes it
#   helper-running   the same, for a FoldoverHelper next to the app
#   agent-running    launchctl print reports a running agent
#   agent-loaded     launchctl print reports a loaded, stopped agent
#   interrupted-swap the old app sits in .Foldover.app.outgoing, as a run
#                    killed mid-swap leaves it
#   mv-into-fails    moving the new copy into place fails
#   ditto-fails, open-fails, xcodebuild-fails,
#   app-ignores-term
fakes="$work/bin"
log="$work/calls.log"
state="$work/state"
mkdir -p "$fakes"

fake() {
  printf '#!/usr/bin/env bash\nlog=%q; state=%q\n%s\n' "$log" "$state" "$2" > "$fakes/$1"
  chmod +x "$fakes/$1"
}
fake uname 'echo Darwin'
fake cargo '
echo "cargo $*" >> "$log"
[[ "$1" == build ]] || exit 0
profile=debug
for arg in "$@"; do [[ "$arg" == --release ]] && profile=release; done
mkdir -p "target/$profile" && touch "target/$profile/foldoverd" "target/$profile/foldover"'
fake xcodegen 'echo "xcodegen $*" >> "$log"'
fake xcodebuild '
echo "xcodebuild $*" >> "$log"
[[ -e "$state/xcodebuild-fails" ]] && exit 1
while [[ $# -gt 0 ]]; do
  case "$1" in
    -configuration) config="$2"; shift ;;
    -derivedDataPath) derived="$2"; shift ;;
  esac
  shift
done
app="$derived/Build/Products/$config/Foldover.app/Contents/MacOS"
mkdir -p "$app" && echo new > "$app/Foldover"'
fake ditto '[[ -e "$state/ditto-fails" ]] && exit 1; cp -R "$1" "$2"'
fake mv '[[ -e "$state/mv-into-fails" && "$1" == *.incoming ]] && exit 1; exec /bin/mv "$@"'
# Both refuse to run unscoped to this user, then match like the real tools.
fake pgrep '
echo "pgrep $*" >> "$log"
[[ "$1 $2" == "-U $(id -u)" ]] || exit 2
[[ -e "$state/app-running" ]] && grep -Eq -- "${!#}" "$state/app-running"'
fake pkill '
echo "pkill $*" >> "$log"
[[ "$2 $3" == "-U $(id -u)" ]] || exit 2
[[ -e "$state/app-running" ]] && grep -Eq -- "${!#}" "$state/app-running" || exit 1
[[ -e "$state/app-ignores-term" ]] || rm -f "$state/app-running"'
fake sleep ':'
fake launchctl '
echo "launchctl $*" >> "$log"
case "$1" in
  print)
    if [[ -e "$state/agent-running" ]]; then echo "	state = running"
    elif [[ -e "$state/agent-loaded" ]]; then echo "	state = not running"
    else exit 113; fi ;;
esac'
fake open 'echo "open $*" >> "$log"; [[ ! -e "$state/open-fails" ]]'

# Regex metacharacters and a space: the process pattern must quote them.
apps="$work/My Apps (test)"
installed="$apps/Foldover.app/Contents/MacOS/Foldover"

# run NAME [STATE...] -- ARGS: reset the fakes, preinstall an old app,
# run build.sh, and keep its output in $out and exit status in $status.
# applications_dir=... before run passes another FOLDOVER_APPLICATIONS_DIR.
run() {
  name="$1"; shift
  rm -rf "$state" "$apps" "$repo/dist" "$repo/build" "$repo/target" "$log"
  mkdir -p "$state" "$(dirname "$installed")"
  echo old > "$installed"
  while [[ "$1" != "--" ]]; do touch "$state/$1"; shift; done
  shift
  if [[ -e "$state/hashed-agent" ]]; then
    local plist="$apps/Foldover.app/Contents/Library/LaunchAgents/ch.lkmc.foldover.agent.plist"
    mkdir -p "$(dirname "$plist")"
    printf '%s\n' '<plist version="1.0"><dict><key>Label</key><string>ch.lkmc.foldover.agent.0123456789abcdef0123456789abcdef01234567</string></dict></plist>' > "$plist"
  fi
  if [[ -e "$state/app-running" ]]; then
    echo "$installed" > "$state/app-running"
  fi
  if [[ -e "$state/helper-running" ]]; then
    echo "${installed}Helper" > "$state/app-running"
  fi
  if [[ -e "$state/interrupted-swap" ]]; then
    mv "$apps/Foldover.app" "$apps/.Foldover.app.outgoing"
  fi
  echo "== $name"
  status=0
  out="$(PATH="$fakes:$PATH" FOLDOVER_APPLICATIONS_DIR="${applications_dir:-$apps}" \
    "$BASH" "$repo/scripts/build.sh" "$@" 2>&1)" || status=$?
  touch "$log"
}

expect_status() { [[ "$status" -eq "$1" ]] || fail "$name: exit $status, expected $1"; }
expect_out() { grep -qF -- "$1" <<<"$out" || fail "$name: output lacks '$1'"; }
expect_call() { grep -qF -- "$1" "$log" || fail "$name: no call '$1'"; }
expect_no_call() { ! grep -qF -- "$1" "$log" || fail "$name: unexpected call '$1'"; }
expect_installed() {
  [[ "$(cat "$installed" 2>/dev/null)" == "$1" ]] || fail "$name: installed app is not the $1 one"
}

agent="gui/$(id -u)/ch.lkmc.foldover.agent"

# launchd cannot start a rebuilt ad-hoc-signed agent under its old
# registration: the reopened app restarts it by registering it anew.
run "replaces a running app and leaves its agent to the app" app-running agent-running -- app --install
expect_status 0
expect_installed new
expect_out "install app: ok ($apps/Foldover.app)"
expect_call "pkill -TERM -U $(id -u) -f"
[[ ! -e "$state/app-running" ]] || fail "$name: the running app was not quit"
expect_call "launchctl print $agent"
expect_no_call "kickstart"
expect_out "Foldover restarts the background agent as it opens"
expect_call "open $apps/Foldover.app"
[[ ! -e "$apps/.Foldover.app.incoming" && ! -e "$apps/.Foldover.app.outgoing" ]] \
  || fail "$name: staging copies left behind"

run "leaves a stopped app and an unloaded agent alone" -- app --install
expect_status 0
expect_installed new
expect_no_call "pkill"
expect_no_call "kickstart"
expect_no_call "open $apps/Foldover.app"

# That run loaded no agent, so its output must not mention one.
if grep -qF "background agent" <<<"$out"; then fail "$name: mentioned an agent that is not loaded"; fi

run "asks to open Foldover for a loaded agent" agent-loaded -- app --install
expect_status 0
expect_installed new
expect_no_call "kickstart"
expect_no_call "open $apps/Foldover.app"
expect_out "install app: ok ($apps/Foldover.app); open Foldover to restart the background agent"

run "asks to open Foldover for a running agent" agent-running -- app --install
expect_status 0
expect_installed new
expect_no_call "pkill"
expect_no_call "kickstart"
expect_no_call "open $apps/Foldover.app"
expect_out "install app: ok ($apps/Foldover.app); open Foldover to restart the background agent"

run "finds the installed build agent before replacing it" hashed-agent agent-running -- app --install
expect_status 0
expect_call "launchctl print gui/$(id -u)/ch.lkmc.foldover.agent.0123456789abcdef0123456789abcdef01234567"
expect_out "open Foldover to restart the background agent"

run "finds the hashed agent after recovering an interrupted swap" hashed-agent interrupted-swap agent-running -- app --install
expect_status 0
expect_call "launchctl print gui/$(id -u)/ch.lkmc.foldover.agent.0123456789abcdef0123456789abcdef01234567"
expect_out "open Foldover to restart the background agent"

run "does not quit another app with a similar name" helper-running -- app --install
expect_status 0
expect_no_call "pkill"

run "builds without installing by default" -- app
expect_status 0
expect_installed old
expect_no_call "launchctl"

run "installs nothing when the build fails" xcodebuild-fails -- app --install
expect_status 1
expect_installed old
expect_out "app: FAILED (xcodebuild)"
if grep -qF "install app" <<<"$out"; then fail "$name: attempted an install"; fi

run "keeps the old app when Foldover does not quit" app-running app-ignores-term -- app --install
expect_status 1
expect_installed old
expect_out "install app: FAILED (Foldover did not quit; quit it and rerun)"
[[ ! -e "$apps/.Foldover.app.incoming" ]] || fail "$name: incoming copy left behind"

run "keeps the old app when the copy fails" ditto-fails -- app --install
expect_status 1
expect_installed old
expect_out "install app: FAILED (cannot write $apps"

run "restores the old app when the swap fails" mv-into-fails -- app --install
expect_status 1
expect_installed old
expect_out "install app: FAILED (cannot move the new app into place)"
[[ ! -e "$apps/.Foldover.app.incoming" && ! -e "$apps/.Foldover.app.outgoing" ]] \
  || fail "$name: staging copies left behind"

run "recovers the app an interrupted swap left aside" interrupted-swap ditto-fails -- app --install
expect_status 1
expect_installed old

run "reports an app that does not reopen" app-running open-fails -- app --install
expect_status 0
expect_installed new
expect_out "install app: ok ($apps/Foldover.app), but Foldover did not reopen"

run "asks to open an app that does not reopen for its agent" app-running agent-running open-fails -- app --install
expect_status 0
expect_installed new
expect_out "install app: ok ($apps/Foldover.app), but Foldover did not reopen; open Foldover to restart the background agent"

applications_dir="$apps/" run "quits the app under a trailing-slash directory" app-running -- app --install
expect_status 0
expect_installed new
[[ ! -e "$state/app-running" ]] || fail "$name: the running app was not quit"
expect_out "install app: ok ($apps/Foldover.app)"

run "skips core, which the app embeds" -- core --install
expect_status 0
expect_out "install core: skipped (Foldover.app embeds its own agent and CLI)"

run "only plans an install under --check" -- app --install --check
expect_status 0
expect_installed old
expect_out "-- would install: $apps/Foldover.app"
expect_no_call "xcodebuild"

if [[ $failures -ne 0 ]]; then
  echo "test-build-install: $failures failure(s)" >&2
  exit 1
fi
echo "test-build-install: ok"
