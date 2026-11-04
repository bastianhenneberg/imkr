#!/usr/bin/env python3
"""Real Codex #1343 regression, stdlib only, local dummy Responses provider.
Usage: python3 tests/codex_submit.py --codex /absolute/path/to/codex
Add --managed for default daemon mode; the default uses --no-daemon.
Evidence is retained in a fresh temp directory, printed at startup.
"""
import argparse
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', default='target/debug/imkr')
    parser.add_argument('--codex', required=True)
    parser.add_argument('--managed', action='store_true')
    parser.add_argument('--daemon-runtime', help='seed this local release directory into private HOME')
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve())
    codex = str(Path(args.codex).resolve())
    root = Path(tempfile.mkdtemp(prefix='imkr-submit-1343-'))
    print(f'Evidence: {root}', flush=True)
    for name in ['home/.codex', 'config', 'state', 'runtime', 'work', 'bin']:
        (root / name).mkdir(parents=True, exist_ok=True)
    if args.daemon_runtime:
        assert args.managed, '--daemon-runtime requires --managed'
        source = Path(args.daemon_runtime).resolve()
        packages = root / 'home/.codex/packages/app-server-daemon'
        destination = packages / 'releases' / source.name
        shutil.copytree(source, destination, symlinks=True)
        (packages / 'current').symlink_to(destination)
        (packages / 'auto-update-version').write_text(source.name)
    # All three shields are established before any application/PTY starts.
    (root / 'imkr.toml').write_text('notifications = "off"\n')
    notifier = root / 'bin/notify-send'
    notifier.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$SUBMIT_TEST_ROOT/notify.log"\n')
    notifier.chmod(0o755)
    (root / 'notify.log').touch()
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(('IMKR', 'CODEX', 'CLAUDE', 'ANTHROPIC', 'OPENAI'))
           and k not in ('DISPLAY', 'WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS', 'NO_COLOR')}
    env.update(HOME=str(root / 'home'), CODEX_HOME=str(root / 'home/.codex'),
               XDG_CONFIG_HOME=str(root / 'config'), XDG_STATE_HOME=str(root / 'state'),
               XDG_RUNTIME_DIR=str(root / 'runtime'), IMKR_CONFIG=str(root / 'imkr.toml'),
               SHELL='/bin/bash', TERM='xterm-256color', SUBMIT_TEST_ROOT=str(root),
               PATH=str(root / 'bin') + ':' + os.environ['PATH'],
               DBUS_SESSION_BUS_ADDRESS='unix:path=/nonexistent/imkr-submit-1343')
    requests = []
    received = collections.Counter()
    received_text = {}
    release = threading.Event()
    started = threading.Event()

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b'{"data":[]}')

        def do_POST(self):
            data = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            requests.append(data)
            with (root / 'requests.jsonl').open('a') as log:
                log.write(json.dumps(data) + '\n')
            users = [m for m in data.get('input', []) if m.get('role') == 'user']
            text = '\n'.join(c.get('text', '') for c in users[-1].get('content', [])) if users else ''
            markers = re.findall(r'MARKER_[A-Z0-9_]+', text)
            # Title-generation requests have no tools; they are not agent turns.
            turn = bool(data.get('tools'))
            if turn:
                received.update(markers)
                for marker in markers:
                    received_text[marker] = text
            if turn and 'MARKER_BUSY' in markers:
                started.set()
                release.wait(20)
            answer = 'ACK ' + ','.join(markers)
            item = {'type': 'message', 'id': 'msg_test', 'role': 'assistant',
                    'status': 'completed', 'content': [
                        {'type': 'output_text', 'text': answer, 'annotations': []}]}
            if turn and 'MARKER_APPROVAL' in markers:
                item = {'type': 'function_call', 'name': 'exec_command',
                        'call_id': 'call_approval', 'id': 'fc_approval',
                        'arguments': json.dumps({'cmd': 'true',
                            'sandbox_permissions': 'require_escalated',
                            'justification': 'Allow this isolated test command?'})}
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.end_headers()
            events = [
                {'type': 'response.created', 'response': {'id': 'resp_test'}},
                {'type': 'response.output_item.added', 'output_index': 0,
                 'item': dict(item, content=[])},
                {'type': 'response.output_text.delta', 'item_id': 'msg_test',
                 'output_index': 0, 'content_index': 0, 'delta': answer},
                {'type': 'response.output_item.done', 'output_index': 0, 'item': item},
                {'type': 'response.completed', 'response': {'id': 'resp_test',
                 'status': 'completed', 'output': [item], 'usage': {
                     'input_tokens': 10, 'output_tokens': 10, 'total_tokens': 20}}},
            ]
            if item['type'] == 'function_call':
                events[1]['item'] = dict(item, arguments='')
                events[2] = {'type': 'response.function_call_arguments.delta',
                    'item_id': item['id'], 'output_index': 0, 'delta': item['arguments']}
            for event in events:
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

    def cli(session, *cmd):
        result = subprocess.run([binary, '-s', session, *cmd], env=env,
                                text=True, capture_output=True, timeout=15)
        with (root / 'cli.log').open('a') as log:
            log.write(json.dumps([session, *cmd]) + '\n' + result.stdout + result.stderr)
        if result.returncode:
            raise AssertionError(result.stderr or result.stdout)
        return result.stdout

    def until(test, label, seconds=15):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if test():
                return
            time.sleep(.1)
        raise AssertionError(f'Timed out: {label}; received={received}')

    def screen(session):
        return cli(session, 'read', '--pane', '0')

    def idle(session):
        return json.loads(cli(session, 'explain', '--pane', '0', '--json'))['state'] in ('idle', 'done')

    def answered(session, marker):
        until(lambda: 'ACK ' + marker in screen(session), marker + ' response')
        assert received[marker] == 1, (marker, received)
        (root / (marker + '.txt')).write_text(screen(session))
        until(lambda: idle(session), session + ' idle')

    sessions = ['submit-keeper', 'submit-worker', 'submit-approval']
    try:
        for session in sessions:
            command = [codex]
            if not args.managed:
                command += ['--no-daemon']
            command += ['-C', str(root / 'work')]
            command += (['-a', 'on-request'] if session == 'submit-approval' else
                        ['--dangerously-bypass-approvals-and-sandbox'])
            cli(session, 'new', '--start', '--window', '0', '--dir', str(root / 'work'), '--', *command)
            until(lambda: '? for shortcuts' in screen(session), session + ' startup')
            until(lambda: idle(session), session + ' initial idle')
        if args.managed:
            daemon = json.loads((root / 'home/.codex/app-server-daemon/daemon.pid').read_text())
            executable = os.readlink(f'/proc/{daemon["pid"]}/exe')
            assert str(root) in executable, executable
            if args.daemon_runtime:
                assert Path(args.daemon_runtime).name in executable, executable
            (root / 'runtime.json').write_text(json.dumps({'frontend': codex,
                'daemon_executable': executable, 'pid': daemon['pid']}, indent=2))
        # Direct single-line burst is the failing baseline path: no fallback Enter.
        single = 'MARKER_SINGLELINE ' + 'x' * 1142
        cli(sessions[1], 'send', '--pane', '0', single)
        answered(sessions[1], 'MARKER_SINGLELINE')
        assert received_text['MARKER_SINGLELINE'] == single
        long = 'MARKER_LONG\n' + 'long line 123456789\n' * 800
        cli(sessions[1], 'send', '--pane', '0', long)
        answered(sessions[1], 'MARKER_LONG')
        # Codex strips the terminal trailing newline when submitting a paste.
        assert received_text['MARKER_LONG'] == long.rstrip('\n')
        print('PASS: real frontend submits single-line burst and long bracketed paste', flush=True)
        # Worker reports completion to the keeper; keeper assigns the next job.
        cli(sessions[1], 'msg', '--from', sessions[1] + '/0', sessions[0] + '/0', 'MARKER_COMPLETE')
        answered(sessions[0], 'MARKER_COMPLETE')
        cli(sessions[0], 'msg', '--from', sessions[0] + '/0', sessions[1] + '/0', 'MARKER_FOLLOWUP')
        answered(sessions[1], 'MARKER_FOLLOWUP')
        print('PASS: worker -> keeper completion -> worker continuation', flush=True)
        cli(sessions[1], 'send', '--pane', '0', 'MARKER_BUSY')
        assert started.wait(10), 'dummy provider did not receive busy turn'
        until(lambda: not idle(sessions[1]), 'working state')
        cli(sessions[0], 'msg', sessions[1] + '/0', 'MARKER_AFTER_BUSY')
        time.sleep(1)
        assert not received['MARKER_AFTER_BUSY'], received
        release.set()
        answered(sessions[1], 'MARKER_AFTER_BUSY')
        print('PASS: mail waits during work and submits after idle', flush=True)
        # User draft is left untouched, including across a server handover.
        cli(sessions[1], 'send', '--pane', '0', '--no-enter', 'HUMAN_DRAFT')
        cli(sessions[0], 'msg', sessions[1] + '/0', 'MARKER_DRAFT_HELD')
        cli(sessions[1], 'update')
        time.sleep(3)
        assert not received['MARKER_DRAFT_HELD'], received
        assert 'HUMAN_DRAFT' in screen(sessions[1]), screen(sessions[1])
        (root / 'draft-mail.json').write_text(cli(sessions[1], 'mail', '--json'))
        assert 'MARKER_DRAFT_HELD' in (root / 'draft-mail.json').read_text()
        print('PASS: human draft and pending mail survive server handover without submission', flush=True)
        switch = subprocess.run([binary, '-s', sessions[1], 'account', 'switch',
            'codex:standard', '--pane', '0'], env=env, text=True, capture_output=True, timeout=15)
        (root / 'account-switch.txt').write_text(switch.stdout + switch.stderr)
        assert switch.returncode and 'would throw' in switch.stderr, switch
        assert 'HUMAN_DRAFT' in screen(sessions[1])
        assert not received['MARKER_DRAFT_HELD'], received
        print('PASS: account switch refuses to destroy or submit the human draft', flush=True)
        # A genuine Codex tool approval stays unanswered while mail waits.
        cli(sessions[2], 'send', '--pane', '0', 'MARKER_APPROVAL')
        until(lambda: json.loads(cli(sessions[2], 'explain', '--pane', '0', '--json'))['state']
              == 'blocked', 'real approval dialog')
        before = len(requests)
        cli(sessions[0], 'msg', sessions[2] + '/0', 'MARKER_APPROVAL_HELD')
        cli(sessions[2], 'update')
        time.sleep(3)
        assert len(requests) == before, 'approval was answered'
        assert not received['MARKER_APPROVAL_HELD'], received
        (root / 'approval.txt').write_text(screen(sessions[2]))
        (root / 'approval-mail.json').write_text(cli(sessions[2], 'mail', '--json'))
        assert 'MARKER_APPROVAL_HELD' in (root / 'approval-mail.json').read_text()
        print('PASS: real approval and held mail survive handover without approval submission', flush=True)
        # Shell input retains normal single-line command semantics.
        cli('submit-shell', 'new', '--start', '--window', '0', '--',
            '/bin/bash', '--noprofile', '--norc')
        shell_output = root / 'shell-output'
        cli('submit-shell', 'send', '--pane', '0', f"printf shell-ok > {shell_output}")
        until(lambda: shell_output.exists(), 'shell command')
        assert shell_output.read_text() == 'shell-ok'
        print('PASS: shell single-line command still executes normally', flush=True)
        time.sleep(3)
        assert all(count == 1 for count in received.values()), received
        assert (root / 'notify.log').read_text() == '', 'notification attempted'
        (root / 'result.json').write_text(json.dumps({'received': received, 'mode':
            'managed' if args.managed else 'no-daemon', 'notification_calls': 0}, indent=2))
        print('PASS: exact markers received once; fake notifier has zero calls', flush=True)
    finally:
        release.set()
        for session in [*sessions, 'submit-shell']:
            subprocess.run([binary, '-s', session, 'kill-server'], env=env,
                           capture_output=True, timeout=15)
        server.shutdown()
        # Managed Codex can leave its own daemon/updater in our private HOME.
        # Match /proc environment before stopping only those isolated children.
        children = []
        for entry in Path('/proc').iterdir():
            if not entry.name.isdigit() or int(entry.name) == os.getpid():
                continue
            try:
                process_env = (entry / 'environ').read_bytes().split(b'\0')
                if ('SUBMIT_TEST_ROOT=' + str(root)).encode() in process_env:
                    os.kill(int(entry.name), signal.SIGTERM)
                    children.append(entry)
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                pass
        time.sleep(.5)
        for entry in children:
            try:
                # Recheck ownership rather than relying on a possibly reused PID.
                process_env = (entry / 'environ').read_bytes().split(b'\0')
                if ('SUBMIT_TEST_ROOT=' + str(root)).encode() in process_env:
                    os.kill(int(entry.name), signal.SIGKILL)
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                pass
        # Retain requests/screens/logs, not hundreds of MB of downloaded binaries.
        packages = root / 'home/.codex/packages'
        if packages.exists():
            shutil.rmtree(packages)


if __name__ == '__main__':
    main()
