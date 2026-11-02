import { readFileSync } from 'node:fs'
import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { readBaselineFile } from '../../scripts/read-sv-tests-baseline.mjs'
const root = fileURLToPath(new URL('../../', import.meta.url))
const baseline = 'conformance/sv-tests/expected.tsv'
const json = name => JSON.parse(readFileSync(new URL(name, import.meta.url), 'utf8'))
export default {
  watch: [`${root}${baseline}`, `${root}docs/verification/*.json`],
  load() {
    const commit = execFileSync('git', ['log', '-1', '--format=%H', '--', baseline], { cwd: root, encoding: 'utf8' }).trim()
    const recorded_at = execFileSync('git', ['show', '-s', '--format=%cI', commit], { cwd: root, encoding: 'utf8' }).trim()
    const metadata = json('main-tags.json')
    const pinned = readFileSync(`${root}conformance/sv-tests/revision`, 'utf8').trim()
    if (metadata.revision !== pinned) throw new Error('sv-tests tag metadata is stale; regenerate snapshot_metadata.py output')
    const main = readBaselineFile(`${root}${baseline}`, metadata, { baseline_revision: commit, recorded_at })
    return { main, imported: json('sv-tests.json'), cores: json('cores.json') }
  },
}
