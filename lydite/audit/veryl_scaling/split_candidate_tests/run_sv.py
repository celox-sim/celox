#!/usr/bin/env python3
"""Finite differential tests of actual emitted SV; no symbolic/solver assumptions.
Independent architectural oracle is audit.veryl_scaling.rv32i_isa.execute.
Synthetic Harvard memory obeys atomic W-store / same-edge M-load contract.
"""
import argparse, json, random, subprocess, sys, hashlib
from pathlib import Path
ROOT=Path(__file__).resolve().parents[3];sys.path.insert(0,str(ROOT))
from audit.veryl_scaling.rv32i_isa import execute, INSTRUCTIONS
from audit.veryl_scaling.test_rv32i_pipeline import enc_i,enc_r,enc_s,enc_b

def cases():
 p=[]
 def add(name,seq):p.append((name,seq))
 # Every integer arithmetic operation with sign, overflow and shift edge values.
 for a in (0,1,0x7fffffff,0x80000000,0xffffffff):
  for b in (0,1,31,32,63,0x80000000,0xffffffff):
   def li(r,v):return [(v&0xfffff000)|(r<<7)|0x37,enc_i(0x13,r,r,(v&4095))] if v&2048==0 else [(((v+4096)&0xfffff000))|(r<<7)|0x37,enc_i(0x13,r,r,v&4095)]
   seq=li(1,a)+li(2,b)
   seq += [enc_r(3,1,2,f,f7) for f in range(8) for f7 in ([0,32] if f in (0,5) else [0])]
   seq += [enc_i(0x13,3,1,j,f) for f in (0,2,3,4,6,7) for j in (-2048,-1,0,2047)]
   seq += [enc_i(0x13,3,1,j|(f7<<5),f) for f in (1,5) for f7 in ([0,32] if f==5 else [0]) for j in (0,1,31)]
   add('alu_edges',seq+[0x73])
 for f in (0,1,4,5,6,7):
  for a,b in ((0,0),(-1,1),(1,-1)):
   add('branches',[enc_i(0x13,1,0,a),enc_i(0x13,2,0,b),enc_b(1,2,16,f),enc_i(0x13,3,0,4),enc_i(0x13,4,0,5),enc_i(0x13,5,0,6),0x73])
 add('redirect_squash',[enc_b(0,0,16),enc_s(0,0,0),0xffffffff,enc_i(3,1,0,-4,2),enc_i(0x13,1,0,7),0x73])
 add('jal',[0x010000ef,enc_s(0,0,0),0xffffffff,enc_i(3,1,0,-4,2),enc_i(0x13,2,1,0),0x73])
 add('jalr_alias',[enc_i(0x13,1,0,17),enc_i(0x67,1,1,0),enc_s(0,0,0),0xffffffff,enc_i(0x13,2,1,0),0x73])
 add('repeated_pc',[enc_i(0x13,1,0,5),enc_i(0x13,1,1,-1),enc_b(1,0,-4,1),0x73])
 add('auipc_fence',[0x80000097,enc_i(0x13,2,1,1),0xfff0808f,0x100073])
 for f in (0,1,2):
  for lane in range(4):
   for lf in (0,1,2,4,5):
    add('lanes_store_load',[enc_i(0x13,1,0,-129),enc_s(0,1,lane,f),enc_i(3,2,0,lane,lf),enc_r(3,2,2),0x73])
 for load in (True,False):
  for addr in (-4,-3,255,256,257):
   add('fault_priority',[enc_i(0x13,1,0,addr),(enc_i(3,0,1,0,2) if load else enc_s(1,0,0,2)),enc_s(0,0,0),0x73])
 for ir in (0,0xffffffff,0x2001033,0x40009093,0x100f,0x200073):add('illegal',[enc_i(0x13,1,0,7),ir,enc_s(0,0,0)])
 for off in (2,6):add('target_misaligned',[enc_b(0,0,off),enc_s(0,0,0)])
 add('fetch_fault',[enc_i(0x67,1,0,1024),enc_s(0,0,0)])
 add('youngest_and_mixed',[enc_i(0x13,1,0,3),enc_i(0x13,1,0,7),enc_r(2,1,1),enc_i(0x13,3,0,9),enc_r(4,2,3),enc_i(3,5,0,0,2),enc_r(6,4,5),0x73])
 add('consecutive_partial_stores',[enc_i(0x13,1,0,-129),enc_s(0,1,0,0),enc_s(0,1,1,0),enc_i(3,2,0,0,2),enc_s(0,1,2,1),enc_i(3,3,0,0,2),0x73])
 add('trap_kills_m',[0xffffffff,enc_s(0,0,0),enc_i(3,1,0,-4,2),enc_s(0,0,4)])
 add('x0_alias',[enc_i(0x13,0,0,7),enc_i(0x13,1,0,1),enc_r(1,1,1),enc_r(1,1,1),enc_r(0,1,1),enc_r(2,0,1),0x73])
 rng=random.Random(987213)
 for t in range(30):
  seq=[]
  for _ in range(90):
   d,a,b=[rng.randrange(10) for _ in range(3)];kind=rng.randrange(4)
   if kind==0:seq.append(enc_i(0x13,d,a,rng.randrange(-2048,2048),rng.choice([0,2,3,4,6,7])))
   elif kind==1:
    f=rng.randrange(8);seq.append(enc_r(d,a,b,f,32 if f in (0,5) and rng.randrange(2) else 0))
   elif kind==2:
    f=rng.choice([0,1,2,4,5]);seq.append(enc_i(3,d,0,rng.randrange(32)*(1<<(f&3)),f))
   else:
    f=rng.randrange(3);seq.append(enc_s(0,b,rng.randrange(32)*(1<<f),f))
  add('random_dependencies',seq+[0x73])
 return p

def selector_cases():
 """Every rs1/rs2 pair from a distinct-valued full architectural bank."""
 out=[]
 bank=[enc_i(0x13,r,0,r*37-500) for r in range(1,32)]
 for a in range(32):
  for b in range(32):
   out.append((f'all_selectors_{a}_{b}',bank+[0x13]*4+[enc_r(a,a,b,4),enc_r(b,a,b,0),0x73]))
 for r in range(32):
  out.append((f'm_over_w_both_sources_{r}',bank+[0x13]*4+[enc_i(0x13,r,0,1),enc_i(0x13,r,0,2),enc_r((r+1)%32,r,r),0x73]))
 return out

def expected(seq,covered):
 regs=[0]*32;mem=[0x807f80ff^j for j in range(64)];pc=0;rows=[]
 for _ in range(2000):
  ir=seq[pc//4] if pc%4==0 and pc//4<len(seq) else 0
  op=ir&127;off=ir>>20 if op==3 else ((ir>>25)<<5)|((ir>>7)&31)
  if off&2048:off-=4096
  addr=(regs[(ir>>15)&31]+off)&0xffffffff;valid=addr//4<64
  e=execute(ir,pc,regs,mem[addr//4] if valid else 0,pc//4>=len(seq),not valid)
  if e.mnemonic:covered.add(e.mnemonic)
  if e.rd:regs[e.rd]=e.value
  if e.store_mask:
   for lane in range(4):
    if e.store_mask>>lane&1:mem[e.store_address//4]=(mem[e.store_address//4]&~(255<<(lane*8)))|(e.store_data&(255<<(lane*8)))
  rows.append([pc,ir,int(e.trap),e.cause,e.trap_value,int(bool(e.store_mask)),e.store_address,e.store_data,e.store_mask,*regs])
  if e.trap:return rows
  pc=e.next_pc
 raise AssertionError('oracle did not terminate')

TB='''module tb;
bit clk=0,rst_n=0,stall=0; bit [31:0] imem_response,dmem_response; bit imem_fault,dmem_fault;
wire [31:0] imem_address,dmem_address,write_address,write_data,trap_pc,trap_value;
wire imem_valid,dmem_valid,dmem_write,write_enable,commit,retire,trap_valid,halted;
wire [3:0] write_mask,trap_cause; wire [31:0] r[0:31];
MODULE dut(.clk(clk),.rst_n(rst_n),.stall(stall),.imem_response(imem_response),.imem_fault(imem_fault),.dmem_response(dmem_response),.dmem_fault(dmem_fault),.imem_address(imem_address),.dmem_address(dmem_address),.imem_valid(imem_valid),.dmem_valid(dmem_valid),.dmem_write(dmem_write),.write_enable(write_enable),.write_address(write_address),.write_data(write_data),.write_mask(write_mask),.commit(commit),.retire(retire),.trap_valid(trap_valid),.trap_cause(trap_cause),.trap_pc(trap_pc),.trap_value(trap_value),.halted(halted),REGPORTS);
bit [31:0] rom[0:4095],ram[0:63],ex[0:2047][0:40];
integer n,ne,fd,rc,t,c,j,k,idx,retired_total=0,collisions=0,stalls=0;
bit [31:0] want[0:31];bit retired_now;string datafile;
wire [STATEWIDTH-1:0] state_bits=STATECONCAT;bit [STATEWIDTH-1:0] saved_state;
integer enabled,qhead,qtail,token_time[0:8191],token_pc[0:8191],token_id[0:8191],serial=0,latency,minlat=999,maxlat=0;

always_comb begin
 imem_response=0;imem_fault=1;
 if(imem_address[31:2]<n) begin imem_response=rom[imem_address[31:2]];imem_fault=0;end
 dmem_response=0;dmem_fault=1;
 if(dmem_address[31:2]<64) begin
 dmem_response=ram[dmem_address[31:2]];dmem_fault=0;
 if(write_enable && write_address[31:2]==dmem_address[31:2])
 for(integer lane=0;lane<4;lane++) if(write_mask[lane]) dmem_response[lane*8+:8]=write_data[lane*8+:8];
 end
end
initial begin
 if(!$value$plusargs("data=%s",datafile)) $fatal(1,"missing data");fd=$fopen(datafile,"r");t=0;
 while(!$feof(fd)) begin
 rc=$fscanf(fd,"%d %d",n,ne);if(rc!=2)break;
 for(j=0;j<n;j++)rc=$fscanf(fd,"%h",rom[j]);
 for(j=0;j<ne;j++)for(k=0;k<41;k++)rc=$fscanf(fd,"%h",ex[j][k]);
 rst_n=0;stall=0;clk=0;#2;clk=1;#2;ALIGNCHECKclk=0;#2;
 // Populate D/X/M, then reset while stalled to check reset priority and all state.
 rst_n=1;repeat(3)begin #2;clk=1;#2;ALIGNCHECKclk=0;end
 rst_n=0;stall=1;#2;clk=1;#2;ALIGNCHECKclk=0;#2;
 RESETCHECK
 stall=0;
 for(j=0;j<32;j++)begin want[j]=0;if(r[j]!==0)$fatal(1,"reset register");end
 for(j=0;j<64;j++)ram[j]=32'h807f80ff^j;
 rst_n=1;idx=0;enabled=0;qhead=0;qtail=0;
 for(c=0;c<4000 && idx<ne;c++) begin
 stall=((c*13+t*7)%17<4);#2;
 if(stall)begin stalls++;if(retire||commit||trap_valid||write_enable||imem_valid||dmem_valid)$fatal(1,"stall pulse");end
 saved_state=state_bits;
 if(!stall)enabled++;
 retired_now=retire;
 if(retire)begin
 if(qhead>=qtail || token_pc[qhead]!==dut.w_pc)$fatal(1,"dynamic token mismatch");
 latency=enabled-token_time[qhead];qhead++;
 if(latency<minlat)minlat=latency;if(latency>maxlat)maxlat=latency;
 $display("LAT %0d %0d %0d %0d",t,idx,latency,token_id[qhead-1]);
 if(dut.w_redirect || trap_valid)qtail=qhead;
 if(dut.w_pc!==ex[idx][0] || dut.w_ir!==ex[idx][1] || trap_valid!==ex[idx][2][0])$fatal(1,"retirement t=%0d c=%0d idx=%0d pc=%h expected=%h trap=%b expected=%h",t,c,idx,dut.w_pc,ex[idx][0],trap_valid,ex[idx][2]);
 if(trap_valid && (trap_cause!==ex[idx][3][3:0] || trap_value!==ex[idx][4] || trap_pc!==ex[idx][0]))$fatal(1,"trap details t=%0d idx=%0d",t,idx);
 if(write_enable!==ex[idx][5][0])$fatal(1,"write enable");
 if(write_enable && (write_address!==ex[idx][6] || write_data!==ex[idx][7] || write_mask!==ex[idx][8][3:0]))$fatal(1,"store details");
 for(j=0;j<32;j++)want[j]=ex[idx][9+j];
 $display("RET %0d %0d %0d %h",t,c,idx,dut.w_pc);
 idx++;retired_total++;
 end
 if(imem_valid)begin token_pc[qtail]=imem_address;token_time[qtail]=enabled;token_id[qtail]=serial;serial++;qtail++;end
 if(write_enable)begin
 if(dmem_valid&&!dmem_write&&write_address[31:2]==dmem_address[31:2])collisions++;
 for(j=0;j<4;j++)if(write_mask[j])ram[write_address[31:2]][j*8+:8]=write_data[j*8+:8];
 end
 #2;clk=1;#2;ALIGNCHECK
 if(stall && state_bits!==saved_state)$fatal(1,"stall state changed");
 for(j=0;j<32;j++)if(r[j]!==want[j])$fatal(1,"register t=%0d c=%0d r=%0d got=%h expected=%h",t,c,j,r[j],want[j]);
 clk=0;#2;
 end
 if(idx!=ne || !halted)$fatal(1,"missing terminal trap");
 stall=0;repeat(4)begin #2;if(retire||commit||trap_valid||write_enable||imem_valid||dmem_valid)$fatal(1,"halt pulse");clk=1;#2;ALIGNCHECKclk=0;end
 t++;
 end
 $display("PASS cases=%0d retirements=%0d sameedge_store_load=%0d stalls=%0d observed_enabled_latency=%0d..%0d",t,retired_total,collisions,stalls,minlat,maxlat);$finish;
end
endmodule
'''

def main():
 ap=argparse.ArgumentParser();ap.add_argument('--all-selectors',action='store_true');ap.add_argument('--sv',type=Path,required=True);ap.add_argument('--out',type=Path,required=True);ap.add_argument('--tools',type=Path,required=True);args=ap.parse_args();args.out.mkdir(parents=True,exist_ok=True)
 covered=set();cs=cases()+(selector_cases() if args.all_selectors else []);lines=[];total=0
 for name,p in cs:
  rows=expected(p,covered);total+=len(rows);lines += [f'{len(p)} {len(rows)}',' '.join(f'{v:x}' for v in p)]+[' '.join(f'{v:x}' for v in row) for row in rows]
 assert set(INSTRUCTIONS)<=covered,set(INSTRUCTIONS)-covered
 (args.out/'cases.txt').write_text('\n'.join(lines)+'\n');(args.out/'cases.json').write_text(json.dumps([{'name':n,'program':[hex(v) for v in p]} for n,p in cs],indent=2))
 import re
 mod=re.search(r'module\s+(\w+)',args.sv.read_text())[1]
 states=sorted(set(re.findall(r'\b(\w+)\s*<=',args.sv.read_text())))
 concat='{'+','.join('dut.'+k for k in states)+'}'
 onehot={'d_sel1','d_sel2'}<=set(states)
 resetcheck='\n'.join('if(dut.'+k+"!=="+("32'd1" if onehot and k in ('d_sel1','d_sel2') else "'0")+')$fatal(1,"reset '+k+'");' for k in states)
 aligncheck='''if(dut.d_sel1!==(32'd1 << dut.d_ir[19:15]) || dut.d_sel2!==(32'd1 << dut.d_ir[24:20]))$fatal(1,"onehot selector identity including invalid payload");''' if onehot else ''
 tb=TB.replace('RESETCHECK',resetcheck).replace('ALIGNCHECK',aligncheck).replace('STATEWIDTH','$bits('+concat+')').replace('STATECONCAT',concat).replace('MODULE',mod).replace('REGPORTS',','.join(f'.r{j}(r[{j}])' for j in range(32)))
 (args.out/'tb.sv').write_text(tb)
 cmd=[str(args.tools/'verilator'),'--binary','--timing','-Wno-fatal','--top-module','tb','--Mdir',str(args.out/'obj'),'-o','sim',str(args.sv),str(args.out/'tb.sv')]
 cp=subprocess.run(cmd,capture_output=True,text=True);(args.out/'compile.log').write_text(cp.stdout+cp.stderr);assert cp.returncode==0,'compile failed'
 cp=subprocess.run([str(args.out/'obj/sim'),'+data='+str(args.out/'cases.txt')],capture_output=True,text=True);(args.out/'simulation.log').write_text(cp.stdout+cp.stderr)
 report={'boundary':'actual Veryl-emitted SystemVerilog simulated by Verilator; independent handwritten integer oracle; synthetic Harvard memory contract','cases':len(cs),'exhaustive_selector_pairs':1024 if args.all_selectors else 0,'expected_retirements':total,'instruction_names':sorted(covered),'sv_sha256':hashlib.sha256(args.sv.read_bytes()).hexdigest(),'returncode':cp.returncode,'tail':cp.stdout.splitlines()[-5:]}
 from collections import Counter
 dist=Counter();examples={}
 for line in cp.stdout.splitlines():
  if line.startswith('LAT '):
   _,c,i,lat,t=line.split();dist[lat]+=1;examples.setdefault(lat,dict(case=int(c),name=cs[int(c)][0],retirement_index=int(i),dynamic_token_id=int(t)))
 report.update(onehot_selector_identity_checked=onehot,observed_enabled_edge_latency_histogram=dict(dist),latency_examples=examples,sequential_state_variables_checked=len(states),limits=['Finite concrete simulation, not universal refinement or latency proof','Synthetic Harvard memory; not physical RAM integration','Latency is accepted-fetch edge to own retirement edge, stalls excluded; younger flushes removed by dynamic FIFO'])
 (args.out/'report.json').write_text(json.dumps(report,indent=2));print(json.dumps(report,indent=2));assert cp.returncode==0,'simulation failed'
if __name__=='__main__':main()
