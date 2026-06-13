import { Link } from '@tanstack/react-router'
import type { ReactNode } from 'react'
import { useEffect, useState } from 'react'
import { MoonMark } from '../icons'
import { fetchMe, loginUrl, logout, type Me } from '../../lib/relay'

function useMe(): { me: Me | null; loading: boolean } {
  const [me, setMe] = useState<Me | null>(null)
  const [loading, setLoading] = useState(true)
  useEffect(() => {
    let alive = true
    fetchMe().then((m) => {
      if (!alive) return
      setMe(m)
      setLoading(false)
    })
    return () => {
      alive = false
    }
  }, [])
  return { me, loading }
}

export function DashShell({ children }: { children: (me: Me) => ReactNode }) {
  const { me, loading } = useMe()

  return (
    <div className="min-h-screen bg-washi text-ink font-sans flex flex-col">
      <header className="border-b border-rule/70 bg-panel/60 backdrop-blur sticky top-0 z-10">
        <div className="mx-auto max-w-[1180px] px-[var(--gut)] h-14 flex items-center justify-between">
          <Link to="/app" className="flex items-center gap-2.5 group">
            <span className="text-indigo group-hover:text-amber transition-colors">
              <MoonMark />
            </span>
            <span className="font-serif text-[1.05rem] tracking-tight">
              <span lang="ja" className="text-ink-soft mr-1.5">
                本屋
              </span>
              <span className="text-ink-faint-text text-sm">remote</span>
            </span>
          </Link>
          <nav className="flex items-center gap-4 text-sm">
            <a href="/" className="text-ink-faint-text hover:text-ink transition-colors">
              ← honya.altqx.com
            </a>
            {me ? (
              <>
                <span className="text-ink-soft">
                  <span className="text-sage">●</span> @{me.login}
                </span>
                <button
                  type="button"
                  onClick={() => logout().then(() => location.reload())}
                  className="text-ink-faint-text hover:text-vermilion transition-colors"
                >
                  ออกจากระบบ
                </button>
              </>
            ) : null}
          </nav>
        </div>
      </header>

      <main className="flex-1 mx-auto w-full max-w-[1180px] px-[var(--gut)] py-10">
        {loading ? <Loading /> : me ? children(me) : <SignIn />}
      </main>

      <footer className="border-t border-rule/60 py-6 text-center text-xs text-ink-faint-text">
        ควบคุมเซสชันการแปลของคุณจากระยะไกล · เปิดใช้งานในแอปที่ Settings → Ctrl-R
      </footer>
    </div>
  )
}

function Loading() {
  return (
    <div className="flex items-center justify-center py-24 text-ink-faint-text font-mono text-sm">
      <span className="animate-pulse">◐ กำลังโหลด…</span>
    </div>
  )
}

function SignIn() {
  return (
    <div className="max-w-md mx-auto text-center py-16">
      <div className="text-indigo text-4xl mb-5">
        <span className="inline-block">
          <MoonMark />
        </span>
      </div>
      <h1 className="font-serif text-2xl text-ink mb-3">เข้าสู่ระบบเพื่อควบคุม honya</h1>
      <p className="text-ink-soft text-[0.95rem] leading-relaxed mb-8">
        เชื่อมต่อบัญชี GitHub ของคุณเพื่อดูและสั่งงานเซสชันการแปลที่กำลังทำงานบนเครื่องของคุณแบบเรียลไทม์
      </p>
      <a
        href={loginUrl()}
        className="inline-flex items-center gap-2.5 rounded-[9px] bg-thai text-washi px-5 py-3 text-sm font-medium hover:bg-indigo transition-colors"
      >
        <svg viewBox="0 0 16 16" width="18" height="18" fill="currentColor" aria-hidden="true">
          <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8Z" />
        </svg>
        เข้าสู่ระบบด้วย GitHub
      </a>
    </div>
  )
}
