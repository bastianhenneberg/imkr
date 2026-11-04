# Sourced by demo.tape: a world of its own for the recording, with a small
# project to work in. Nothing of your own sessions, config or state shows.
D=/tmp/imkr-demo
rm -rf "$D" && mkdir -p "$D/run" "$D/state" "$D/bin" "$D/home" "$D/config"
unset IMKR_SOCKET TMUX DBUS_STARTER_ADDRESS DBUS_STARTER_BUS_TYPE DBUS_SESSION_BUS_PID DISPLAY WAYLAND_DISPLAY XAUTHORITY
export DBUS_SESSION_BUS_ADDRESS=unix:path=$D/no-session-bus
export DBUS_SYSTEM_BUS_ADDRESS=unix:path=$D/no-system-bus
export IMKR_DEMO_NOTIFY_LOG=$D/intercepted-notifications.log
printf '#!/bin/sh\nprintf "%%s\\n" intercepted >> "$IMKR_DEMO_NOTIFY_LOG"\n' > "$D/bin/notify-send"
chmod +x "$D/bin/notify-send"
chmod 700 "$D/run"
export HOME=$D/home XDG_CONFIG_HOME=$D/config
export XDG_RUNTIME_DIR=$D/run XDG_STATE_HOME=$D/state IMKR_CONFIG=$D/config.toml
export PATH=$D/bin:$PWD/docs/demo/bin:$PATH
# Panes get a plain bash: your own rc files could put the real claude first.
printf '#!/bin/sh\nPS1="$ " exec bash --norc --noprofile -i\n' > "$D/sh"
chmod +x "$D/sh"
export SHELL=$D/sh
printf 'notifications = "off"\ntheme = "imkr"\nname = "laptop"\n' > "$IMKR_CONFIG"
mkdir -p "$D/api/src/auth"
printf '[package]\nname = "api"\n' > "$D/api/Cargo.toml"
printf 'fn main() {}\n' > "$D/api/src/main.rs"
printf 'pub fn login() {}\n' > "$D/api/src/auth/login.rs"
git -C "$D/api" init -q -b main
git -C "$D/api" add -A
git -C "$D/api" -c user.name=demo -c user.email=demo@example.com commit -qm start
printf 'pub fn limit() {}\n' > "$D/api/src/auth/limit.rs"
cd "$D/api" && clear
