import { expect, test } from 'claude-code/testing'

import { extract, isQuiet, short } from './register'

const hunk = { oldStart: 1, oldLines: 0, newStart: 1, newLines: 3, lines: ['+a', '+', '-old', ' ctx'] }
const diff = (paths: string[]) => ({
  files: paths.map(filePath => ({ filePath, hunks: [hunk] })),
  moreFiles: 0,
})
const bashOut = (paths: string[]) => ({
  stdout: ' M App.swift',
  stderr: '',
  interrupted: false,
  bashEditDiff: diff(paths),
})
const toolResult = (surface: 'terminal' | 'desktop', paths: string[]) =>
  ({
    surface,
    component: 'ToolResult',
    requestId: 't1',
    props: { tool_use_id: 't1', tool: 'Bash', isErrored: false, output: bashOut(paths) },
  }) as const

test('protocol paths are quiet, project files are not', () => {
  expect(isQuiet('/w/.ws/notebook/notebook.me.md')).toBe(true)
  expect(isQuiet('/w/.cs/memory/narrative.me.md')).toBe(true)
  expect(isQuiet('/w/Sources/App.swift')).toBe(false)
  expect(isQuiet(undefined)).toBe(false)
  expect(short('/Users/x/proj/.ws/notebook/n.md')).toBe('.ws/notebook/n.md')
})

test('extract keeps added lines and counts removals, per tool', () => {
  const [bash] = extract('Bash', bashOut(['/w/.ws/notebook/n.md', '/w/App.swift']))
  expect(bash).toEqual({ path: '/w/.ws/notebook/n.md', lines: ['a', ''], added: 2, removed: 1 })
  expect(extract('Bash', bashOut(['/w/App.swift']))).toEqual([])
  const edit = extract('Edit', { filePath: '/w/.ws/README.md', structuredPatch: [hunk] })
  expect(edit[0]?.added).toBe(2)
  const created = extract('Write', { type: 'create', filePath: '/w/.ws/handoffs/h.md', content: 'x\ny', structuredPatch: [] })
  expect(created[0]?.lines).toEqual(['x', 'y'])
  expect(extract('Edit', { filePath: '/w/App.swift', structuredPatch: [hunk] })).toEqual([])
  expect(extract('Read', { filePath: '/w/.ws/README.md' })).toEqual([])
})

for (const surface of ['terminal', 'desktop'] as const) {
  test(`${surface}: a notebook append loses its diff and keeps the stdout`, async ($, on) => {
    let seen: any
    on('ui.render', { component: 'ToolResult' }, ($, e) => {
      seen = e.props.output
      const { Text } = $.ui.resolve(e)
      return <Text>engine</Text>
    })
    await $.ui.render(toolResult(surface, ['/w/.ws/notebook/notebook.me.md']))
    expect(seen.stdout).toBe(' M App.swift')
    expect(seen.bashEditDiff).toBeUndefined()
  })

  test(`${surface}: other files keep their diff`, async ($, on) => {
    let seen: any
    on('ui.render', { component: 'ToolResult' }, ($, e) => {
      seen = e.props.output
      const { Text } = $.ui.resolve(e)
      return <Text>engine</Text>
    })
    await $.ui.render(toolResult(surface, ['/w/.ws/notebook/n.md', '/w/App.swift']))
    expect(seen.bashEditDiff.files.map((f: any) => f.filePath)).toEqual(['/w/App.swift'])
  })
}

for (const surface of ['terminal', 'desktop'] as const) {
  test(`${surface}: the band counts notes and Read opens the reader`, async ($, on) => {
    on('tool.call', () => ({ result: bashOut(['/w/.ws/notebook/n.md']) }) as any)
    let opened = ''
    on('ui.open', ($, e) => {
      opened = e.id
      return { value: true } as any
    })
    on('ui.render', { component: 'AbovePrompt' }, ($, e) => {
      const { Text } = $.ui.resolve(e)
      return <Text>engine band</Text>
    })
    const band = { component: 'AbovePrompt', props: { hasSurvey: false, isWorking: false, maxRows: 10, bodyColumns: 80 } } as any

    let ui = await $.ui.mount({ plugin: 'ws-quiet-notes', surface, ...band })
    expect(await ui.find({ key: 'read' })).toBeUndefined()
    await ui.unmount()

    await $.tool.call({ tool: 'Bash', command: 'cat >> .ws/notebook/n.md' } as any)
    await $.tool.call({ tool: 'Bash', command: 'cat >> .ws/notebook/n.md' } as any)
    ui = await $.ui.mount({ plugin: 'ws-quiet-notes', surface, ...band })
    expect(await ui.find({ type: 'Text', text: /2 notes/ })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: /2 new/ })).toBeDefined()

    await ui.press({ key: 'read' })
    expect(opened).toBe('ws-notes')
    expect(await ui.find({ type: 'Text', text: /new/ })).toBeUndefined()
    await ui.unmount()
  })
}
