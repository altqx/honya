// Keep wire types in lockstep with PROTOCOL.md and src/remote/protocol.rs.

import { useEffect, useRef, useState } from 'react'
import { RELAY_URL } from '../data/site'

export interface ChapterId {
  vol: number
  ch: number
}

export interface Tally {
  done: number
  working: number
  pending: number
  failed: number
  total: number
}

export interface Usage {
  prompt: number
  completion: number
  total: number
  cost_usd: number
}

export interface LogLine {
  level: string
  msg: string
}

export interface Snapshot {
  app_version: string
  project: string | null
  vol: number | null
  run_active: boolean
  paused: boolean
  running: ChapterId | null
  queue: ChapterId[]
  tally: Tally
  usage_run: Usage
  usage_chapter: Usage
  log_tail: LogLine[]
}

export type Command =
  | { op: 'pause' }
  | { op: 'stop' }
  | { op: 'start_project' }
  | { op: 'enqueue'; vol: number; chapters: number[] }
  | { op: 'queue_move_up'; vol: number; ch: number }
  | { op: 'queue_move_down'; vol: number; ch: number }
  | { op: 'dequeue'; vol: number; ch: number }

export interface Device {
  id: string
  label: string
  last_seen: number
  online: boolean
}

export interface Me {
  login: string
  uid: string
}

function api(path: string): string {
  return `${RELAY_URL}${path}`
}

export async function fetchMe(): Promise<Me | null> {
  try {
    const r = await fetch(api('/api/me'), { credentials: 'include' })
    if (!r.ok) return null
    return (await r.json()) as Me
  } catch {
    return null
  }
}

export async function fetchDevices(): Promise<Device[]> {
  const r = await fetch(api('/api/devices'), { credentials: 'include' })
  if (!r.ok) return []
  const body = (await r.json()) as { devices: Device[] }
  return body.devices ?? []
}

export async function unpairDevice(id: string): Promise<void> {
  await fetch(api(`/api/devices/${id}`), {
    method: 'DELETE',
    credentials: 'include',
  })
}

export function loginUrl(): string {
  return api('/auth/github/login')
}

export async function logout(): Promise<void> {
  await fetch(api('/auth/logout'), { method: 'POST', credentials: 'include' })
}

function emptySnapshot(): Snapshot {
  return {
    app_version: '',
    project: null,
    vol: null,
    run_active: false,
    paused: false,
    running: null,
    queue: [],
    tally: { done: 0, working: 0, pending: 0, failed: 0, total: 0 },
    usage_run: { prompt: 0, completion: 0, total: 0, cost_usd: 0 },
    usage_chapter: { prompt: 0, completion: 0, total: 0, cost_usd: 0 },
    log_tail: [],
  }
}

export type LinkState = 'connecting' | 'open' | 'closed'

export interface ChunkProgress {
  chapter: number
  chunk: number
  total: number
  state: string
}

export interface StreamState {
  chapter: number
  chunk: number
  role: string
  text: string
}

export interface SessionView {
  link: LinkState
  appOnline: boolean
  snapshot: Snapshot
  chunk: ChunkProgress | null
  stream: StreamState | null
  chapterStatuses: Record<number, string>
  send: (cmd: Command) => void
}

function wsUrl(deviceId: string): string {
  const base = RELAY_URL.replace(/^http/, 'ws')
  return `${base}/relay/${deviceId}`
}

export function useSession(deviceId: string): SessionView {
  const [link, setLink] = useState<LinkState>('connecting')
  const [appOnline, setAppOnline] = useState(true)
  const [snapshot, setSnapshot] = useState<Snapshot>(emptySnapshot)
  const [chunk, setChunk] = useState<ChunkProgress | null>(null)
  const [stream, setStream] = useState<StreamState | null>(null)
  const [chapterStatuses, setChapterStatuses] = useState<Record<number, string>>({})
  const wsRef = useRef<WebSocket | null>(null)

  useEffect(() => {
    if (typeof window === 'undefined') return
    let closed = false
    let backoff = 1000
    let timer: ReturnType<typeof setTimeout> | undefined

    const connect = () => {
      setLink('connecting')
      const ws = new WebSocket(wsUrl(deviceId))
      wsRef.current = ws

      ws.onopen = () => {
        backoff = 1000
        setLink('open')
        setAppOnline(true)
      }
      ws.onclose = () => {
        if (closed) return
        setLink('closed')
        timer = setTimeout(connect, backoff)
        backoff = Math.min(backoff * 2, 15000)
      }
      ws.onerror = () => ws.close()
      ws.onmessage = (ev) => {
        let frame: { type?: string; data?: unknown; app_online?: boolean }
        try {
          frame = JSON.parse(ev.data as string)
        } catch {
          return
        }
        if (frame.type === 'snapshot') {
          setSnapshot(frame.data as Snapshot)
          setAppOnline(true)
        } else if (frame.type === 'delta') {
          applyDelta(frame.data, {
            setSnapshot,
            setChunk,
            setStream,
            setChapterStatuses,
          })
        } else if (frame.type === 'presence') {
          setAppOnline(frame.app_online !== false)
        }
      }
    }

    connect()
    return () => {
      closed = true
      if (timer) clearTimeout(timer)
      wsRef.current?.close()
    }
  }, [deviceId])

  const send = (cmd: Command) => {
    const ws = wsRef.current
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: 'command', data: cmd }))
    }
  }

  return { link, appOnline, snapshot, chunk, stream, chapterStatuses, send }
}

interface DeltaSetters {
  setSnapshot: React.Dispatch<React.SetStateAction<Snapshot>>
  setChunk: React.Dispatch<React.SetStateAction<ChunkProgress | null>>
  setStream: React.Dispatch<React.SetStateAction<StreamState | null>>
  setChapterStatuses: React.Dispatch<React.SetStateAction<Record<number, string>>>
}

type Delta = { kind: string } & Record<string, unknown>

function applyDelta(raw: unknown, s: DeltaSetters): void {
  const d = raw as Delta
  switch (d.kind) {
    case 'queue':
      s.setSnapshot((prev) => ({
        ...prev,
        running: (d.running as ChapterId) ?? null,
        queue: (d.pending as ChapterId[]) ?? [],
      }))
      break
    case 'tally':
      s.setSnapshot((prev) => ({ ...prev, tally: d as unknown as Tally }))
      break
    case 'chapter':
      s.setChapterStatuses((prev) => ({
        ...prev,
        [d.chapter as number]: d.status as string,
      }))
      break
    case 'chunk':
      s.setChunk((prev) => {
        const total = (d.total as number) || prev?.total || 0
        return {
          chapter: d.chapter as number,
          chunk: d.chunk as number,
          total,
          state: d.state as string,
        }
      })
      break
    case 'stream':
      s.setStream((prev) => {
        const same =
          prev && prev.chapter === d.chapter && prev.chunk === d.chunk && prev.role === d.role
        return {
          chapter: d.chapter as number,
          chunk: d.chunk as number,
          role: d.role as string,
          text: (same ? prev!.text : '') + (d.delta as string),
        }
      })
      break
    case 'usage':
      s.setSnapshot((prev) => ({
        ...prev,
        usage_run: d.run as Usage,
        usage_chapter: d.chapter as Usage,
      }))
      break
    case 'log':
      s.setSnapshot((prev) => ({
        ...prev,
        log_tail: [...prev.log_tail.slice(-39), d as unknown as LogLine],
      }))
      break
    case 'run_finished':
      s.setSnapshot((prev) => ({ ...prev, run_active: false, paused: false }))
      s.setStream(null)
      s.setChunk(null)
      break
  }
}
