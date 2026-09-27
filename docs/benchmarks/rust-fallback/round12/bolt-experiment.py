from pathlib import Path
import os,subprocess,json,hashlib
root=Path.cwd()
out=root/'target/round12/bolt'
tools=Path('/home/gardnmi/Projects/ttfx/target/toolchain/bolt20/usr')
env={**os.environ,'LD_LIBRARY_PATH':str(tools/'lib/x86_64-linux-gnu')}
base=root/'target/round12/bolt-build/release/ttfx'
instrumented=out/'ttfx.instrumented'
profiles=out/'profiles'
profiles.mkdir(exist_ok=True)
assert not list(profiles.iterdir()), 'Do not mix profiles from different binaries'
def run(cmd, logfile, **kw):
    print(' '.join(map(str,cmd)),flush=True)
    with (out/logfile).open('w') as log:
        subprocess.run(list(map(str,cmd)),env=env,stdout=log,stderr=subprocess.STDOUT,check=True,**kw)
run([tools/'bin/llvm-bolt-20',base,'-instrument','-o',instrumented,
     '-runtime-instrumentation-lib='+str(tools/'lib/llvm-20/lib/libbolt_rt_instr.a'),
     '-instrumentation-file='+str(profiles/'training.fdata'),
     '-instrumentation-file-append-pid'], 'instrument.log')
run(['python3','tools/tests/train_pgo.py','--binary',instrumented,'--profiles',out/'unused-llvm-profiles','--json',out/'training.json'],'training.log')
files=sorted(profiles.iterdir())
assert len(files)>=111,files
with (out/'merged.fdata').open('w') as f, (out/'merge.log').open('w') as err:
    subprocess.run([str(tools/'bin/merge-fdata-20'),*map(str,files)],env=env,stdout=f,stderr=err,check=True)
run([tools/'bin/llvm-bolt-20',base,'-o',out/'ttfx.bolt','-data='+str(out/'merged.fdata'),
     '-reorder-blocks=ext-tsp','-reorder-functions=cdsort','-split-functions',
     '-split-all-cold','-split-eh','-dyno-stats'],'optimize.log')
report={'sources':{str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(Path('src').rglob('*.rs'))},
        'binaries':{str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in [base,instrumented,out/'ttfx.bolt']},
        'profiles':len(files),'flags':'CARGO_PROFILE_RELEASE_STRIP=none RUSTFLAGS=-C link-arg=-Wl,--emit-relocs --no-default-features'}
(out/'build.json').write_text(json.dumps(report,indent=2)+'\n')
print('Finished: '+str(out/'ttfx.bolt'),flush=True)
