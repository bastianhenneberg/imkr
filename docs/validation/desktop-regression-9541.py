"""Isolated overdue-mail and OSC regression probe using real server adoption.

Run: python3 desktop-regression-9541.py BINARY LABEL [--fresh]
The child wrapper owns its PTY child before execing imkr. No live sockets,
configs, desktop command, network or real agent are used. Mail timestamps are
seeded in the supported handover format; no production timer is shortened.
"""
import argparse
import hashlib
import json
import os
import pathlib
import pty
import shlex
import socket
import struct
import subprocess
import sys
import tempfile
import time
import threading
import re


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


def spawn(binary, world, age):
    """Exec the server in the same process that created its PTY child."""
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(str(world / 'session.sock'))
    listener.listen()
    client_fd = int(os.environ.get('REGRESSION_CLIENT_FD', '-1'))
    if client_fd >= 0: os.set_inheritable(client_fd, False)
    pid, master = pty.fork()
    if pid == 0:
        os.execve(str(world / 'bin/codex'), [str(world / 'bin/codex')], os.environ)
    for fd in [listener.fileno(), master] + ([client_fd] if client_fd >= 0 else []):
        os.set_inheritable(fd, True)
    then = int(time.time()) - age
    mails = [dict(seq=i, pane=0, **{'from': 'review'}, text=f'private fixture {i}', at=then)
             for i in range(1, 16)]
    notes = [dict(at=then, seq=m['seq'], pane=0, peer='review', sent=False,
                  text=m['text'], what='message') for m in mails]
    saved = dict(version=1, active=1,
                 workspaces={'1': dict(layout=dict(root=dict(Leaf=0)), floats=[],
                                       focus=0, last_tiled=0, name='fixture-project', zoomed=None)},
                 panes={'0': dict(cwd=str(world), agent='codex', session=None, name='fixture-bee')})
    handed = dict(listener=listener.fileno(), client=None, screen=[100, 24],
                  caps=dict(graphics=False, cell_width=0, cell_height=0),
                  focus_mode=False, saved=saved, mail=mails, mail_log=notes,
                  panes={'0': dict(fd=master, pid=pid, rows=24, cols=100, history='', screen=[],
                                   reported='working', takes_mail=False)})
    if client_fd >= 0:
        handed['clients'] = [dict(fd=client_fd, size=[100, 24], caps=handed['caps'], workspace=1)]
    handover = world / 'handover.json'
    handover.write_text(json.dumps(handed))
    env = os.environ.copy()
    env['IMKR_UPGRADE'] = str(handover)
    os.execve(str(binary), [str(binary), 'server'], env)


if len(sys.argv) > 1 and sys.argv[1] == '--spawn':
    spawn(pathlib.Path(sys.argv[2]), pathlib.Path(sys.argv[3]), int(sys.argv[4]))
    raise AssertionError('exec returned')

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('binary', type=pathlib.Path)
parser.add_argument('label')
parser.add_argument('--fresh', action='store_true')
parser.add_argument('--terminal', action='store_true')
parser.add_argument('--redelivery', action='store_true')
parser.add_argument('--expect-first', type=int)
parser.add_argument('--expect-replay', type=int)
args = parser.parse_args()
binary = args.binary.resolve()
world = pathlib.Path(tempfile.mkdtemp(prefix='imkr-reg9541-'))
for name in ['home', 'runtime', 'state', 'bin']:
    (world / name).mkdir()
env = {k: v for k, v in os.environ.items()
       if not k.startswith(('IMKR', 'CLAUDE', 'CODEX', 'AI_BRAIN', 'SSH'))}
env.update(HOME=str(world/'home'), XDG_CONFIG_HOME=str(world/'home/config'),
           XDG_STATE_HOME=str(world/'state'), XDG_RUNTIME_DIR=str(world/'runtime'),
           PATH=str(world/'bin')+':/usr/bin:/bin', SHELL='/bin/bash',
           IMKR_CONFIG=str(world/'config.toml'), IMKR_SOCKET=str(world/'session.sock'),
           IMKR_SESSION='regression', IMKR_HOSTNAME='private-regression',
           REGRESSION_WORLD=str(world))
isolate_desktop(env, world)
(world/'config.toml').write_text(('notifications = "terminal"\n' if args.terminal else 'notifications = "system"\n') + 'brain_inbox = false\nweb = false\n'
                               'checkpoints = false\nrevive_sessions = false\nhosts = []\n')


def executable(name, code):
    path = world/'bin'/name
    path.write_text(code)
    path.chmod(0o755)


executable('notify-send', '''#!/usr/bin/python3
import os,sys,json,fcntl
with open(os.environ['REGRESSION_WORLD']+'/argv.jsonl','a') as f:
 fcntl.flock(f,fcntl.LOCK_EX);f.write(json.dumps(sys.argv[1:])+"\\n")
''')
for name in ['ssh', 'curl', 'tailscale', 'ai-brain-cli', 'ai-brain-harness']:
    executable(name, '#!/bin/sh\nexit 1\n')
osc = (r'\033]0;Claude title JT $(touch OSC_EXECUTED)\007'
       r'\033]2;JT from OSC2\007'
       r'\033]9;raw terminal notification\007'
       r'\033]9;4;1;75\007'
       r'\033]9;9;/tmp/CWD $(touch OSC_EXECUTED)\007'
       r'\033]7;file://localhost/tmp/CWD\007'
       r'\033]1337;CurrentDir=/tmp/CWD\007'
       r'\033]777;notify;terminal title;terminal body\007')
executable('codex', '#!/bin/bash\nwhile read -r line; do\n'
           ' if [[ "$line" == "emit-oscs" ]]; then printf "%b" '+shlex.quote(osc)+'; fi\ndone\n')

check_guard(env, world/'bin'/'notify-send')


def read_exact(conn, n):
    data = b''
    while len(data) < n:
        chunk = conn.recv(n-len(data))
        if not chunk:
            raise RuntimeError('server closed the private connection')
        data += chunk
    return data


def request(body, tag=4):
    with socket.socket(socket.AF_UNIX) as conn:
        conn.settimeout(10)
        conn.connect(env['IMKR_SOCKET'])
        if tag == 4:
            if isinstance(body, str):
                body = {'cmd': body.lower()}
            else:
                name, fields = next(iter(body.items()))
                name = 'take-mail' if name == 'TakeMail' else name.lower()
                body = dict(cmd=name, **fields)
        payload = json.dumps(body).encode() if tag == 4 else b''
        conn.sendall(bytes([tag])+struct.pack('>I', len(payload))+payload)
        if tag != 4:
            return None
        header = read_exact(conn, 5)
        assert header[0] == 4, header
        reply = json.loads(read_exact(conn, struct.unpack('>I',header[1:])[0]))
        if 'Err' in reply:
            raise RuntimeError(reply['Err'])
        return reply['Ok']


def notices():
    path = world/'argv.jsonl'
    return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []


def mail():
    return request({'Mail': {'who': None}})


raw = bytearray()
client = peer = None
pass_fds = ()
if args.terminal:
    client, peer = socket.socketpair()
    env['REGRESSION_CLIENT_FD'] = str(client.fileno())
    pass_fds = (client.fileno(),)
    def collect():
        while True:
            try: chunk = peer.recv(65536)
            except OSError: return
            if not chunk: return
            raw.extend(chunk)
    threading.Thread(target=collect, daemon=True).start()

def oscs():
    return [value.decode() for value in re.findall(rb'\x1b\]9;([^\x07]*)\x07', bytes(raw))]

log = open(world/'server.log', 'w')
server = subprocess.Popen([sys.executable, str(pathlib.Path(__file__).resolve()), '--spawn',
                           str(binary), str(world), '0' if args.fresh else '601'],
                          env=env, stdout=log, stderr=log, pass_fds=pass_fds)
if client: client.close()
try:
    for _ in range(100):
        try:
            panes = request('List')
            break
        except (OSError, RuntimeError):
            if server.poll() is not None:
                raise RuntimeError((world/'server.log').read_text())
            time.sleep(.05)
    else:
        raise RuntimeError('private server did not answer')
    time.sleep(2)
    first = notices()
    initial_oscs = oscs()
    first_mail = mail()
    # Same waiting process and unchanged mail: no new event after another tick.
    time.sleep(1)
    repeated = notices()[len(first):]
    before = len(notices())
    before_oscs = len(oscs())
    request({'Send': {'pane': 0, 'text': 'emit-oscs', 'enter': True}})
    time.sleep(2)
    osc_notices = notices()[before:]
    injected_oscs = oscs()[before_oscs:]
    osc_list = request('List')
    # Only the explicit private socket is upgraded; no session enumeration.
    before = len(notices())
    before_oscs = len(oscs())
    request({'Upgrade': {'exe': str(binary)}})
    time.sleep(3)
    replay = notices()[before:]
    after_mail = mail()
    replay_oscs = oscs()[before_oscs:]
    taken = request({'TakeMail': {'pane': 0}})
    time.sleep(1)
    drained = mail()
    again = []
    again_notes = 0
    final_waiting = len(drained['waiting'])
    if args.redelivery:
        before = len(notices())
        request({'Status': {'pane': 0, 'state': None}})
        time.sleep(2)
        again = notices()[before:]
        after_again = mail()
        again_notes = sum(n['what']=='again' for n in after_again['notes'])
        final_waiting = len(after_again['waiting'])
    result = dict(label=args.label, binary=str(binary), sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                  artifact_directory=str(world), age=0 if args.fresh else 601,
                  terminal=args.terminal, first=first, repeated=repeated, osc_notices=osc_notices,
                  initial_oscs=initial_oscs, injected_oscs=injected_oscs, replay_oscs=replay_oscs,
                  after_again=again, again_notes=again_notes, final_waiting=final_waiting,
                  osc_title=osc_list[0].get('title'), update_replay=replay,
                  initial_waiting=len(first_mail['waiting']), after_waiting=len(after_mail['waiting']),
                  initial_overdue=sum(n['what']=='overdue' for n in first_mail['notes']),
                  after_overdue=sum(n['what']=='overdue' for n in after_mail['notes']),
                  drained_waiting=len(drained['waiting']), taken=len(taken['messages']))
    assert result['initial_waiting'] == result['after_waiting'] == result['taken'] == 15, result
    assert result['drained_waiting'] == 0 and not repeated and not osc_notices, result
    assert not injected_oscs, injected_oscs
    assert not (world/'OSC_EXECUTED').exists() and not (pathlib.Path.cwd()/'OSC_EXECUTED').exists()
    if args.expect_first is not None: assert len(first) == args.expect_first, result
    if args.expect_replay is not None: assert len(replay) == args.expect_replay, result
    if args.terminal:
        assert len(initial_oscs) == len(replay_oscs) == (0 if args.fresh else 15), result
    if args.redelivery:
        assert again_notes == final_waiting == 15, result
        delivery = [argv for argv in again if 'message delivery delayed' in argv[-2]]
        assert len(delivery) == 1, result
    (world/'result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))
finally:
    if server.poll() is None:
        try:
            request(None, tag=2)
            server.wait(timeout=10)
        except (OSError, RuntimeError, subprocess.TimeoutExpired):
            server.terminate()
            server.wait(timeout=10)
    log.close()
    if peer: peer.close()
