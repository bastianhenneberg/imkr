#!/usr/bin/env python3
"""Fake ssh for the pairing test: picks the world by address, lets in by
that world's authorized_keys like sshd (first line with the offered key
wins; command= is forced with SSH_ORIGINAL_COMMAND), and runs the command
with that world's environment. No network, no real sshd."""
import json, os, subprocess, sys, time

BASE = os.environ["PW_BASE"]
WORLDS = json.load(open(os.path.join(BASE, "worlds.json")))  # name -> {addr, home, env}
LOG = os.path.join(BASE, "ssh.log")

args = sys.argv[1:]
keys, user, dest, rest = [], None, None, []
i = 0
show_config = False
while i < len(args):
    a = args[i]
    if a == "--":
        dest = args[i + 1]
        rest = args[i + 2:]
        break
    if a in ("-o", "-p", "-F", "-S", "-E", "-b", "-c", "-D", "-L", "-R", "-W", "-J"):
        i += 2
        continue
    if a == "-i":
        keys.append(args[i + 1]); i += 2; continue
    if a == "-l":
        user = args[i + 1]; i += 2; continue
    if a == "-G":
        show_config = True; i += 1; continue
    if a.startswith("-"):
        i += 1
        continue
    dest = a
    rest = args[i + 1:]
    break

if show_config:
    print(f"hostname {dest}\nuser {os.environ.get('USER', 'x')}")
    sys.exit(0)

if dest and "@" in dest:
    user, dest = dest.split("@", 1)
target = next((w for n, w in WORLDS.items() if dest in (n, w["addr"])), None)
caller = next((w for w in WORLDS.values() if w["home"] == os.environ.get("HOME")), None)
command = " ".join(rest)
with open(LOG, "a") as log:
    log.write(json.dumps({"from": caller and caller["addr"], "to": dest, "keys": keys,
                          "cmd": command[:120], "at": time.time()}) + "\n")
if target is None:
    print(f"ssh: Could not resolve hostname {dest}", file=sys.stderr)
    sys.exit(255)

home = os.environ.get("HOME", "")
if not keys:
    keys = [os.path.join(home, ".ssh", "id_ed25519")]
offered = []
for k in keys:
    try:
        w = open(k + ".pub").read().split()
        offered.append(w[0] + " " + w[1])
    except OSError:
        pass

def parse(line):
    """(options, 'type blob') of an authorized_keys line, like pairing.rs."""
    line = line.strip()
    if not line or line.startswith("#"):
        return None
    words = line.split()
    if words[0].startswith(("ssh-", "ecdsa-", "sk-")):
        opts, rest = "", line
    else:
        q, end, esc = False, len(line), False
        for j, c in enumerate(line):
            if esc:
                esc = False
            elif c == "\\":
                esc = True
            elif c == '"':
                q = not q
            elif c in " \t" and not q:
                end = j
                break
        opts, rest = line[:end], line[end:].strip()
    w = rest.split()
    if len(w) < 2:
        return None
    return opts, w[0] + " " + w[1]

forced = None
let_in = False
ak = os.path.join(target["home"], ".ssh", "authorized_keys")
try:
    lines = open(ak).read().splitlines()
except OSError:
    lines = []
# Like sshd: keys in the order offered, the first line with that key.
found = None
for key in offered:
    for line in lines:
        p = parse(line)
        if p and p[1] == key:
            found = p
            break
    if found:
        break
for p in [found] if found else []:
    if True:
        let_in = True
        opts = p[0]
        if 'command="' in opts:
            start = opts.index('command="') + len('command="')
            end = start
            while opts[end] != '"' or opts[end - 1] == "\\":
                end += 1
            forced = opts[start:end].replace('\\"', '"').replace("\\\\", "\\")
        break
if not let_in:
    print("Permission denied (publickey).", file=sys.stderr)
    sys.exit(255)

env = dict(target["env"])
env["SSH_CONNECTION"] = f"{caller['addr'] if caller else '127.0.0.9'} 50000 {target['addr']} 22"
if forced:
    if command:
        env["SSH_ORIGINAL_COMMAND"] = command
    run = forced
else:
    run = command or "sh"
result = subprocess.run(["/bin/sh", "-c", run], env=env, cwd=target["home"])
sys.exit(result.returncode)
