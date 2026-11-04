#!/usr/bin/env python3
"""Isolated #1339 integration regression. Run after cargo build; stdlib only.
No login, network endpoint, real config, or existing imkr server is used.
Desktop isolation: notifications off, PATH notify-send shim, private DBus/runtime.
"""
import json
import os
import shutil
import tomllib
from pathlib import Path
import subprocess
import sys
import tempfile
import time

FAKE = r'''#!/usr/bin/python3
import json, os, select, sys, tty, time
from pathlib import Path
root = Path(os.environ['MAIL_TEST_ROOT'])
tty.setraw(0)
buf = ''; history = []; mode = ''; cr = 0; accepted = []
if (root/'observed.json').exists():
    previous = json.loads((root/'observed.json').read_text())
    accepted = previous['accepted']; cr = previous['enters']
def save():
    (root/'observed.json').write_text(json.dumps({'accepted': accepted, 'enters': cr, 'input': buf}))
def accept():
    global buf
    if buf:
        accepted.append(buf); history.append('› '+buf); buf = ''
    save()
def draw():
    h, w = os.get_terminal_size(0).lines, os.get_terminal_size(0).columns
    # Keep the most recent submitted text inside the old eight-line tail.
    text = history[-1][-max(1,w*4):] if history else 'Fake Codex ready'
    out = f'\x1b[2J\x1b[{h-10};1H'+text
    if mode == 'restart':
        out += '\r\nRestarting fake Codex'
    elif mode == 'approval':
        out += '\r\nApproval required\r\n› 1. Yes\r\n  2. No\x1b[3;3H'
    else:
        shown = 'a real draft' if mode == 'draft' else buf
        if mode == 'paste': shown = '[Pasted Content 17321 chars]'
        if not shown: shown = '\x1b[2mAsk Codex to do anything\x1b[0m'
        out += f'\x1b[{h-6};1H› {shown}\x1b[{h-3};1H  GPT-6 · fake\x1b[{h-2};1H  ? for shortcuts\x1b[{h-6};3H'
    sys.stdout.write(out); sys.stdout.flush()
while True:
    new_mode = (root/'mode').read_text().strip()
    if new_mode != mode:
        mode = new_mode
        if mode == 'accept': accept()
        draw(); save()
        if mode == 'restart':
            time.sleep(1)
            (root/'mode').write_text('normal')
            os.execv(sys.executable, [sys.executable, __file__])
    if mode == 'working': draw()
    if select.select([0], [], [], 0.05)[0]:
        data = os.read(0, 65536).decode()
        for ch in data:
            if ch == '\r':
                cr += 1
                if mode not in ('swallow','approval','draft','paste'): accept()
                save(); draw()
            elif ch >= ' ': buf += ch
        save()
'''


def run(binary):
    with tempfile.TemporaryDirectory(prefix='imkr-mail-stall-') as folder:
        root = Path(folder)
        fake = root / 'codex'
        fake.write_text(FAKE)
        fake.chmod(0o755)
        (root / 'mode').write_text('normal')
        (root / 'config.toml').write_text('notifications = "off"\n')
        # Save all desktop guards before starting any app/PTY process.
        notify_log = root / 'intercepted-notifications.log'
        notify = root / 'notify-send'
        notify.write_text('#!/bin/sh\nprintf \"%s\\n\" intercepted >> \"$MAIL_TEST_NOTIFY_LOG\"\n')
        notify.chmod(0o755)
        runtime = root / 'runtime'; runtime.mkdir(mode=0o700)
        blocked_session = root / 'no-session-bus'
        blocked_system = root / 'no-system-bus'
        env = {k: v for k, v in os.environ.items()
               if not k.startswith(('IMKR', 'CODEX', 'CLAUDE', 'ANTHROPIC', 'DBUS_'))
               and k not in ('DISPLAY', 'WAYLAND_DISPLAY', 'XAUTHORITY')}

        env.update(HOME=folder, XDG_STATE_HOME=folder, XDG_CONFIG_HOME=folder,
                   IMKR_SOCKET=str(root / 'server.sock'), IMKR_CONFIG=str(root / 'config.toml'),
                   MAIL_TEST_ROOT=folder, MAIL_TEST_NOTIFY_LOG=str(notify_log),
                   PATH=folder+':'+os.environ['PATH'], XDG_RUNTIME_DIR=str(runtime),
                   DBUS_SESSION_BUS_ADDRESS='unix:path='+str(blocked_session),
                   DBUS_SYSTEM_BUS_ADDRESS='unix:path='+str(blocked_system))
        assert tomllib.loads((root/'config.toml').read_text())['notifications']=='off'
        assert shutil.which('notify-send',path=env['PATH'])==str(notify)
        assert not blocked_session.exists() and not blocked_system.exists()
        subprocess.run(['notify-send','guard self-check'],env=env,check=True,timeout=5)
        assert notify_log.read_text()=='intercepted\n'
        notify_log.unlink()


        def cli(*args):
            return subprocess.run([binary, *args], env=env, text=True,
                                  capture_output=True, timeout=15)

        def observed():
            try:
                return json.loads((root / 'observed.json').read_text())
            except (FileNotFoundError, json.JSONDecodeError):
                return {'accepted': [], 'enters': 0, 'input': ''}

        def until(predicate, label, seconds=15):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if predicate(): return
                time.sleep(.1)
            raise AssertionError(f'{label}: {observed()}\n{cli("mail", "--json").stdout}')

        def mode(value):
            (root / 'next-mode').write_text(value)
            (root / 'next-mode').replace(root / 'mode')
            time.sleep(.2)

        try:
            started = cli('new', '--start', '--', str(fake))
            assert started.returncode == 0, started.stderr
            panes = json.loads(cli('list', '--json').stdout)
            # Find the fake by program, independent of any initial shell.
            pane = str(next(p['pane'] for p in panes if p.get('program') == 'codex'))
            guarded = cli('notify', '--pane', pane, 'isolated notification guard check')
            assert guarded.returncode == 0, guarded.stderr
            assert not notify_log.exists(), 'notifications off must prevent even shim dispatch'


            def msg(text):
                result = cli('msg', pane, text)
                assert result.returncode == 0, result.stderr

            until(lambda: 'Ask Codex' in cli('read', pane).stdout, 'startup')
            # A fast answer: the fake accepts and finishes within the same
            # read, with no Working hook and text still in the bottom eight.
            msg('FAST-ONE')
            until(lambda: len(observed()['accepted']) == 1, 'first message')
            msg('FAST-TWO')
            until(lambda: len(observed()['accepted']) == 2, 'history must not hold second')
            print('PASS: fast reply leaves history without holding the queue', flush=True)
            for i in range(12): msg(f'QUEUE-{i:02}')
            until(lambda: len(observed()['accepted']) == 14, 'long queue', 40)
            print('PASS: long queue', flush=True)
            accepted = observed()['accepted']
            assert all(f'QUEUE-{i:02}' in accepted[i+2] for i in range(12)), accepted
            assert observed()['enters'] == 14, observed()

            # All these screens must hold new mail without sending an
            # approval, expiring a draft, or typing into a collapsed paste.
            for state in ('draft', 'paste', 'approval', 'working'):
                mode(state)
                msg(f'AFTER-{state}')
                time.sleep(2)
                assert len(observed()['accepted']) == 14, (state, observed())
                assert observed()['enters'] == 14, (state, observed())
            print('PASS: drafts, collapsed paste, approval, working all hold mail', flush=True)
            mode('normal')
            until(lambda: len(observed()['accepted']) == 18, 'working to idle', 20)

            # Swallowed Enter keeps its input through retries and a live
            # server handover. Queue must not append to it or duplicate it.
            mode('swallow')
            msg('STUCK-INPUT')
            until(lambda: observed()['enters'] == 19, 'stuck input')
            msg('AFTER-STUCK')
            time.sleep(4)
            assert observed()['enters'] == 19, observed()
            updated = cli('update')
            assert updated.returncode == 0, updated.stderr
            time.sleep(3)
            assert observed()['enters'] == 19, observed()
            assert 'AFTER-STUCK' not in observed()['input'], observed()
            mode('accept')
            until(lambda: len(observed()['accepted']) == 20, 'handover recovery')
            accepted = observed()['accepted']
            assert 'STUCK-INPUT' in accepted[-2] and 'AFTER-STUCK' not in accepted[-2]
            assert 'AFTER-STUCK' in accepted[-1]
            assert observed()['enters'] == 20, observed()
            mode('restart')
            msg('AFTER-RESTART')
            assert observed()['enters'] == 20, observed()
            until(lambda: len(observed()['accepted']) == 21, 'process restart')
            assert 'AFTER-RESTART' in observed()['accepted'][-1]
            assert observed()['enters'] == 21, observed()
            print('PASS: 21 messages in order; fast reply, queue, draft, paste, approval, working/idle, swallowed Enter, handover and process restart; no extra Enter')
        finally:
            cli('kill-server')
            assert not notify_log.exists(), 'unexpected notification safely intercepted by test shim'


if __name__ == '__main__':
    run(str(Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/imkr').resolve()))
