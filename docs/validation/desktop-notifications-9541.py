"""Record real imkr desktop argv using private servers; never send host popups.

Usage: python3 desktop-notifications-9541.py BINARY LABEL [--copies 1|3]
Use --copies 3 for baseline 91fd2ca, --controls-only to skip the remote burst.
Requires Linux /proc, Python 3 and bash. All network commands are stand-ins.
"""
import os, pathlib, subprocess, json, time, sys, pty, select, re, signal, argparse, tempfile
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('binary', type=pathlib.Path)
parser.add_argument('label')
parser.add_argument('--copies', type=int, default=1)
parser.add_argument('--controls-only', action='store_true')
args = parser.parse_args()
if pathlib.Path(args.label).name != args.label or args.label in ('.', '..'):
    parser.error('label must be a single directory name')
ROOT = pathlib.Path(tempfile.mkdtemp(prefix='imkr-n9541-'))
BIN = args.binary.resolve()
tag = args.label
w = ROOT / tag
w.mkdir()
for d in ['home', 'state', 'runtime', 'bin']:
    (w / d).mkdir(exist_ok=True)
env = {k: v for k, v in os.environ.items() if not k.startswith(('IMKR', 'CLAUDE', 'CODEX', 'SSH', 'AI_BRAIN'))}
env.update(HOME=str(w / 'home'), XDG_STATE_HOME=str(w / 'state'), XDG_RUNTIME_DIR=str(w / 'runtime'), XDG_CONFIG_HOME=str(w / 'home/config'), SHELL='/bin/bash', PATH=str(w / 'bin') + ':/usr/bin:/bin', CHECK_WORLD=str(w), IMKR_HOSTNAME='isolated-desktop')

def executable(name, text):
    p = w / 'bin' / name
    p.write_text(text)
    p.chmod(493)
executable('notify-send', '#!/usr/bin/python3\nimport os,json,sys,fcntl\nwith open(os.environ[\'CHECK_WORLD\']+\'/argv.jsonl\',\'a\') as f:\n fcntl.flock(f,fcntl.LOCK_EX);f.write(json.dumps(sys.argv[1:])+"\\n")\n')
executable('ssh', '#!/usr/bin/python3\nimport os,sys,json\np=os.environ[\'CHECK_WORLD\']\nif "\'ls\' \'--json\'" in sys.argv[-1] or \'ls --json\' in sys.argv[-1]:\n print(open(p+\'/remote.json\').read())\n with open(p+\'/polls\',\'a\') as f:f.write(\'poll\\n\')\nelse: sys.exit(1)\n')
for name in ['curl', 'tailscale', 'ai-brain-cli', 'ai-brain-harness']:
    executable(name, '#!/bin/sh\nexit 1\n')
for name in ['codex', 'claude']:
    executable(name, '#!/bin/bash\nwhile :; do read -r line; done\n')
servers = []

def remote(state, n=15):
    data = [dict(name='project-review', panes=n, windows=1, agent_panes=[dict(pane=i, program='codex' if i % 2 else 'claude', workspace=1, dir='imkr', state=state, title='JT $(touch TITLE_INJECTED)', tail='RAW SCREEN') for i in range(n)])]
    p = w / 'remote.tmp'
    p.write_text(json.dumps(data))
    p.replace(w / 'remote.json')

def call(e, *args, stdin=''):
    p = subprocess.run([str(BIN), *args], env=e, input=stdin, text=True, capture_output=True, timeout=15)
    if p.returncode:
        raise RuntimeError((args, p.stdout, p.stderr))
    return p.stdout.strip()

def start(name, hosts=False, mode='system'):
    e = env.copy()
    e.update(IMKR_SOCKET=str(w / (name + '.sock')), IMKR_SESSION=name, IMKR_CONFIG=str(w / (name + '.toml')))
    pathlib.Path(e['IMKR_CONFIG']).write_text(f'notifications = "{mode}"\nweb = false\nbrain_inbox = false\ncheckpoints = false\nrevive_sessions = false\nhosts = ' + ('["synthetic-host"]' if hosts else '[]') + '\n')
    log = open(w / (name + '.log'), 'w')
    p = subprocess.Popen([str(BIN), 'server'], env=e, stdout=log, stderr=log, cwd=w)
    servers.append((p, e, log))
    for _ in range(100):
        if pathlib.Path(e['IMKR_SOCKET']).exists():
            break
        time.sleep(0.05)
    else:
        raise RuntimeError('no socket ' + name)
    return e

def payloads():
    p = w / 'argv.jsonl'
    return [json.loads(x) for x in p.read_text().splitlines()] if p.exists() else []

def wait(sec):
    time.sleep(sec)
try:
    blocked = repeat = failed = done = []
    failover = None
    if not args.controls_only:
        remote('working')
        a = start('watch-a', True)
        b = start('watch-b', True)
        c = start('watch-c', True)
        wait(8)
        before = len(payloads())
        remote('blocked')
        wait(8)
        blocked = payloads()[before:]
        before = len(payloads())
        remote('blocked')
        wait(8)
        repeat = payloads()[before:]
        before = len(payloads())
        remote('failed')
        wait(8)
        failed = payloads()[before:]
        before = len(payloads())
        remote('working')
        wait(8)
        remote('idle')
        wait(8)
        done = payloads()[before:]
        owners = []
        for proc, e, log in servers:
            try:
                if any(('desktop-notifiers' in os.readlink(fd) for fd in pathlib.Path(f'/proc/{proc.pid}/fd').iterdir())):
                    owners.append((proc, e))
            except OSError:
                pass
        if owners:
            assert len(owners) == 1, owners
            proc, owner_env = owners[0]
            call(owner_env, 'kill-server')
            proc.wait(timeout=10)
            count = len(payloads())
            remote('working')
            wait(8)
            remote('blocked')
            wait(8)
            failover = len(payloads()) - count
    for p, e, log in servers:
        if p.poll() is None:
            call(e, 'kill-server')
            p.wait(timeout=10)
        log.close()
    servers = []
    e = start('local')
    call(e, 'send', '--pane', '0', str(w / 'bin/codex'))
    wait(2)
    call(e, 'rename', '--pane', '0', 'bee-review')
    call(e, 'rename', '--window', '1', 'imkr')
    local = []
    for state in ['working', 'blocked', 'blocked', 'failed', 'working', 'idle']:
        count = len(payloads())
        call(e, 'status', state, '--pane', '0', stdin=json.dumps({'error': 'rate_limit'}))
        wait(1.2)
        local.append((state, payloads()[count:]))
    count = len(payloads())
    call(e, 'notify', '--title', 'User title <literal>', 'User CWD/JT $(touch USER_INJECTED)')
    wait(0.5)
    manual = payloads()[count:]
    off = start('off', mode='off')
    count = len(payloads())
    call(off, 'send', '--pane', '0', str(w / 'bin/claude'))
    wait(2)
    for state in ['working', 'blocked', 'failed', 'working', 'idle']:
        call(off, 'status', state, '--pane', '0')
        wait(0.7)
    call(off, 'notify', 'off control')
    wait(0.5)
    off_count = len(payloads()) - count
    term = start('terminal', mode='terminal')
    pane = call(term, 'new', '--window', '2', '--', str(w / 'bin/claude'))
    wait(2)
    pid, fd = pty.fork()
    if pid == 0:
        os.execve(str(BIN), [str(BIN), 'attach'], term)

    def drain(seconds):
        end = time.monotonic() + seconds
        data = b''
        while time.monotonic() < end:
            if select.select([fd], [], [], 0.1)[0]:
                try:
                    data += os.read(fd, 1000000)
                except OSError:
                    break
        return data
    drain(1)
    raw = b''
    for state in ['working', 'blocked', 'failed', 'working', 'idle']:
        call(term, 'status', state, '--pane', pane)
        raw += drain(1.2)
    call(term, 'notify', '--title', 'User literal', 'terminal literal')
    raw += drain(0.7)
    terminal = [x.decode() for x in re.findall(b'\\x1b\\]9;([^\\x07]*)\\x07', raw)]
    os.kill(pid, signal.SIGTERM)
    os.waitpid(pid, 0)
    os.close(fd)
    result = dict(failover=failover, off_count=off_count, terminal=terminal, binary=str(BIN), remote_counts=dict(blocked=len(blocked), repeat=len(repeat), failed=len(failed), done=len(done)), remote_samples=dict(blocked=blocked[:2], failed=failed[:2], done=done[:2]), local=local, manual=manual)
    assert off_count == 0, result
    assert [len(x[1]) for x in local] == [0, 1, 0, 1, 0, 1], local
    assert manual == [['--app-name=imkr', 'User title <literal>', 'User CWD/JT $(touch USER_INJECTED)']], manual
    assert len(terminal) == 4, terminal
    if not args.controls_only:
        assert result['remote_counts'] == dict(blocked=15 * args.copies, repeat=0, failed=15 * args.copies, done=15 * args.copies), result
        assert failover == (15 if args.copies == 1 else None), result
        assert all(('RAW SCREEN' not in str(x) and 'TITLE_INJECTED' not in str(x) for x in blocked + failed + done))
        if args.copies == 1:
            for events in [blocked, failed, done]:
                assert len({tuple(x) for x in events}) == 15, events
    (w / 'result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))
    print('Artifacts:', w)
finally:
    for p, e, log in servers:
        try:
            call(e, 'kill-server')
            p.wait(timeout=10)
        except Exception:
            p.terminate()
        log.close()
