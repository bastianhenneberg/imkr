#!/usr/bin/env python3
"""E2E for #9900: `imkr operator` names the machine the user last typed at.

Three worlds and a fake ssh. The user sits at `alpha`; their client shows the
session `work` on `beta` and types into it. Then a client from a third
machine, `gamma`, attaches to the same session and never types (like a terminal reconnecting on an idle
machine: every client sends a resize when it attaches). Two more hosts never
answer: `down` fails, `slow` hangs past the time limit.

  1. on beta:  `imkr operator`  -> alpha   (before: gamma, the newest attach)
  2. on alpha: `imkr operator`  -> alpha, here, `down` and `slow` named as
                                   unanswered, within the time limit
                                   (before: "nobody is attached ... on this
                                   machine", the client shows a remote session)

Usage: cargo build && python3 tests/operator_where.py [--binary PATH]
Test shield (CLAUDE.md): own HOME/config/state/runtime per world, no
IMKR_SOCKET, notifications off, fake notify-send in front, no DBus/display.
"""
import argparse, json, os, pathlib, shutil, subprocess, sys, tempfile, time

parser = argparse.ArgumentParser()
parser.add_argument("--binary", type=pathlib.Path,
                    default=pathlib.Path(__file__).resolve().parent.parent / "target/debug/imkr")
args = parser.parse_args()
BIN = args.binary.resolve()
ROOT = pathlib.Path(tempfile.mkdtemp(prefix="imkr-op-", dir="/tmp"))


def tmux_of(env):
    """A tmux server per world: one shared server would start every
    client with the environment of the world that started it."""
    return ["tmux", "-L", f"imkr-op-{os.getpid()}-{pathlib.Path(env['HOME']).parent.name}"]

FAKE_SSH = r'''#!/usr/bin/python3
import json, os, sys
root = os.environ["OP_ROOT"]
worlds = json.load(open(os.path.join(root, "worlds.json")))
args, dest, rest, show = sys.argv[1:], None, [], False
i = 0
while i < len(args):
    a = args[i]
    if a == "--":
        dest, rest = args[i + 1], args[i + 2:]; break
    if a in ("-o", "-p", "-F", "-S", "-E", "-b", "-c", "-D", "-L", "-R", "-W", "-J", "-i", "-l"):
        i += 2; continue
    if a == "-G":
        show = True; i += 1; continue
    if a.startswith("-"):
        i += 1; continue
    dest, rest = a, args[i + 1:]; break
if show:
    print(f"hostname {dest}\nuser test"); sys.exit(0)
with open(os.path.join(root, "ssh.log"), "a") as f:
    f.write(f"{dest} {' '.join(rest)}\n")
if dest == "slow":
    import time; time.sleep(30); sys.exit(255)
if dest not in worlds:
    sys.stderr.write(f"ssh: connect to host {dest} port 22: No route to host\n"); sys.exit(255)
env = worlds[dest]
os.execve("/bin/sh", ["sh", "-c", " ".join(rest)], env)
'''


def world(name, hosts):
    w = ROOT / name
    for d in ["home/.cargo/bin", "home/config/imkr", "state", "run", "bin"]:
        (w / d).mkdir(parents=True, exist_ok=True)
    (w / "home/.cargo/bin/imkr").symlink_to(BIN)
    (w / "bin/imkr").symlink_to(BIN)
    (w / "bin/ssh").write_text(FAKE_SSH)
    (w / "bin/notify-send").write_text("#!/bin/sh\necho \"$@\" >> \"$HOME/notify.log\"\n")
    for n in ["ssh", "notify-send"]:
        (w / "bin" / n).chmod(0o755)
    for n in ["tailscale", "netbird", "ai-brain-harness", "ai-brain-cli", "curl", "xdg-open"]:
        (w / "bin" / n).write_text("#!/bin/sh\nexit 1\n")
        (w / "bin" / n).chmod(0o755)
    config = w / "home/config/imkr/config.toml"
    config.write_text(f'name = "{name}"\nnotifications = "off"\nhosts = {json.dumps(hosts)}\n'
                      'revive_sessions = false\nweb = false\n')
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("IMKR", "CLAUDE", "CODEX", "SSH", "AI_BRAIN", "DBUS_", "TMUX"))
           and k not in ("DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY", "HYPRLAND_INSTANCE_SIGNATURE")}
    env.update(HOME=str(w / "home"), XDG_CONFIG_HOME=str(w / "home/config"),
               XDG_STATE_HOME=str(w / "state"), XDG_RUNTIME_DIR=str(w / "run"),
               IMKR_CONFIG=str(config), IMKR_HOSTNAME=name, OP_ROOT=str(ROOT),
               PATH=f"{w / 'bin'}:/usr/bin:/bin", SHELL="/bin/sh", TERM="xterm-256color",
               DBUS_SESSION_BUS_ADDRESS=f"unix:path={w}/no-session-bus",
               DBUS_SYSTEM_BUS_ADDRESS=f"unix:path={w}/no-system-bus")
    # The guard, before any imkr process starts.
    assert pathlib.Path(shutil.which("notify-send", path=env["PATH"])).resolve() == (w / "bin/notify-send").resolve()
    assert not any(k in env for k in ("DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY", "IMKR_SOCKET"))
    assert "notifications = \"off\"" in config.read_text()
    return env


def imkr(env, *argv, timeout=30):
    return subprocess.run([str(BIN), *argv], env=env, capture_output=True, text=True, timeout=timeout)


def tmux(env, *argv):
    return subprocess.run(tmux_of(env) + list(argv), env=env, capture_output=True, text=True, check=True)


def wait_for(what, check, within=20):
    end = time.time() + within
    while time.time() < end:
        if check():
            return
        time.sleep(0.3)
    raise SystemExit(f"FAIL: timed out waiting for {what}")


alpha = world("alpha", ["beta", "down", "slow"])
beta = world("beta", [])
gamma = world("gamma", ["beta"])
json.dump({"beta": beta, "alpha": alpha, "gamma": gamma}, open(ROOT / "worlds.json", "w"))

failed = []
try:
    # The user's client at alpha shows `work` on beta, and types into it.
    tmux(alpha, "new-session", "-d", "-x", "120", "-y", "35", "-s", "user",
         f"{BIN} --host beta -s work")
    wait_for("the session on beta", lambda: "work" in imkr(beta, "ls").stdout)
    time.sleep(2)
    tmux(alpha, "send-keys", "-t", "user", "-l", "echo typed-at-alpha")
    tmux(alpha, "send-keys", "-t", "user", "Enter")
    time.sleep(1)
    # Later a terminal at gamma attaches too and never types, like the
    # desktop's reconnecting at 10:41 on 13.11. while Bastian typed at the laptop.
    tmux(gamma, "new-session", "-d", "-x", "100", "-y", "30", "-s", "idle",
         f"{BIN} --host beta -s work")
    time.sleep(3)

    r = imkr(beta, "operator", "--json")
    print("beta:  imkr operator --json ->", r.returncode, r.stdout.strip().replace("\n", " "), r.stderr.strip())
    got = json.loads(r.stdout) if r.returncode == 0 else {}
    if got.get("machine") != "alpha" or got.get("here") is not False:
        failed.append("on beta, the operator is not alpha (the idle client that attached last won)")

    started = time.time()
    r = imkr(alpha, "operator", "--json")
    took = time.time() - started
    print(f"alpha: imkr operator --json -> {r.returncode} ({took:.1f} s)",
          r.stdout.strip().replace("\n", " "), "| stderr:", r.stderr.strip())
    got = json.loads(r.stdout) if r.returncode == 0 else {}
    if got.get("machine") != "alpha" or got.get("here") is not True:
        failed.append("on alpha, the operator is not alpha/here")
    if [u.get("host") for u in got.get("unanswered", [])] != ["down", "slow"] or "slow" not in r.stderr:
        failed.append("the machines that did not answer are not named")
    if took > 8:
        failed.append(f"asking the other machines took {took:.1f} s")
    asked = [l.split()[0] for l in open(ROOT / "ssh.log") if "operator" in l]
    print("ssh calls for operator:", asked)
    if sorted(asked) != ["beta", "down", "slow"]:
        failed.append(f"not exactly one ssh call per machine: {asked}")
finally:
    for env in (alpha, beta, gamma):
        subprocess.run(tmux_of(env) + ["kill-server"], env=env, capture_output=True)
        # Only the test worlds' servers: their sockets are under ROOT.
        for sock in pathlib.Path(env["XDG_RUNTIME_DIR"]).glob("imkr/*.sock"):
            subprocess.run([str(BIN), "-s", sock.stem, "kill-server"], env=env,
                           capture_output=True, timeout=10)

print("PASS" if not failed else "FAIL:\n  " + "\n  ".join(failed))
sys.exit(1 if failed else 0)
