#!/usr/bin/env python3
"""Run executable bash examples embedded in pChronicle cases documents."""
from __future__ import annotations
import argparse, datetime as dt, os, re, subprocess, tempfile, time
from pathlib import Path

CASE_RE=re.compile(r'^##\s+([SP]\d{2})：?\s*(.*)$')

def parse(path):
    lines=path.read_text(encoding='utf-8').splitlines(); out=[]; i=0
    while i<len(lines):
        m=CASE_RE.match(lines[i])
        if not m: i+=1; continue
        ident,title=m.groups(); i+=1; code=None
        while i<len(lines) and not CASE_RE.match(lines[i]):
            if lines[i].strip()=='```bash':
                i+=1; buf=[]
                while i<len(lines) and lines[i].strip()!='```': buf.append(lines[i]); i+=1
                code='\n'.join(buf); break
            i+=1
        if code: out.append((ident,title,code))
    return out

def main():
    ap=argparse.ArgumentParser(); ap.add_argument('--document',type=Path,required=True); ap.add_argument('--pchronicle',default='pchronicle'); ap.add_argument('--case',default=''); ap.add_argument('--list',action='store_true'); ap.add_argument('--keep',action='store_true'); ap.add_argument('--report',type=Path); ap.add_argument('--timeout',type=int,default=120); a=ap.parse_args()
    cases=parse(a.document)
    if a.list:
        for x in cases: print(f'{x[0]}\t{x[1]}')
        return 0
    wanted={x for x in a.case.split(',') if x}; cases=[c for c in cases if not wanted or c[0] in wanted]
    results=[]
    for ident,title,code in cases:
        # Long-running server examples are documentation smoke cases; execute them only explicitly.
        if re.search(r'(^|\n)\s*pchronicle serve\s',code) and '--case' not in os.environ.get('PCHRONICLE_CASE_MODE',''):
            results.append((ident,'MANUAL','server command requires a running client')); print(f'{ident} MANUAL {title}'); continue
        root=Path(tempfile.mkdtemp(prefix=f'pchronicle-{ident.lower()}-')); env=os.environ.copy(); env['PCHRONICLE_CASE_WORKSPACE']=str(root); env['PCHRONICLE_BIN']=a.pchronicle
        code=code.replace('pchronicle',a.pchronicle)
        started=time.monotonic()
        try:
            p=subprocess.run(['bash','-euo','pipefail','-c',code],cwd=root,env=env,text=True,capture_output=True,timeout=a.timeout)
            ok=p.returncode==0; status='PASS' if ok else 'FAIL'; detail=(p.stdout+p.stderr).strip()
        except subprocess.TimeoutExpired as e: status='FAIL'; detail=f'timeout after {a.timeout}s\n{e.stdout or ""}'
        results.append((ident,status,detail)); print(f'{ident} {status} {title}')
        if detail: print(detail[-2000:])
        if not a.keep:
            import shutil; shutil.rmtree(root,ignore_errors=True)
    if a.report:
        a.report.parent.mkdir(parents=True,exist_ok=True); now=dt.datetime.now(dt.timezone.utc).isoformat(); lines=[f'# pChronicle cases\n\nGenerated {now}.\n','| Case | Status | Detail |','|---|---|---|']
        lines += [f'| {i} | {s} | {d.replace(chr(10)," ")[:500]} |' for i,s,d in results]; a.report.write_text('\n'.join(lines)+'\n',encoding='utf-8')
    return 1 if any(s=='FAIL' for _,s,_ in results) else 0
if __name__=='__main__': raise SystemExit(main())
