#!/usr/bin/env bash
#
# Build every Foldover artifact this host can build, and print one clear
# summary. The single local entry point; CI builds the same artifacts in
# .github/workflows/ci.yml.
#
#   core - the Rust binaries foldoverd and foldover (needs cargo)
#   app  - the macOS Foldover.app from apple/ (needs macOS, cargo,
#          Xcode and XcodeGen)
#
# Usage:
#   scripts/build.sh                # every target this host can build
#   scripts/build.sh core           # just this one (naming a target turns an
#                                   # infeasible target into an error, not a skip)
#   scripts/build.sh all            # both targets, all infeasible ones fail
#   scripts/build.sh --debug        # debug/Debug builds instead of release
#   scripts/build.sh --clean        # wipe build output first
#   scripts/build.sh --check        # print resolved config; build nothing
#   scripts/build.sh --install      # also install what this run built
#
# --install replaces /Applications/Foldover.app with the app this run
# built. A running Foldover is quit first and reopened afterwards, and a
# loaded background agent is restarted: both pin the installed code at
# launch and refuse the changed bundle until they restart (ADR 0006).
# FOLDOVER_APPLICATIONS_DIR installs elsewhere, e.g. ~/Applications.
# core installs nothing: the app embeds its own agent and CLI, and Linux
# has no installer yet.
#
# Every produced artifact is copied into dist/ at the repo root (one
# folder with the final outputs), and on macOS dist/ is opened in the
# Finder when anything landed there.
#
# App builds target macOS 27 on Apple silicon and sign ad hoc
# (CODE_SIGN_IDENTITY=-). Downloaded builds need the quarantine-removal
# command in README.md; Developer ID and notarization are not used.
#
# Exit status is non-zero if any requested target fails to build. Targets
# that cannot build on this host are reported as skipped, and only fail
# the run when named explicitly.
set -uo pipefail

# Absolute path to this script: usage() must find it after the cd
# below, even when invoked via a relative path from a subdirectory.
SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1

PROFILE="release"
CLEAN=false
CHECK=false
INSTALL=false
declare -a REQUESTED=()
EXPLICIT=0

usage() {
  # Print the leading comment block (the file header), minus the shebang.
  awk 'NR==1 && /^#!/ {next} /^#/ {sub(/^# ?/,""); print; next} {exit}' "$SELF"
  exit "${1:-0}"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help) usage 0 ;;
    --debug) PROFILE="debug"; shift ;;
    --clean) CLEAN=true; shift ;;
    --check) CHECK=true; shift ;;
    --install) INSTALL=true; shift ;;
    core|app) REQUESTED+=("$1"); EXPLICIT=1; shift ;;
    all) REQUESTED=(core app); EXPLICIT=1; shift ;;
    *) echo "unknown argument: $1" >&2; usage 1 ;;
  esac
done

if [[ ${#REQUESTED[@]} -eq 0 ]]; then
  REQUESTED=(core app)
fi

case "$(uname -s)" in
  Darwin) HOST="macos" ;;
  Linux) HOST="linux" ;;
  MINGW*|MSYS*|CYGWIN*) HOST="windows" ;;
  *) HOST="unknown" ;;
esac

# The committed workspace version, from the same field
# scripts/sync-cargo-version.sh rewrites on a release.
VERSION=""
if [[ -f Cargo.toml ]]; then
  VERSION="$(sed -nE '/^\[workspace\.package\]/,/^\[/ s/^[[:space:]]*version[[:space:]]*=[[:space:]]*"([^"]*)".*/\1/p' Cargo.toml | head -n 1)"
fi
[[ -n "$VERSION" ]] || VERSION="unknown"

echo "Foldover build"
echo "  host:    $HOST ($(uname -s))"
echo "  profile: $PROFILE"
echo "  version: $VERSION"
echo "  targets: ${REQUESTED[*]}"
echo

have() { command -v "$1" >/dev/null 2>&1; }

declare -a RESULTS=()
record() { RESULTS+=("$1"); }

DIST="$ROOT/dist"
STAGED=0
# Targets this run built, in order; --install acts on these only.
declare -a BUILT=()
stage() {
  local src="$1"
  local name="${2:-$(basename "$1")}"
  mkdir -p "$DIST" || return 1
  rm -rf "${DIST:?}/$name" || return 1
  cp -R "$src" "$DIST/$name" || return 1
  STAGED=$((STAGED + 1))
  echo "-- staged dist/$name"
}

was_named() {
  [[ $EXPLICIT -eq 1 ]] || return 1
  local t
  for t in "${REQUESTED[@]}"; do [[ "$t" == "$1" ]] && return 0; done
  return 1
}

# Skip a target: a soft skip on a default run, a hard error when named.
skip_or_fail() {
  local target="$1" reason="$2"
  if was_named "$target"; then
    echo "!! $target: cannot build on this host - $reason" >&2
    record "$target: FAILED ($reason)"
    return 1
  fi
  echo ".. $target: skipped - $reason"
  record "$target: skipped ($reason)"
  return 0
}

# ---------------------------------------------------------------------------
build_core() {
  echo "==> core (Rust workspace)"

  local -a args=(-p foldoverd -p foldover-cli)
  local profile_dir="debug"
  if [[ "$PROFILE" == "release" ]]; then
    args+=(--release)
    profile_dir="release"
  fi

  # --check answers "what would this run do", so it reports the plan
  # even on a host that could not carry it out.
  if $CHECK; then
    echo "-- would run: cargo build ${args[*]}"
    echo "-- would stage: dist/foldoverd, dist/foldover"
    record "core: checked (would build $PROFILE)"
    return 0
  fi

  if ! have cargo; then skip_or_fail core "cargo not found (see https://rustup.rs)"; return; fi
  if [[ ! -f Cargo.toml ]]; then
    skip_or_fail core "no Cargo.toml at the repo root"; return
  fi

  if $CLEAN; then
    echo "-- cargo clean -p foldoverd -p foldover-cli"
    cargo clean -p foldoverd -p foldover-cli \
      || { record "core: FAILED (clean)"; return 1; }
  fi

  echo "-- cargo build ${args[*]}"
  if ! cargo build "${args[@]}"; then
    echo "!! core: cargo build failed" >&2
    record "core: FAILED (cargo)"; return 1
  fi

  local bin
  for bin in foldoverd foldover; do
    local src="target/$profile_dir/$bin"
    if [[ ! -f "$src" ]]; then
      echo "!! core: expected binary $src missing after build" >&2
      record "core: FAILED (artifact $bin not found)"
      return 1
    fi
    if ! stage "$src" "$bin"; then
      echo "!! core: failed to stage $src" >&2
      record "core: FAILED (staging)"; return 1
    fi
  done
  BUILT+=(core)
  record "core: built ($PROFILE) -> dist/foldoverd, dist/foldover"
}

# ---------------------------------------------------------------------------
build_app() {
  echo "==> app (macOS)"

  local config="Release"
  if [[ "$PROFILE" == "debug" ]]; then
    config="Debug"
  fi
  # Under build/, which .gitignore covers, and shared by every app build
  # on this host so SwiftPM and xcodebuild reuse their caches.
  local derived="$ROOT/build/DerivedData"

  if $CHECK; then
    echo "-- would run: cargo xtask ffi"
    echo "-- would run: xcodegen generate --spec apple/project.yml"
    echo "-- would run: xcodebuild -project apple/Foldover.xcodeproj -scheme Foldover -configuration $config -derivedDataPath $derived CODE_SIGN_IDENTITY=- build"
    echo "-- would stage: dist/Foldover.app"
    $INSTALL && echo "-- would install: $APPLICATIONS_DIR/Foldover.app"
    record "app: checked (would build $config)"
    return 0
  fi

  if [[ "$HOST" != "macos" ]]; then
    skip_or_fail app "macOS is required (Xcode, SwiftUI, the UniFFI XCFramework)"; return
  fi
  if ! have cargo; then skip_or_fail app "cargo not found (cargo xtask ffi builds the core)"; return; fi
  if ! have xcodebuild; then skip_or_fail app "Xcode (xcodebuild) not found"; return; fi
  if ! have xcodegen; then
    skip_or_fail app "XcodeGen not found - install it with: brew install xcodegen"; return
  fi

  if $CLEAN; then
    echo "-- removing $derived and apple/Foldover.xcodeproj"
    rm -rf "$derived" apple/Foldover.xcodeproj
  fi

  # The UniFFI XCFramework and Swift bindings must exist before
  # xcodebuild resolves the FoldoverCore package.
  echo "-- cargo xtask ffi"
  if ! cargo xtask ffi; then
    echo "!! app: cargo xtask ffi failed" >&2
    record "app: FAILED (xtask ffi)"; return 1
  fi

  # The .xcodeproj is generated and gitignored; regenerate it here so a
  # fresh clone builds without a manual step.
  echo "-- xcodegen generate --spec apple/project.yml"
  if ! xcodegen generate --spec apple/project.yml; then
    echo "!! app: xcodegen failed" >&2
    record "app: FAILED (xcodegen)"; return 1
  fi

  echo "-- xcodebuild ($config)"
  if ! xcodebuild -project apple/Foldover.xcodeproj \
       -scheme Foldover -configuration "$config" \
       -derivedDataPath "$derived" \
       CODE_SIGN_IDENTITY=- build; then
    echo "!! app: xcodebuild failed" >&2
    record "app: FAILED (xcodebuild)"; return 1
  fi

  local built="$derived/Build/Products/$config/Foldover.app"
  if [[ ! -d "$built" ]]; then
    echo "!! app: built Foldover.app not found under $derived" >&2
    record "app: FAILED (product not found)"; return 1
  fi

  if ! stage "$built" "Foldover.app"; then
    echo "!! app: failed to stage $built as dist/Foldover.app" >&2
    record "app: FAILED (staging)"; return 1
  fi
  BUILT+=(app)
  record "app: built ($config) -> dist/Foldover.app"
}

# ---------------------------------------------------------------------------
APPLICATIONS_DIR="${FOLDOVER_APPLICATIONS_DIR:-/Applications}"
AGENT_LABEL="ch.lkmc.foldover.agent"
APP_QUIT_SECONDS=10

# Wait for the processes matching $1 to exit; non-zero on timeout.
wait_for_exit() {
  local pattern="$1" waited=0
  while pgrep -f "$pattern" >/dev/null; do
    if [[ $waited -ge $APP_QUIT_SECONDS ]]; then return 1; fi
    sleep 1
    waited=$((waited + 1))
  done
}

# Replace the installed app with dist/Foldover.app. The copy lands next
# to the destination first, so the swap is two renames on one volume and
# a failed copy leaves the installed app untouched.
install_app() {
  local dest="$APPLICATIONS_DIR/Foldover.app"
  local incoming="$APPLICATIONS_DIR/.Foldover.app.incoming"
  local outgoing="$APPLICATIONS_DIR/.Foldover.app.outgoing"
  local running="^$dest/Contents/MacOS/Foldover"
  local agent
  agent="gui/$(id -u)/$AGENT_LABEL"
  echo "==> install app"

  echo "-- copying dist/Foldover.app next to $dest"
  if ! rm -rf "$incoming" "$outgoing" || ! ditto "$DIST/Foldover.app" "$incoming"; then
    rm -rf "$incoming"
    echo "!! install app: cannot write $APPLICATIONS_DIR" >&2
    record "install app: FAILED (cannot write $APPLICATIONS_DIR; FOLDOVER_APPLICATIONS_DIR=~/Applications installs there)"
    return 1
  fi

  local reopen=false
  if pgrep -f "$running" >/dev/null; then
    echo "-- quitting Foldover"
    reopen=true
    pkill -TERM -f "$running"
    if ! wait_for_exit "$running"; then
      rm -rf "$incoming"
      echo "!! install app: Foldover did not quit within ${APP_QUIT_SECONDS}s" >&2
      record "install app: FAILED (Foldover did not quit; quit it and rerun)"
      return 1
    fi
  fi

  if [[ -e "$dest" ]] && ! mv "$dest" "$outgoing"; then
    rm -rf "$incoming"
    record "install app: FAILED (cannot move the installed app aside)"
    return 1
  fi
  if ! mv "$incoming" "$dest"; then
    [[ -e "$outgoing" ]] && mv "$outgoing" "$dest"
    record "install app: FAILED (cannot move the new app into place)"
    return 1
  fi
  rm -rf "$outgoing"
  echo "-- installed $dest"

  # Only an agent the user enabled is restarted; enabling stays a choice
  # made in the app.
  if launchctl print "$agent" >/dev/null 2>&1; then
    echo "-- restarting the background agent"
    if ! launchctl kickstart -k "$agent"; then
      echo "!! install app: the background agent did not restart" >&2
      record "install app: FAILED (installed $dest, but the agent did not restart; choose Enable Background Agent in Foldover)"
      return 1
    fi
  fi

  if $reopen; then
    echo "-- reopening Foldover"
    open "$dest"
  fi
  record "install app: ok ($dest)"
}

install_core() {
  local reason="Foldover.app embeds its own agent and CLI"
  [[ "$HOST" == "macos" ]] || reason="no $HOST installer yet; run dist/foldoverd and dist/foldover"
  echo ".. install core: skipped - $reason"
  record "install core: skipped ($reason)"
}

# ---------------------------------------------------------------------------
FAILED=0
for target in "${REQUESTED[@]}"; do
  case "$target" in
    core) build_core || FAILED=1 ;;
    app) build_app || FAILED=1 ;;
  esac
  echo
done

if $INSTALL; then
  for target in ${BUILT[@]+"${BUILT[@]}"}; do
    case "$target" in
      core) install_core ;;
      app) install_app || FAILED=1 ;;
    esac
    echo
  done
fi

echo "Summary"
for line in "${RESULTS[@]}"; do echo "  $line"; done

if [[ "$STAGED" -gt 0 ]]; then
  echo "  artifacts: $DIST"
  [[ "$(uname -s)" == "Darwin" ]] && open "$DIST"
fi

exit "$FAILED"
