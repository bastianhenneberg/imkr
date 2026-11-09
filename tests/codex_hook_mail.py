#!/usr/bin/env python3
"""Real Codex #1358: mail reaches a working Codex through its own hooks.
Usage: cargo build && python3 tests/codex_hook_mail.py --codex /absolute/path/to/codex
Real Codex frontend against a local dummy Responses provider: no login, no
account, no real ~/.codex. `imkr hooks --install` runs in a private HOME and
CODEX_HOME. Desktop isolation: notifications off, PATH notify-send shim,
private DBus/runtime. Evidence stays in a fresh temp directory, printed first.
"""
import argparse
import base64
import collections
import http.server
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import tempfile
import threading
import time
import tomllib


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', default='target/debug/imkr')
    parser.add_argument('--codex', required=True)
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve())
    codex = str(Path(args.codex).resolve())
    root = Path(tempfile.mkdtemp(prefix='imkr-hook-mail-1358-'))
    print(f'Evidence: {root}', flush=True)
    for name in ['home/.codex', 'config', 'state', 'runtime', 'work', 'bin']:
        (root / name).mkdir(parents=True, exist_ok=True)
    # All shields are established before any application/PTY starts.
    (root / 'imkr.toml').write_text('notifications = "off"\n')
    notifier = root / 'bin/notify-send'
    notifier.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$HOOK_MAIL_TEST_ROOT/notify.log"\n')
    notifier.chmod(0o755)
    (root / 'notify.log').touch()
    # The hook script runs the `imkr` on the PATH: this build, not an installed one.
    (root / 'bin/imkr').symlink_to(binary)
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(('IMKR', 'CODEX', 'CLAUDE', 'ANTHROPIC', 'OPENAI', 'DBUS_'))
           and k not in ('DISPLAY', 'WAYLAND_DISPLAY', 'XAUTHORITY', 'NO_COLOR')}
    env.update(HOME=str(root / 'home'), CODEX_HOME=str(root / 'home/.codex'),
               XDG_CONFIG_HOME=str(root / 'config'), XDG_STATE_HOME=str(root / 'state'),
               XDG_RUNTIME_DIR=str(root / 'runtime'), IMKR_CONFIG=str(root / 'imkr.toml'),
               SHELL='/bin/bash', TERM='xterm-256color', HOOK_MAIL_TEST_ROOT=str(root),
               PATH=str(root / 'bin') + ':' + os.environ['PATH'],
               DBUS_SESSION_BUS_ADDRESS='unix:path=' + str(root / 'no-session-bus'),
               DBUS_SYSTEM_BUS_ADDRESS='unix:path=' + str(root / 'no-system-bus'))
    assert tomllib.loads((root / 'imkr.toml').read_text())['notifications'] == 'off'
    assert Path(shutil.which('notify-send', path=env['PATH'])).resolve() == notifier.resolve()
    assert Path(shutil.which('imkr', path=env['PATH'])).resolve() == Path(binary)
    subprocess.run(['notify-send', 'guard self-check'], env=env, check=True)
    assert (root / 'notify.log').read_text().strip() == 'guard self-check'
    (root / 'notify.log').write_text('')

    received = collections.Counter()
    turns = []
    release = threading.Event()
    started = threading.Event()

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b'{"data":[]}')

        def do_POST(self):
            data = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            with (root / 'requests.jsonl').open('a') as log:
                log.write(json.dumps(data) + '\n')
            # Only what came in since the last answer: the new prompt, a
            # hook's context or a hook prompt, not the whole history.
            items = data.get('input', [])
            last = max((i for i, m in enumerate(items) if m.get('role') == 'assistant'),
                       default=-1)
            text = '\n'.join(c.get('text', '') for m in items[last + 1:]
                             if m.get('role') in ('user', 'developer')
                             for c in m.get('content', []) if isinstance(c, dict))
            markers = re.findall(r'MARKER_[A-Z0-9_]+', text)
            # Title-generation requests have no tools; they are not agent turns.
            turn = bool(data.get('tools'))
            if turn:
                received.update(markers)
                turns.append(text)
            if turn and 'MARKER_BUSY' in markers:
                started.set()
                release.wait(30)
            answer = 'ACK ' + ','.join(markers)
            item = {'type': 'message', 'id': 'msg_test', 'role': 'assistant',
                    'status': 'completed', 'content': [
                        {'type': 'output_text', 'text': answer, 'annotations': []}]}
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.end_headers()
            for event in [
                {'type': 'response.created', 'response': {'id': 'resp_test'}},
                {'type': 'response.output_item.added', 'output_index': 0,
                 'item': dict(item, content=[])},
                {'type': 'response.output_text.delta', 'item_id': 'msg_test',
                 'output_index': 0, 'content_index': 0, 'delta': answer},
                {'type': 'response.output_item.done', 'output_index': 0, 'item': item},
                {'type': 'response.completed', 'response': {'id': 'resp_test',
                 'status': 'completed', 'output': [item], 'usage': {
                     'input_tokens': 10, 'output_tokens': 10, 'total_tokens': 20}}},
            ]:
                self.wfile.write(('event: ' + event['type'] + '\ndata: '
                                  + json.dumps(event) + '\n\n').encode())
                self.wfile.flush()

        def log_message(self, *unused):
            pass

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    (root / 'home/.codex/config.toml').write_text(f'''model = "fake"
model_provider = "localtest"
check_for_update_on_startup = false
[model_providers.localtest]
name = "localtest"
base_url = "http://127.0.0.1:{server.server_port}/v1"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
[projects."{root / 'work'}"]
trust_level = "trusted"
''')

    def run(*cmd, session=None, codex_home=None):
        full = [binary, *(['-s', session] if session else []), *cmd]
        world = dict(env, CODEX_HOME=str(codex_home)) if codex_home else env
        result = subprocess.run(full, env=world, text=True, capture_output=True, timeout=15)
        with (root / 'cli.log').open('a') as log:
            log.write(json.dumps(full[1:]) + '\n' + result.stdout + result.stderr)
        if result.returncode:
            raise AssertionError(result.stderr or result.stdout)
        return result.stdout

    def screen(session):
        return run('read', '--pane', '0', session=session)

    def state(session):
        return json.loads(run('explain', '--pane', '0', '--json', session=session))['state']

    def notes(session):
        return json.loads(run('mail', '--json', session=session))['notes']

    def until(test, label, seconds=20):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if test():
                return
            time.sleep(.2)
        raise AssertionError(f'Timed out: {label}; received={received}')

    def key(session, raw):
        # Raw keys, not `send`: that frames text as a paste for Codex.
        try:
            subprocess.run([binary, 'tap', session + '/0'], env=env, timeout=2, text=True,
                           capture_output=True,
                           input=json.dumps({'input': base64.b64encode(raw).decode()}) + '\n')
        except subprocess.TimeoutExpired:
            pass

    def start(session, trust, codex_home):
        # Codex keeps the trust in its config.toml: one CODEX_HOME each.
        run('new', '--start', '--window', '0', '--dir', str(root / 'work'), '--', codex,
            '--no-daemon', '-C', str(root / 'work'),
            '--dangerously-bypass-approvals-and-sandbox', session=session,
            codex_home=codex_home)
        until(lambda: 'Hooks need review' in screen(session), session + ' hook review')
        (root / (session + '-review.txt')).write_text(screen(session))
        # 2. Trust all and continue / 3. Continue without trusting.
        for raw in [b'\x1b[B'] * (1 if trust else 2) + [b'\r']:
            key(session, raw)
            time.sleep(1)
        until(lambda: 'Ask Codex to do anything' in screen(session), session + ' composer')

    def answered(session, marker):
        until(lambda: 'ACK' in screen(session) and marker in screen(session), marker)
        until(lambda: state(session) in ('idle', 'done'), session + ' idle')

    sessions = ['hooked', 'untrusted']
    try:
        out = subprocess.run([binary, 'hooks', '--install'], env=env, text=True,
                             capture_output=True, timeout=30)
        (root / 'install.txt').write_text(out.stdout + out.stderr)
        assert out.returncode == 0, out.stderr
        assert 'imkr-agent-state.sh' in (root / 'home/.codex/hooks.json').read_text()
        untrusted = root / 'home/.codex-untrusted'
        shutil.copytree(root / 'home/.codex', untrusted)
        start('hooked', trust=True, codex_home=root / 'home/.codex')
        start('untrusted', trust=False, codex_home=untrusted)

        # A first prompt of the user's: its hooks report, `Stop` fetches
        # (nothing yet) and the pane counts as one that takes its mail.
        run('send', '--pane', '0', 'MARKER_FIRST', session='hooked')
        answered('hooked', 'MARKER_FIRST')
        print('PASS: hooks trusted, first turn reported', flush=True)

        # The bug: two messages for a Codex that is working. They are
        # neither held as "working" nor typed, the `Stop` at the end of the
        # turn hands both over, in order, and Codex goes on with them.
        run('send', '--pane', '0', 'MARKER_BUSY', session='hooked')
        assert started.wait(10), 'dummy provider did not receive busy turn'
        until(lambda: state('hooked') == 'working', 'working state')
        run('msg', '--from', 'keeper/0', 'hooked/0', 'MARKER_MAIL_ONE', session='hooked')
        run('msg', '--from', 'keeper/0', 'hooked/0', 'MARKER_MAIL_TWO', session='hooked')
        time.sleep(4)
        assert not received['MARKER_MAIL_ONE'], received
        assert 'MARKER_MAIL' not in screen('hooked'), 'mail was typed into the composer'
        waiting = json.loads(run('mail', '--json', session='hooked'))
        (root / 'busy-mail.json').write_text(json.dumps(waiting, indent=2))
        assert not any(n['what'] == 'held' for n in waiting['notes']), waiting
        release.set()
        until(lambda: received['MARKER_MAIL_TWO'] >= 1, 'mail through Stop hook')
        answered('hooked', 'MARKER_MAIL_TWO')
        hook_turn = next(t for t in turns if 'MARKER_MAIL_ONE' in t)
        assert '<hook_prompt' in hook_turn, hook_turn
        assert hook_turn.index('MARKER_MAIL_ONE') < hook_turn.index('MARKER_MAIL_TWO'), hook_turn
        (root / 'hooked-after-mail.txt').write_text(screen('hooked'))
        kinds = [n['what'] for n in notes('hooked')]
        assert kinds.count('taken') == 2 and 'typed' not in kinds, kinds
        print('PASS: mail for a working Codex came through its Stop hook, in order, '
              'nothing typed', flush=True)

        # An idle Codex fires no hook: after the grace the message is typed,
        # and its UserPromptSubmit confirms it arrived.
        run('msg', '--from', 'keeper/0', 'hooked/0', 'MARKER_IDLE', session='hooked')
        until(lambda: received['MARKER_IDLE'] >= 1, 'idle mail typed', seconds=40)
        answered('hooked', 'MARKER_IDLE')
        until(lambda: 'confirmed' in [n['what'] for n in notes('hooked')], 'confirmed')
        print('PASS: idle Codex gets the message typed, confirmed by its hook', flush=True)

        # A Codex whose hooks were not trusted runs none of them: its mail
        # waits while it works and is typed once it is idle, as before.
        release.clear()
        started.clear()
        run('send', '--pane', '0', 'MARKER_BUSY again', session='untrusted')
        assert started.wait(10), 'dummy provider did not receive busy turn'
        until(lambda: state('untrusted') == 'working', 'untrusted working')
        run('msg', '--from', 'keeper/0', 'untrusted/0', 'MARKER_UNTRUSTED', session='untrusted')
        time.sleep(3)
        assert not received['MARKER_UNTRUSTED'], received
        release.set()
        until(lambda: received['MARKER_UNTRUSTED'] >= 1, 'untrusted mail typed', seconds=40)
        answered('untrusted', 'MARKER_UNTRUSTED')
        kinds = [n['what'] for n in notes('untrusted')]
        assert 'typed' in kinds and 'taken' not in kinds, kinds
        print('PASS: Codex without trusted hooks keeps the typed delivery', flush=True)

        time.sleep(3)
        for marker in ['MARKER_MAIL_ONE', 'MARKER_MAIL_TWO', 'MARKER_IDLE', 'MARKER_UNTRUSTED']:
            assert received[marker] == 1, (marker, received)
        assert (root / 'notify.log').read_text() == '', 'notification attempted'
        (root / 'result.json').write_text(json.dumps({'received': received,
                                                      'notification_calls': 0}, indent=2))
        print('PASS: every message arrived exactly once; fake notifier has zero calls',
              flush=True)
    finally:
        release.set()
        for session in sessions:
            subprocess.run([binary, '-s', session, 'kill-server'], env=env,
                           capture_output=True, timeout=15)
        server.shutdown()
        # Stop only processes carrying this test's marker in their environment.
        for entry in Path('/proc').iterdir():
            if not entry.name.isdigit() or int(entry.name) == os.getpid():
                continue
            try:
                process_env = (entry / 'environ').read_bytes().split(b'\0')
                if ('HOOK_MAIL_TEST_ROOT=' + str(root)).encode() in process_env:
                    os.kill(int(entry.name), signal.SIGTERM)
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                pass


if __name__ == '__main__':
    main()
