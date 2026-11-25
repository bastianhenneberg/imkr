#!/usr/bin/env python3
"""#10254 end to end: a bee's done goes to its keeper's board and not into
its conversation, the keeper hears of it in one line (with the user's next
prompt, or after resting), blocked still comes in whole, and the question
board counts, shows and closes questions. Run after cargo build:

    python3 tests/keeper_board.py [path/to/imkr]

The world and the fake Claude Code are the ones of tests/keeper_report.py:
HOME, XDG_* and runtime in a temporary folder, no IMKR_SOCKET, notifications
off, notify-send a shim, both buses pointed at sockets that do not exist.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from keeper_report import FAKE  # noqa: E402

MACHINE = 'testhost'
DIGEST_AFTER = 20
results = []


def check(label, ok, detail=''):
    results.append((label, ok))
    print(('PASS ' if ok else 'FAIL ') + label + ('' if ok else f'\n     {detail}'), flush=True)


def run(binary):
    with tempfile.TemporaryDirectory(prefix='imkr-10254-') as folder:
        root = Path(folder)
        fake = root / 'bin' / 'claude'
        fake.parent.mkdir()
        fake.write_text(FAKE)
        fake.chmod(0o755)
        config = root / 'config.toml'
        config.write_text(f'notifications = "off"\nname = "{MACHINE}"\n'
                          f'keeper_digest_after = {DIGEST_AFTER}\n')
        notify_log = root / 'intercepted-notifications.log'
        notify = root / 'bin' / 'notify-send'
        notify.write_text('#!/bin/sh\nprintf "%s\\n" intercepted >> "$KEEPER_TEST_NOTIFY_LOG"\n')
        notify.chmod(0o755)
        runtime = root / 'run'
        runtime.mkdir(mode=0o700)
        no_session_bus, no_system_bus = root / 'no-session-bus', root / 'no-system-bus'
        env = {k: v for k, v in os.environ.items()
               if not k.startswith(('IMKR', 'CODEX', 'CLAUDE', 'ANTHROPIC', 'DBUS_', 'TMUX'))
               and k not in ('DISPLAY', 'WAYLAND_DISPLAY', 'XAUTHORITY')}
        env.update(HOME=folder, XDG_STATE_HOME=folder + '/state', XDG_CONFIG_HOME=folder + '/config',
                   XDG_CACHE_HOME=folder + '/cache', XDG_RUNTIME_DIR=str(runtime),
                   IMKR_CONFIG=str(config), IMKR_HOSTNAME=MACHINE, SHELL='/bin/sh',
                   KEEPER_TEST_ROOT=folder, KEEPER_TEST_IMKR=binary,
                   KEEPER_TEST_NOTIFY_LOG=str(notify_log),
                   PATH=str(fake.parent) + ':' + os.environ['PATH'],
                   DBUS_SESSION_BUS_ADDRESS='unix:path=' + str(no_session_bus),
                   DBUS_SYSTEM_BUS_ADDRESS='unix:path=' + str(no_system_bus))
        # The guard, checked before any imkr process starts.
        assert tomllib.loads(config.read_text())['notifications'] == 'off'
        assert shutil.which('notify-send', path=env['PATH']) == str(notify)
        assert not no_session_bus.exists() and not no_system_bus.exists()
        subprocess.run(['notify-send', 'guard self-check'], env=env, check=True, timeout=5)
        assert notify_log.read_text() == 'intercepted\n'
        notify_log.unlink()

        def cli(*args, stdin=None, timeout=20):
            return subprocess.run([binary, *args], env=env, text=True, input=stdin,
                                  capture_output=True, timeout=timeout)

        def observed(name):
            try:
                return json.loads((root / name / 'observed.json').read_text())
            except (FileNotFoundError, json.JSONDecodeError):
                return {'accepted': [], 'hooked': [], 'input': ''}

        def got(name):
            seen = observed(name)
            return seen['accepted'] + seen['hooked']

        def until(predicate, seconds):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if predicate():
                    return True
                time.sleep(.2)
            return predicate()

        def start(session, name, *more):
            done = cli('-s', session, 'new', '--start', *more, '--', str(fake), name)
            assert done.returncode == 0, done.stderr
            panes = json.loads(cli('-s', session, 'list', '--json').stdout)
            pane = next(p['pane'] for p in panes if p.get('program') == 'claude')
            assert until(lambda: (root / name / 'observed.json').exists(), 10), name
            return f'{MACHINE}:{session}/{pane}'

        def tap_text(address, text):
            tap = subprocess.Popen([binary, 'tap', address], env=env, text=True,
                                   stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL)
            tap.stdin.write(json.dumps({'text': text}) + '\n')
            tap.stdin.flush()
            time.sleep(1.5)
            tap.kill()
            tap.wait()

        def do(name, what):
            (root / name / 'do.tmp').write_text(what)
            (root / name / 'do.tmp').replace(root / name / 'do')

        def notes(address, what):
            m = json.loads(cli('mail', '--json', address).stdout or '{}')
            return [n for n in m.get('notes', []) if n.get('what') == what]

        def in_chat(text):
            return [t for t in got('keeper') if text in t]

        def keeper_resting():
            return until(lambda: not observed('keeper').get('working'), 20)

        def q(*args, stdin=None):
            return cli('-s', 'keeper', 'question', *args, stdin=stdin)

        try:
            keeper = start('keeper', 'keeper')
            bee = start('bee', 'bee', '--by', keeper)

            # (1) The bee is done: on the board, not in the conversation.
            cli('msg', bee, 'AUFTRAG-1')
            on_board = until(lambda: len(notes(keeper, 'status')) >= 1, 60)
            check('(1) a done bee stands on the keeper\'s board',
                  on_board, notes(keeper, 'status'))
            time.sleep(3)
            check('(1) and not in its conversation', not in_chat('[imkr] bee'), got('keeper'))
            status = notes(keeper, 'status')
            check('(1) the board keeps what it said',
                  bool(status) and 'ERGEBNIS-1' in status[0]['text'], status)
            shown = cli('mail', keeper).stdout
            check('(1) the message pane shows it', '● bee bee' in shown, shown)

            # (2) The user writes to the keeper: the one line rides along,
            # names and states only.
            keeper_resting()
            tap_text(keeper, 'Wie steht es?\r')
            told = until(lambda: bool(in_chat('bees that stopped')), 20)
            line = in_chat('bees that stopped')
            check('(2) with the user\'s next prompt the keeper gets one line',
                  told and 'bee done' in line[0], got('keeper'))
            check('(2) without what the bee said', told and 'ERGEBNIS' not in line[0], line)

            # (3) Done again, the keeper rests: after keeper_digest_after the
            # line comes by itself, once.
            assert keeper_resting()
            before = len(in_chat('bees that stopped'))
            cli('msg', bee, 'AUFTRAG-2')
            assert until(lambda: len(notes(keeper, 'status')) >= 2, 60), notes(keeper, 'status')
            time.sleep(8)
            check('(3) not at once', len(in_chat('bees that stopped')) == before, got('keeper'))
            came = until(lambda: len(in_chat('bees that stopped')) > before, DIGEST_AFTER + 40)
            check('(3) to a resting keeper after a while, by itself', came, got('keeper'))
            time.sleep(DIGEST_AFTER + 5)
            check('(3) once', len(in_chat('bees that stopped')) == before + 1, in_chat('bees'))

            # (4) Blocked still comes in whole.
            assert keeper_resting()
            do('bee', 'block')
            came = until(lambda: bool(in_chat('is blocked and asks')), 60)
            check('(4) a blocked bee still reaches the conversation', came,
                  f'{got("keeper")[-2:]} / {notes(keeper, "status")[-1:]}')

            # (5) The question board.
            first = q('add', '--from', 'bee bee', '--recommend', 'ja, pushen',
                      'Darf die Biene den Zweig pushen?')
            second = q('add', '--ref', 'F1', '--stdin', stdin='Und danach mergen?\n')
            check('(5) questions get F1, F2',
                  first.stdout.strip() == 'F1' and second.stdout.strip() == 'F2',
                  (first.stdout, first.stderr, second.stdout, second.stderr))
            ls = json.loads(cli('ls', '--json').stdout)
            count = next((s.get('questions', 0) for s in ls if s['name'] == 'keeper'), None)
            check('(5) ls --json counts the open ones', count == 2, ls)
            pane = q('pane')
            panes = json.loads(cli('-s', 'keeper', 'list', '--json').stdout)
            board = next((p['pane'] for p in panes if p.get('name') == 'questions'), None)
            check('(5) question pane opens a pane named questions', board is not None,
                  (pane.stdout, pane.stderr, panes))
            shows = lambda text: text in cli('read', f'{MACHINE}:keeper/{board}').stdout
            check('(5) it shows the open questions',
                  board is not None and until(lambda: shows('F1') and shows('→ ja, pushen')
                                              and shows('see F1'), 10),
                  cli('read', f'{MACHINE}:keeper/{board}').stdout)
            q('answer', 'F1', 'Ja, pushen, aber nicht mergen.')
            q('drop', 'F2', '--by', 'F1')
            check('(5) answered and dropped ones leave the pane',
                  until(lambda: shows('none open') and not shows('F1'), 10),
                  cli('read', f'{MACHINE}:keeper/{board}').stdout)
            everything = json.loads(q('ls', '--all', '--json').stdout)
            check('(5) the answer is kept word for word',
                  everything[0].get('answer') == 'Ja, pushen, aber nicht mergen.'
                  and everything[1].get('state') == 'superseded', everything)
            ls = json.loads(cli('ls', '--json').stdout)
            count = next((s.get('questions', 0) for s in ls if s['name'] == 'keeper'), None)
            check('(5) and the count goes back to none', count == 0, ls)
            bad = q('answer', 'F9', 'x')
            check('(5) an unknown number is refused', bad.returncode != 0 and 'F9' in bad.stderr,
                  (bad.stdout, bad.stderr))
        finally:
            for session in ('bee', 'keeper'):
                cli('-s', session, 'kill-server')
            assert not notify_log.exists(), 'a notification reached the shim'
    failed = [label for label, ok in results if not ok]
    print(f'{len(results) - len(failed)} passed, {len(failed)} failed')
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(run(str(Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/imkr').resolve())))
