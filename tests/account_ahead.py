#!/usr/bin/env python3
"""E2E #9899: bee starts in the freest account; resting agent moves ahead of
the threshold, working one does not; nothing below -> stays.
Desktop isolation: notifications off, notify-send shim, private DBus/runtime,
own HOME/state/config, no IMKR_SOCKET."""
import json, os, shutil, subprocess, sys, tempfile, time, tomllib
from pathlib import Path

# Usage: cargo build && python3 -I tests/account_ahead.py "$PWD/target/debug/imkr"
BIN = sys.argv[1]
FAKE = r'''#!/usr/bin/env python3
import json, os, sys, select, subprocess, time, uuid
sock = os.path.basename(os.environ.get("IMKR_SOCKET", "x"))
home = os.environ["HOME"]
args = sys.argv[1:]
sid = args[args.index("--resume") + 1] if "--resume" in args else str(uuid.uuid4())
print("fake claude in", os.environ.get("CLAUDE_CONFIG_DIR", "standard"), "args:", " ".join(args), flush=True)
def hook(state, event, **more):
    data = dict(session_id=sid, hook_event_name=event, **more)
    subprocess.run(["imkr", "status", state, "--agent", "claude"], input=json.dumps(data),
                   text=True, capture_output=True)
hook("idle", "SessionStart")
busy = False
while True:
    if os.path.exists(f"{home}/busy-{sock}"):
        if not busy:
            busy = True; hook("working", "PreToolUse")
        print("working", time.time(), flush=True); time.sleep(0.3); continue
    if busy:
        busy = False; hook("idle", "Stop")
    r, _, _ = select.select([sys.stdin], [], [], 1)
    if r:
        line = sys.stdin.readline()
        if not line: break
        hook("working", "UserPromptSubmit", prompt=line.strip())
        d = f"{home}/.claude/projects/-fake"; os.makedirs(d, exist_ok=True)
        with open(f"{d}/{sid}.jsonl", "a") as f: f.write(json.dumps({"prompt": line.strip()}) + "\n")
        print("got:", line.strip()[:60], flush=True)
        hook("idle", "Stop")
'''

root = Path(tempfile.mkdtemp(prefix='kv-', dir='/tmp/claude-1000'))
home = root / 'h'; home.mkdir()
binp = home / '.local/bin'; binp.mkdir(parents=True)
(binp / 'claude').write_text(FAKE); (binp / 'claude').chmod(0o755)
os.symlink(BIN, binp / 'imkr')
conf = home / 'imkr.toml'
conf.write_text('notifications = "off"\naccount_fallback = "auto"\naccount_switch_at = 95\nsession_banner = false\n')
notify_log = root / 'notify.log'
(binp / 'notify-send').write_text(f'#!/bin/sh\necho intercepted >> {notify_log}\n')
(binp / 'notify-send').chmod(0o755)
runtime = root / 'r'; runtime.mkdir(mode=0o700)
env = {k: v for k, v in os.environ.items()
       if not k.startswith(('IMKR', 'CODEX', 'CLAUDE', 'ANTHROPIC', 'DBUS_'))
       and k not in ('DISPLAY', 'WAYLAND_DISPLAY', 'XAUTHORITY')}
env.update(HOME=str(home), XDG_STATE_HOME=str(home / 'state'), XDG_CONFIG_HOME=str(home / 'cfg'),
           IMKR_CONFIG=str(conf), XDG_RUNTIME_DIR=str(runtime), SHELL='/bin/bash',
           PATH=f'{binp}:/usr/bin:/bin', IMKR_HOSTNAME='kv-test',
           DBUS_SESSION_BUS_ADDRESS=f'unix:path={root}/no-bus',
           DBUS_SYSTEM_BUS_ADDRESS=f'unix:path={root}/no-sysbus')
assert tomllib.loads(conf.read_text())['notifications'] == 'off'
assert shutil.which('notify-send', path=env['PATH']) == str(binp / 'notify-send')
assert shutil.which('claude', path=env['PATH']) == str(binp / 'claude')
subprocess.run(['notify-send', 'self-check'], env=env, check=True)
assert notify_log.read_text() == 'intercepted\n'; notify_log.unlink()

# Three Claude subscriptions: standard (~/.claude), work, spare.
def login(d, email, org):
    d.mkdir(parents=True, exist_ok=True)
    (d / '.credentials.json').write_text('secret')
    return {"oauthAccount": {"emailAddress": email, "organizationUuid": org}}
(home / '.claude.json').write_text(json.dumps(login(home / '.claude', 'std@x', 'o1')))
acc = home / 'state/imkr/accounts'
for name, org in [('work', 'o2'), ('spare', 'o3')]:
    d = acc / 'claude' / name
    (d / '.claude.json').parent.mkdir(parents=True, exist_ok=True)
    (d / '.claude.json').write_text(json.dumps(login(d, f'{name}@x', org)))
(acc / 'accounts.toml').write_text('[[account]]\nname = "work"\ntool = "claude"\n\n[[account]]\nname = "spare"\ntool = "claude"\n')

def usage(numbers):
    acc_list = [{"tool": "claude", "name": n, "names": [n], "default": n == 'standard', "plan": None,
                 "windows": [{"label": "week", "used": w, "resets_at": None},
                             {"label": "5h", "used": h, "resets_at": None}], "note": None}
                for n, (w, h) in numbers.items()]
    (acc / 'usage.json').write_text(json.dumps({"at": int(time.time()), "report": {"accounts": acc_list, "tokens": []}}))

def cli(*a, inp=None):
    return subprocess.run([BIN, *a], env=env, text=True, capture_output=True, timeout=120, input=inp)

def screen(addr):
    return cli('read', addr).stdout

repo = home / 'repo'; repo.mkdir()
brief = root / 'brief.md'; brief.write_text('Do the thing.\n')
ok = True
def check(cond, what):
    global ok
    print(('PASS ' if cond else 'FAIL ') + what, flush=True)
    ok &= bool(cond)

try:
    # 1. A bee without --account starts in the freest account.
    usage({'standard': (80, 50), 'work': (40, 10), 'spare': (60, 5)})
    r = cli('spawn', 'bee1', '--brief', str(brief), '--dir', str(repo))
    print('spawn bee1 stdout:', r.stdout.strip(), '| stderr:', r.stderr.strip())
    check(r.returncode == 0, 'spawn bee1 succeeded')
    check('claude starts in work (40 % week): freer than the default std (80 % week)' in r.stderr,
          'spawn names the freest account and why')
    time.sleep(2)
    s1 = screen('bee1/0'); print(s1.strip()[-400:])
    check('accounts/claude/work' in s1, 'bee1 agent runs with CLAUDE_CONFIG_DIR of work')

    # 2. A second bee explicitly in work, kept busy.
    r = cli('spawn', 'bee2', '--brief', str(brief), '--dir', str(repo), '--account', 'work')
    check(r.returncode == 0, 'spawn bee2 (--account work) succeeded: ' + r.stderr.strip())
    sock2 = [p for p in runtime.rglob('bee2*') if p.is_socket()]
    sock1 = [p for p in runtime.rglob('bee1*') if p.is_socket()]
    print('sockets', sock1, sock2)
    (home / f'busy-{sock2[0].name}').write_text('')
    time.sleep(3)

    # 3. work reaches 97 % of its five hours: the resting bee1 moves to spare
    #    (standard is at 80 % week, spare 60 %), the working bee2 stays.
    usage({'standard': (80, 50), 'work': (40, 97), 'spare': (60, 5)})
    deadline = time.time() + 40
    while time.time() < deadline and 'accounts/claude/spare' not in screen('bee1/0'):
        time.sleep(1)
    s1 = screen('bee1/0'); s2 = screen('bee2/0')
    print('--- bee1 ---\n' + s1.strip()[-600:]); print('--- bee2 ---\n' + s2.strip()[-300:])
    check('accounts/claude/spare' in s1 and '--resume' in s1, 'resting bee1 restarted in spare, same conversation')
    check('accounts/claude/spare' not in s2, 'working bee2 not moved')
    l1 = json.loads(cli('-s', 'bee1', 'list', '--json').stdout)
    print('bee1 pane accounts:', [p.get('accounts') for p in l1])

    # 4. bee2 rests now, but nothing is below 95 %: it stays.
    (home / f'busy-{sock2[0].name}').unlink()
    usage({'standard': (96, 50), 'work': (40, 97), 'spare': (60, 99)})
    time.sleep(30)
    s2 = screen('bee2/0')
    print('--- bee2 after ---\n' + '\n'.join(l for l in s2.splitlines() if not l.startswith('working'))[-400:])
    check('spare' not in s2 and 'standard' not in s2.split('fake claude in')[-1] and 'account run claude:' not in s2,
          'resting bee2 stays in work when no account is below 95 %')
    check(not notify_log.exists(), 'no notification escaped')
finally:
    for s in ('bee1', 'bee2'):
        cli('-s', s, 'kill-server')
    print('world', root)
print('ALL PASS' if ok else 'SOME FAILED')
sys.exit(0 if ok else 1)
