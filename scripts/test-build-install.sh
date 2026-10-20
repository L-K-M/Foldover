#!/usr/bin/env bash
# Check scripts/build.sh --install against a fake macOS toolchain: the
# app replaces the installed copy, a running app is quit and reopened,
# a loaded agent is restarted, and failures leave the installed app
# alone. Runs on Linux and macOS; touches nothing outside a temp dir.
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
#   app-running   pgrep finds the installed app until pkill removes this
#   agent-loaded  launchctl print succeeds
#   kickstart-fails, xcodebuild-fails, app-ignores-term
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
fake ditto 'cp -R "$1" "$2"'
fake pgrep 'echo "pgrep $*" >> "$log"; [[ -e "$state/app-running" ]]'
fake pkill '
echo "pkill $*" >> "$log"
[[ -e "$state/app-ignores-term" ]] || rm -f "$state/app-running"'
fake sleep ':'
fake launchctl '
echo "launchctl $*" >> "$log"
case "$1" in
  print) [[ -e "$state/agent-loaded" ]] ;;
  kickstart) [[ ! -e "$state/kickstart-fails" ]] ;;
esac'
fake open 'echo "open $*" >> "$log"'

apps="$work/Applications"
installed="$apps/Foldover.app/Contents/MacOS/Foldover"

# run NAME [STATE...] -- ARGS: reset the fakes, preinstall an old app,
# run build.sh, and keep its output in $out and exit status in $status.
run() {
  name="$1"; shift
  rm -rf "$state" "$apps" "$repo/dist" "$repo/build" "$repo/target" "$log"
  mkdir -p "$state" "$(dirname "$installed")"
  echo old > "$installed"
  while [[ "$1" != "--" ]]; do touch "$state/$1"; shift; done
  shift
  echo "== $name"
  status=0
  out="$(PATH="$fakes:$PATH" FOLDOVER_APPLICATIONS_DIR="$apps" \
    bash "$repo/scripts/build.sh" "$@" 2>&1)" || status=$?
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

run "replaces a running app and restarts its agent" app-running agent-loaded -- app --install
expect_status 0
expect_installed new
expect_out "install app: ok ($apps/Foldover.app)"
expect_call "pkill -TERM -f ^$apps/Foldover.app/Contents/MacOS/Foldover"
expect_call "launchctl kickstart -k $agent"
expect_call "open $apps/Foldover.app"
[[ ! -e "$apps/.Foldover.app.incoming" && ! -e "$apps/.Foldover.app.outgoing" ]] \
  || fail "$name: staging copies left behind"

run "leaves a stopped app and an unloaded agent alone" -- app --install
expect_status 0
expect_installed new
expect_no_call "pkill"
expect_no_call "kickstart"
expect_no_call "open $apps/Foldover.app"

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

run "reports an agent that does not restart" agent-loaded kickstart-fails -- app --install
expect_status 1
expect_installed new
expect_out "the agent did not restart"

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
