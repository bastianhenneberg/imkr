#!/usr/bin/env python3
"""Prepare a portable, credential-free #1339 real Codex acceptance bundle.
Nothing is installed, started or sent to another host by this packaging command.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import tarfile

RUNNER = '''#!/usr/bin/env python3
"""Run isolated real Codex acceptance on this host. No production server updates."""
import argparse, json, pathlib, subprocess, sys, time
root = pathlib.Path(__file__).resolve().parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--mode', choices=['all', 'no-daemon', 'managed'], default='all')
parser.add_argument('--rows', type=int)
parser.add_argument('--cols', type=int)
args = parser.parse_args()
manifest = json.loads((root / 'manifest.json').read_text())
import hashlib
for name, expected in manifest['sha256'].items():
    actual = hashlib.file_digest((root / name).open('rb'), 'sha256').hexdigest()
    if actual != expected:
        raise SystemExit('Bundle checksum mismatch: ' + name)
modes = ['no-daemon', 'managed'] if args.mode == 'all' else [args.mode]
results = []
for mode in modes:
    log = root / ('run-' + mode + '-' + str(time.time_ns()) + '.log')
    cmd = [sys.executable, str(root / 'codex_submit.py'), '--binary', str(root / 'imkr'),
           '--codex', str(root / 'codex')]
    if args.rows is not None or args.cols is not None:
        if not args.rows or not args.cols:
            raise SystemExit('--rows and --cols must be supplied together')
        cmd += ['--rows', str(args.rows), '--cols', str(args.cols)]
    if mode == 'managed':
        cmd += ['--managed', '--daemon-runtime', str(root / manifest['runtime'])]
    print('START', mode, 'log:', log, flush=True)
    with log.open('w') as output:
        try:
            result = subprocess.run(cmd, stdout=output, stderr=subprocess.STDOUT, timeout=180)
            code = result.returncode
        except subprocess.TimeoutExpired:
            code = 124
    text = log.read_text()
    print(text, flush=True)
    results.append({'mode': mode, 'exit': code, 'log': str(log)})
(root / 'summary.json').write_text(json.dumps(results, indent=2))
raise SystemExit(0 if all(r['exit'] == 0 for r in results) else 1)
'''

README = '''Bug #1339 acceptance bundle (session/worktree name retains 1343)

This archive contains executable-only test resources, no credentials, account
configuration or conversations. Review manifest.json and the two Python scripts.
The SHA-256 manifest detects accidental changes; it is not a signature.

After review, extract to a NEW private directory on EACH host. Example:

    dir=$(mktemp -d /tmp/imkr-1339-review.XXXXXX)
    tar -xzf codex-submit-1339.tgz -C "$dir"
    python3 "$dir/run.py"

Run that command locally on laptop and through SSH on codingmachine. The runner
has no implicit host selection or production installation/update action. To run
only one variant, pass --mode no-daemon or --mode managed. Both variants use the
bundled real frontend; managed uses a private copy of the bundled runtime.

codex_submit.py establishes notifications=off, fake notify-send first in PATH,
invalid DBus and no DISPLAY/WAYLAND_DISPLAY BEFORE any application PTY. It creates
fresh HOME/XDG/config/state/sockets/work directories, starts only a loopback fake
Responses provider and private imkr sessions, and uses no paid model account.
Requires Linux /proc, Python 3.11+, bash, and an x86-64 runtime compatible with the
bundled binaries. Run as your ordinary user. No desktop, credentials or original
queues are needed. Allow approximately one minute per variant; each provider and
CLI operation has a bounded timeout.

Expect: real assistant ACKs for exact markers; full single-line/long-paste bodies;
worker -> keeper completion -> worker follow-up; busy -> idle delivery;
human draft and actual command approval untouched across server handover;
account switch refusal with draft; ordinary shell command; no false Confirmed;
zero fake notifier calls. Each marker must reach one actual model turn.

Read run-*.log, summary.json and the printed /tmp/imkr-submit-1343-* evidence
folders. Nonzero exit is a failed acceptance, not a successful delivery. Keep the
failed bootstrap log too. Harness finally cleans its private servers/daemons;
a forcibly killed harness may need cleanup of ONLY processes bearing that exact
SUBMIT_TEST_ROOT. Never resend production queues or send a spare Enter to them.

The direct single-line send regression is fixed by explicit paste framing.
Real live ANSI snapshots also prove a missing-tooltip-footer false hold; the
parser now recognizes that structured composer without releasing actual drafts.
Real isolated TUI turns still displayed hints: no footerless end-to-end claim
is made. Passing this suite does not resolve all production symptoms or authorize
releasing old queues. Baseline
91fd2ca already had the history/composer fix; this suite does not disprove it.
No production rollout is included or authorized by this package.
'''


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, default=Path('target/debug/imkr'))
    p.add_argument('--codex', type=Path, required=True)
    p.add_argument('--runtime', type=Path, required=True,
                   help='executable-only release directory, e.g. 0.166.0-x86_64-unknown-linux-musl')
    p.add_argument('--out', type=Path, required=True)
    args = p.parse_args()
    harness = Path(__file__).with_name('codex_submit.py')
    runtime = args.runtime.resolve()
    package = json.loads((runtime / 'codex-package.json').read_text())
    files = {'imkr': args.binary.resolve(), 'codex': args.codex.resolve(),
             'codex_submit.py': harness.resolve()}
    # Only the release's executable resource tree is included. Reject account
    # folders or an arbitrary directory passed as --runtime.
    assert package['layoutVersion'] == 1 and (runtime / 'bin/codex').is_file()
    hashes = {name: hashlib.file_digest(path.open('rb'), 'sha256').hexdigest()
              for name, path in files.items()}
    for path in sorted(runtime.rglob('*')):
        assert not path.is_symlink() or path.resolve().is_relative_to(runtime), path
        assert not path.is_symlink() or not path.readlink().is_absolute(), path
        if path.is_file() and not path.is_symlink():
            hashes['runtime/' + runtime.name + '/' + str(path.relative_to(runtime))] = (
                hashlib.file_digest(path.open('rb'), 'sha256').hexdigest())
    hashes['run.py'] = hashlib.sha256(RUNNER.encode()).hexdigest()
    manifest = {'bug': 1339, 'duplicate': 1343, 'runtime': 'runtime/' + runtime.name,
                'runtime_version': package['version'], 'sha256': hashes}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    # Exclusive creation protects a previous reviewed bundle from replacement.
    with args.out.open('xb') as raw, tarfile.open(fileobj=raw, mode='w:gz', compresslevel=1) as archive:
        for name, path in files.items():
            archive.add(path, arcname=name)
        archive.add(runtime, arcname=manifest['runtime'])
        for name, value in {'run.py': RUNNER, 'README.txt': README,
                            'manifest.json': json.dumps(manifest, indent=2)}.items():
            data = value.encode()
            info = tarfile.TarInfo(name)
            info.size = len(data)
            info.mode = 0o700 if name.endswith('.py') else 0o600
            archive.addfile(info, io.BytesIO(data))
    checksum = hashlib.file_digest(args.out.open('rb'), 'sha256').hexdigest()
    args.out.with_suffix(args.out.suffix + '.sha256').write_text(checksum + '  ' + args.out.name + '\n')
    print(args.out)
    print('SHA256', checksum)


if __name__ == '__main__':
    main()
