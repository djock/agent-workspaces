import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import type { Note } from '../types'

// Files the agent writes for the workspace protocol (notebooks, summaries,
// handoffs). Their diffs are bookkeeping and push the answer out of view, so
// the transcript shows one dim line, the band counts them, and /notes reads them.
const QUIET = ['/.ws/', '/.cs/']
const PANE = 'ws-notes'
const PER_PAGE = 4

const notes = atom({ plugin: 'ws-quiet-notes', key: 'notes' } as const, [])
const readUpTo = atom({ plugin: 'ws-quiet-notes', key: 'readUpTo' } as const, 0)
const page = atom({ plugin: 'ws-quiet-notes', key: 'page' } as const, 0)

export const isQuiet = (path: unknown): path is string =>
  typeof path === 'string' && QUIET.some(dir => path.includes(dir))

/** The path from the protocol folder on: `.ws/notebook/notebook.me.md`. */
export const short = (path: string): string => {
  const at = Math.min(...QUIET.map(d => path.indexOf(d)).filter(i => i >= 0))
  return Number.isFinite(at) ? path.slice(at + 1) : path
}

type Hunk = { lines: string[] }
export type Found = Omit<Note, 'id' | 'at'>

const fromHunks = (path: string, hunks: readonly Hunk[]): Found => {
  const lines: string[] = []
  let removed = 0
  for (const h of hunks) {
    for (const l of h.lines) {
      if (l.startsWith('+')) lines.push(l.slice(1))
      else if (l.startsWith('-')) removed += 1
    }
  }
  return { path, lines, added: lines.length, removed }
}

/** The protocol-file writes in one tool result, whichever tool made them. */
export const extract = (tool: string, result: unknown): Found[] => {
  if (result === null || typeof result !== 'object') return []
  const r = result as {
    filePath?: unknown
    structuredPatch?: Hunk[]
    content?: string
    type?: string
    bashEditDiff?: { files?: { filePath: string; hunks: Hunk[] }[] }
  }
  if (tool === 'Bash') {
    const files = r.bashEditDiff?.files ?? []
    return files.filter(f => isQuiet(f.filePath)).map(f => fromHunks(f.filePath, f.hunks))
  }
  if ((tool === 'Edit' || tool === 'Write') && isQuiet(r.filePath)) {
    const patch = r.structuredPatch ?? []
    if (patch.length === 0 && r.type === 'create' && typeof r.content === 'string') {
      const lines = r.content.split('\n')
      return [{ path: r.filePath, lines, added: lines.length, removed: 0 }]
    }
    return [fromHunks(r.filePath, patch)]
  }
  return []
}

const label = (f: Found): string => `${short(f.path)} updated (+${f.added} -${f.removed})`

const clock = (ms: number): string => {
  const d = new Date(ms)
  const two = (n: number) => String(n).padStart(2, '0')
  return `${two(d.getHours())}:${two(d.getMinutes())}`
}

async function openReader($: EngineInterface): Promise<void> {
  const all = await read($, notes)
  await update($, readUpTo, () => all.length)
  await update($, page, () => 0)
  await $.ui.open({ id: PANE, title: `Notes · ${all.length}` })
}

export const register: Register = on => {

  on('session.start', async ($, e, next) => {
    await $.command.register({
      name: 'notes',
      description: 'Read the workspace notes this session wrote (.ws/, .cs/)',
    })
    return next(e)
  })

  on('command.run', { command: 'notes' }, async $ => {
    const all = await read($, notes)
    if (all.length === 0) return { text: 'No notes written in this session yet.' }
    await openReader($)
    return { text: `Notes pane opened (${all.length}).` }
  })

  // Collect from the result itself, not from what gets drawn: a result is
  // drawn again on every scroll and resize, a tool call ends once.
  on('tool.call', async ($, e, next) => {
    const ran = await next(e)
    // The tool has run: nothing below may throw, or the .catch would run it again.
    try {
      if (ran.deny !== undefined || ran.isError) return ran
      const found = extract(e.tool, ran.result)
      if (found.length > 0) {
        const at = Date.now()
        const fresh: Note[] = found.map(f => ({ ...f, id: `${e.tool_use_id}:${f.path}`, at }))
        await update($, notes, list => {
          const known = new Set(list.map(n => n.id))
          return [...list, ...fresh.filter(n => !known.has(n.id))].slice(-200)
        })
      }
    } catch {
      // A note the band misses is fine; a tool call that fails because of it is not.
    }
    return ran
  }).catch(($, e, next) => next(e))

  on('ui.render', { component: 'ToolResult' }, async ($, e, next) => {
    const { tool, output, isErrored } = e.props
    if (isErrored) return next(e)
    const found = extract(tool, output)
    if (found.length === 0) return next(e)
    const { Box, Text } = $.ui.resolve(e)
    const quiet = <Text dimColor>{found.map(label).join(' · ')}</Text>

    if (tool !== 'Bash') return quiet

    // Bash: keep the command's own output and every other file's diff.
    const o = output as Record<string, unknown> & {
      bashEditDiff: { files: { filePath: string }[]; moreFiles: number }
    }
    const keep = o.bashEditDiff.files.filter(f => !isQuiet(f.filePath))
    const rest: Record<string, unknown> = { ...o }
    if (keep.length > 0 || o.bashEditDiff.moreFiles > 0) rest.bashEditDiff = { ...o.bashEditDiff, files: keep }
    else delete rest.bashEditDiff
    const drawn = await next({ ...e, props: { ...e.props, output: rest } })
    return (
      <Box flexDirection="column">
        {drawn}
        {quiet}
      </Box>
    )
  })

  // The indicator: how many notes, how many unread, and a button to read them.
  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    const all = await read($, notes)
    if (e.props.hasSurvey || all.length === 0) return next(e)
    const unread = all.length - (await read($, readUpTo))
    const { Box, Button, Text } = $.ui.resolve(e)
    return (
      <Box>
        <Text dimColor>{`✎ ${all.length} note${all.length === 1 ? '' : 's'}`}</Text>
        {unread > 0 ? <Text color="yellow">{` · ${unread} new`}</Text> : null}
        <Text> </Text>
        <Button key="read" label="Read" hotkey="n" onPress={() => openReader($)} />
      </Box>
    )
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const { Box, Button, Text } = $.ui.resolve(e)
    const all = await read($, notes)
    const newest = [...all].reverse()
    const pages = Math.max(1, Math.ceil(newest.length / PER_PAGE))
    const p = Math.min(await read($, page), pages - 1)
    const shown = newest.slice(p * PER_PAGE, (p + 1) * PER_PAGE)
    return (
      <Box flexDirection="column">
        {shown.length === 0 ? <Text dimColor>No notes written in this session yet.</Text> : null}
        {shown.map(n => (
          <Box flexDirection="column" key={n.id}>
            <Text>
              <Text bold>{`${clock(n.at)} ${short(n.path)}`}</Text>
              <Text dimColor>{` +${n.added} -${n.removed}`}</Text>
            </Text>
            {n.lines.map(l => <Text>{l === '' ? ' ' : l}</Text>)}
            <Text> </Text>
          </Box>
        ))}
        {pages > 1 ? (
          <Box>
            <Text dimColor>{`page ${p + 1}/${pages}  `}</Text>
            <Button key="newer" label="Newer" hotkey="k" onPress={() => update($, page, x => Math.max(0, x - 1))} />
            <Text> </Text>
            <Button key="older" label="Older" hotkey="j" onPress={() => update($, page, x => Math.min(pages - 1, x + 1))} />
          </Box>
        ) : null}
      </Box>
    )
  })
}
