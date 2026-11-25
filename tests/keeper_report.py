#!/usr/bin/env python3
"""Bug #1538 end to end: mail reaches a parked keeper, and a bee tells its
keeper when it stops. Run after cargo build; stdlib only:

    python3 tests/keeper_report.py [path/to/imkr]

Each case prints PASS or FAIL and the run goes on, so that an older binary
shows which cases it fails. Exit status 1 when any failed.

A world of its own: HOME, XDG_* and the runtime directory in a temporary
folder, no IMKR_SOCKET (two sessions find each other by name), a config
with notifications off and a fixed machine name, notify-send replaced by a
shim, the session and system bus pointed at sockets that do not exist.
The agents are a fake Claude Code (Python): it draws an input box between
two rules with the footer under it, and reports its hooks through
`imkr status` the way Claude Code's hook script does.
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

FAKE = r'''#!/usr/bin/python3
import json, os, re, select, subprocess, sys, tty, time
from pathlib import Path
name = sys.argv[1]
root = Path(os.environ['KEEPER_TEST_ROOT']) / name
root.mkdir(exist_ok=True)
imkr = os.environ['KEEPER_TEST_IMKR']
tty.setraw(0)
buf = ''; transcript = ['Fake Claude ready']; accepted = []; hooked = []
working_until = None; working_on = ''; spinner = False; turns = []; jobs = False
def save():
    (root/'observed.json').write_text(json.dumps(
        {'accepted': accepted, 'hooked': hooked, 'input': buf, 'working': working_until is not None}))
def hook(state, event, **more):
    payload = dict(hook_event_name=event, session_id='session-'+name, **more)
    out = subprocess.run([imkr, 'status', state], input=json.dumps(payload),
                         capture_output=True, text=True).stdout.strip()
    return json.loads(out) if out else None
def draw():
    h, w = os.get_terminal_size(0).lines, os.get_terminal_size(0).columns
    out = '\x1b[2J\x1b[H'
    if working_until is not None or spinner:
        status = '✽ Working… (3s · esc to interrupt)'
    elif jobs:
        # Claude Code's done row with jobs left, long enough to wrap.
        status = '✻ Crunched for 1m 32s · done 2:59 PM · 1 shell, 1 monitor, 1 background agent still running'
    else:
        status = '✻ Cooked for 3s'
    rows = -(-len(status) // w)
    # A blank row between prompt and answer, as Claude Code draws them.
    for line in transcript[-max(1, (h - 6 - rows) // 2):]:
        out += line[:w] + '\r\n\r\n'
    out += f'\x1b[{h - 4 - rows};1H' + status
    rule = '─' * min(40, w)
    shown = buf if buf else '\x1b[2mTry "write a test"\x1b[0m'
    out += f'\x1b[{h-4};1H\x1b[38;5;244m{rule}\x1b[39m'
    out += f'\x1b[{h-3};1H❯ {shown}'
    out += f'\x1b[{h-2};1H\x1b[38;5;244m{rule}\x1b[39m'
    footer = '  ⏵⏵ bypass permissions on' + (' · 1 shell, 1 monitor, 1 background agent' if jobs else '')
    out += f'\x1b[{h-1};1H' + footer[:w - 1]
    out += f'\x1b[{h-3};{3 + len(buf)}H'
    sys.stdout.write(out); sys.stdout.flush()
def begin(text, whole=''):
    global working_until, working_on, jobs
    working_on = text
    if 'JOBS' in whole:
        jobs = True
    working_until = time.monotonic() + 3
    transcript.append('> ' + text[:70])
    if 'JOBS' in text:
        jobs = True
    if 'TELL:' in text:
        to = text.split('TELL:')[1].split()[0]
        subprocess.run([imkr, 'msg', to, 'FERTIG, selbst gemeldet von ' + name],
                       capture_output=True, text=True)
    save(); draw()
def submit(text):
    accepted.append(text)
    got = hook('working', 'UserPromptSubmit', prompt=text)
    if got:
        hooked.append(got.get('hookSpecificOutput', {}).get('additionalContext', ''))
    begin(text, text + ' '.join(hooked[-1:]) if got else text)
def finish():
    global working_until
    working_until = None
    turns.append(working_on)
    transcript.append(f'● ERGEBNIS-{len(turns)} fertig')
    draw(); save()
    got = hook('idle', 'Stop')
    if got and got.get('decision') == 'block':
        hooked.append(got.get('reason', ''))
        begin('(hook) ' + got.get('reason', '')[-40:], got.get('reason', ''))
draw(); save()
while True:
    order = root / 'do'
    if order.exists():
        what = order.read_text().strip(); order.unlink()
        if what == 'flicker':
            spinner = True; draw(); hook('working', 'PreToolUse')
            time.sleep(3)
            spinner = False; draw(); hook('idle', 'Stop')
        if what == 'block':
            # A question of its own on the screen, reported as Claude Code's
            # Notification hook does.
            transcript.append('● Darf ich den Zweig pushen?')
            draw(); hook('blocked', 'Notification')
        if what == 'jobs-end':
            # The job ends, the agent takes up its turn and finishes it.
            # As Claude Code: the spinner first, its hook only later (here
            # after 12 s, longer than a stop takes to settle; a turn of text
            # alone sends none at all).
            jobs = False; spinner = True; draw()
            time.sleep(12)
            hook('working', 'PreToolUse')
            time.sleep(3)
            spinner = False; turns.append('jobs'); transcript.append('● JOBS-ERLEDIGT')
            draw(); hook('idle', 'Stop')
    if working_until is not None and time.monotonic() >= working_until:
        finish()
    if select.select([0], [], [], 0.05)[0]:
        data = os.read(0, 65536).decode(errors='replace')
        data = re.sub(r'\x1b\[[0-9;?]*[A-Za-z~]', '', data)
        for ch in data:
            if ch == '\r':
                if buf:
                    text, buf = buf, ''
                    submit(text)
            elif ch == '\x15':
                buf = ''
            elif ch >= ' ':
                buf += ch
        save(); draw()
'''

MACHINE = 'testhost'
results = []


def check(label, ok, detail=''):
    results.append((label, ok))
    print(('PASS ' if ok else 'FAIL ') + label + ('' if ok else f'\n     {detail}'), flush=True)


def run(binary):
    with tempfile.TemporaryDirectory(prefix='imkr-1538-') as folder:
        root = Path(folder)
        fake = root / 'bin' / 'claude'
        fake.parent.mkdir()
        fake.write_text(FAKE)
        fake.chmod(0o755)
        config = root / 'config.toml'
        # The notes' wording is what this test reads: in the conversation,
        # as before #10254 (tests/keeper_board.py covers the board).
        config.write_text(f'notifications = "off"\nname = "{MACHINE}"\n'
                          'keeper_pings_in_chat = true\n')
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
            # Keys from a client, like a person's: raw input over a tap.
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

        def mail(address):
            return json.loads(cli('mail', '--json', address).stdout or '{}')

        def need(ok, what):
            if not ok:
                print(f'SETUP FAILED: {what}')
                for address in (keeper, bee):
                    m = cli('mail', '--json', address); print(address, m.returncode, m.stdout[:1500], m.stderr[:300])
                    print(cli('read', address).stdout[-800:])
                    print(cli('list', '--json', '-s', address.split(':')[1].split('/')[0]).stdout[:800])
                raise AssertionError(what)

        def bee_notes():
            return [t for t in got('keeper') if '[imkr] bee ' in t]

        try:
            keeper = start('keeper', 'keeper')
            bee = start('bee', 'bee', '--by', keeper)
            # The keeper had a turn of its own before, its hook fetched mail
            # since: what the keeper connect-manager was on 20.11.2026.
            sent = cli('msg', keeper, 'WARMUP')
            print('msg:', sent.returncode, sent.stdout.strip(), sent.stderr.strip())
            need(until(lambda: any('WARMUP' in t for t in observed('keeper')['accepted']), 30), 'warmup')
            assert until(lambda: not observed('keeper').get('working'), 15)

            # (c) A bee started for the keeper is done: the keeper is told,
            # once, with what the bee said last above its input line.
            cli('msg', bee, 'AUFTRAG-1')
            told = until(lambda: len(bee_notes()) >= 1, 60)
            notes = bee_notes()
            check('(c) a bee that is done tells its keeper', told, f'keeper got {got("keeper")}')
            if notes:
                print(f'INFO keeper read: {notes[0][notes[0].find("[imkr] bee"):]}')
                check('(c) it names what the bee said last, not its footer',
                      notes[0].endswith('is done: ERGEBNIS-1 fertig') and 'bypass' not in notes[0],
                      f"{notes[0]} / bee took {observed('bee')['accepted']}")

            # (d) Idle and working flicker without a new prompt: nothing more.
            for _ in range(3):
                do('bee', 'flicker')
                time.sleep(5)
            time.sleep(14)
            check('(d) a flicker between idle and working tells nothing more',
                  len(bee_notes()) == len(notes), f'{len(bee_notes())} notes: {bee_notes()}')

            # (c2) The keeper's condition: a bee that wrote to its keeper in
            # this turn is announced done in one line, without its screen.
            before = len(bee_notes())
            cli('msg', bee, f'AUFTRAG-2 TELL:{keeper}')
            told = until(lambda: len(bee_notes()) > before, 60)
            last = bee_notes()[-1] if told else ''
            check('(c2) a bee that wrote itself is told done in one line',
                  told and 'it wrote to you this turn' in last and 'ERGEBNIS' not in last,
                  f'keeper got {got("keeper")}')
            check('(c2) its own message came through as well',
                  until(lambda: any('selbst gemeldet' in t for t in got('keeper')), 30),
                  f'keeper got {got("keeper")}')

            # (e) The first real report, 20.11.2026: a bee stopped with
            # background jobs left, in a pane its status row wraps in. It is
            # not done, and its status row is nothing it said.
            # Notes may come in another order than they were sent (by hook
            # while the keeper works): wait for the one asked about.
            def note_with(text, wait=60):
                until(lambda: any(text in t for t in bee_notes()), wait)
                return next((t for t in bee_notes() if text in t), '')
            cli('msg', bee, 'AUFTRAG-3 JOBS')
            last = note_with('waits for its background jobs')
            told = bool(last)
            print(f'INFO keeper read: {last[last.find("[imkr] bee"):]}')
            check('(e) a bee with jobs left is told waiting, not done',
                  told and 'waits for its background jobs: 1 shell, 1 monitor, 1 background agent' in last and 'is done' not in last,
                  f'keeper got {got("keeper")[-2:]}; bee took {observed("bee")}; '
                  f'bee shows:\n{cli("read", bee).stdout}')
            do('bee', 'jobs-end')
            last = note_with('JOBS-ERLEDIGT')
            told = bool(last)
            # And nothing in between: no done with the old line.
            early = [t for t in bee_notes() if 'is done: ERGEBNIS-3' in t]
            print(f'INFO keeper read: {last[last.find("[imkr] bee"):]}')
            check('(e) when its jobs are over and it stops, it is done with what it said',
                  told and last.endswith('is done: JOBS-ERLEDIGT') and not early,
                  f'keeper got {got("keeper")[-2:]}; bee took {observed("bee")}; '
                  f'bee shows:\n{cli("read", bee).stdout}')

            # (a2) Two messages for the parked keeper, no key at all: a
            # parked agent fires no hook, so they have to be typed.
            assert until(lambda: not observed('keeper').get('working'), 20)
            time.sleep(6)
            cli('msg', keeper, 'PARKED-1')
            cli('msg', keeper, 'PARKED-2')
            arrived = until(lambda: all(any(f'PARKED-{i}' in t for t in got('keeper'))
                                        for i in (1, 2)), 45)
            whys = {n.get('why') for n in mail(keeper).get('notes', []) if n.get('what') == 'held'}
            check('(a2) two messages reach a parked keeper without anybody writing to it',
                  arrived, f'held because {whys}; keeper got {got("keeper")[-3:]}')

            # (a) The case of 20.11.2026: the parked keeper, a key that
            # writes nothing (an arrow), then two messages from bees.
            assert until(lambda: not observed('keeper').get('working'), 20)
            time.sleep(6)
            tap_text(keeper, '\x1b[A')
            assert observed('keeper')['input'] == '', observed('keeper')
            cli('msg', keeper, 'ZUSTELLUNG-1')
            cli('msg', keeper, 'ZUSTELLUNG-2')
            arrived = until(lambda: all(any(f'ZUSTELLUNG-{i}' in t for t in got('keeper'))
                                        for i in (1, 2)), 45)
            whys = {n.get('why') for n in mail(keeper).get('notes', []) if n.get('what') == 'held'}
            check('(a) a parked keeper gets its mail after a key that wrote nothing', arrived,
                  f'held because {whys}; keeper got {got("keeper")[-3:]}')
            if arrived:
                texts = got('keeper')
                first = next(i for i, t in enumerate(texts) if 'ZUSTELLUNG-1' in t)
                second = next(i for i, t in enumerate(texts) if 'ZUSTELLUNG-2' in t)
                check('(a) in the order they were sent', first <= second, texts)

            # (b) Real unsent text in the keeper's input line is never
            # written over, and the message waits.
            assert until(lambda: not observed('keeper').get('working'), 20)
            time.sleep(6)
            tap_text(keeper, 'halb geschrieben')
            cli('msg', keeper, 'ZUSTELLUNG-3')
            time.sleep(25)
            seen = observed('keeper')
            check('(b) text a person wrote stays as it is, the message waits',
                  seen['input'] == 'halb geschrieben'
                  and not any('ZUSTELLUNG-3' in t for t in got('keeper')),
                  seen)
            tap_text(keeper, '\x15')
        finally:
            for session in ('bee', 'keeper'):
                cli('-s', session, 'kill-server')
            assert not notify_log.exists(), 'a notification reached the shim'
    failed = [label for label, ok in results if not ok]
    print(f'{len(results) - len(failed)} passed, {len(failed)} failed')
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(run(str(Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/imkr').resolve())))
