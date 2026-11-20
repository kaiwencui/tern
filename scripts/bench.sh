#!/bin/zsh
# Compares Tern with the other terminals on this Mac: how fast each takes
# output, how long each takes to launch, and how much memory each holds.
#
# Usage: scripts/bench.sh [rounds]      (default 3; needs build/Tern.app)
#
# It measures Tern and every one of these that is installed: Ghostty,
# Alacritty, kitty, WezTerm, iTerm2, Rio and Apple's Terminal. To choose, set
# TERN_BENCH_TERMINALS, for example TERN_BENCH_TERMINALS="ghostty alacritty".
# An app that is not in /Applications is found through GHOSTTY_APP,
# ALACRITTY_APP, KITTY_APP, WEZTERM_APP, ITERM_APP, RIO_APP or TERMINAL_APP.
#
# All terminals are opened the same way (open -n), with the same font, the
# same 120x40 grid, 10,000 lines of history where the terminal counts history
# in lines, and the same command: termbench, which writes each payload to the
# terminal and waits for the terminal to answer a cursor position request. It
# waits a second and a half first, so no terminal is timed while it is still
# starting up. Every round runs each terminal once, and each round starts with
# a different one. Each launch takes the keyboard focus for a few seconds, so
# leave the machine alone while it runs, and keep its screen unlocked: several
# terminals open no window behind the lock screen.
#
# Memory is read twice each time: at once, and again after the terminal has
# been left alone for five seconds (TERN_BENCH_REST to change that), which is
# the state a terminal spends most of its life in.
#
# With TERN_BENCH_KEYS=N it also times N key presses in each terminal, from
# the key going down to the frame that shows the letter (see run_keys below
# for the permission that needs).
#
# termbench also records what each terminal's own processes used: processor
# time and energy for the whole of the output, and per second while the
# terminal is left alone with nothing to do but show its cursor, and again
# with the cursor hidden (some terminals blink theirs out of the box and
# some do not).
#
# Ghostty, iTerm2 and Terminal are run from copies with their own bundle
# identifiers, so the ones you have open, their saved windows and their
# settings are not touched. The others are given a settings file of their own.
#
# Everything the terminals read lives in a temporary folder, not in the
# project: macOS asks for permission before an app may read Desktop, Documents
# or Downloads, and a terminal waiting on that question never starts.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"
ROUNDS="${1:-3}"
FONT="${TERN_BENCH_FONT:-Menlo}"
# The font's PostScript name, which iTerm2 and Terminal ask for.
FONT_PS="${TERN_BENCH_FONT_PS:-$FONT-Regular}"
TERN_SOURCE="$ROOT/build/Tern.app"
TMP="${TMPDIR:-/tmp}"
WORK="${TERN_BENCH_DIR:-${TMP%/}/tern-bench}"
OUT="$WORK/results"
TERN_APP="$WORK/Tern.app"

typeset -A APP BIN
APP[ghostty]="${GHOSTTY_APP:-/Applications/Ghostty.app}"
APP[alacritty]="${ALACRITTY_APP:-/Applications/Alacritty.app}"
APP[kitty]="${KITTY_APP:-/Applications/kitty.app}"
APP[wezterm]="${WEZTERM_APP:-/Applications/WezTerm.app}"
APP[iterm]="${ITERM_APP:-/Applications/iTerm.app}"
APP[rio]="${RIO_APP:-/Applications/rio.app}"
APP[terminal]="${TERMINAL_APP:-/System/Applications/Utilities/Terminal.app}"
KNOWN=(ghostty alacritty kitty wezterm iterm rio terminal)

[[ -d "$TERN_SOURCE" ]] || { echo "build/Tern.app is missing; run scripts/build-app.sh first" >&2; exit 1 }

OTHERS=()
if [[ -n "${TERN_BENCH_TERMINALS:-}" ]]; then
    for name in ${=TERN_BENCH_TERMINALS}; do
        [[ "$name" == tern ]] && continue
        (( ${KNOWN[(Ie)$name]} )) || { echo "unknown terminal '$name'; the choices are: $KNOWN" >&2; exit 1 }
        [[ -d "${APP[$name]}" ]] || { echo "${APP[$name]} is missing" >&2; exit 1 }
        OTHERS+=("$name")
    done
else
    for name in $KNOWN; do
        [[ -d "${APP[$name]}" ]] && OTHERS+=("$name")
    done
fi
(( ${#OTHERS} )) || { echo "no other terminal is installed to compare with" >&2; exit 1 }
TERMINALS=("${OTHERS[@]}" tern)

cargo build --release -p tern-bench
mkdir -p "$WORK" "$OUT"
cp "$ROOT/target/release/termbench" "$WORK/termbench"
BENCH="$WORK/termbench"
# The bundle holds a fixed set of files, so copying over an older copy is enough.
mkdir -p "$TERN_APP"
cp -R "$TERN_SOURCE/" "$TERN_APP/"
[[ -f "$WORK/payloads/long.txt" ]] || "$BENCH" gen "$WORK/payloads" --mb 30
PAYLOADS=("$WORK"/payloads/{ascii,dense,unicode,sgr,long}.txt)

# A private copy of an app under its own bundle identifier, so the real one,
# its settings and its saved windows are left alone.
#   private_copy <name> <copy's file name> <identifier>
private_copy() {
    local copy="$WORK/$2"
    if [[ ! -d "$copy" ]]; then
        cp -R "${APP[$1]}" "$copy"
        /usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $3" "$copy/Contents/Info.plist"
        xattr -cr "$copy"
        codesign --force --deep --sign - "$copy" >/dev/null 2>&1
        defaults write "$3" SUEnableAutomaticChecks -bool false
        defaults write "$3" SUHasLaunchedBefore -bool true
    fi
    APP[$1]="$copy"
}

ITERM_ID=com.kaicui.tern.bench.iterm
TERMINAL_ID=com.kaicui.tern.bench.terminal
APP[tern]="$TERN_APP"
BIN[tern]="$TERN_APP/Contents/MacOS/tern"
for name in $OTHERS; do
    case "$name" in
        ghostty) private_copy ghostty GhosttyBench.app com.kaicui.tern.bench.ghostty; BIN[ghostty]="${APP[ghostty]}/Contents/MacOS/ghostty" ;;
        iterm) private_copy iterm ITermBench.app "$ITERM_ID"; BIN[iterm]="${APP[iterm]}/Contents/MacOS/iTerm2" ;;
        terminal) private_copy terminal TerminalBench.app "$TERMINAL_ID"; BIN[terminal]="${APP[terminal]}/Contents/MacOS/Terminal" ;;
        alacritty) BIN[alacritty]="${APP[alacritty]}/Contents/MacOS/alacritty" ;;
        kitty) BIN[kitty]="${APP[kitty]}/Contents/MacOS/kitty" ;;
        wezterm) BIN[wezterm]="${APP[wezterm]}/Contents/MacOS/wezterm-gui" ;;
        rio) BIN[rio]="${APP[rio]}/Contents/MacOS/rio" ;;
    esac
done

# Terminal keeps its font as an archived object; make that once.
if (( ${OTHERS[(Ie)terminal]} )) && [[ ! -f "$WORK/terminal-font.b64" ]]; then
    print -r -- 'import AppKit
let font = NSFont(name: CommandLine.arguments[1], size: 13) ?? NSFont.userFixedPitchFont(ofSize: 13)!
let data = try! NSKeyedArchiver.archivedData(withRootObject: font, requiringSecureCoding: true)
print(data.base64EncodedString())' > "$WORK/terminal-font.swift"
    swift "$WORK/terminal-font.swift" "$FONT_PS" > "$WORK/terminal-font.b64"
fi

# Seconds a terminal is left alone before its memory is read a second time.
REST="${TERN_BENCH_REST:-5}"

# Adds a process's memory to a result file: its physical footprint, which is
# what Activity Monitor shows.
#   footprint <pid> <label> <result file> <when>
footprint() {
    local mb
    mb="$(vmmap --summary "$1" 2>/dev/null | awk '/Physical footprint:/ { v=$3; if (v ~ /G$/) { sub(/G/,"",v); v*=1024 } else if (v ~ /K$/) { sub(/K/,"",v); v/=1024 } else { sub(/M/,"",v) } printf "%.1f", v; exit }')"
    [[ -n "$mb" ]] && echo "{\"label\":\"$2\",\"event\":\"memory\",\"when\":\"$4\",\"footprint_mb\":$mb}" >> "$3"
    return 0
}

# Follows one launch to its end: records when the window reached the screen,
# waits for termbench to finish, records the terminal's memory at once and
# again after a rest, then waits for the terminal to exit by itself (most quit
# when termbench ends).
#   follow <label> <result file> <terminal binary> <launch time> <when, for the memory line>
follow() {
    local label="$1" file="$2" binary="$3" t0="$4" when="$5" pid="" waited=0
    while [[ -z "$pid" ]]; do
        pid="$(pgrep -f "$binary" | head -1 || true)"
        sleep 0.01
        waited=$((waited + 1))
        if (( waited > 1000 )); then echo "$label: did not start" >&2; return 1; fi
    done
    "$BENCH" window "$pid" "$t0" "$label" "$file" || true
    waited=0
    while ! grep -q '"event":"done"' "$file" 2>/dev/null; do
        sleep 0.2
        waited=$((waited + 1))
        if (( waited > 600 )); then
            echo "$label: no result after 2 minutes; is the screen locked, or a permission question or another dialog waiting on it?" >&2
            pkill -f "$binary" 2>/dev/null || true
            return 1
        fi
    done
    # An idle launch settles for a moment first, so start-up work is not counted.
    [[ "$when" == "after launch" ]] && sleep 1.5
    footprint "$pid" "$label" "$file" "$when"
    # And once more after the terminal has been left alone for a while, which
    # is the state a terminal spends most of its life in.
    sleep "$REST"
    footprint "$pid" "$label" "$file" "at rest $when"
    waited=0
    while pgrep -f "$binary" >/dev/null; do
        sleep 0.2
        waited=$((waited + 1))
        if (( waited > 5 * (REST + 6) )); then pkill -f "$binary" 2>/dev/null || true; sleep 0.5; break; fi
    done
}

# Each start_<name> opens that terminal running the command given as its
# arguments, at 120x40 in the bench font.

start_tern() {
    local conf="$WORK/tern.conf"
    print -r -- "font-family = $FONT
font-size = 13
window-width = 120
window-height = 40
confirm-close = false
quit-after-last-window-closed = true" > "$conf"
    open -n "${APP[tern]}" --args --config-file "$conf" -e "$@"
}

start_ghostty() {
    local conf="$WORK/ghostty.conf"
    print -r -- "font-family = $FONT
font-size = 13
window-width = 120
window-height = 40
window-save-state = never
auto-update = off
confirm-close-surface = false
quit-after-last-window-closed = true
shell-integration = none
initial-command = direct:$*" > "$conf"
    open -n "${APP[ghostty]}" --args --config-default-files=false "--config-file=$conf"
}

start_alacritty() {
    local conf="$WORK/alacritty.toml"
    print -r -- "[font]
size = 13
normal = { family = \"$FONT\" }

[window]
dimensions = { columns = 120, lines = 40 }

[scrolling]
history = 10000" > "$conf"
    open -n "${APP[alacritty]}" --args --config-file "$conf" -e "$@"
}

start_kitty() {
    local conf="$WORK/kitty.conf"
    print -r -- "font_family $FONT
font_size 13
remember_window_size no
initial_window_width 120c
initial_window_height 40c
scrollback_lines 10000
confirm_os_window_close 0
update_check_interval 0
shell_integration disabled
macos_quit_when_last_window_closed yes" > "$conf"
    open -n "${APP[kitty]}" --args --config "$conf" "$@"
}

start_wezterm() {
    local conf="$WORK/wezterm.lua"
    print -r -- "local wezterm = require 'wezterm'
return {
  font = wezterm.font('$FONT'),
  font_size = 13,
  initial_cols = 120,
  initial_rows = 40,
  scrollback_lines = 10000,
  check_for_updates = false,
  window_close_confirmation = 'NeverPrompt',
  exit_behavior = 'Close',
}" > "$conf"
    open -n "${APP[wezterm]}" --args --config-file "$conf" start --always-new-process -- "$@"
}

# Rio sizes its window in points, not cells; these make 120x40 in Menlo 13.
RIO_WIDTH="${TERN_BENCH_RIO_WIDTH:-968}"
RIO_HEIGHT="${TERN_BENCH_RIO_HEIGHT:-652}"
start_rio() {
    mkdir -p "$WORK/rio"
    print -r -- "confirm-before-quit = false

[fonts]
size = 13
family = \"$FONT\"

[window]
width = $RIO_WIDTH
height = $RIO_HEIGHT

[scroll]
history = 10000" > "$WORK/rio/config.toml"
    open -n "${APP[rio]}" --env "RIO_CONFIG_HOME=$WORK/rio" --args -e "$@"
}

# iTerm2 and Terminal take their command from their settings, so each launch
# writes the settings of the private copy first.
start_iterm() {
    local plist="$WORK/iterm.plist"
    print -r -- "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\"><dict>
<key>Default Bookmark Guid</key><string>tern-bench</string>
<key>New Bookmarks</key><array><dict>
  <key>Guid</key><string>tern-bench</string>
  <key>Name</key><string>bench</string>
  <key>Custom Command</key><string>Yes</string>
  <key>Command</key><string>$*</string>
  <key>Columns</key><integer>120</integer>
  <key>Rows</key><integer>40</integer>
  <key>Normal Font</key><string>$FONT_PS 13</string>
  <key>Scrollback Lines</key><integer>10000</integer>
  <key>Terminal Type</key><string>xterm-256color</string>
  <key>Close Sessions On End</key><true/>
  <key>Prompt Before Closing 2</key><integer>0</integer>
</dict></array>
<key>QuitWhenAllWindowsClosed</key><true/>
<key>PromptOnQuit</key><false/>
<key>OnlyWhenMoreTabs</key><false/>
<key>NoSyncTipsDisabled</key><true/>
<key>NoSyncNeverRemindPrefsChangesLostForFile</key><true/>
<key>OpenArrangementAtStartup</key><false/>
<key>OpenNoWindowsAtStartup</key><false/>
<key>moveToApplicationsFolderAlertSuppress</key><true/>
<key>SUEnableAutomaticChecks</key><false/>
<key>SUHasLaunchedBefore</key><true/>
</dict></plist>" > "$plist"
    defaults import "$ITERM_ID" "$plist"
    open -n "${APP[iterm]}"
}

start_terminal() {
    local plist="$WORK/terminal.plist"
    print -r -- "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\"><dict>
<key>Default Window Settings</key><string>Bench</string>
<key>Startup Window Settings</key><string>Bench</string>
<key>NSQuitAlwaysKeepsWindows</key><false/>
<key>Window Settings</key><dict><key>Bench</key><dict>
  <key>name</key><string>Bench</string>
  <key>type</key><string>Window Settings</string>
  <key>ProfileCurrentVersion</key><real>2.07</real>
  <key>CommandString</key><string>$*</string>
  <key>RunCommandAsShell</key><false/>
  <key>columnCount</key><integer>120</integer>
  <key>rowCount</key><integer>40</integer>
  <key>Font</key><data>$(<"$WORK/terminal-font.b64")</data>
  <key>ShouldLimitScrollback</key><integer>1</integer>
  <key>ScrollbackLines</key><integer>10000</integer>
  <key>shellExitAction</key><integer>0</integer>
  <key>warnOnShellCloseAction</key><integer>0</integer>
</dict></dict>
</dict></plist>" > "$plist"
    defaults import "$TERMINAL_ID" "$plist"
    open -n "${APP[terminal]}"
}

#   run <terminal> <round> <idle|flood>
# With no payloads termbench only reports that it started and stays alive for
# a few seconds.
run() {
    local name="$1" file="$OUT/$1-$3-$2.jsonl" when="after launch" t0
    local -a files
    [[ "$3" == flood ]] && { files=("${PAYLOADS[@]}"); when="after the flood" }
    rm -f "$file"
    t0="$("$BENCH" now)"
    # An idle launch stays longer: what the terminal uses at rest is measured
    # twice, around the second look at its memory.
    local hold=$(( (REST + 7) * 1000 ))
    [[ "$3" == idle ]] && hold=$(( (2 * REST + 8) * 1000 ))
    # What termbench is told goes into a file, a line each: iTerm2 and Terminal
    # take their command as one string, and Terminal starts nothing when the
    # string is longer than about a thousand characters.
    print -rl -- inside --out "$file" --label "$name" --t0 "$t0" --iters 1 --settle-ms 1500 --hold-ms "$hold" --usage-of "${APP[$name]}" --rest-ms $(( (REST - 2) * 1000 )) "${files[@]}" > "$WORK/args-$name"
    "start_$name" "$BENCH" "@$WORK/args-$name"
    follow "$name" "$file" "${BIN[$name]}" "$t0" "$when" || true
}

# Runs vtebench, the benchmark Alacritty's authors wrote, in one terminal and
# leaves its samples in a file. It is a second opinion from a tool that was
# not written for Tern.
#   run_vtebench <terminal>
VTEBENCH="${TERN_BENCH_VTEBENCH:-}"
VTE_SECS="${TERN_BENCH_VTEBENCH_SECS:-5}"
run_vtebench() {
    local name="$1" dat="$OUT/vtebench-$1.dat" script="$WORK/vtebench-$1.sh" waited=0
    rm -f "$dat" "$dat.done"
    print -r -- "#!/bin/sh
sleep 1.5
cd '$VTEBENCH' && ./target/release/vtebench -s -b benchmarks --max-secs $VTE_SECS --dat '$dat'
stty size > '$dat.done'" > "$script"
    chmod +x "$script"
    "start_$name" /bin/sh "$script"
    while [[ ! -f "$dat.done" ]]; do
        sleep 0.5
        waited=$((waited + 1))
        if (( waited > 600 )); then echo "$name: vtebench did not finish in 5 minutes" >&2; break; fi
    done
    waited=0
    while pgrep -f "${BIN[$name]}" >/dev/null; do
        sleep 0.2
        waited=$((waited + 1))
        if (( waited > 15 )); then pkill -f "${BIN[$name]}" 2>/dev/null || true; sleep 0.5; break; fi
    done
}

# Times key presses in one terminal: how long from a key going down to the
# frame that shows the letter (scripts/keylat.swift). The terminal runs cat
# with its cursor hidden, so that a key press changes the screen and nothing
# else does. Watching the screen and pressing keys need permission, which
# macOS gives to an app: build/KeyLatency.app, which runs the tool. It has to
# be given Screen Recording and Accessibility once, under Privacy & Security
# in System Settings; it keeps them for as long as the app is not built anew,
# which is why it is kept beside Tern.app and not with the temporary files.
#   run_keys <terminal>
KEYS="${TERN_BENCH_KEYS:-0}"
KEYLAT_APP="$ROOT/build/KeyLatency.app"
KEYLAT="$WORK/keylat"
build_keylat() {
    # The app is built once and then left alone: permission is given to the
    # program as it was built.
    if [[ ! -x "$KEYLAT_APP/Contents/MacOS/KeyLatency" ]]; then
        mkdir -p "$KEYLAT_APP/Contents/MacOS"
        cc -O -o "$KEYLAT_APP/Contents/MacOS/KeyLatency" "$ROOT/scripts/keylat-launcher.c" -framework ApplicationServices
        print -r -- '<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.kaicui.tern.bench.keylatency</string>
<key>CFBundleName</key><string>KeyLatency</string>
<key>CFBundleExecutable</key><string>KeyLatency</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>1</string>
<key>LSBackgroundOnly</key><true/>
<key>NSScreenCaptureUsageDescription</key><string>Times how long a key press takes to reach the screen in a terminal.</string>
</dict></plist>' > "$KEYLAT_APP/Contents/Info.plist"
        codesign --force --sign - "$KEYLAT_APP" >/dev/null 2>&1
    fi
    if [[ ! -x "$KEYLAT" || "$ROOT/scripts/keylat.swift" -nt "$KEYLAT" ]]; then
        swiftc -O -o "$KEYLAT" "$ROOT/scripts/keylat.swift"
    fi
}
run_keys() {
    local name="$1" file="$OUT/keys-$1.jsonl" script="$WORK/keys.sh" pid="" waited=0
    rm -f "$file"
    print -r -- "#!/bin/sh
printf '\\033[?25l'
exec cat > /dev/null" > "$script"
    chmod +x "$script"
    "start_$name" /bin/sh "$script"
    while [[ -z "$pid" ]]; do
        pid="$(pgrep -f "${BIN[$name]}" | head -1 || true)"
        sleep 0.05
        waited=$((waited + 1))
        if (( waited > 200 )); then echo "$name: did not start" >&2; return 1; fi
    done
    # Its window up and at rest.
    sleep 3
    open -g -n "$KEYLAT_APP" --args "$KEYLAT" "$pid" "$KEYS" "$file" "$name" || true
    # Until the tool says it is done or says what stopped it: a press takes a
    # third of a second or so.
    waited=0
    while ! grep -qE '"event":"(keys|error)"' "$file" 2>/dev/null; do
        sleep 0.5
        waited=$((waited + 1))
        if (( waited > KEYS + 60 )); then echo "$name: the key presses were not all timed" >&2; break; fi
    done
    grep -h '"event":"error"' "$file" 2>/dev/null | sed -E 's/.*"label":"([^"]*)".*"message":"([^"]*)".*/\1: \2/' >&2 || true
    pkill -f "${BIN[$name]}" 2>/dev/null || true
    sleep 0.5
}

rm -f "$OUT"/*.jsonl(N) "$OUT"/*.dat(N) "$OUT"/*.done(N)
COUNT=${#TERMINALS}
if (( ROUNDS > 0 )); then
    for round in $(seq 1 "$ROUNDS"); do
        echo "round $round of $ROUNDS"
        for mode in idle flood; do
            for i in $(seq 0 $((COUNT - 1))); do
                run "${TERMINALS[$(( (i + round - 1) % COUNT + 1 ))]}" "$round" "$mode"
                sleep 1
            done
        done
    done
fi
if [[ -n "$VTEBENCH" ]]; then
    [[ -x "$VTEBENCH/target/release/vtebench" ]] || { echo "$VTEBENCH/target/release/vtebench is missing; build vtebench there with cargo build --release" >&2; exit 1 }
    for name in $TERMINALS; do
        echo "vtebench in $name"
        run_vtebench "$name"
        sleep 1
    done
fi
if (( KEYS > 0 )); then
    build_keylat
    # Whether the app may watch the screen and press keys; if not, it asks,
    # which is what puts it in the lists to be switched on.
    rm -f "$WORK/keylat-permission"
    open -g -n "$KEYLAT_APP" --args "$KEYLAT" --check "$WORK/keylat-permission" || true
    for _ in {1..50}; do [[ -s "$WORK/keylat-permission" ]] && break; sleep 0.1; done
    if ! grep -q '"screen":true,"keys":true' "$WORK/keylat-permission" 2>/dev/null; then
        open -g -n "$KEYLAT_APP" || true
        echo "key presses were not timed: KeyLatency ($KEYLAT_APP) needs Screen Recording and Accessibility, under Privacy & Security in System Settings ($(sed -E 's/.*"screen":([a-z]+),"keys":([a-z]+).*/screen recording: \1, accessibility: \2/' "$WORK/keylat-permission" 2>/dev/null)); switch both on for it and run this again" >&2
        KEYS=0
    fi
    if (( KEYS > 0 )); then
        for name in $TERMINALS; do
            echo "$KEYS key presses in $name"
            run_keys "$name"
            sleep 1
        done
    fi
fi

echo
echo "font: $FONT 13    grid: 120x40    Tern: $("${BIN[tern]}" --version)"
for name in $OTHERS; do
    echo "$name: $(defaults read "${APP[$name]}/Contents/Info" CFBundleShortVersionString 2>/dev/null)"
done
if (( ROUNDS > 0 || KEYS > 0 )); then
    # A terminal measured at another size was given a different amount of work.
    # The size is read as each payload starts: some terminals start the
    # command before their window has its final size.
    grep -h '"event":"run"' "$OUT"/*.jsonl | grep -v '"cols":120,"rows":40' | sed -E 's/.*"label":"([^"]*)".*"cols":([0-9]+),"rows":([0-9]+).*/warning: \1 ran at \2x\3, not 120x40/' | sort -u || true
    "$BENCH" summarize "$OUT"/*.jsonl
fi
if [[ -n "$VTEBENCH" ]]; then
    echo
    echo "vtebench, mean milliseconds per sample (lower is better)"
    for name in $TERMINALS; do
        [[ "$(cat "$OUT/vtebench-$name.dat.done" 2>/dev/null)" == "40 120" ]] || echo "warning: $name ran vtebench at '$(cat "$OUT/vtebench-$name.dat.done" 2>/dev/null)' (rows columns), not 40 120"
    done
    "$BENCH" vtebench "$OUT"/vtebench-*.dat
fi
