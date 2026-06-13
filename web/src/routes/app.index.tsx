import { Link, createFileRoute } from '@tanstack/react-router'
import { useEffect, useState } from 'react'
import { DashShell } from '../components/dashboard/DashShell'
import { ChevronIcon } from '../components/icons'
import { fetchDevices, unpairDevice, type Device } from '../lib/relay'

export const Route = createFileRoute('/app/')({
  component: DevicePicker,
})

function DevicePicker() {
  return (
    <DashShell>
      {() => <DeviceList />}
    </DashShell>
  )
}

function DeviceList() {
  const [devices, setDevices] = useState<Device[] | null>(null)

  const reload = () => fetchDevices().then(setDevices)
  useEffect(() => {
    reload()
  }, [])

  return (
    <div className="grid gap-8 md:grid-cols-[1fr_320px]">
      <section>
        <h1 className="font-serif text-xl text-ink mb-1">เครื่องของคุณ</h1>
        <p className="text-sm text-ink-faint-text mb-5">
          เลือกเครื่องเพื่อดูและควบคุมเซสชันการแปลที่กำลังทำงาน
        </p>

        {devices === null ? (
          <p className="text-ink-faint-text font-mono text-sm animate-pulse">◐ กำลังโหลด…</p>
        ) : devices.length === 0 ? (
          <div className="rounded-[14px] border border-dashed border-rule bg-inset/40 p-8 text-center text-ink-soft text-sm">
            ยังไม่มีเครื่องที่เชื่อมต่อ — ดูวิธีจับคู่ทางขวา
          </div>
        ) : (
          <ul className="space-y-3">
            {devices.map((d) => (
              <li key={d.id}>
                <div className="group flex items-center gap-3 rounded-[12px] border border-rule bg-panel/50 hover:border-indigo-soft transition-colors">
                  <Link
                    to="/app/$deviceId"
                    params={{ deviceId: d.id }}
                    className="flex-1 flex items-center gap-3 px-4 py-3.5"
                  >
                    <span
                      className={d.online ? 'text-sage' : 'text-ink-faint'}
                      title={d.online ? 'ออนไลน์' : 'ออฟไลน์'}
                    >
                      ●
                    </span>
                    <span className="flex-1 min-w-0">
                      <span className="block font-medium text-ink truncate">{d.label}</span>
                      <span className="block text-xs text-ink-faint-text font-mono">
                        {d.online ? 'ออนไลน์' : `เห็นล่าสุด ${ago(d.last_seen)}`}
                      </span>
                    </span>
                    <span className="text-ink-faint group-hover:text-indigo transition-colors">
                      <ChevronIcon />
                    </span>
                  </Link>
                  <button
                    type="button"
                    onClick={() => {
                      if (confirm(`ยกเลิกการจับคู่ "${d.label}"?`)) {
                        unpairDevice(d.id).then(reload)
                      }
                    }}
                    className="px-3 mr-1 text-ink-faint hover:text-vermilion transition-colors text-sm"
                    title="ยกเลิกการจับคู่"
                  >
                    ✕
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </section>

      <aside className="rounded-[14px] border border-rule bg-inset/40 p-5 h-fit">
        <h2 className="font-serif text-base text-ink mb-3">จับคู่เครื่องใหม่</h2>
        <ol className="space-y-3 text-sm text-ink-soft list-decimal list-inside marker:text-amber-text">
          <li>
            ในแอป honya เปิด <code className="font-mono text-xs bg-washi px-1 rounded">Settings</code>{' '}
            (กด <code className="font-mono text-xs bg-washi px-1 rounded">:</code> แล้วเลือก)
          </li>
          <li>
            กด <code className="font-mono text-xs bg-washi px-1 rounded">Ctrl-A</code>{' '}
            เพื่อเข้าสู่ระบบด้วย GitHub
          </li>
          <li>ป้อนรหัสที่แอปแสดงที่ github.com/login/device</li>
          <li>
            เครื่องจะปรากฏที่นี่ — เปิด{' '}
            <code className="font-mono text-xs bg-washi px-1 rounded">Ctrl-R</code> เพื่อเริ่มแชร์เซสชัน
          </li>
        </ol>
        <p className="mt-4 text-xs text-ink-faint-text leading-relaxed">
          เครื่องจะลิงก์กับบัญชี GitHub ของคุณ เฉพาะคุณเท่านั้นที่ดูและควบคุมได้
        </p>
      </aside>
    </div>
  )
}

function ago(unix: number): string {
  if (!unix) return '—'
  const secs = Math.max(0, Math.floor(Date.now() / 1000 - unix))
  if (secs < 60) return `${secs}s`
  if (secs < 3600) return `${Math.floor(secs / 60)}m`
  if (secs < 86400) return `${Math.floor(secs / 3600)}h`
  return `${Math.floor(secs / 86400)}d`
}
