"""Derive auto_array_sum.json: the array_sum contract with no manually chosen partition hint."""
import json
from pathlib import Path
D=Path(__file__).parent
d=json.loads((D/'array_sum.json').read_text());d['program_contract'].pop('split')
(D/'auto_array_sum.json').write_text(json.dumps(d,indent=2)+'\n')
