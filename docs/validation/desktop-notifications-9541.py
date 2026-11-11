"""Record real imkr desktop argv using private servers; never send host popups.

Usage: python3 desktop-notifications-9541.py BINARY LABEL [--copies 1|3]
Use --copies 3 for baseline 91fd2ca, --controls-only to skip burst scenarios.
Requires Linux /proc, Python 3 and bash. All network commands are stand-ins.
"""
import argparse
import json
import os
import pathlib
import pty
import re
import select
import signal
import subprocess
import tempfile
import time

def isolate_desktop(env, root):
    """Test shield (CLAUDE.md): no inherited desktop or DBus. These probes need
    notifications = "system", so the fake notify-send in front of PATH is the
    only receiver; check_guard proves it before any imkr process starts."""
    for k in [k for k in env if k.startswith('DBUS_') or k in ('DISPLAY', 'WAYLAND_DISPLAY', 'XAUTHORITY')]:
        del env[k]
    env['DBUS_SESSION_BUS_ADDRESS'] = 'unix:path=' + str(root / 'no-session-bus')
    env['DBUS_SYSTEM_BUS_ADDRESS'] = 'unix:path=' + str(root / 'no-system-bus')


def check_guard(env, fake):
    import shutil
    found = shutil.which('notify-send', path=env['PATH'])
    assert found and pathlib.Path(found).resolve() == fake.resolve(), found
    assert not any(k in env for k in ('DISPLAY', 'WAYLAND_DISPLAY', 'XAUTHORITY'))

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('binary', type=pathlib.Path)
parser.add_argument('label')
parser.add_argument('--copies', type=int, choices=(1, 3), default=1)
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
isolate_desktop(env, w)

def executable(name, text):
    p = w / 'bin' / name
    p.write_text(text)
    p.chmod(493)
executable('notify-send', '#!/usr/bin/python3\nimport os,json,sys,fcntl,time\nworld=os.environ[\'CHECK_WORLD\']\nwith open(world+\'/argv.jsonl\',\'a\') as f:\n fcntl.flock(f,fcntl.LOCK_EX)\n f.write(json.dumps(sys.argv[1:])+"\\n")\nwith open(world+\'/timing.jsonl\',\'a\') as f:\n fcntl.flock(f,fcntl.LOCK_EX)\n f.write(json.dumps({\'at\':time.monotonic(),\'parent\':os.getppid(),\'argv\':sys.argv[1:]})+"\\n")\n')
executable('ssh', '#!/usr/bin/python3\nimport os,sys,json,time,fcntl,hashlib\np=os.environ[\'CHECK_WORLD\']\nif "\'ls\' \'--json\'" in sys.argv[-1] or \'ls --json\' in sys.argv[-1]:\n data=open(p+\'/remote.json\').read()\n print(data)\n with open(p+\'/polls\',\'a\') as f:f.write(\'poll\\n\')\n with open(p+\'/observations.jsonl\',\'a\') as f:\n  fcntl.flock(f,fcntl.LOCK_EX)\n  f.write(json.dumps({\'at\':time.monotonic(),\'parent\':os.getppid(),\'version\':hashlib.sha256(data.encode()).hexdigest()})+"\\n")\nelse: sys.exit(1)\n')
for name in ['curl', 'tailscale', 'ai-brain-cli', 'ai-brain-harness']:
    executable(name, '#!/bin/sh\nexit 1\n')
for name in ['codex', 'claude']:
    executable(name, '#!/bin/bash\nwhile :; do read -r line; done\n')
check_guard(env, w / 'bin' / 'notify-send')
servers = []

def write_remote(data):
    p = w / 'remote.tmp'
    p.write_text(json.dumps(data))
    p.replace(w / 'remote.json')

def remote(state, n=15, split=False):
    agents = [dict(pane=i, program='codex' if i % 2 else 'claude', workspace=1, dir='imkr', state=state if state != 'mixed' else ['idle', 'blocked', 'failed'][i % 3], title='JT $(touch TITLE_INJECTED)', tail='RAW SCREEN') for i in range(n)]
    if split:
        data = [dict(name=name, panes=len(part), windows=1, agent_panes=part) for name, part in [('project-review', agents[:7]), ('other-review', agents[7:])]]
        for session in data:
            for i, agent in enumerate(session['agent_panes']):
                agent['pane'] = i
    else:
        data = [dict(name='project-review', panes=n, windows=1, agent_panes=agents)]
    write_remote(data)

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
    pending_recovery = []
    changed_question = selection_repeat = mixed = boundaries = []
    local_burst = []
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
        data = json.loads((w / 'remote.json').read_text())
        data[0]['agent_panes'][0]['tail'] = 'Which project?\n > 1. First\n   2. Second'
        before = len(payloads())
        write_remote(data)
        wait(8)
        changed_question = payloads()[before:]
        data[0]['agent_panes'][0]['tail'] = 'Which project?\n   1. First\n > 2. Second'
        before = len(payloads())
        write_remote(data)
        wait(8)
        selection_repeat = payloads()[before:]
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
        remote('working')
        wait(8)
        before = len(payloads())
        remote('mixed')
        wait(8)
        mixed = payloads()[before:]
        remote('working', split=True)
        wait(8)
        before = len(payloads())
        remote('blocked', split=True)
        wait(8)
        boundaries = payloads()[before:]
        remote('working')
        wait(8)
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
        if args.copies == 1:
            remote('working'); wait(8)
            owner = next(proc for proc, _, _ in servers if proc.poll() is None and
                         any('desktop-notifiers' in os.readlink(fd) for fd in pathlib.Path(f'/proc/{proc.pid}/fd').iterdir()))
            before = len(payloads()); triggered = time.monotonic(); remote('failed')
            version = __import__('hashlib').sha256((w / 'remote.json').read_bytes()).hexdigest()
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                observations = [json.loads(x) for x in (w / 'observations.jsonl').read_text().splitlines()]
                if any(x['parent'] == owner.pid and x['version'] == version and x['at'] >= triggered for x in observations): break
                wait(.02)
            else: raise AssertionError('owner did not observe pending failure')
            # Only this private Popen process is killed; no process-name matching.
            wait(.6)
            assert len(payloads()) == before, 'owner emitted before injected crash'
            owner.kill(); owner.wait(timeout=10)
            wait(8)
            pending_recovery = payloads()[before:]
    for p, e, log in servers:
        if p.poll() is None:
            call(e, 'kill-server')
            p.wait(timeout=10)
        log.close()
    servers = []
    if not args.controls_only:
        targets = []
        for name in ['local-a', 'local-b', 'local-c']:
            local_env = start(name)
            for window in range(2, 7):
                pane = call(local_env, 'new', '--window', str(window), '--', str(w / 'bin/codex'))
                call(local_env, 'status', 'working', '--pane', pane)
                targets.append((local_env, pane))
        wait(4)
        before = len(payloads())
        for local_env, pane in targets:
            call(local_env, 'status', 'blocked', '--pane', pane)
        wait(3)
        local_burst = payloads()[before:]
        before = len(payloads())
        for local_env, pane in targets:
            call(local_env, 'status', 'blocked', '--pane', pane)
        wait(3)
        assert len(payloads()) == before, 'repeated local questions'
    e = start('local')
    call(e, 'send', '--pane', '0', str(w / 'bin/codex'))
    wait(4)
    call(e, 'rename', '--pane', '0', 'bee-review')
    call(e, 'rename', '--window', '1', 'imkr')
    local = []
    for state in ['working', 'blocked', 'blocked', 'failed', 'working', 'idle']:
        count = len(payloads())
        call(e, 'status', state, '--pane', '0', stdin=json.dumps({'error': 'rate_limit'}))
        wait(2.2)
        local.append((state, payloads()[count:]))
    count = len(payloads())
    call(e, 'notify', '--title', 'User title <literal>', 'User CWD/JT $(touch USER_INJECTED)')
    wait(0.5)
    manual = payloads()[count:]
    exit_env = start('exit-error')
    call(exit_env, 'send', '--pane', '0', str(w / 'bin/codex')); wait(4)
    before = len(payloads())
    call(exit_env, 'status', 'failed', '--pane', '0'); wait(.6)
    call(exit_env, 'kill-server'); wait(.5)
    exit_failure = payloads()[before:]
    assert len(exit_failure) == 1, exit_failure
    if args.copies == 1: assert exit_failure[0][1] == '--urgency=critical', exit_failure
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
    result = dict(exit_failure=exit_failure, pending_recovery=pending_recovery, changed_question=changed_question, selection_repeat=selection_repeat, mixed=mixed, session_boundaries=boundaries, local_burst=local_burst, failover=failover, off_count=off_count, terminal=terminal, binary=str(BIN), remote_counts=dict(blocked=len(blocked), repeat=len(repeat), failed=len(failed), done=len(done)), remote_samples=dict(blocked=blocked[:2], failed=failed[:2], done=done[:2]), local=local, manual=manual)
    assert off_count == 0, result
    assert [len(x[1]) for x in local] == [0, 1, 0, 1, 0, 1], local
    assert manual == [['--app-name=imkr', 'User title <literal>', 'User CWD/JT $(touch USER_INJECTED)']], manual
    assert len(terminal) == 4, terminal
    if not args.controls_only:
        assert result['remote_counts'] == dict(blocked=45 if args.copies == 3 else 1, repeat=0, failed=45 if args.copies == 3 else 1, done=45 if args.copies == 3 else 1), result
        assert failover == (1 if args.copies == 1 else None), result
        assert all(('RAW SCREEN' not in str(x) and 'TITLE_INJECTED' not in str(x) for x in blocked + failed + done))
        if args.copies == 1:
            assert len(pending_recovery) == 1 and pending_recovery[0][1] == '--urgency=critical', pending_recovery
            assert len(changed_question) == 1, changed_question
            assert not selection_repeat, selection_repeat
            assert len(mixed) == 3, mixed
            assert [x[1] for x in mixed] == ['--urgency=critical', '--urgency=normal', '--urgency=low'], mixed
            assert {x[1] for x in mixed} == {'--urgency=critical', '--urgency=normal', '--urgency=low'}, mixed
            assert len(boundaries) == 2, boundaries
            assert len(local_burst) == 3, local_burst
            for events in [blocked, failed, done]:
                assert events[0][-2].startswith('15 bees'), events
                assert len(events[0][-1].splitlines()) == 16, events
            assert blocked[0][1] == '--urgency=normal'
            assert failed[0][1] == '--urgency=critical'
            assert done[0][1] == '--urgency=low'
    assert not (w / 'USER_INJECTED').exists()
    timing_path = w / 'timing.jsonl'
    observations_path = w / 'observations.jsonl'
    timings = [json.loads(x) for x in timing_path.read_text().splitlines()] if timing_path.exists() else []
    observations = [json.loads(x) for x in observations_path.read_text().splitlines()] if observations_path.exists() else []
    delays = []
    for event in timings:
        before = [x for x in observations if x['parent'] == event['parent'] and x['at'] < event['at']]
        if before:
            version = before[-1]['version']
            segment = []
            for observed in reversed(before):
                if observed['version'] != version: break
                segment.append(observed['at'])
            first = min(segment)
            delays.append(dict(title=event['argv'][-2], seconds=round(event['at'] - first, 4)))
    result['remote_receipt_delays'] = delays
    if not args.controls_only and args.copies == 1:
        assert delays and all(1 <= x['seconds'] <= 8 for x in delays), delays
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
