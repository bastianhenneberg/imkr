#!/usr/bin/env python3
"""End-to-end test of `imkr hosts pair/join` (#605) in three worlds.

Run: cargo build && python3 tests/pairing.py   (about five minutes: the
register travels with the minutely `ls --json` exchange).

alpha and beta know each other by hand (plain lines in authorized_keys,
`hosts add`). gamma is new: it pairs with alpha only. Expected: gamma's
key reaches beta over the register, beta's reaches gamma, both reach each
other through `imkr gate`; `hosts rm gamma` on beta takes gamma's key out
on beta and on alpha; lines nobody of imkr wrote stay byte for byte.
`server` is never paired, alpha reaches it by a line set up by hand: it
never stores a key of another machine nor touches its authorized_keys.

Guard (CLAUDE.md, Test-Abschirmung): notifications off, fake notify-send
first in PATH, DBus on missing sockets, no DISPLAY/WAYLAND, own runtime/
HOME/state/config per world, no IMKR_SOCKET, no real ssh (fake by address).
"""
import json, os, shutil, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
# Outside the repo; short, for the socket paths of the runtime dirs.
BASE = tempfile.mkdtemp(prefix="imkr-pairing-")
WORLD_ADDR = {"gamma": "127.0.0.1", "alpha": "127.0.0.2", "beta": "127.0.0.3", "server": "127.0.0.4"}
FOREIGN = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQforeignforeignforeign someone@else\n"
PORT = 47581

def log(*a):
    print(time.strftime("%H:%M:%S"), *a, flush=True)

def setup():
    if os.path.exists(BASE):
        shutil.rmtree(BASE)
    os.makedirs(BASE)
    fakebin = os.path.join(BASE, "fakebin")
    os.makedirs(fakebin)
    shutil.copy(os.path.join(HERE, "fake_ssh_pairing.py"), os.path.join(fakebin, "ssh"))
    os.chmod(os.path.join(fakebin, "ssh"), 0o755)
    with open(os.path.join(fakebin, "notify-send"), "w") as f:
        f.write(f"#!/bin/sh\necho \"$@\" >> {BASE}/notify.log\n")
    os.chmod(os.path.join(fakebin, "notify-send"), 0o755)
    binary = os.path.join(BASE, "imkr")
    shutil.copy(os.path.join(REPO, "target/debug/imkr"), binary)
    worlds = {}
    for name, addr in WORLD_ADDR.items():
        home = os.path.join(BASE, name)
        runtime = tempfile.mkdtemp(prefix=f"pw{name[0]}", dir="/tmp")
        for d in [".cargo/bin", ".ssh", ".config/imkr", ".local/state"]:
            os.makedirs(os.path.join(home, d), exist_ok=True)
        os.chmod(os.path.join(home, ".ssh"), 0o700)
        os.symlink(binary, os.path.join(home, ".cargo/bin/imkr"))
        config = os.path.join(home, ".config/imkr/config.toml")
        with open(config, "w") as f:
            f.write(f'name = "{name}"\nnotifications = "off"\nweb = false\nbrain_inbox = false\n'
                    'revive_sessions = false\nrevive_at_boot = false\n')
        env = {
            "HOME": home, "USER": "bastian", "SHELL": "/bin/sh", "TERM": "xterm-256color",
            "PATH": f"{fakebin}:{home}/.cargo/bin:/usr/bin:/bin",
            "XDG_RUNTIME_DIR": runtime, "XDG_STATE_HOME": f"{home}/.local/state",
            "XDG_CONFIG_HOME": f"{home}/.config", "IMKR_CONFIG": config,
            "IMKR_HOSTNAME": name, "IMKR_MACHINE_ID": f"id-{name}",
            "DBUS_SESSION_BUS_ADDRESS": f"unix:path={runtime}/no-bus",
            "DBUS_SYSTEM_BUS_ADDRESS": f"unix:path={runtime}/no-system-bus",
            "PW_BASE": BASE, "LANG": "C.UTF-8",
        }
        worlds[name] = {"addr": addr, "home": home, "env": env}
    json.dump(worlds, open(os.path.join(BASE, "worlds.json"), "w"), indent=1)
    return worlds

def run(worlds, name, *args, check=True, input=None):
    r = subprocess.run(list(args), env=worlds[name]["env"], cwd=worlds[name]["home"],
                       capture_output=True, text=True, input=input, timeout=60)
    if check and r.returncode != 0:
        raise SystemExit(f"{name}: {args} failed {r.returncode}\n{r.stdout}\n{r.stderr}")
    return r

def guard(worlds):
    for name in worlds:
        r = run(worlds, name, "sh", "-c", "command -v notify-send; command -v ssh; command -v imkr; "
                "echo $DBUS_SESSION_BUS_ADDRESS; echo ${DISPLAY:-nodisplay} ${WAYLAND_DISPLAY:-nowl}")
        out = r.stdout.split()
        assert out[0].startswith(BASE) and out[1].startswith(BASE) and out[2].startswith(worlds[name]["home"]), out
        assert "nodisplay" in out and "nowl" in out, out
    log("guard ok: fake notify-send/ssh/imkr resolve in all worlds, no display, DBus isolated")

def ak(worlds, name):
    try:
        return open(os.path.join(worlds[name]["home"], ".ssh/authorized_keys")).read()
    except OSError:
        return ""

def wait_for(what, check, timeout=300):
    start = time.time()
    while time.time() - start < timeout:
        if check():
            log("ok:", what, f"({int(time.time() - start)} s)")
            return
        time.sleep(3)
    raise SystemExit(f"TIMEOUT: {what}")

def main():
    worlds = setup()
    guard(worlds)
    # alpha and beta: by hand, as today.
    # As today: one key, copied to both by a dotfile manager.
    run(worlds, "alpha", "ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "imkr@shared",
        "-f", os.path.join(worlds["alpha"]["home"], ".ssh/imkr"))
    for ext in ["", ".pub"]:
        shutil.copy(os.path.join(worlds["alpha"]["home"], ".ssh/imkr" + ext),
                    os.path.join(worlds["beta"]["home"], ".ssh/imkr" + ext))
    shared = open(os.path.join(worlds["alpha"]["home"], ".ssh/imkr.pub")).read()
    hand = {
        "alpha": FOREIGN + shared,
        "beta": "# hand-written\n" + shared + FOREIGN.rstrip("\n"),  # no newline at the end
        "gamma": FOREIGN,
        "server": "# server, by hand\n" + shared + FOREIGN,
    }
    for name, text in hand.items():
        path = os.path.join(worlds[name]["home"], ".ssh/authorized_keys")
        open(path, "w").write(text)
        os.chmod(path, 0o600)
    run(worlds, "alpha", "imkr", "hosts", "add", "beta", "127.0.0.3", "--user", "bastian")
    run(worlds, "beta", "imkr", "hosts", "add", "alpha", "127.0.0.2", "--user", "bastian")
    run(worlds, "alpha", "imkr", "hosts", "add", "server", "127.0.0.4", "--user", "bastian")
    for name in worlds:
        run(worlds, name, "imkr", "new", "--start", "--window", "0", "--", "sleep", "100000", check=False)
    time.sleep(2)
    for name in worlds:
        log(name, "ls:", run(worlds, name, "imkr", "ls", check=False).stdout.strip().replace("\n", " | "))

    def register(name):
        try:
            return json.load(open(os.path.join(worlds[name]["home"], ".local/state/imkr/machines/register.json")))
        except OSError:
            return []

    def has_key(name, of):
        return any(e.get("id") == f"id-{of}" and e.get("key") for e in register(name))

    time.sleep(5)
    assert not has_key("alpha", "beta") and not has_key("beta", "alpha"), "the copied key travelled"
    # Migration of a machine set up by hand: its own key.
    r = run(worlds, "beta", "imkr", "hosts", "key")
    log("beta hosts key:", r.stdout.strip().replace("\n", " | ")[:200])
    wait_for("beta asked alpha with its own key in the register",
             lambda: any('"key"' in json.dumps(e) for e in register("beta")) and
             any(json.loads(l)["from"] == "127.0.0.3" and json.loads(l)["to"] == "127.0.0.2" and
                 "IMKR_HOSTS_TOO" in json.loads(l)["cmd"] and json.loads(l)["at"] > time.time() - 20
                 for l in open(os.path.join(BASE, "ssh.log"))))
    time.sleep(3)
    # alpha is not paired yet: it keeps no key of others and leaves its file alone.
    assert not has_key("alpha", "beta"), "an unpaired machine stored a key"
    assert ak(worlds, "alpha") == hand["alpha"], "an unpaired machine wrote authorized_keys"
    assert not os.path.exists(os.path.join(worlds["alpha"]["home"], ".ssh/authorized_keys.imkr-backup"))
    log("unpaired alpha took no key from beta and left its file alone")

    # gamma pairs with alpha.
    pair = subprocess.Popen(["imkr", "hosts", "pair", "--listen", f"127.0.0.2:{PORT}"],
                            env=worlds["alpha"]["env"], cwd=worlds["alpha"]["home"],
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    code = None
    for line in pair.stdout:
        log("alpha pair:", line.rstrip())
        if "hosts join" in line:
            code = line.split()[-1]
            break
    # A wrong code first: counted, nothing taken.
    bad = run(worlds, "gamma", "imkr", "hosts", "join", f"127.0.0.2:{PORT}", "ZZZZ-ZZZZ", check=False)
    assert bad.returncode != 0 and "wrong code" in bad.stderr, bad.stderr
    log("wrong code refused:", bad.stderr.strip())
    r = run(worlds, "gamma", "imkr", "hosts", "join", f"127.0.0.2:{PORT}", code)
    log("gamma join:", r.stdout.strip().replace("\n", " | "))
    rest, _ = pair.communicate(timeout=20)
    log("alpha pair:", rest.strip().replace("\n", " | "), "exit", pair.returncode)
    assert pair.returncode == 0

    assert os.path.exists(os.path.join(worlds["gamma"]["home"], ".ssh/imkr-id-gamma")), "gamma got no key"
    assert os.path.exists(os.path.join(worlds["alpha"]["home"], ".ssh/imkr-id-alpha")), "alpha got no own key"
    a, g = ak(worlds, "alpha"), ak(worlds, "gamma")
    assert a.startswith(hand["alpha"]) and "imkr:id-gamma" in a, a
    assert g.startswith(hand["gamma"]) and "imkr:id-alpha" in g, g
    log("after pairing: alpha lets gamma in, gamma lets alpha in")
    wait_for("alpha, paired now, lets beta in by its own key", lambda: "imkr:id-beta" in ak(worlds, "alpha"))
    wait_for("gamma lets beta in (beta's key from alpha's register)", lambda: "imkr:id-beta" in ak(worlds, "gamma"))
    for name in ["alpha", "gamma"]:
        st = os.stat(os.path.join(worlds[name]["home"], ".ssh/authorized_keys"))
        assert oct(st.st_mode & 0o777) == "0o600", oct(st.st_mode)
        backup = open(os.path.join(worlds[name]["home"], ".ssh/authorized_keys.imkr-backup")).read()
        assert backup == hand[name], "backup is not the file before"
    log("0600 and backups are the files before")

    wait_for("beta lets gamma in (key over the register)", lambda: "imkr:id-gamma" in ak(worlds, "beta"))
    wait_for("beta lets alpha in by alpha's own key", lambda: "imkr:id-alpha" in ak(worlds, "beta"))
    b = ak(worlds, "beta")
    assert b.startswith(hand["beta"]), repr(b)
    def reached(name, other):
        lines = run(worlds, name, "imkr", "hosts").stdout.splitlines()
        return any(l.split()[:2] == [other, "remembered"] for l in lines if l.strip())
    wait_for("beta reached gamma (through gamma's gate) and shows it as host", lambda: reached("beta", "gamma"))
    wait_for("gamma reached beta (through beta's gate) and shows it as host", lambda: reached("gamma", "beta"))
    for name in worlds:
        log(f"{name} hosts:", run(worlds, name, "imkr", "hosts").stdout.strip().replace("\n", " | "))
    # Through the gate: imkr's own commands yes, a shell no.
    gk = os.path.join(worlds["gamma"]["home"], ".ssh/imkr-id-gamma")
    ok = run(worlds, "gamma", "ssh", "-i", gk, "--", "127.0.0.3",
             "exec \"${SHELL:-sh}\" -lc 'PATH=$HOME/.cargo/bin:$HOME/.local/bin:$PATH exec imkr ls --json'",
             check=False)
    assert ok.returncode == 0 and ok.stdout.startswith("["), (ok.stdout, ok.stderr)
    for cmd in ["id", "sh -i", "cat .ssh/authorized_keys",
                "exec \"${SHELL:-sh}\" -lc 'PATH=$HOME/.cargo/bin:$HOME/.local/bin:$PATH exec imkr hosts rm alpha'",
                "exec \"${SHELL:-sh}\" -lc 'PATH=$HOME/.cargo/bin:$HOME/.local/bin:$PATH exec imkr ls; id'"]:
        no = run(worlds, "gamma", "ssh", "-i", gk, "--", "127.0.0.3", cmd, check=False)
        assert no.returncode != 0 and "uid=" not in no.stdout, (cmd, no.stdout, no.stderr)
        log("gate refused:", cmd[:60], "→", no.stderr.strip().splitlines()[-1][:110])
    shell = run(worlds, "gamma", "ssh", "-i", gk, "--", "127.0.0.3", check=False)
    assert shell.returncode != 0, shell.stdout
    log("gate refused a login without command:", shell.stderr.strip()[-90:])

    # Revoke on beta.
    r = run(worlds, "beta", "imkr", "hosts", "rm", "gamma")
    log("beta hosts rm gamma:", r.stdout.strip())
    assert "imkr:id-gamma" not in ak(worlds, "beta")
    assert "imkr:id-alpha" in ak(worlds, "beta") and ak(worlds, "beta").startswith(hand["beta"])
    wait_for("alpha took gamma's key out too", lambda: "imkr:id-gamma" not in ak(worlds, "alpha"))
    assert ak(worlds, "alpha").startswith(hand["alpha"]) and "imkr:id-beta" in ak(worlds, "alpha")
    no = run(worlds, "gamma", "ssh", "-i", gk, "--", "127.0.0.2", "id", check=False)
    assert "Permission denied" in no.stderr, no.stderr
    log("gamma no longer gets into alpha:", no.stderr.strip())
    time.sleep(70)
    assert "imkr:id-gamma" not in ak(worlds, "alpha") and "imkr:id-gamma" not in ak(worlds, "beta"), \
        "gamma came back"
    log("gamma stays out after another minute")
    # End of the migration: the lines of the copied key go, by hand.
    for name in ["alpha", "beta"]:
        path = os.path.join(worlds[name]["home"], ".ssh/authorized_keys")
        text = open(path).read().replace(shared, "").replace(shared.rstrip("\n"), "")
        open(path, "w").write(text)
    for (src, dst, addr) in [("alpha", "beta", "127.0.0.3"), ("beta", "alpha", "127.0.0.2")]:
        own = os.path.join(worlds[src]["home"], f".ssh/imkr-id-{src}")
        r = run(worlds, src, "ssh", "-i", own, "-i", os.path.join(worlds[src]["home"], ".ssh/imkr"),
                "--", addr,
                "exec \"${SHELL:-sh}\" -lc 'PATH=$HOME/.cargo/bin:$HOME/.local/bin:$PATH exec imkr ls --json'",
                check=False)
        assert r.returncode == 0 and r.stdout.startswith("["), (src, r.stderr)
        old = run(worlds, src, "ssh", "-i", os.path.join(worlds[src]["home"], ".ssh/imkr"), "--", addr,
                  "id", check=False)
        assert "Permission denied" in old.stderr, old.stderr
    log("without the copied key: alpha and beta reach each other by their own keys only")
    for name in worlds:
        log(f"{name} authorized_keys:\n" + ak(worlds, name))
    # The server: alpha's imkr asked it every minute (`ls --json` with its
    # register, keys included), as for any host.
    asked = [json.loads(l) for l in open(os.path.join(BASE, "ssh.log"))]
    asked = [a for a in asked if a["to"] == "127.0.0.4" and "IMKR_HOSTS_TOO" in a["cmd"]]
    assert asked, "alpha never asked the server with its register"
    sent = run(worlds, "alpha", "cat", os.path.join(worlds["alpha"]["home"],
               ".local/state/imkr/machines/register.json")).stdout
    assert '"key"' in sent, "alpha had no keys to send"
    assert ak(worlds, "server") == hand["server"], "the server's authorized_keys changed"
    sh = os.path.join(worlds["server"]["home"], ".ssh")
    assert sorted(os.listdir(sh)) == ["authorized_keys"], os.listdir(sh)
    assert '"key"' not in json.dumps(register("server")), register("server")
    log(f"server: asked {len(asked)} times with keys, authorized_keys byte for byte, "
        "no backup, no key of its own, no key in its register")
    log("PASS")

if __name__ == "__main__":
    try:
        main()
    finally:
        base = BASE
        if os.path.exists(os.path.join(base, "worlds.json")):
            worlds = json.load(open(os.path.join(base, "worlds.json")))
            for name, w in worlds.items():
                subprocess.run(["imkr", "kill-server"], env=w["env"], capture_output=True, timeout=20)
