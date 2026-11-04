#!/usr/bin/env python3
"""Isolated real imkr PTYs + fake agents; no model calls or original sessions.
Run: python3 tests/handoff.py target/debug/imkr
"""
import json, os, re, shlex, subprocess, sys, tempfile, time
from pathlib import Path

FAKE = r'''#!/usr/bin/python3
import json, os, signal, sys, time
from pathlib import Path
root = Path(os.environ['HANDOFF_TEST_ROOT'])
tool = Path(sys.argv[0]).name
if len(sys.argv)==1: os.chdir(root/"actual worktree ' $(touch CWD_INJECTED)")
with (root/'launches.jsonl').open('a') as log: log.write(json.dumps({'tool':tool,'argv':sys.argv,'cwd':os.getcwd()})+'\n')
if len(sys.argv)>1 and 'Read the saved agent handoff at ' in sys.argv[-1]:
    prompt=sys.argv[-1]
    path=Path(prompt.split('Read the saved agent handoff at ',1)[1].split(' and continue its original task.',1)[0])
    md=path.read_text(); raw=(path.parent/'source.jsonl').read_text()
    (root/'received.json').write_text(json.dumps({'tool':tool,'prompt':prompt,'path':str(path),'md':md,'raw':raw,'argv':sys.argv,'provider':os.environ.get('ANTHROPIC_BASE_URL'),'cwd':os.getcwd()}))
    print('HANDOFF READ OK',flush=True)
else:
    source=root/('.codex/sessions/source-exact.jsonl' if tool=='codex' else '.claude/projects/test/source-exact.jsonl')
    source.parent.mkdir(parents=True,exist_ok=True)
    records=([{'type':'session_meta','payload':{'id':'source-exact','cwd':os.getcwd()}}, {'type':'response_item','payload':{'type':'message','role':'user','content':[{'type':'input_text','text':'LITERAL TASK repair the orchard'}]}}, {'type':'compacted','payload':{'summary':'DECISION preserve keeper role'}}, {'type':'response_item','payload':{'type':'message','role':'assistant','content':[{'type':'output_text','text':'WORK modified orchard.rs; next validate'}]}}] if tool=='codex' else [{'type':'user','sessionId':'source-exact','message':{'content':'LITERAL TASK repair the orchard'}},{'type':'summary','summary':'DECISION preserve keeper role'},{'type':'assistant','message':{'content':'WORK modified orchard.rs; next validate'}}])
    source.write_text(''.join(json.dumps(r)+'\n' for r in records))
    held=source.open('a') # exact open FD, Codex has no status hook
    if tool=='claude':
        import subprocess
        subprocess.run([os.environ['TEST_IMKR'],'status','idle'],input=json.dumps({'session_id':'source-exact','transcript_path':str(source)}),text=True,stdout=subprocess.DEVNULL)
    (root/'source-ready').write_text(str(os.getpid()))
    def stop(*args):
        snapshots=[{'path':str(p),'raw_contains_task':'LITERAL TASK repair the orchard' in (p.parent/'source.jsonl').read_text()} for p in (root/'imkr/handoffs').glob('*/handoff.md')]
        (root/'stopped').write_text(json.dumps(snapshots))
        if os.environ.get('REMOVE_CWD_ON_STOP'): Path.cwd().rename(root/'removed-worktree')
        sys.exit(0)
    signal.signal(signal.SIGTERM,stop)
    if tool=='codex':
        h=os.get_terminal_size(0).lines
        print(f'\x1b[2J\x1b[{h-6};1H› \x1b[2mAsk Codex to do anything\x1b[0m\x1b[{h-3};1HGPT-6 · fake\x1b[{h-2};1H? for shortcuts\x1b[{h-6};3H',end='',flush=True)
    else: print('SOURCE READY',flush=True)
while True: time.sleep(.1)
'''

def scenario(binary, source_tool, failure=None, provider=False, resume=False):
    with tempfile.TemporaryDirectory(prefix='imkr-handoff-') as folder:
        # Deliberately hostile shell metacharacters in the persistent path.
        root=Path(folder)/"space ' $(touch INJECTED)";root.mkdir()
        workdir=root/"actual worktree ' $(touch CWD_INJECTED)";workdir.mkdir()
        for tool in ('claude','codex'):
            p=root/tool;p.write_text(FAKE);p.chmod(0o755)
        policy={'claude':'--dangerously-skip-permissions','codex':'--yolo'}
        (root/'config.toml').write_text('[agent_commands]\n'+''.join(tool+' = '+json.dumps(tool+" --model 'custom model' "+policy[tool])+'\n' for tool in ('claude','codex')))
        env={k:v for k,v in os.environ.items() if not k.startswith(('IMKR','CODEX','CLAUDE','ANTHROPIC'))}
        env.update(HOME=str(root),XDG_STATE_HOME=str(root),XDG_CONFIG_HOME=str(root),
                   IMKR_CONFIG=str(root/'config.toml'),IMKR_SOCKET=str(Path(folder)/'s.sock'),
                   HANDOFF_TEST_ROOT=str(root),TEST_IMKR=binary,PATH=str(root)+':'+os.environ['PATH'],SHELL='/bin/bash')
        def cli(*args, **kw):
            return subprocess.run([binary,*args],env=env,cwd=root,text=True,capture_output=True,timeout=15,**kw)
        def until(test,label):
            end=time.monotonic()+20
            while time.monotonic()<end:
                if test(): return
                time.sleep(.1)
            raise AssertionError(label+'\n'+cli('list','--json').stdout+'\n'+cli('read','--pane',pane).stdout)
        if failure=='cwd-race':env['REMOVE_CWD_ON_STOP']='1'
        tap=None
        try:
            r=cli('new','--start','--','bash','--noprofile','--norc');assert r.returncode==0,r.stderr
            pane=str(json.loads(cli('list','--json').stdout)[-1]['pane'])
            r=cli('send','--pane',pane,shlex.quote(str(root/source_tool)));assert r.returncode==0,r.stderr
            until(lambda:(root/'source-ready').exists(),'source starts')
            until(lambda:any(p.get('program')==source_tool for p in json.loads(cli('list','--json').stdout)),'source recognized')
            # Set keeper identity and queue mail behind a blocked agent.
            cli('keeper','--mark')
            target=source_tool if resume else ('codex' if source_tool=='claude' else 'claude')
            target_account=target+':standard'
            if provider:
                r=cli('account','add','claude','local','--url','http://127.0.0.1:9','--model','fake');assert r.returncode==0,r.stderr
                target_account='claude:local'
            if failure=='missing':
                for p in root.rglob('source-exact.jsonl'):p.unlink()
            if failure=='write':
                base=root/'imkr/handoffs';base.parent.mkdir(parents=True,exist_ok=True);base.write_text('blocked')
            if failure=='cwd-missing':workdir.rmdir()
            if failure=='draft':
                tap=subprocess.Popen([binary,'tap',pane],env=env,cwd=root,text=True,stdin=subprocess.PIPE,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
                tap.stdin.write(json.dumps({'text':'KEEP THIS DRAFT'})+'\n');tap.stdin.flush();time.sleep(.5)
            if not failure and not resume:
                cli('status','blocked','--pane',pane,input='{}')
                sent=cli('msg',pane,'PENDING MAIL preserve orchard decision')
                assert sent.returncode==0,sent.stderr
            r=cli('account','switch',target_account,'--pane',pane,'--json')
            if failure=='cwd-race':
                assert r.returncode==0,(r.stdout,r.stderr)
                until(lambda:'source working directory unavailable; target not started' in cli('read','--pane',pane).stdout,'runtime CWD refusal')
                assert len((root/'launches.jsonl').read_text().splitlines())==1
                assert not (root/'received.json').exists()
                print('PASS',source_tool,'missing CWD at startup refuses target instead of falling back to shell CWD',flush=True)
                return
            if failure:
                assert r.returncode!=0,(failure,r.stdout,r.stderr)
                assert not (root/'stopped').exists(),failure
                os.kill(int((root/'source-ready').read_text()),0)
                print('PASS',source_tool,failure,'refuses with source alive',flush=True)
                return
            assert r.returncode==0,(r.stdout,r.stderr)
            reply=json.loads(r.stdout)
            if resume:
                assert reply['conversation']=='same' and reply['handoff'] is None,reply
                until(lambda:len((root/'launches.jsonl').read_text().splitlines())==2,'native resume launch')
                launches=[json.loads(line) for line in (root/'launches.jsonl').read_text().splitlines()]
                assert launches[-1]['argv'][1:]==['--model','custom model',policy[source_tool]]+(['resume','source-exact'] if source_tool=='codex' else ['--resume','source-exact']),launches
                assert launches[-1]['cwd']==str(workdir),launches
                assert not (root/'imkr/handoffs').exists()
                print('PASS',source_tool,'same agent uses exact native resume without handoff',flush=True)
                return
            assert reply['conversation']=='handoff'
            until(lambda:(root/'received.json').exists(),'target reads handoff')
            until(lambda:any(p.get('program')==target for p in json.loads(cli('list','--json').stdout)),'target recognized in foreground')
            got=json.loads((root/'received.json').read_text())
            assert got['path']==reply['handoff']
            for marker in ('LITERAL TASK repair the orchard','DECISION preserve keeper role','WORK modified orchard.rs; next validate'):
                assert marker in got['md'],marker
                assert marker in got['raw'],marker
            assert '"keeper": true' in got['md']
            assert 'PENDING MAIL preserve orchard decision' in got['md']
            assert got['cwd']==str(workdir),('TARGET CWD WRONG',got['cwd'],str(workdir))
            assert not (root/'CWD_INJECTED').exists()
            assert got['tool']==target
            assert got['argv'][1:4]==['--model','custom model',policy[target]],got['argv']
            if provider:assert got['provider']=='http://127.0.0.1:9'
            path=Path(got['path'])
            for p in path.parent.iterdir():assert p.stat().st_mode & 0o777==0o600
            assert path.parent.stat().st_mode & 0o777==0o700
            assert not (root/'INJECTED').exists()
            assert json.loads((root/'stopped').read_text())==[{'path':reply['handoff'],'raw_contains_task':True}]
            print('PASS',source_tool,'->',target,'provider' if provider else 'standard','target reads exact path, task, decision, work; private and quoted',flush=True)
        finally:
            if tap:tap.terminate();tap.wait(timeout=5)
            cli('kill-server')

if __name__=='__main__':
    binary=str(Path(sys.argv[1] if len(sys.argv)>1 else 'target/debug/imkr').resolve())
    scenario(binary,'claude')
    scenario(binary,'codex',provider=True)
    for fail in ('missing','write','draft'):scenario(binary,'claude',fail)
    scenario(binary,'codex','missing')

    for tool in ('claude','codex'):scenario(binary,tool,resume=True)

    for tool in ('claude','codex'):
        scenario(binary,tool,'cwd-missing')
        scenario(binary,tool,'cwd-race')
