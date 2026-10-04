"""Independent byte-array semantics and actual source memory-effect controls."""
import re
import shutil
from protocols.axi4lite_project import replay

OBLIGATIONS = ('requests','capacity','effects','completion','response','readback')


def upgrade64(path):
    p = path / 'memory.veryl'; text = p.read_text()
    text = text.replace("32'h11223344", "64'h1122334455667788").replace("32'h55667788", "64'h99aabbccddeeff00")
    text = text.replace('bit<32>', 'bit<64>').replace('bit<4>', 'bit<8>').replace("32'd", "64'd").replace("8'hfc", "8'hf8").replace('word_addr == 4', 'word_addr == 8')
    text = re.sub(r"\b4'd", "8'd", text)
    extra = ''.join(f'    var lane{cell}_{lane}: bit<64>;\n' for cell in (0,1) for lane in range(4,8)) + '    always_comb {\n'
    for cell in (0,1):
        for lane in range(4,8):
            mask = 255 << (8 * lane)
            extra += f"        if (held_strb & 8'd{1<<lane}) != 0 {{ lane{cell}_{lane} = held_data & 64'd{mask}; }}\n        else {{ lane{cell}_{lane} = mem{cell} & 64'd{mask}; }}\n"
        old = ' | '.join(f'lane{cell}_{lane}' for lane in range(4))
        text = text.replace(old + ';', old + ' | ' + ' | '.join(f'lane{cell}_{lane}' for lane in range(4,8)) + ';')
    extra += '    }\n'; text = text.replace('    assign awready', extra + '    assign awready'); p.write_text(text)
    p = path / 'memory.hwv'; p.write_text(p.read_text().replace('bv<32>','bv<64>').replace('bv<4>','bv<8>').replace('0u32','0u64').replace('0u4','0u8'))
    p = path / 'memory-project.json'; m = replay.load_json(p)
    for signal, width in [('bus_wdata',64),('bus_rdata',64),('bus_wstrb',8)]: m['signals'][signal]['type'] = {'bv':width}
    replay.write(p,m)
    p = path / 'memory-contract.json'; c = replay.load_json(p); c['contract']['data_width'] = 64
    c['contract']['locations'][0]['initial'] = 0x1122334455667788
    c['contract']['locations'][1].update(address=8,initial=0x99aabbccddeeff00); replay.write(p,c)
    p = path / 'memory-axi.json'; c = replay.load_json(p); c['config']['data_width'] = 64; replay.write(p,c)


def check_memory(trace, contract):
    """Byte-array oracle, independent of the generated native mask expressions."""
    lanes = contract['data_width'] // 8
    memory = {r['address']: bytearray(r['initial'].to_bytes(lanes,'little')) for r in contract['locations']}
    aw = None; w = None; applied = False; read = None; effects = 0; responses = 0; reads = []
    def value(row, name): return int(row[name])
    for frame in trace:
        after = frame['after']
        if frame['edge']:
            before = frame['before']; old_memory = {a: bytes(b) for a,b in memory.items()}
            if value(before,'apply_write'):
                if aw is None or w is None or applied: raise ValueError('effect without a fresh accepted AW/W pair')
                word = aw // lanes * lanes
                if word in memory:
                    data, strobes = w; incoming = data.to_bytes(lanes,'little')
                    for lane in range(lanes):
                        if strobes >> lane & 1: memory[word][lane] = incoming[lane]
                applied = True; effects += 1
            if value(before,'bvalid') and value(before,'bready'):
                if not applied or aw is None or w is None: raise ValueError('response without applied request')
                responses += 1; aw = None; w = None; applied = False
            if value(before,'awvalid') and value(before,'awready'):
                if aw is not None: raise ValueError('accepted AW overwrites pending request')
                aw = value(before,'awaddr')
            if value(before,'wvalid') and value(before,'wready'):
                if w is not None: raise ValueError('accepted W overwrites pending request')
                w = value(before,'wdata'),value(before,'wstrb')
            if value(before,'rvalid') and value(before,'rready'):
                if read is None or (value(before,'rdata'),value(before,'rresp')) != read: raise ValueError('wrong accepted readback')
                reads.append(read); read = None
            if value(before,'arvalid') and value(before,'arready'):
                if read is not None: raise ValueError('accepted AR overwrites pending one-slot read')
                word = value(before,'araddr') // lanes * lanes
                read = (int.from_bytes(old_memory[word],'little'),0) if word in old_memory else (0,3)
        for location in contract['locations']:
            if value(after,location['state']) != int.from_bytes(memory[location['address']],'little'): raise ValueError('memory bytes differ from accepted-request semantics')
        if value(after,'bvalid'):
            if not applied or aw is None or w is None: raise ValueError('offered response precedes application')
            if value(after,'bresp') != (0 if aw // lanes * lanes in memory else 3): raise ValueError('write response status does not match address region')
        if bool(value(after,'rvalid')) != (read is not None): raise ValueError('read offer tracking mismatch')
        if read is not None and (value(after,'rdata'),value(after,'rresp')) != read: raise ValueError('readback or stalled data mismatch')
    return {'effect_events':effects,'accepted_write_responses':responses,'accepted_reads':reads,
            'memory':{str(a):int.from_bytes(b,'little') for a,b in memory.items()}}


def run(root, good, cli, axi, out):
    results = []
    for width in (32,64):
        path = root / ('byte-memory-'+str(width)); shutil.copytree(good,path)
        if width == 64: upgrade64(path)
        binding = path / 'memory-contract.json'; contract = replay.load_json(binding)['contract']; lanes = width//8
        for obligation in OBLIGATIONS:
            result = cli(f'memory-{width}-search-{obligation}','search',binding,obligation)
            if result['status'] != 'bounded_no_failure': raise RuntimeError(result)
        protocol = axi(f'memory-{width}-axi-search','search',path/'memory-axi.json')
        if protocol['status'] != 'bounded_no_failure' or protocol['capacity_search'] != 'bounded_no_failure' or protocol['structural']['status'] != 'verified': raise RuntimeError(protocol)
        results.append({'case':f'memory_{width}_all_searches','status':'passed','depth':10,'obligations':list(OBLIGATIONS),'axi':'bounded_no_failure'})
        m = replay.load_json(path/'memory-project.json'); flags = {'rst','awvalid','wvalid','arvalid','bready','rready'}
        def row(**values): return {**{k:False if k in flags else 0 for k in m['inputs']},**values}
        z = row(rst=True); data = 0xaabbccdd if width==32 else 0x0123456789abcdef
        def write(address=0, strobes=None, order='together'):
            a = {'awvalid':True,'awaddr':address}; w = {'wvalid':True,'wdata':data,'wstrb':(1<<lanes)-1 if strobes is None else strobes}
            offers = [row(**a),row(),row(**w)] if order=='aw_first' else [row(**w),row(),row(**a)] if order=='w_first' else [row(**a,**w)]
            return [z,row(),*offers,row(),row(),row(),row(bready=True),row(arvalid=True,araddr=address),row(rready=True)]
        scenarios = {'full_write':write(),'aw_first':write(order='aw_first'),'w_first':write(order='w_first'),
                     'zero_strobes':write(strobes=0),'sparse_strobes':write(strobes=0x5 if width==32 else 0x81),
                     'other_word':write(address=lanes),'unmapped':write(address=lanes*2),
                     'pending_aw': [z,row(),row(awvalid=True)] + [row(bready=True)]*6,
                     'pending_aw_backpressure':[z,row(),row(awvalid=True),row(awvalid=True,awaddr=lanes),row(awvalid=True,awaddr=lanes,wvalid=True,wdata=data,wstrb=(1<<lanes)-1),row(awvalid=True,awaddr=lanes),row(awvalid=True,awaddr=lanes,bready=True),row(awvalid=True,awaddr=lanes)],
                     'reset_idle':[z,row(),row()],
                     'read_write_same_edge':[z,row(),row(awvalid=True,wvalid=True,wdata=data,wstrb=(1<<lanes)-1),row(arvalid=True),row(bready=True,rready=True),row(arvalid=True),row(rready=True)],
                     'stalled_read_during_write':[z,row(),row(arvalid=True),row(awvalid=True,wvalid=True,wdata=data,wstrb=(1<<lanes)-1),row(),row(bready=True),row(rready=True)],
                     'successive_writes':[z,row(),row(awvalid=True,wvalid=True,wdata=data,wstrb=1),row(),row(bready=True),row(awvalid=True,wvalid=True,wdata=0,wstrb=2),row(),row(bready=True),row(arvalid=True),row(rready=True)]}
        for offset in range(lanes): scenarios['offset_'+str(offset)] = write(address=offset,strobes=((1<<lanes)-1) ^ ((1<<offset)-1))
        for name, frames in scenarios.items():
            file = path/(name+'.json'); replay.write(file,frames)
            for obligation in OBLIGATIONS:
                result = cli(f'memory-{width}-{name}-{obligation}','stimulus',binding,obligation,'--inputs',file)
                if result['status'] != 'trace_no_failure': raise RuntimeError((name,obligation,result))
            evidence = out/f'memory-{width}-{name}-effects'
            observed = check_memory(replay.load_json(evidence/'simulation.json')['trace'],contract)
            result = axi(f'memory-{width}-{name}-axi','stimulus',path/'memory-axi.json','--inputs',file)
            if result['status'] != 'trace_no_failure' or result['independent']['status'] != 'sampled_prefix_passed': raise RuntimeError(result)
            expected_count = 0 if name in ('pending_aw','reset_idle') else 2 if name=='successive_writes' else 1
            if observed['effect_events'] != expected_count or observed['accepted_write_responses'] != expected_count: raise RuntimeError('memory progress cover vacuous')
            if name=='read_write_same_edge' and observed['accepted_reads'] != [(contract['locations'][0]['initial'],0),(data,0)]: raise RuntimeError('read-before-write semantics not exercised')
            results.append({'case':f'memory_{width}_{name}','status':'passed','oracle':observed})
        high = root / ('memory-active-high-'+str(width)); shutil.copytree(path,high)
        source=high/'memory.veryl';source.write_text(source.read_text().replace('!rst_n','rst_n'))
        manifest=high/'memory-project.json';m_high=replay.load_json(manifest);m_high['reset']['active']=1;replay.write(manifest,m_high)
        for obligation in OBLIGATIONS:
            result=cli(f'memory-{width}-active-high-{obligation}','stimulus',high/'memory-contract.json',obligation,'--inputs',high/'full_write.json')
            if result['status']!='trace_no_failure':raise RuntimeError(result)
        check_memory(replay.load_json(out/f'memory-{width}-active-high-effects'/'simulation.json')['trace'],contract)
        results.append({'case':f'memory_{width}_reset_polarity','status':'passed'})
        cleared=root/('memory-idle-read-clear-'+str(width));shutil.copytree(path,cleared)
        source=cleared/'memory.veryl';source.write_text(source.read_text().replace('if rvalid && rready { rvalid = 0; }','if rvalid && rready { rvalid = 0; rdata = 0; rresp = 0; }'))
        for obligation in OBLIGATIONS:
            result=cli(f'memory-{width}-idle-clear-search-{obligation}','search',cleared/'memory-contract.json',obligation)
            if result['status']!='bounded_no_failure':raise RuntimeError(result)
        file=cleared/'idle-clear.json';replay.write(file,[z,row(),row(arvalid=True),row(),row(rready=True),row(),row(arvalid=True,araddr=lanes*2),row(),row(rready=True),row()])
        result=cli(f'memory-{width}-idle-clear-readback','stimulus',cleared/'memory-contract.json','readback','--inputs',file)
        if result['status']!='trace_no_failure':raise RuntimeError(result)
        observed=check_memory(replay.load_json(out/f'memory-{width}-idle-clear-readback'/'simulation.json')['trace'],contract)
        if observed['accepted_reads']!=[(contract['locations'][0]['initial'],0),(0,3)]:raise RuntimeError('idle-clear cover vacuous')
        result=axi(f'memory-{width}-idle-clear-axi','stimulus',cleared/'memory-axi.json','--inputs',file)
        if result['status']!='trace_no_failure' or result['independent']['status']!='sampled_prefix_passed':raise RuntimeError(result)
        results.append({'case':f'memory_{width}_idle_read_payload_clear','status':'passed'})
        # An actual one-slot DUT must not replace a stalled read, even when
        # old/new responses happen to have equal data. Retire/refill is permitted.
        overwrite=root/('memory-read-overwrite-'+str(width));shutil.copytree(path,overwrite)
        source=overwrite/'memory.veryl';source.write_text(source.read_text().replace('assign arready = !rvalid;','assign arready = 1;').replace('if rvalid && rready { rvalid = 0; }','if rvalid && rready && !(arvalid && arready) { rvalid = 0; }'))
        for obligation in ('capacity','readback'):
            saved=out/f'memory-{width}-read-overwrite-{obligation}.regression.json'
            result=cli(f'memory-{width}-read-overwrite-search-{obligation}','search',overwrite/'memory-contract.json',obligation,'--regression',saved)
            if result['status']!='reset_reachable_failure':raise RuntimeError(result)
            result=cli(f'memory-{width}-read-overwrite-replay-{obligation}','replay',overwrite/'memory-contract.json',obligation,'--regression',saved)
            if result['status']!='reset_reachable_failure':raise RuntimeError(result)
        for label,address in [('different_data',lanes),('equal_data',0),('different_response',lanes*2)]:
            frames=[z,row(),row(arvalid=True),row(arvalid=True,araddr=address),row(arvalid=True,araddr=address),row(arvalid=True,araddr=address,rready=True),row(arvalid=True,araddr=address),row(rready=True)]
            file=overwrite/(label+'.json');replay.write(file,frames)
            for obligation in OBLIGATIONS:
                result=cli(f'memory-{width}-read-stall-good-{label}-{obligation}','stimulus',binding,obligation,'--inputs',file)
                if result['status']!='trace_no_failure':raise RuntimeError(result)
            check_memory(replay.load_json(out/f'memory-{width}-read-stall-good-{label}-readback'/'simulation.json')['trace'],contract)
            for obligation in ('capacity','readback'):
                result=cli(f'memory-{width}-read-overwrite-{label}-{obligation}','stimulus',overwrite/'memory-contract.json',obligation,'--inputs',file)
                expected='trace_no_failure' if label=='equal_data' and obligation=='readback' else 'reset_reachable_failure'
                if result['status']!=expected:raise RuntimeError(result)
            observed=replay.load_json(out/f'memory-{width}-read-overwrite-{label}-capacity'/'simulation.json')['trace']
            try:check_memory(observed,contract)
            except ValueError as e:
                if 'accepted AR overwrites pending' not in str(e):raise
            else:raise RuntimeError('oracle missed pending read overwrite')
            results.append({'case':f'memory_{width}_read_overwrite_{label}','status':'passed','scope':'explicit one-slot application read capacity; equal responses do not hide accepted requests'})
        # The same always-ready fixture is valid on this retire/refill trace.
        # This checks the contract relation, not global AXI structural compliance.
        file=overwrite/'refill.json';replay.write(file,[z,row(),row(arvalid=True),row(arvalid=True,araddr=lanes,rready=True),row(rready=True)])
        for obligation in OBLIGATIONS:
            result=cli(f'memory-{width}-read-refill-{obligation}','stimulus',overwrite/'memory-contract.json',obligation,'--inputs',file)
            if result['status']!='trace_no_failure':raise RuntimeError(result)
        observed=check_memory(replay.load_json(out/f'memory-{width}-read-refill-readback'/'simulation.json')['trace'],contract)
        if observed['accepted_reads']!=[(contract['locations'][0]['initial'],0),(contract['locations'][1]['initial'],0)]:raise RuntimeError('read retire/refill cover vacuous')
        results.append({'case':f'memory_{width}_read_retire_refill','status':'passed'})
        if width!=32: continue
        overflow=root/'memory-overrun';shutil.copytree(path,overflow);source=overflow/'memory.veryl';source.write_text(source.read_text().replace('assign awready = !a_full;','assign awready = 1;'))
        result=cli('memory-overrun-search','search',overflow/'memory-contract.json','capacity')
        if result['status']!='reset_reachable_failure':raise RuntimeError(result)
        result=cli('memory-overrun-concrete','stimulus',overflow/'memory-contract.json','capacity','--inputs',overflow/'pending_aw_backpressure.json')
        if result['status']!='reset_reachable_failure':raise RuntimeError(result)
        bus=axi('memory-overrun-axi','stimulus',overflow/'memory-axi.json','--inputs',overflow/'pending_aw_backpressure.json')
        if bus['status']!='scope_exceeded' or not bus['independent']['capacity_exceeded']:raise RuntimeError(bus)
        results.append({'case':'memory_capacity_overrun','status':'passed','scope':'declared single-write capacity, not an AXI outstanding limit'})
        mutants = [
            ('wrong_reset_contents', lambda s:s.replace("mem0 = 32'h11223344;", 'mem0 = 0;'),'effects','reset_idle'),
            ('ignore_strobes', lambda s: re.sub(r"\(held_strb & 4'd[1248]\) != 0", '1', s), 'effects','zero_strobes'),
            ('wrong_lane', lambda s:s.replace("(held_strb & 4'd1) != 0", "(held_strb & 4'd2) != 0",1),'effects','sparse_strobes'),
            ('wrong_word', lambda s:s.replace('if word_addr == 0 { mem0 =','if word_addr == 4 { mem0 ='),'effects','full_write'),
            ('lost_payload', lambda s:s.replace('held_data = wdata;', 'held_data = 0;'),'requests','full_write'),
            ('disabled_lane_clobber', lambda s:s.replace("lane0_1 = mem0 & 32'd65280;",'lane0_1 = 0;'),'effects','zero_strobes'),
            ('ack_without_effect', lambda s:s.replace('assign apply_write = a_full && d_full && !write_done;','assign apply_write = 0;').replace('assign bvalid = write_done;','assign bvalid = a_full && d_full;'),'response','full_write'),
            ('duplicate_application', lambda s:s.replace('assign apply_write = a_full && d_full && !write_done;','assign apply_write = a_full && d_full;'),'completion','full_write'),
            ('wrong_readback', lambda s:s.replace('rdata = mem0;', 'rdata = mem1;'),'readback','full_write'),
            ('unsolicited_effect', lambda s:s.replace('        } else {\n            if awvalid', '        } else {\n            if !apply_write { mem0 = 0; }\n            if awvalid'),'effects','reset_idle'),
        ]
        for name, mutate, obligation, scenario in mutants:
            bad = root/('memory-mutant-'+name); shutil.copytree(path,bad); source = bad/'memory.veryl'; original=source.read_text(); modified=mutate(original)
            if modified==original: raise RuntimeError('memory mutation drift: '+name)
            source.write_text(modified); saved=out/('memory-'+name+'.regression.json')
            result=cli('memory-'+name+'-search','search',bad/'memory-contract.json',obligation,'--regression',saved)
            if result['status']!='reset_reachable_failure':raise RuntimeError(result)
            again=cli('memory-'+name+'-replay','replay',bad/'memory-contract.json',obligation,'--regression',saved)
            if again['status']!='reset_reachable_failure':raise RuntimeError(again)
            file=bad/(scenario+'.json')
            concrete=cli('memory-'+name+'-concrete','stimulus',bad/'memory-contract.json',obligation,'--inputs',file)
            if concrete['status']!='reset_reachable_failure':raise RuntimeError(concrete)
            # Ordinary bus checks pass these defects; actual source memory/event evidence differs.
            bus=axi('memory-'+name+'-axi','stimulus',bad/'memory-axi.json','--inputs',file)
            if bus['status']!='trace_no_failure' or bus['independent']['status']!='sampled_prefix_passed':raise RuntimeError(bus)
            try: check_memory(replay.load_json(out/('memory-'+name+'-axi')/'simulation.json')['trace'],contract)
            except ValueError as e: oracle_error=str(e)
            else:raise RuntimeError('independent byte oracle missed '+name)
            results.append({'case':'memory_mutant_'+name,'status':'passed','contract_failure':obligation,'ordinary_axi':'passed','independent_oracle':oracle_error})
            if name=='ignore_strobes':
                changed=replay.load_json(bad/'memory-contract.json');changed['contract']['locations'][0]['initial'] ^= 1;replay.write(bad/'memory-contract.json',changed)
                stale=cli('memory-stale-semantics','replay',bad/'memory-contract.json',obligation,'--regression',saved,allowed=(2,))
                if stale['status']!='project_error' or 'identity' not in stale['error']:raise RuntimeError(stale)
                results.append({'case':'memory_stale_semantics','status':'passed'})
        for name, change in [('aliased_cells',lambda c:c['locations'][1].update(state='mem0')),('overlapping_locations',lambda c:c['locations'][1].update(address=0)),('unaligned_location',lambda c:c['locations'][1].update(address=1)),('bad_initial_value',lambda c:c['locations'][0].update(initial=-1)),('input_as_effect',lambda c:c['signals'].update(apply='bus_awvalid'))]:
            changed=replay.load_json(binding);change(changed['contract']);file=path/(name+'.json');replay.write(file,changed)
            result=cli('memory-reject-'+name,'search',file,'effects',allowed=(2,))
            if result['status']!='project_error':raise RuntimeError(result)
            results.append({'case':'memory_reject_'+name,'status':'passed'})
    return results
