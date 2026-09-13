import collections, json, math, pathlib, statistics, sys
root=pathlib.Path(sys.argv[1]);manifest=json.loads((root/'manifest.json').read_text())
assert manifest['complete'] and len(manifest['runs'])==48 and all(r['complete'] and r['exit_code']==0 for r in manifest['runs'])
series=collections.defaultdict(lambda: collections.defaultdict(dict))
def put(name,product,block,values): series[name][product][block]=values if isinstance(values,list) else [values]
for run in manifest['runs']:
 x=json.loads((root/run['label']/'evidence.json').read_text());w,p,b=run['workload'],run['product'],run['block']
 if w=='idle-burst':
  put('single-viewer key latency (ms)',p,b,[s*1000 for s in x['latency_samples_s']])
  for name,key in [('idle CPU (s/10s)','idle_cpu_s_per_10s'),('20k output burst (s)','burst_20000_lines_s'),('post-burst RSS (KiB)','rss_after_burst_kib')]:put(name,p,b,x[key])
 elif w=='many-viewers':
  r=x['results'][0]
  put('four-viewer key latency (ms)',p,b,[s*1000 for s in r['latency_samples_s']])
  put('four-viewer frame bytes/key',p,b,r['frame_byte_samples'])
  put('four-viewer CPU (s/1000 keys)',p,b,r['server_cpu_s_per_1000_keystrokes'])
  put('four-viewer burst frame bytes',p,b,r['burst_bytes_first_viewer'])
 elif w=='resize':
  for r in x['results']:
   put(f'resize {r["panes"]} panes (ms)',p,b,r['request_samples_ms'])
   put(f'resize {r["panes"]} panes CPU (s/200 ops)',p,b,r['server_cpu_s'])
   put(f'resize {r["panes"]} panes frame bytes',p,b,r['viewer_bytes'])
 elif w=='history-memory':put('10k plain+wide history RSS (KiB)',p,b,x['rss_after_wide_kib'])
 elif w=='scroll-manager':
  for key in ['scroll','manager']:put(key+' latency (ms)',p,b,x[key]['samples_ms'])
  for name,key in [('scroll PTY bytes','scroll_pty_bytes'),('scroll server CPU (s/100 events)','scroll_server_cpu_s'),('scroll viewer CPU (s/100 events)','scroll_viewer_cpu_s'),('manager CPU (s/200 ops)','manager_server_cpu_s')]:put(name,p,b,x[key])
  for owner in ['server','viewer']:put('scroll '+owner+' sampled peak RSS (KiB)',p,b,max(r[owner+'_kib'] for r in x['rss_samples']))
 elif w=='recovery':
  for name,key in [('recovery latency (ms)','recovery_ms'),('recovery retry latency (ms)','idempotent_recovery_ms'),('recovery server CPU (s/op)','server_cpu_s')]:put(name,p,b,[r[key] for r in x['samples']])
 elif w=='journal':
  for kind in ['adopt','inspect','idempotent-adopt']:
   samples=[s for r in x['runs'] for s in r['samples'] if s['kind']==kind]
   put('journal '+kind+' latency (ms)',p,b,[s['elapsed_ms'] for s in samples])
   put('journal '+kind+' child CPU (ms/op)',p,b,[s['child_cpu_ms'] for s in samples])
 elif w=='pressure':
  assert .95 <= x['calibration']['ratio'] <= 1.05
  for row in x['results']:
   case=f'pressure {row["panes"]}p/{row["viewers"]}v/slow={row["slow"]}'
   for owner in ['fux','zor']:
    snapshots=[phase[side]['owners'][owner] for phase in row['phases'] for side in ['before','after']]
    put(case+' '+owner+' sampled peak RSS (KiB)',p,b,max(s['rss_bytes'] for s in snapshots)/1024)
   put(case+' decoder backlog high-water (bytes)',p,b,max(v['pending_high_water_bytes'] for phase in row['phases'] for side in ['before','after'] for v in phase[side]['viewers']))
   for phase in row['phases']:
    if phase['visible_ms'] is not None:put(case+' '+phase['name']+' visible (ms)',p,b,phase['visible_ms'])
    for owner in ['fux','zor']:
     start,end=(phase[side]['owners'][owner] for side in ['before','after'])
     assert start['pid']==end['pid'] and start['start_abstime']==end['start_abstime']
     ticks=end['user_ticks']+end['system_ticks']-start['user_ticks']-start['system_ticks']
     put(case+' '+phase['name']+' '+owner+' CPU (ms)',p,b,ticks*end['timebase_numer']/end['timebase_denom']/1e6)
def stats(values):
 ordered=sorted(values)
 return {'count':len(values),'median':statistics.median(values),'p95':ordered[max(0,math.ceil(len(ordered)*.95)-1)],'min':min(values),'max':max(values)}
result={'complete':True,'comparison_root':str(root),'metrics':{},'limits':'Three alternating blocks on one host. Pooled percentiles are descriptive, not confidence intervals. Peak values are maxima of discrete samples. Decoder backlog is reader-side pending data, not runtime queue allocation. Existing idle CPU is rounded to 0.001 s.'}
for name,products in series.items():
 row={}
 for product,blocks in products.items():
  assert set(blocks)=={1,2,3}
  row[product]={'pooled':stats([v for block in sorted(blocks) for v in blocks[block]]),'blocks':{block:stats(v) for block,v in blocks.items()}}
 a,b=row['baseline']['pooled']['median'],row['candidate']['pooled']['median']
 row['median_change_percent']=(b/a-1)*100 if a else None
 result['metrics'][name]=row
(root/'analysis.json').write_text(json.dumps(result,indent=2)+'\n')
for name,row in result['metrics'].items():
 if name.startswith('pressure'):continue
 print(name, 'baseline',round(row['baseline']['pooled']['median'],5),'candidate',round(row['candidate']['pooled']['median'],5),'change',None if row['median_change_percent'] is None else round(row['median_change_percent'],1))
