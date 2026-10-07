export type Note = {
  /** The tool call that wrote it, plus the file: one call can write several. */
  id: string
  path: string
  /** The lines it added, without the leading `+`. */
  lines: string[]
  added: number
  removed: number
  /** Epoch milliseconds. */
  at: number
}

declare module 'claude-code' {
  interface PluginState {
    'ws-quiet-notes': {
      notes: Note[]
      /** How many notes there were when the reader was last opened. */
      readUpTo: number
      /** Which page of the reader is showing, 0 being the newest. */
      page: number
    }
  }
}
