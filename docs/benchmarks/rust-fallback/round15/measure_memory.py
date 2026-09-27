"""Separate untimed runs, measuring peak RSS with wait4."""
from pathlib import Path
import hashlib,json,os,statistics,subprocess,tempfile
os.sched_setaffinity(0,{2})
root=Path.cwd(); inp=root/'docs/benchmarks/rust-fallback/pr/input.txt'
modes={'goal_start':root/'target/round12/ttfx-pr41','checkpoint':root/'target/round15/ttfx-before','candidate':root/'target/round15/ttfx-final'}
env={**os.environ,'TTFX_ASM':'0','COLUMNS':'200','LINES':'50'}
report={'method':'Linux wait4 ru_maxrss, KiB; separate complete untimed CLI runs','cpu':2,'input_sha256':hashlib.file_digest(inp.open('rb'),'sha256').hexdigest(),'binary_sha256':{name:hashlib.file_digest(path.open('rb'),'sha256').hexdigest() for name,path in modes.items()},'results':{}}
for effect in ['binarypath','rings','colorshift','burn','laseretch','errorcorrect','bouncyballs','matrix','spotlights','overflow']:
 samples={name:[] for name in modes}
 for repetition in range(3):
  names=list(modes); names=names[repetition:]+names[:repetition]
  for name in names:
   with inp.open('rb') as source,tempfile.TemporaryFile() as errors:
    process=subprocess.Popen([str(modes[name]),'--seed','1','--frame-rate','0','--virtual-clock',effect],stdin=source,stdout=subprocess.DEVNULL,stderr=errors,env=env)
    pid,status,usage=os.wait4(process.pid,0)
    process.returncode=os.waitstatus_to_exitcode(status)
    errors.seek(0); stderr=errors.read()
    assert process.returncode==0 and not stderr,(effect,name,process.returncode,stderr)
    samples[name].append(usage.ru_maxrss)
 report['results'][effect]={'rss_kib':samples,'median_mib':{name:statistics.median(values)/1024 for name,values in samples.items()}}
 print(effect,report['results'][effect]['median_mib'],flush=True)
 Path('target/round15/memory.json').write_text(json.dumps(report,indent=2)+'\n')
