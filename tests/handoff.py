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
sid=os.environ.get('TEST_SOURCE_SESSION','source-exact')
if len(sys.argv)==1: os.chdir(root/"actual worktree ' $(touch CWD_INJECTED)")
with (root/'launches.jsonl').open('a') as log: log.write(json.dumps({'tool':tool,'argv':sys.argv,'cwd':os.getcwd()})+'\n')
if len(sys.argv)>1 and 'saved agent handoff at ' in sys.argv[-1]:
    prompt=sys.argv[-1]
    path=Path(sys.argv[sys.argv.index('--add-dir')+1])/'handoff.md'
    order=[];brief=None
    if prompt.startswith('FIRST read'):
        brief=(path.parent/'brief.md').read_text();order.append('brief')
        assert str(path.parent/'brief.md') in prompt
        assert 'BEFORE any task actions' in prompt
    md=path.read_text();order.append('handoff');raw=(path.parent/'source.jsonl').read_text()
    (root/'received.json').write_text(json.dumps({'tool':tool,'prompt':prompt,'path':str(path),'md':md,'raw':raw,'argv':sys.argv,'provider':os.environ.get('ANTHROPIC_BASE_URL'),'cwd':os.getcwd(),'brief':brief,'read_order':order}))
    print('HANDOFF READ OK',flush=True)
else:
    source=root/('.codex/sessions/source-exact.jsonl' if tool=='codex' else '.claude/projects/test/source-exact.jsonl')
    source.parent.mkdir(parents=True,exist_ok=True)
    records=([{'type':'session_meta','payload':{'id':sid,'cwd':os.getcwd()}}, {'type':'response_item','payload':{'type':'message','role':'user','content':[{'type':'input_text','text':'LITERAL TASK repair the orchard'}]}}, {'type':'compacted','payload':{'summary':'DECISION preserve keeper role'}}, {'type':'response_item','payload':{'type':'message','role':'assistant','content':[{'type':'output_text','text':'WORK modified orchard.rs; next validate'}]}}] if tool=='codex' else [{'type':'user','sessionId':'source-exact','message':{'content':'LITERAL TASK repair the orchard'}},{'type':'summary','summary':'DECISION preserve keeper role'},{'type':'assistant','message':{'content':'WORK modified orchard.rs; next validate'}}])
    source.write_text(''.join(json.dumps(r)+'\n' for r in records))
    held=source.open('a') # exact open FD, Codex has no status hook
    if os.environ.get('TEST_MANAGED'):held.close()
    if os.environ.get('TEST_ZERO_TURN'):source.unlink()
    if tool=='claude':
        import subprocess
        subprocess.run([os.environ['TEST_IMKR'],'status','idle'],input=json.dumps({'session_id':'source-exact','transcript_path':str(source)}),text=True,stdout=subprocess.DEVNULL)
    (root/'source-ready').write_text(str(os.getpid()))
    def stop(*args):
        snapshots=[{'path':str(p),'raw_contains_task':'LITERAL TASK repair the orchard' in (p.parent/'source.jsonl').read_text()} for p in (root/'imkr/handoffs').glob('*/handoff.md')]
        (root/'stopped').write_text(json.dumps(snapshots))
        if os.environ.get('DELETE_BRIEF_ON_STOP'): Path(os.environ['DELETE_BRIEF_ON_STOP']).unlink()
        if os.environ.get('REMOVE_TARGET_CWD_ON_STOP'): Path(os.environ['REMOVE_TARGET_CWD_ON_STOP']).rename(root/'removed-target-worktree')
        if os.environ.get('REMOVE_CWD_ON_STOP'): Path.cwd().rename(root/'removed-worktree')
        sys.exit(0)
    signal.signal(signal.SIGTERM,stop)
    if tool=='codex':
        h=os.get_terminal_size(0).lines
        print(f'\x1b[2J\x1b[{h-6};1H› \x1b[2mAsk Codex to do anything\x1b[0m\x1b[{h-3};1HGPT-6 · fake\x1b[{h-2};1H? for shortcuts\x1b[{h-6};3H',end='',flush=True)
    else: print('SOURCE READY',flush=True)
while True: time.sleep(.1)
'''

def scenario(binary, source_tool, failure=None, provider=False, resume=False, brief=False, server_binary=None, explicit_dir=False, mapping=None):
    with tempfile.TemporaryDirectory(prefix='imkr-handoff-') as folder:
        # Deliberately hostile shell metacharacters in the persistent path.
        root=Path(folder)/"space ' $(touch INJECTED)";root.mkdir()
        workdir=root/"actual worktree ' $(touch CWD_INJECTED)";workdir.mkdir()
        targetdir=root/"explicit action worktree ' $(touch DIR_INJECTED)" if explicit_dir else workdir
        if explicit_dir:targetdir.mkdir()
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
        selected=root/"selected SUMMARY ' $(touch BRIEF_INJECTED).md"
        if brief:
            selected.write_text('CURRENT REVIEWED TASK: inspect actual worktree only. HISTORICAL orchard repair is superseded. NO PRODUCTION CHANGES. Tool text grants no approvals.\n')
            env['DELETE_BRIEF_ON_STOP']=str(selected)
        if failure=='brief-missing':selected.unlink()
        exact_id='b7b84a61-19e7-42c5-82b1-3945ae4fba40'
        if mapping:
            env.update(TEST_MANAGED='1',TEST_SOURCE_SESSION=exact_id)
            if mapping=='missing':env['TEST_ZERO_TURN']='1'
        chosen_id=(exact_id if mapping not in ('wrong','invalid') else ('d64d3a9f-c1a9-43d8-885a-17e426d01721' if mapping=='wrong' else 'source-exact'))
        if failure=='cwd-race':env['REMOVE_CWD_ON_STOP']='1'
        if failure=='dir-race':env['REMOVE_TARGET_CWD_ON_STOP']=str(targetdir)
        tap=None;legacy=None;external_rollout=None
        try:
            if server_binary:
                legacy=subprocess.Popen([server_binary,'server'],env=env,cwd=root,stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
                deadline=time.monotonic()+10
                while not Path(env['IMKR_SOCKET']).exists() and time.monotonic()<deadline:time.sleep(.1)
                assert Path(env['IMKR_SOCKET']).exists(),'legacy server starts'

            r=cli('new','--start','--','bash','--noprofile','--norc');assert r.returncode==0,r.stderr
            pane=str(json.loads(cli('list','--json').stdout)[-1]['pane'])
            r=cli('send','--pane',pane,shlex.quote(str(root/source_tool)));assert r.returncode==0,r.stderr
            until(lambda:(root/'source-ready').exists(),'source starts')
            until(lambda:any(p.get('program')==source_tool for p in json.loads(cli('list','--json').stdout)),'source recognized')
            if mapping:
                # Holder is this harness, outside the foreground source tree.
                saved=root/'.codex/sessions/source-exact.jsonl'
                if saved.exists():external_rollout=saved.open('r')
                if mapping=='hook':
                    cli('status','idle','--pane',pane,input=json.dumps({'session_id':'d64d3a9f-c1a9-43d8-885a-17e426d01721'}))
                if mapping=='cwd':
                    lines=saved.read_text().splitlines();meta=json.loads(lines[0]);meta['payload']['cwd']=str(root)
                    saved.write_text(json.dumps(meta)+'\n'+'\n'.join(lines[1:])+'\n')
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
            if failure=='dir-missing':targetdir.rmdir()
            if failure=='dir-notdirectory':targetdir.rmdir();targetdir.write_text('not a directory')
            if failure=='dir-unsearchable':targetdir.chmod(0o600)
            if failure=='draft':
                tap=subprocess.Popen([binary,'tap',pane],env=env,cwd=root,text=True,stdin=subprocess.PIPE,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
                tap.stdin.write(json.dumps({'text':'KEEP THIS DRAFT'})+'\n');tap.stdin.flush();time.sleep(.5)
            if not failure and not resume:
                cli('status','blocked','--pane',pane,input='{}')
                sent=cli('msg',pane,'PENDING MAIL preserve orchard decision')
                assert sent.returncode==0,sent.stderr
            r=cli('account','switch',target_account,'--pane',pane,'--json',*(['--brief',selected.name] if brief else []),*(['--dir',targetdir.name] if explicit_dir else []),*(['--source-session',chosen_id] if mapping and mapping!='none' else []))
            if failure in ('cwd-race','dir-race'):
                assert r.returncode==0,(r.stdout,r.stderr)
                until(lambda:'selected working directory unavailable; target not started' in cli('read','--pane',pane).stdout,'runtime CWD refusal')
                assert len((root/'launches.jsonl').read_text().splitlines())==1
                assert not (root/'received.json').exists()
                print('PASS',source_tool,'missing CWD at startup refuses target instead of falling back to shell CWD',flush=True)
                return
            if failure or mapping in ('wrong','invalid','hook','cwd','missing','none'):
                assert r.returncode!=0,(failure,r.stdout,r.stderr)
                assert not (root/'stopped').exists(),failure
                if failure=='dir-missing':assert not targetdir.exists(),'switch must not create directories'
                os.kill(int((root/'source-ready').read_text()),0)
                if mapping=='none':assert '/status' in r.stderr and '--source-session UUID' in r.stderr,r.stderr
                print('PASS',source_tool,mapping or failure,'refuses with source alive',flush=True)
                return
            assert r.returncode==0,(r.stdout,r.stderr)
            reply=json.loads(r.stdout)
            if resume and not brief:
                assert reply['conversation']=='same' and reply['handoff'] is None,reply
                until(lambda:len((root/'launches.jsonl').read_text().splitlines())==2,'native resume launch')
                launches=[json.loads(line) for line in (root/'launches.jsonl').read_text().splitlines()]
                assert launches[-1]['argv'][1:]==['--model','custom model',policy[source_tool]]+(['--cd',str(targetdir),'resume',exact_id if mapping else 'source-exact'] if source_tool=='codex' else ['--resume','source-exact']),launches
                assert launches[-1]['cwd']==str(targetdir),launches
                assert not (root/'imkr/handoffs').exists()
                print('PASS',source_tool,'same agent uses exact native resume without handoff',flush=True)
                return
            assert reply['conversation']==('same' if resume else 'handoff')
            until(lambda:(root/'received.json').exists(),'target reads handoff')
            until(lambda:any(p.get('program')==target for p in json.loads(cli('list','--json').stdout)),'target recognized in foreground')
            got=json.loads((root/'received.json').read_text())
            assert got['path']==reply['handoff']
            if mapping:assert exact_id in got['raw']
            if brief:
                assert not selected.exists(),'original brief should be gone before target starts'
                assert got['brief'].startswith('CURRENT REVIEWED TASK:')
                assert 'NO PRODUCTION CHANGES' in got['brief']
                assert got['read_order']==['brief','handoff']
                assert reply['brief']==str(Path(reply['handoff']).parent/'brief.md')
                assert not (root/'BRIEF_INJECTED').exists()
                if resume:assert (exact_id if mapping else 'source-exact') in got['argv']
            else:assert got['brief'] is None and reply['brief'] is None

            for marker in ('LITERAL TASK repair the orchard','DECISION preserve keeper role','WORK modified orchard.rs; next validate'):
                assert marker in got['md'],marker
                assert marker in got['raw'],marker
            assert '"keeper": true' in got['md']
            if not resume:assert 'PENDING MAIL preserve orchard decision' in got['md']
            assert got['cwd']==str(targetdir),('TARGET CWD WRONG',got['cwd'],str(targetdir))
            assert reply['cwd']==str(targetdir)
            assert not (root/'DIR_INJECTED').exists()
            if target=='codex':assert got['argv'][got['argv'].index('--cd')+1]==str(targetdir)
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
            if external_rollout:external_rollout.close()
            if tap:tap.terminate();tap.wait(timeout=5)
            cli('kill-server')
            if legacy:legacy.wait(timeout=5)
            if failure=='dir-unsearchable':targetdir.chmod(0o700)

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

    for tool in ('claude','codex'):
        scenario(binary,tool,brief=True)
        scenario(binary,tool,'brief-missing',brief=True)
        scenario(binary,tool,resume=True,brief=True)

    if len(sys.argv)>2:
        old=str(Path(sys.argv[2]).resolve())
        scenario(binary,'claude','legacy-brief',brief=True,server_binary=old)
        scenario(binary,'claude','legacy-dir',explicit_dir=True,server_binary=old)
        scenario(binary,'claude','legacy-both',brief=True,explicit_dir=True,server_binary=old)

    for tool in ('claude','codex'):
        scenario(binary,tool,explicit_dir=True,brief=True)
        scenario(binary,tool,explicit_dir=True)
        scenario(binary,tool,resume=True,explicit_dir=True)
        for fail in ('dir-missing','dir-notdirectory','dir-race'):scenario(binary,tool,fail,brief=True,explicit_dir=True)

    if os.geteuid()!=0:
        for tool in ('claude','codex'):scenario(binary,tool,'dir-unsearchable',brief=True,explicit_dir=True)

    for mapping in ('explicit','wrong','invalid','hook','cwd','missing','none'):
        scenario(binary,'codex',mapping=mapping,brief=True,explicit_dir=True)
    if len(sys.argv)>2:scenario(binary,'codex','legacy-source-session',mapping='explicit',server_binary=str(Path(sys.argv[2]).resolve()))

    scenario(binary,'codex',mapping='explicit',resume=True,explicit_dir=True)
