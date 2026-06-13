import { Link, createFileRoute } from '@tanstack/react-router'
import { DashShell } from '../components/dashboard/DashShell'
import {
  useSession,
  type ChapterId,
  type Command,
  type SessionView,
  type Tally,
  type Usage,
} from '../lib/relay'

export const Route = createFileRoute('/app/$deviceId')({
  component: SessionPage,
})

function SessionPage() {
  const { deviceId } = Route.useParams()
  return <DashShell>{() => <Session deviceId={deviceId} />}</DashShell>
}

function Session({ deviceId }: { deviceId: string }) {
  const s = useSession(deviceId)
  const { snapshot: snap, send } = s

  return (
    <div className="space-y-6">
      <TopBar view={s} />

      <div className="grid gap-6 lg:grid-cols-[300px_1fr]">
        <QueuePanel running={snap.running} queue={snap.queue} send={send} />
        <div className="space-y-6">
          <LivePanel view={s} />
          <UsagePanel run={snap.usage_run} chapter={snap.usage_chapter} />
        </div>
      </div>

      <LogPanel lines={snap.log_tail} />

      <Link to="/app" className="inline-block text-sm text-ink-faint-text hover:text-ink">
        ← เครื่องทั้งหมด
      </Link>
    </div>
  )
}

function TopBar({ view }: { view: SessionView }) {
  const { snapshot: snap, link, appOnline, send } = view
  const offline = link !== 'open' || !appOnline
  return (
    <div className="rounded-[14px] border border-rule bg-panel/50 p-4 flex flex-wrap items-center gap-x-6 gap-y-3">
      <div className="flex items-center gap-2 min-w-0">
        <span className={offline ? 'text-ink-faint' : snap.run_active ? 'text-indigo' : 'text-sage'}>
          {offline ? '○' : snap.run_active ? '◐' : '●'}
        </span>
        <span className="font-serif text-ink truncate">
          {snap.project ?? 'ยังไม่ได้เปิดโปรเจกต์'}
        </span>
        <span className="text-xs text-ink-faint-text font-mono">
          {offline ? 'แอปออฟไลน์' : snap.paused ? 'หยุดชั่วคราว' : snap.run_active ? 'กำลังแปล' : 'ว่าง'}
        </span>
      </div>

      <div className="flex-1" />

      <TallyCluster tally={snap.tally} />

      <div className="flex items-center gap-2">
        {snap.run_active ? (
          <>
            <CtrlButton label={snap.paused ? '▶ ทำต่อ' : '⏸ พัก'} onClick={() => send({ op: 'pause' })} />
            <CtrlButton label="⏹ หยุด" tone="danger" onClick={() => send({ op: 'stop' })} />
          </>
        ) : (
          <CtrlButton
            label="▶ แปลทั้งโปรเจกต์"
            tone="primary"
            disabled={offline}
            onClick={() => send({ op: 'start_project' })}
          />
        )}
      </div>
    </div>
  )
}

function TallyCluster({ tally }: { tally: Tally }) {
  const pct = tally.total > 0 ? Math.round((tally.done / tally.total) * 100) : 0
  return (
    <div className="flex items-center gap-3 font-mono text-sm">
      <Stat glyph="●" n={tally.done} className="text-sage" />
      <Stat glyph="◐" n={tally.working} className="text-indigo" />
      <Stat glyph="○" n={tally.pending} className="text-ink-faint" />
      <Stat glyph="✗" n={tally.failed} className={tally.failed > 0 ? 'text-vermilion' : 'text-ink-faint'} />
      <span className="font-semibold text-ink-soft ml-1">{pct}%</span>
    </div>
  )
}

function Stat({ glyph, n, className }: { glyph: string; n: number; className: string }) {
  return (
    <span className={className}>
      {glyph}
      {n}
    </span>
  )
}

function CtrlButton({
  label,
  onClick,
  tone = 'default',
  disabled,
}: {
  label: string
  onClick: () => void
  tone?: 'default' | 'primary' | 'danger'
  disabled?: boolean
}) {
  const cls =
    tone === 'primary'
      ? 'bg-thai text-washi hover:bg-indigo'
      : tone === 'danger'
        ? 'border border-vermilion/40 text-vermilion hover:bg-vermilion hover:text-washi'
        : 'border border-rule text-ink-soft hover:border-indigo-soft hover:text-ink'
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className={`rounded-[9px] px-3.5 py-2 text-sm font-medium transition-colors disabled:opacity-40 disabled:cursor-not-allowed ${cls}`}
    >
      {label}
    </button>
  )
}

function QueuePanel({
  running,
  queue,
  send,
}: {
  running: ChapterId | null
  queue: ChapterId[]
  send: (c: Command) => void
}) {
  return (
    <section className="rounded-[14px] border border-rule bg-panel/40 p-4">
      <h2 className="font-serif text-base text-ink mb-3">คิว</h2>
      {running ? (
        <div className="mb-2 flex items-center gap-2 rounded-[9px] bg-indigo/10 px-3 py-2">
          <span className="text-indigo animate-pulse">◐</span>
          <span className="font-mono text-sm text-ink">
            Vol.{pad(running.vol)} · ch {running.ch}
          </span>
          <span className="ml-auto text-xs text-indigo-soft">กำลังทำ</span>
        </div>
      ) : null}
      {queue.length === 0 ? (
        <p className="text-sm text-ink-faint-text">ไม่มีบทในคิว</p>
      ) : (
        <ul className="space-y-1.5">
          {queue.map((c, i) => (
            <li
              key={`${c.vol}-${c.ch}`}
              className="group flex items-center gap-2 rounded-[8px] px-3 py-1.5 hover:bg-inset/60"
            >
              <span className="font-mono text-sm text-ink-soft flex-1">
                Vol.{pad(c.vol)} · ch {c.ch}
              </span>
              <span className="hidden group-hover:flex items-center gap-1 text-ink-faint">
                <IconBtn label="↑" disabled={i === 0} onClick={() => send({ op: 'queue_move_up', vol: c.vol, ch: c.ch })} />
                <IconBtn label="↓" disabled={i === queue.length - 1} onClick={() => send({ op: 'queue_move_down', vol: c.vol, ch: c.ch })} />
                <IconBtn label="✕" onClick={() => send({ op: 'dequeue', vol: c.vol, ch: c.ch })} />
              </span>
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}

function IconBtn({ label, onClick, disabled }: { label: string; onClick: () => void; disabled?: boolean }) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className="px-1.5 hover:text-indigo disabled:opacity-30 transition-colors"
    >
      {label}
    </button>
  )
}

function LivePanel({ view }: { view: SessionView }) {
  const { chunk, stream } = view
  const pct = chunk && chunk.total > 0 ? Math.round(((chunk.chunk + 1) / chunk.total) * 100) : 0
  return (
    <section className="rounded-[14px] border border-rule bg-panel/40 p-5">
      <div className="flex items-center justify-between mb-3">
        <h2 className="font-serif text-base text-ink">กำลังแปล</h2>
        {chunk ? (
          <span className="font-mono text-xs text-ink-faint-text">
            ch {chunk.chapter} · chunk {chunk.chunk + 1}/{chunk.total || '—'} · {chunk.state}
          </span>
        ) : null}
      </div>

      {chunk ? (
        <div className="h-1.5 rounded-full bg-inset mb-4 overflow-hidden">
          <div
            className="h-full bg-indigo transition-[width] duration-500"
            style={{ width: `${pct}%` }}
          />
        </div>
      ) : null}

      <div className="rounded-[9px] bg-washi border border-rule/60 p-4 min-h-[180px] max-h-[340px] overflow-y-auto">
        {stream && stream.text ? (
          <p className="font-serif text-[1.05rem] leading-[1.9] text-thai whitespace-pre-wrap">
            {stream.text}
            <span className="inline-block w-[2px] h-[1.1em] align-middle bg-indigo ml-0.5 animate-pulse" />
          </p>
        ) : (
          <p className="text-ink-faint-text text-sm font-mono">
            {view.snapshot.run_active ? '◐ รอข้อความ…' : 'ไม่มีการแปลที่กำลังสตรีม'}
          </p>
        )}
      </div>
    </section>
  )
}

function UsagePanel({ run, chapter }: { run: Usage; chapter: Usage }) {
  return (
    <section className="rounded-[14px] border border-rule bg-panel/40 p-4 grid grid-cols-2 gap-4">
      <UsageBlock label="ทั้งรอบ" u={run} />
      <UsageBlock label="บทนี้" u={chapter} />
    </section>
  )
}

function UsageBlock({ label, u }: { label: string; u: Usage }) {
  return (
    <div>
      <div className="text-xs text-ink-faint-text mb-1">{label}</div>
      <div className="font-mono text-sm text-ink">
        ${u.cost_usd.toFixed(4)}
        <span className="text-ink-faint-text"> · {u.total.toLocaleString()} tok</span>
      </div>
    </div>
  )
}

function LogPanel({ lines }: { lines: { level: string; msg: string }[] }) {
  return (
    <section className="rounded-[14px] border border-rule bg-thai/95 p-4">
      <h2 className="font-mono text-xs text-indigo-on-ink mb-2 uppercase tracking-wider">บันทึกกิจกรรม</h2>
      <div className="font-mono text-xs leading-relaxed max-h-44 overflow-y-auto space-y-0.5">
        {lines.length === 0 ? (
          <span className="text-washi/40">—</span>
        ) : (
          lines.map((l, i) => (
            <div key={i} className={logColor(l.level)}>
              <span className="opacity-50">{logGlyph(l.level)} </span>
              {l.msg}
            </div>
          ))
        )}
      </div>
    </section>
  )
}

function logColor(level: string): string {
  switch (level) {
    case 'error':
      return 'text-[#e0907f]'
    case 'warn':
      return 'text-[#d8b86a]'
    default:
      return 'text-washi/80'
  }
}

function logGlyph(level: string): string {
  switch (level) {
    case 'error':
      return '✗'
    case 'warn':
      return '‖'
    default:
      return '·'
  }
}

function pad(n: number): string {
  return String(n).padStart(2, '0')
}
