#!/usr/bin/env python3
"""Run mesh regressions in bounded child processes; never hang the review indefinitely."""
import argparse
import json
from pathlib import Path
import subprocess
p=argparse.ArgumentParser()
p.add_argument('--interleavings',type=int,default=1000)
p.add_argument('--concurrent',type=int,default=25)
p.add_argument('--timeout',type=float,default=10)
a=p.parse_args()
root=Path(__file__).resolve().parents[1]
build=subprocess.run(['cargo','test','--release','--offline','-p','coagomem-core','--lib','--no-run','--message-format=json'],cwd=root,text=True,capture_output=True,check=True)
binaries=[]
for line in build.stdout.splitlines():
 try:event=json.loads(line)
 except json.JSONDecodeError:continue
 if event.get('reason')=='compiler-artifact' and event.get('executable') and event.get('profile',{}).get('test'):binaries.append(event['executable'])
assert len(binaries)==1,binaries
for test,count in [('mesh::tests::a_new_doorbell_cannot_strand_data_behind_the_cached_tail',a.interleavings),('mesh::tests::grid_delivers_fifo_per_lane_without_loss_under_concurrency',a.concurrent)]:
 for _ in range(count):
  result=subprocess.run([binaries[0],'--exact',test],text=True,capture_output=True,timeout=a.timeout,check=True)
  assert '1 passed' in result.stdout,result.stdout
 print(f'{test}: {count} bounded independent runs passed',flush=True)
print('VERDICT: PASS')
