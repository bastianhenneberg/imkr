#!/usr/bin/env python3
"""Run a reviewed #1339 acceptance archive over SSH on specified hosts.
Only private /tmp directories and isolated test sessions are created.
"""
import argparse
import json
from pathlib import Path
import re
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bundle', type=Path, required=True)
    parser.add_argument('--host', action='append', required=True,
                        help='SSH destination; repeat for laptop and codingmachine')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--rows', type=int)
    parser.add_argument('--cols', type=int)
    args = parser.parse_args()
    if (args.rows is None) != (args.cols is None):
        parser.error('--rows and --cols must be supplied together')
    args.output.mkdir(parents=True, exist_ok=False)
    results = []
    for number, host in enumerate(args.host):
        # Destinations are passed as argv, never shell fragments.
        if not re.fullmatch(r'[A-Za-z0-9_][A-Za-z0-9_.@-]*', host):
            parser.error('invalid SSH destination: ' + host)
        ssh = ['ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10', host]
        print('START', host, flush=True)
        record = {'host': host}
        log = args.output / (str(number) + '-' + host + '.log')
        try:
            root = subprocess.check_output(ssh + [
                'mktemp -d /tmp/imkr-1339-review.XXXXXX'], text=True, timeout=20).strip()
            if not re.fullmatch(r'/tmp/imkr-1339-review\.[A-Za-z0-9]+', root):
                raise RuntimeError('unexpected test directory: ' + root)
            record['directory'] = root
            subprocess.run(['scp', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10',
                str(args.bundle.resolve()), host + ':' + root + '/bundle.tgz'],
                check=True, timeout=300)
            # The only interpolated value is the validated mktemp path.
            command = (f'tar -xzf {root}/bundle.tgz -C {root} && '
                       f'python3 {root}/run.py')
            if args.rows is not None:
                command += f' --rows {args.rows} --cols {args.cols}'
            with log.open('w') as output:
                run = subprocess.run(ssh + [command], stdout=output,
                    stderr=subprocess.STDOUT, timeout=420)
            record['exit'] = run.returncode
        except (subprocess.SubprocessError, OSError, RuntimeError) as error:
            record['exit'] = 1
            record['error'] = str(error)
        record['log'] = str(log)
        results.append(record)
        print('RESULT', json.dumps(record), flush=True)
        (args.output / 'summary.json').write_text(json.dumps(results, indent=2))
    raise SystemExit(0 if all(r['exit'] == 0 for r in results) else 1)


if __name__ == '__main__':
    main()
