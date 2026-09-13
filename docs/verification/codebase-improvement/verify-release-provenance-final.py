import hashlib,json,pathlib,subprocess
work=pathlib.Path('/tmp/fux-codebase-work');repo=pathlib.Path('/Users/kisaczka/Desktop/code/fux')
original=json.loads((work/'release-comparison-20260913/manifest.json').read_text())
followup=json.loads((work/'recovery-readiness-20260913/manifest.json').read_text())
assert original['complete'] and followup['complete']
def digest(p):
 with open(p,'rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()
records=[]
for variant in ['baseline','candidate']:
 source=work/'performance-baseline-source' if variant=='baseline' else repo
 target=work/('release-'+variant)
 for binary in ['fux','zor']:
  path=target/'release'/binary
  expected=original['products'][variant][binary]['sha256']
  assert digest(path)==expected==followup['products'][variant][binary]['sha256']
  args=['cargo','+stable','build','--release','--locked','--target-dir',str(target),'-p',binary,'--bin',binary,'--message-format=json']
  if binary=='zor':args+=['--no-default-features','--features','cli']
  result=subprocess.run(args,cwd=source,capture_output=True,text=True,timeout=300)
  (work/f'final-provenance-{variant}-{binary}.jsonl').write_text(result.stdout)
  (work/f'final-provenance-{variant}-{binary}.log').write_text(result.stderr)
  assert result.returncode==0,result.stderr
  artifacts=[json.loads(line) for line in result.stdout.splitlines()]
  bins=[r for r in artifacts if r.get('reason')=='compiler-artifact' and r.get('executable') and r['target']['name']==binary]
  assert len(bins)==1 and bins[0]['fresh'],bins
  assert digest(path)==expected
  records.append({'variant':variant,'binary':binary,'command':args,'cwd':str(source),'fresh':True,'unchanged_sha256':expected})
files=[repo/'Cargo.toml',repo/'Cargo.lock']
for crate in ['fux','zor','local-ipc']:
 base=repo/'crates'/crate;files.append(base/'Cargo.toml');files+=list((base/'src').rglob('*.rs'))
source_hashes={str(p.relative_to(repo)):digest(p) for p in sorted(files)}
report={'complete':True,'scope':'Cargo confirms measured product binaries fresh for current compilation inputs; before/after hashes match both comparisons. The readiness-based harness has distinct provenance in the final comparison manifest.','records':records,'current_product_source_hashes':source_hashes,'current_harness_sha256':digest(repo/'target/rust-harness/debug/fux-xtask')}
assert report['current_harness_sha256']==followup['harness']['sha256']
(work/'release-provenance-final.json').write_text(json.dumps(report,indent=2)+'\n')
print('PASS four release binaries are fresh and unchanged; current product source hashes retained')
