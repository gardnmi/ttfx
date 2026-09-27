"""Full CLI byte-stream comparisons for row-cache boundaries and color modes."""
import argparse, hashlib, json, os, subprocess, tempfile
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('candidate',type=Path);args=p.parse_args()
root=Path.cwd(); before=root/'target/round15/ttfx-before'; candidate=args.candidate.resolve()
help_text=subprocess.check_output([str(before),'--help'],text=True)
commands=help_text.split('Commands:',1)[1].split('Options:',1)[0]
effects=[line.split()[0] for line in commands.splitlines() if line.strip() and line.split()[0]!='help']
plain=('the quick brown fox jumps over the lazy dog '*2+'\n')*3
colored='\x1b[1;31mRED\x1b[0m plain \x1b[38;2;7;128;255mRGB\x1b[0m\n\x1b[48;5;42mBG\x1b[0m text'
unicode='café λ 界 🙂 🦀\n日本語 Δοκιμή\nmultibyte ü'
cases=[]
for effect in effects:
 for name,data,size,options in [
  ('plain',plain,(65,6),[]),('unicode',unicode,(33,5),[]),
  ('ansi-dynamic',colored,(40,7),['--existing-color-handling','dynamic']),
  ('xterm',colored,(40,7),['--existing-color-handling','always','--xterm-colors']),
  ('no-color',plain,(65,6),['--no-color'])]:
  cases.append((effect,name,data,size,options))
for width in [15,16,17,31,32,33,63,64,65,511,512,513,1023,1024,1025,2048]:
 for effect in ['burn','laseretch','bouncyballs','errorcorrect','waves','matrix']:
  cases.append((effect,'width-'+str(width),('0123456789abcdef'*((min(width,90)+15)//16))[:min(width,90)]+'\nabc界🙂',(width,3),[]))
report={'before_sha256':hashlib.file_digest(before.open('rb'),'sha256').hexdigest(),
        'candidate_sha256':hashlib.file_digest(candidate.open('rb'),'sha256').hexdigest(),'comparisons':0,'cases':[]}
env={**os.environ,'TTFX_ASM':'0','COLUMNS':'80','LINES':'24'}
for index,(effect,name,data,(w,h),options) in enumerate(cases):
 common=['--seed','17','--frame-rate','0','--virtual-clock','--canvas-width',str(w),'--canvas-height',str(h),'--ignore-terminal-dimensions',*options,effect]
 results=[]
 for binary,extra in [(before,{}),(candidate,{}),(candidate,{'TTFX_SIMD':'0'})]:
  with tempfile.TemporaryFile() as output:
   r=subprocess.run([str(binary),*common],input=data.encode(),stdout=output,stderr=subprocess.PIPE,env={**env,**extra},timeout=120)
   output.seek(0);results.append({'bytes':os.fstat(output.fileno()).st_size,'sha256':hashlib.file_digest(output,'sha256').hexdigest(),'stderr':r.stderr.hex(),'returncode':r.returncode})
 assert results[0]==results[1]==results[2] and results[0]['returncode']==0,(effect,name,results)
 report['comparisons']+=2
 report['cases'].append({'effect':effect,'case':name,'canvas':[w,h],'options':options,'output':results[0]})
 if (index+1)%25==0:print(f'{index+1}/{len(cases)} cases passed',flush=True)
Path('target/round15/render-parity.json').write_text(json.dumps(report,indent=2)+'\n')
print(f"{len(cases)} complete scenarios, {report['comparisons']} comparisons passed",flush=True)
