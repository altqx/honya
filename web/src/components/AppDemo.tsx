import { useCallback, useEffect, useRef, useState } from 'react'

/* ── static demo data (mirrors the sample project in the real app) ── */
const TABS: Array<{ n: string; g: string; en: string }> = [
  { n: '1', g: '書架', en: 'Shelf' },
  { n: '2', g: '棚', en: 'Project' },
  { n: '3', g: '訳', en: 'Translate' },
  { n: '4', g: '読', en: 'Reader' },
  { n: '5', g: '辞', en: 'Lexicon' },
]

type ChStatus = 'done' | 'work' | 'pend' | 'img' | 'fail'
const MOON: Record<ChStatus, string> = {
  done: '●',
  work: '◐',
  pend: '○',
  img: '▣',
  fail: '✗',
}
const CHAPTERS: Array<{ id: string; title: string; st: ChStatus }> = [
  { id: 'ch_001', title: 'プロローグ', st: 'done' },
  { id: 'ch_002', title: '転生', st: 'done' },
  { id: 'ch_003', title: '出会い', st: 'work' },
  { id: 'ch_004', title: '口絵', st: 'img' },
  { id: 'ch_005', title: '王都へ', st: 'pend' },
  { id: 'ch_006', title: '剣の稽古', st: 'pend' },
]
const PROJECTS: Array<{ title: string; st: ChStatus; meta: string; date: string }> = [
  { title: '精霊幻想記', st: 'done', meta: '2 vols · 33%', date: '2d' },
  { title: '転生したらスライム', st: 'work', meta: '1 vol', date: '5d' },
  { title: '本好きの下剋上', st: 'pend', meta: 'new', date: 'just now' },
]
const GLOSSARY: Array<{ jp: string; th: string; note: string; lock?: boolean }> = [
  { jp: '精霊', th: 'ภูตธาตุ', note: 'protected', lock: true },
  { jp: '勇者', th: 'ผู้กล้า', note: 'term' },
  { jp: 'リオ', th: 'ริโอ', note: '主人公 · name' },
  { jp: '王都', th: 'เมืองหลวง', note: 'place' },
]
const LEX_TABS = ['Glossary', 'Characters', 'Style']

const LINES: Array<[string, string]> = [
  [
    '少年は静かに本を閉じ、窓の外の月を見上げた。',
    'เด็กหนุ่มปิดหนังสือลงอย่างเงียบ ๆ แล้วเงยหน้ามองดวงจันทร์นอกหน้าต่าง',
  ],
  ['「もう、こんな時間か」と彼は小さく呟いた。', '“ดึกขนาดนี้แล้วหรือนี่” เขาพึมพำกับตัวเองเบา ๆ'],
  [
    '遠くで鐘が鳴り、夜の街はゆっくりと眠りに落ちていく。',
    'เสียงระฆังดังก้องแต่ไกล เมืองยามค่ำคืนค่อย ๆ เคลิ้มเข้าสู่ห้วงนิทรา',
  ],
  ['風が頁をめくり、燭台の炎が静かに揺れた。', 'สายลมพัดพลิกหน้ากระดาษ เปลวเทียนบนเชิงตะเกียงไหวระริก'],
  ['明日もまた、この物語の続きを訳すのだ。', 'พรุ่งนี้ก็จะแปลเรื่องราวบทต่อไปนี้อีกครั้ง'],
]
const SPINS: Record<number, string[]> = {
  0: ['◇', '◈', '◆', '◈'],
  1: ['◤', '◥', '◢', '◣'],
  2: ['◰', '◳', '◲', '◱'],
}
const TOTAL = 7
const GW = 20
const TOK_RATE = 0.0000018
const ORCH_TOOLS = [
  'term 月 → จันทร์',
  'character リオ → ริโอ',
  'continuity · kept honorific',
  'term 王都 → เมืองหลวง',
]
// Floor to 1 decimal so in + out and total stay visually coherent (rounding
// each independently made "3.7k + 584" look like it didn't equal "4.2k").
const fmtTok = (n: number) =>
  n >= 1000 ? (Math.floor(n / 100) / 10).toFixed(1) + 'k' : String(Math.round(n))
// The run total is cumulative across the whole run; seed it with a chapter
// already finished so the RUN row reads larger than the per-CHAP row.
const RUN_BASE = { tin: 8240, tout: 6010, tools: 9 }
function thaiClusterEnd(s: string, i: number) {
  while (i < s.length) {
    const c = s.charCodeAt(i)
    if (c === 0x0e31 || (c >= 0x0e34 && c <= 0x0e3a) || (c >= 0x0e47 && c <= 0x0e4e)) i++
    else break
  }
  return i
}

const HINTS: string[][][] = [
  // per screen: [key, label] pairs
  [['↵', 'open'], ['i', 'import'], ['d', 'delete'], ['R', 'rename'], ['r', 'rescan']],
  [['↵', 'read'], ['t', 'translate'], ['T', 'whole vol'], ['A', 'project'], ['Space', 'mark']],
  [['p', 'pause'], ['s', 'stop'], ['f', 'follow'], ['↵', 'open result'], ['c', 'cycle']],
  [['↑↓', 'scroll'], ['[ ]', 'chapter'], ['s', 'source'], ['z', 'sync'], ['/', 'search']],
  [['↵', 'edit'], ['n', 'new'], ['d', 'del'], ['/', 'search'], ['Tab', 'section']],
]

export function AppDemo() {
  const rootRef = useRef<HTMLDivElement>(null)
  const appRef = useRef<HTMLDivElement>(null)

  const [screen, setScreen] = useState(2) // start on Translate, auto-running
  const [shelfSel, setShelfSel] = useState(0)
  const [projSel, setProjSel] = useState(3) // ch_003
  const [lexSel, setLexSel] = useState(0)
  const [lexTab, setLexTab] = useState(0)
  const [runPhase, setRunPhase] = useState<'running' | 'paused' | 'idle'>('running')

  // Imperative run control read by the rAF loop (avoids re-rendering at 60fps).
  const ctrl = useRef({
    active: true,
    paused: false,
    visible: true,
    screen: 2,
    reset: false,
    stop: false,
  })
  useEffect(() => {
    ctrl.current.screen = screen
  }, [screen])

  const startRun = useCallback(() => {
    ctrl.current.reset = true
    ctrl.current.active = true
    ctrl.current.paused = false
    setRunPhase('running')
    setScreen(2)
  }, [])
  const togglePause = useCallback(() => {
    if (!ctrl.current.active) return
    ctrl.current.paused = !ctrl.current.paused
    setRunPhase(ctrl.current.paused ? 'paused' : 'running')
  }, [])
  const stopRun = useCallback(() => {
    ctrl.current.active = false
    ctrl.current.paused = false
    ctrl.current.stop = true
    setRunPhase('idle')
  }, [])

  /* ── the live-run animation: writes directly into mounted nodes ── */
  useEffect(() => {
    const root = rootRef.current
    if (!root) return
    const $ = (s: string) => root.querySelector(s) as HTMLElement | null
    const cn = $('#ad-chunk-n')
    const cst = $('#ad-chunk-st')
    const csp = $('#ad-chunk-sp')
    const lgf = $('#ad-lg-f')
    const lgt = $('#ad-lg-t')
    const ag = [$('#ad-ag0'), $('#ad-ag1'), $('#ad-ag2')]
    const tab3 = $('#ad-tab3-g')
    const m = {
      in: $('#adm-in'), out: $('#adm-out'), tot: $('#adm-tot'),
      tools: $('#adm-tools'), cost: $('#adm-cost'), retry: $('#adm-retry'),
      cin: $('#admc-in'), cout: $('#admc-out'), ctot: $('#admc-tot'),
      ctools: $('#admc-tools'), ccost: $('#admc-cost'),
    }
    const hsrc = $('#adp-hsrc'), hth = $('#adp-hth')
    const asrc = $('#adp-src'), ath = $('#adp-th'), caret = $('#adp-caret')
    if (!lgf || !lgt) return

    const reduce = window.matchMedia('(prefers-reduced-motion: reduce)').matches

    const agMsg = (i: number, text: string, color = '') => {
      const el = ag[i]
      if (!el) return
      el.classList.remove('idle')
      const msg = el.querySelector('.msg') as HTMLElement
      msg.textContent = text
      msg.style.color = color
    }
    const agIdle = (i: number, text = 'idle') => {
      const el = ag[i]
      if (!el) return
      el.classList.add('idle')
      const msg = el.querySelector('.msg') as HTMLElement
      msg.textContent = text
      msg.style.color = ''
    }
    const gauge = (frac: number) => {
      const f = Math.max(0, Math.min(GW, Math.round(frac * GW)))
      lgf.textContent = new Array(f + 1).join('▰')
      lgt.textContent = new Array(GW - f + 1).join('▱')
    }
    const setActive = (ja: string, th: string, showCaret: boolean) => {
      if (asrc) asrc.textContent = ja || ' '
      if (ath) ath.textContent = th || ''
      if (caret) caret.style.visibility = showCaret ? 'visible' : 'hidden'
    }
    const setHist = (ja: string, th: string) => {
      if (hsrc) hsrc.textContent = ja || ' '
      if (hth) hth.textContent = th || ' '
    }

    const st = {
      chunk: 0, tin: 0, tout: 0, tools: 0, retry: 0, chunkOut: 0, toolIdx: 0,
      lineIdx: 0, charIdx: 0, phase: 'start', phaseT: 0, spinAgent: -1,
    }
    const meter = () => {
      // chapter row = current chapter only
      const cIn = st.tin, cOut = st.tout, cTot = cIn + cOut
      if (m.cin) m.cin.textContent = fmtTok(cIn)
      if (m.cout) m.cout.textContent = fmtTok(cOut)
      if (m.ctot) m.ctot.textContent = fmtTok(cTot)
      if (m.ctools) m.ctools.textContent = String(st.tools)
      if (m.ccost) m.ccost.textContent = (cTot * TOK_RATE).toFixed(4)
      // run row = cumulative across the run (base + current)
      const rIn = RUN_BASE.tin + cIn, rOut = RUN_BASE.tout + cOut, rTot = rIn + rOut
      if (m.in) m.in.textContent = fmtTok(rIn)
      if (m.out) m.out.textContent = fmtTok(rOut)
      if (m.tot) m.tot.textContent = fmtTok(rTot)
      if (m.tools) m.tools.textContent = String(RUN_BASE.tools + st.tools)
      if (m.cost) m.cost.textContent = (rTot * TOK_RATE).toFixed(4)
      if (m.retry) m.retry.textContent = String(st.retry)
    }
    const chunkHdr = (status: string) => {
      if (cn) cn.textContent = st.chunk + '/' + TOTAL
      if (cst) cst.textContent = status
    }

    const renderIdle = () => {
      gauge(0)
      chunkHdr('idle')
      if (csp) csp.textContent = ' '
      agIdle(0); agIdle(1); agIdle(2)
      if (tab3) tab3.textContent = '訳'
      st.chunk = 0; st.tin = 0; st.tout = 0; st.tools = 0; st.retry = 0
      st.toolIdx = 0; st.lineIdx = 0
      meter()
      setHist(' ', ' ')
      setActive(LINES[0][0], '', false)
    }
    const resetRun = () => {
      st.chunk = 0; st.tin = 0; st.tout = 0; st.tools = 0; st.retry = 0
      st.toolIdx = 0; st.lineIdx = 0; st.charIdx = 0
      st.phase = 'start'; st.phaseT = 0; st.spinAgent = -1
      gauge(0); chunkHdr('starting…'); meter()
      setHist(' ', ' '); setActive(LINES[0][0], '', true)
    }

    if (reduce) {
      // settled finished frame, no animation
      gauge(1); chunkHdr('done')
      agMsg(0, 'volume recap updated'); agMsg(1, 'returned · 1.4k tok')
      agMsg(2, '✓ approved', 'var(--sage-text)')
      st.tin = 7200; st.tout = 5300; st.tools = 9; st.retry = 1; meter()
      setHist(LINES[3][0], LINES[3][1]); setActive(LINES[4][0], LINES[4][1], false)
      return
    }

    let lastSpin = 0, spinFrame = 0
    const startChunk = () => {
      gauge(st.chunk / TOTAL)
      chunkHdr('working')
      agMsg(1, 'requesting chunk ' + (st.chunk + 1) + ' (attempt 1)')
      agIdle(2, 'waiting'); agIdle(0, 'idle')
      st.spinAgent = 1; st.chunkOut = 0
      st.phase = 'translate'; st.phaseT = 0
    }
    const tick = (dt: number) => {
      lastSpin += dt
      if (lastSpin >= 110) {
        lastSpin = 0
        spinFrame = (spinFrame + 1) % 4
        const a = st.spinAgent
        if (a >= 0 && ag[a]) {
          const sp = ag[a]!.querySelector('.ad-sp') as HTMLElement
          if (sp) sp.textContent = SPINS[a][spinFrame]
          if (tab3) tab3.textContent = SPINS[a][spinFrame]
        }
        for (let k = 0; k < 3; k++) {
          if (k !== a && ag[k]) (ag[k]!.querySelector('.ad-sp') as HTMLElement).textContent = ' '
        }
        if (csp) csp.textContent = a >= 0 ? SPINS[a][spinFrame] : ' '
      }
      st.phaseT += dt
      switch (st.phase) {
        case 'start':
          if (st.phaseT > 320) startChunk()
          break
        case 'translate':
          st.tin += Math.round(dt * 0.9); meter()
          if (st.phaseT > 560) {
            setActive(LINES[st.lineIdx % LINES.length][0], '', true)
            st.phase = 'type'; st.phaseT = 0; st.charIdx = 0
          }
          break
        case 'type': {
          const th = LINES[st.lineIdx % LINES.length][1]
          st.charIdx = thaiClusterEnd(th, Math.min(th.length, st.charIdx + 2))
          setActive(LINES[st.lineIdx % LINES.length][0], th.slice(0, st.charIdx), true)
          const d = Math.round(dt * 0.5); st.tout += d; st.chunkOut += d; meter()
          if (st.charIdx >= th.length) {
            st.spinAgent = 2; agMsg(2, 'reviewing …')
            agMsg(1, 'returned · ' + fmtTok(st.chunkOut) + ' tok')
            st.phase = 'review'; st.phaseT = 0
          }
          break
        }
        case 'review':
          if (st.phaseT > 720) {
            if (st.chunk === 2 && st.retry === 0) {
              st.retry += 1; meter()
              agMsg(2, 'retry 1/3 · pronoun consistency', 'var(--vermilion)')
              agMsg(1, 'requesting chunk ' + (st.chunk + 1) + ' (attempt 2)')
              st.spinAgent = 1; st.phase = 'retry'; st.phaseT = 0
            } else {
              agMsg(2, '✓ approved', 'var(--sage-text)')
              const ln = LINES[st.lineIdx % LINES.length]
              setHist(ln[0], ln[1]); st.lineIdx += 1
              setActive(LINES[st.lineIdx % LINES.length][0], '', true)
              st.spinAgent = 0; st.phase = 'commit'; st.phaseT = 0
            }
          }
          break
        case 'retry':
          st.tin += Math.round(dt * 0.5); meter()
          if (st.phaseT > 520) {
            agMsg(2, 'reviewing …'); st.spinAgent = 2
            st.phase = 'review'; st.phaseT = 0
          }
          break
        case 'commit':
          st.chunk += 1; gauge(st.chunk / TOTAL); chunkHdr('working')
          agMsg(0, ORCH_TOOLS[st.toolIdx % ORCH_TOOLS.length])
          st.toolIdx += 1; st.tools += 1; meter()
          st.phase = st.chunk >= TOTAL ? 'finish' : 'next'; st.phaseT = 0
          break
        case 'next':
          if (st.phaseT > 380) startChunk()
          break
        case 'finish':
          st.spinAgent = 0; agMsg(0, 'volume recap updated for ch 3')
          if (st.phaseT > 900) {
            st.tools += 1; meter()
            agMsg(1, 'done'); agMsg(2, '✓ approved', 'var(--sage-text)')
            gauge(1); chunkHdr('done'); st.spinAgent = -1
            if (csp) csp.textContent = ' '
            if (tab3) tab3.textContent = '訳'
            st.phase = 'done'; st.phaseT = 0
          }
          break
        case 'done':
          if (st.phaseT > 2400) resetRun()
          break
      }
    }

    resetRun()
    let last: number | null = null, raf = 0
    const loop = (ts: number) => {
      if (last === null) last = ts
      const dt = Math.min(80, ts - last)
      last = ts
      const c = ctrl.current
      if (c.stop) {
        c.stop = false
        renderIdle()
      } else if (c.reset) {
        c.reset = false
        resetRun()
      } else if (c.active && !c.paused && c.visible) {
        tick(dt)
      }
      raf = requestAnimationFrame(loop)
    }
    raf = requestAnimationFrame(loop)

    let io: IntersectionObserver | null = null
    if ('IntersectionObserver' in window) {
      io = new IntersectionObserver(
        (es) => es.forEach((e) => {
          ctrl.current.visible = e.isIntersecting
          if (e.isIntersecting) last = null
        }),
        { threshold: 0.05 },
      )
      io.observe(root)
    }
    const onVis = () => {
      ctrl.current.visible = !document.hidden
      if (!document.hidden) last = null
    }
    document.addEventListener('visibilitychange', onVis)
    return () => {
      cancelAnimationFrame(raf)
      io?.disconnect()
      document.removeEventListener('visibilitychange', onVis)
    }
  }, [])

  /* ── keyboard control ── */
  const move = (set: (f: (n: number) => number) => void, len: number, d: number) =>
    set((n) => (n + d + len) % len)

  const onKeyDown = (e: React.KeyboardEvent) => {
    const k = e.key
    if (k === 'Tab') {
      e.preventDefault()
      if (screen === 4) move(setLexTab, LEX_TABS.length, e.shiftKey ? -1 : 1)
      else setScreen((s) => (s + (e.shiftKey ? 4 : 1)) % 5)
      return
    }
    if (k >= '1' && k <= '5') {
      e.preventDefault()
      setScreen(+k - 1)
      return
    }
    const dir = k === 'ArrowUp' || k === 'k' ? -1 : k === 'ArrowDown' || k === 'j' ? 1 : 0
    if (dir) e.preventDefault()
    if (screen === 0) {
      if (dir) move(setShelfSel, PROJECTS.length + 1, dir)
    } else if (screen === 1) {
      if (dir) move(setProjSel, CHAPTERS.length + 1, dir)
      if (k === 't' || k === 'T' || k === 'A') {
        e.preventDefault()
        startRun()
      }
    } else if (screen === 2) {
      if (k === 'p') {
        e.preventDefault()
        togglePause()
      } else if (k === 's') {
        e.preventDefault()
        stopRun()
      }
    } else if (screen === 4) {
      if (dir) move(setLexSel, GLOSSARY.length, dir)
    }
  }

  const focusApp = () => appRef.current?.focus()

  // header tally from chapter statuses
  const done = CHAPTERS.filter((c) => c.st === 'done').length
  const work = CHAPTERS.filter((c) => c.st === 'work').length
  const pend = CHAPTERS.filter((c) => c.st === 'pend' || c.st === 'img').length
  const pct = Math.round((done / CHAPTERS.length) * 100)

  return (
    <aside className="demo-frame" aria-label="ตัวอย่างแอป honya แบบโต้ตอบ" ref={rootRef}>
      <div className="term-shell">
        <div className="term-shadow" aria-hidden="true" />
        <div className="terminal">
          <div className="titlebar">
            <div className="dots" aria-hidden="true">
              <i />
              <i />
              <i />
            </div>
            <div className="title">
              <span className="ja" lang="ja">
                本屋
              </span>{' '}
              honya
            </div>
            <div className="cwd">~/novels</div>
          </div>

          <div
            className="app"
            role="group"
            aria-label="honya TUI — กด  1–5 สลับหน้าจอ, t เริ่มแปล"
            tabIndex={0}
            ref={appRef}
            onKeyDown={onKeyDown}
            onMouseDown={focusApp}
          >
            {/* header: breadcrumb + status tally */}
            <div className="app-head">
              <span className="ah-crumb">
                <span className={screen === 0 ? '' : 'nm'}>honya 本屋</span>
                {screen !== 0 && (
                  <>
                    <span className="sep">·</span>
                    <span className="dim">精霊幻想記 · Vol.01</span>
                  </>
                )}
              </span>
              <span className="ah-tally" aria-hidden="true">
                <span>
                  <span className="gly t-done">●</span>
                  {done}
                </span>
                <span>
                  <span className="gly t-work">◐</span>
                  {work}
                </span>
                <span>
                  <span className="gly t-pend">○</span>
                  {pend}
                </span>
                <span className="t-pct">{pct}%</span>
              </span>
            </div>

            {/* tab strip */}
            <div className="app-tabs" role="tablist" aria-label="หน้าจอ">
              {TABS.map((t, i) => (
                <button
                  key={t.n}
                  type="button"
                  role="tab"
                  aria-selected={i === screen}
                  className={`ad-tab${i === screen ? ' active' : ''}`}
                  onClick={() => setScreen(i)}
                >
                  <span className="n">{t.n}</span>
                  {i === 2 ? (
                    <span className="g" id="ad-tab3-g" lang="ja">
                      訳
                    </span>
                  ) : (
                    <span className="g" lang="ja">
                      {t.g}
                    </span>
                  )}
                  <span className="en">{t.en}</span>
                </button>
              ))}
            </div>
            <div className="app-rule" />

            {/* body — all screens mounted; only active is shown */}
            <div className="app-body">
              {/* 1 · Shelf */}
              <div className={`ad-screen${screen === 0 ? ' show' : ''}`}>
                <div className="ad-shead">
                  <span className="l">
                    <b className="ja" lang="ja">
                      書架
                    </b>
                    — your shelf
                  </span>
                  <span className="r">./ (3 projects · 1 source)</span>
                </div>
                <div className="ad-list">
                  {PROJECTS.map((p, i) => (
                    <div
                      key={p.title}
                      className={`ad-row${shelfSel === i ? ' sel' : ''}`}
                      onClick={() => setShelfSel(i)}
                    >
                      <span className="bar" />
                      <span className={`ad-moon ${p.st}`}>{MOON[p.st]}</span>
                      <span className="ad-title" lang="ja">
                        {p.title}
                      </span>
                      <span className="ad-meta">
                        {p.meta} · {p.date}
                      </span>
                    </div>
                  ))}
                  <div
                    className={`ad-row ad-import${shelfSel === PROJECTS.length ? ' sel' : ''}`}
                    onClick={() => setShelfSel(PROJECTS.length)}
                  >
                    <span className="bar" />
                    <span className="ad-title">＋ Import file …</span>
                  </div>
                </div>
              </div>

              {/* 2 · Project */}
              <div className={`ad-screen${screen === 1 ? ' show' : ''}`}>
                <div className="ad-dash">
                  <div className="ad-dash-r1">
                    <span className="t">
                      <span className="k ja" lang="ja">
                        棚
                      </span>
                      精霊幻想記
                    </span>
                    <span className="v">Vol.01 · 1 vol</span>
                  </div>
                  <div className="ad-dash-r2">
                    <span className="ad-gauge">
                      <span className="gf">▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱</span>
                      <span className="gt" />
                    </span>
                    <span className="ad-gl">2/6 chapters · 33%</span>
                  </div>
                </div>
                <div className="ad-list">
                  <div
                    className={`ad-row${projSel === 0 ? ' sel' : ''}`}
                    onClick={() => setProjSel(0)}
                  >
                    <span className="bar" />
                    <span className="ad-moon work">▾</span>
                    <span className="ad-title" lang="ja">
                      Vol.01 · 精霊幻想記
                    </span>
                  </div>
                  {CHAPTERS.map((c, i) => (
                    <div
                      key={c.id}
                      className={`ad-row${projSel === i + 1 ? ' sel' : ''}`}
                      onClick={() => setProjSel(i + 1)}
                    >
                      <span className="bar" />
                      <span className="ad-indent">│</span>
                      <span className={`ad-moon ${c.st}`}>{MOON[c.st]}</span>
                      <span className="ad-id">{c.id}</span>
                      <span className="ad-title" lang="ja">
                        {c.title}
                      </span>
                    </div>
                  ))}
                </div>
              </div>

              {/* 3 · Translate (live) */}
              <div className={`ad-screen${screen === 2 ? ' show' : ''}`}>
                <div className={`ad-phase ${runPhase}`}>
                  {runPhase === 'running'
                    ? 'いま訳しているところ — Now translating · ch 3'
                    : runPhase === 'paused'
                      ? '一時停止 — Paused · ch 3'
                      : '訳 Translate — idle · last ch 3'}
                </div>
                <div className="ad-chunk" aria-hidden="true">
                  <span className="ttl" lang="ja">
                    出会い
                  </span>
                  <span className="cn">
                    chunk <span id="ad-chunk-n">0/7</span>
                  </span>
                  <span className="ad-sp" id="ad-chunk-sp">
                    {' '}
                  </span>
                  <span className="stt" id="ad-chunk-st">
                    starting…
                  </span>
                </div>
                <div className="ad-linegauge" aria-hidden="true">
                  <span className="gf" id="ad-lg-f" />
                  <span className="gt" id="ad-lg-t">
                    ▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱
                  </span>
                </div>
                <div className="ad-agents" aria-hidden="true">
                  <div className="ad-agent orch idle" id="ad-ag0">
                    <span className="badge">◆ Orch</span>
                    <span className="ad-sp"> </span>
                    <span className="msg">idle</span>
                  </div>
                  <div className="ad-agent trans idle" id="ad-ag1">
                    <span className="badge">▲ Trans</span>
                    <span className="ad-sp"> </span>
                    <span className="msg">idle</span>
                  </div>
                  <div className="ad-agent rev idle" id="ad-ag2">
                    <span className="badge">■ Review</span>
                    <span className="ad-sp"> </span>
                    <span className="msg">idle</span>
                  </div>
                </div>
                <div className="ad-meter" aria-hidden="true">
                  <span className="ml">run</span>
                  <span className="g">
                    <b>in</b>
                    <span className="v" id="adm-in">0</span>
                  </span>
                  <span className="g">
                    <b>out</b>
                    <span className="v" id="adm-out">0</span>
                  </span>
                  <span className="g">
                    <b>total</b>
                    <span className="v" id="adm-tot">0</span>
                  </span>
                  <span className="g">
                    <b>tools</b>
                    <span className="v" id="adm-tools">0</span>
                  </span>
                  <span className="g">
                    <b>$</b>
                    <span className="v" id="adm-cost">0.0000</span>
                  </span>
                  <span className="g">
                    <b>retries</b>
                    <span className="v" id="adm-retry">0</span>
                  </span>
                </div>
                <div className="ad-meter" aria-hidden="true">
                  <span className="ml">chap</span>
                  <span className="g">
                    <b>in</b>
                    <span className="v" id="admc-in">0</span>
                  </span>
                  <span className="g">
                    <b>out</b>
                    <span className="v" id="admc-out">0</span>
                  </span>
                  <span className="g">
                    <b>total</b>
                    <span className="v" id="admc-tot">0</span>
                  </span>
                  <span className="g">
                    <b>tools</b>
                    <span className="v" id="admc-tools">0</span>
                  </span>
                  <span className="g">
                    <b>$</b>
                    <span className="v" id="admc-cost">0.0000</span>
                  </span>
                </div>
                <div className="ad-preview" aria-hidden="true">
                  <div className="pv-ttl">
                    <span>ไทย (translated)</span>
                    <span className="f" id="adp-follow">f: following</span>
                  </div>
                  <div className="prow">
                    <div className="src" lang="ja" id="adp-hsrc">
                      {' '}
                    </div>
                    <div className="th" lang="th" id="adp-hth">
                      {' '}
                    </div>
                  </div>
                  <div className="prow">
                    <div className="src" lang="ja" id="adp-src">
                      少年は静かに本を閉じ、窓の外の月を見上げた。
                    </div>
                    <div className="th" lang="th">
                      <span id="adp-th" />
                      <span className="caret" id="adp-caret" />
                    </div>
                  </div>
                </div>
              </div>

              {/* 4 · Reader */}
              <div className={`ad-screen${screen === 3 ? ' show' : ''}`}>
                <div className="ad-cols" aria-hidden="true">
                  <div className="ad-col">
                    <div className="ch">日本語 (raw)</div>
                    <div className="jp" lang="ja">
                      少年は静かに本を閉じ、窓の外の月を見上げた。
                    </div>
                    <div className="jp" lang="ja">
                      遠くで鐘が鳴っていた。
                    </div>
                  </div>
                  <div className="ad-col">
                    <div className="ch">ไทย (translated)</div>
                    <div className="th" lang="th">
                      เด็กหนุ่มปิดหนังสือลงเงียบ ๆ แล้วเงยหน้ามองดวงจันทร์
                    </div>
                    <div className="th" lang="th">
                      เสียงระฆังดังก้องแต่ไกล
                    </div>
                  </div>
                </div>
                <div className="ad-readstat" aria-hidden="true">
                  <span className="seg">[ ch 2 ]</span>
                  <span>出会い</span>
                  <span style={{ marginLeft: 'auto' }}>z synced · ln 12</span>
                </div>
              </div>

              {/* 5 · Lexicon */}
              <div className={`ad-screen${screen === 4 ? ' show' : ''}`}>
                <div className="ad-lextabs">
                  {LEX_TABS.map((t, i) => (
                    <button
                      key={t}
                      type="button"
                      className={`ad-lx${lexTab === i ? ' active' : ''}`}
                      onClick={() => setLexTab(i)}
                    >
                      {lexTab === i ? `〔 ${t} 〕` : t}
                    </button>
                  ))}
                  <span className="ct">
                    {lexTab === 0 ? '(4 terms)' : lexTab === 1 ? '(3 characters)' : '(style)'}
                  </span>
                </div>
                {lexTab === 0 ? (
                  <>
                    <div className="ad-lexhead">
                      <span className="c1">JP term</span>
                      <span className="c2">Thai term</span>
                      <span className="c3">notes</span>
                    </div>
                    <div className="ad-list">
                      {GLOSSARY.map((g, i) => (
                        <div
                          key={g.jp}
                          className={`ad-lexrow${lexSel === i ? ' sel' : ''}`}
                          onClick={() => setLexSel(i)}
                        >
                          <span className="c1 jp" lang="ja">
                            {g.jp}
                          </span>
                          <span className="c2">
                            <span className="ar">→ </span>
                            <span className="th" lang="th">
                              {g.th}
                            </span>
                          </span>
                          <span className="c3 note">
                            {g.lock ? (
                              <span className="lock">⚿ {g.note}</span>
                            ) : (
                              g.note
                            )}
                          </span>
                        </div>
                      ))}
                    </div>
                  </>
                ) : lexTab === 1 ? (
                  <div className="ad-list">
                    <div className="ad-lexrow sel">
                      <span className="c1 jp" lang="ja">
                        リオ
                      </span>
                      <span className="c2">
                        <span className="ar">→ </span>
                        <span className="th" lang="th">
                          ริโอ
                        </span>
                      </span>
                      <span className="c3 note">主人公 · ผม / 俺</span>
                    </div>
                    <div className="ad-lexrow">
                      <span className="c1 jp" lang="ja">
                        セリア
                      </span>
                      <span className="c2">
                        <span className="ar">→ </span>
                        <span className="th" lang="th">
                          เซเลีย
                        </span>
                      </span>
                      <span className="c3 note">先生 · คุณครู</span>
                    </div>
                  </div>
                ) : (
                  <div className="ad-list">
                    <div className="ad-lexrow">
                      <span className="note">น้ำเสียงสุภาพ คงคำเรียกขานญี่ปุ่นเป็นไทย</span>
                    </div>
                    <div className="ad-lexrow">
                      <span className="note">-san → คุณ · -senpai → รุ่นพี่</span>
                    </div>
                  </div>
                )}
              </div>

              <div className="ad-hint-chip show" aria-hidden="true">
                คลิกเพื่อโต้ตอบ · 1–5 · t แปล
              </div>
            </div>

            {/* footer hints */}
            <div className="app-foot" aria-hidden="true">
              <span className="af-hints">
                {HINTS[screen].map(([key, label]) => (
                  <span key={key}>
                    <b>{key}</b> {label}
                  </span>
                ))}
              </span>
              <span className="af-global">
                <span>
                  <b>?</b> help
                </span>
                <span>
                  <b>:</b> cmd
                </span>
                <span>
                  <b>q</b> quit
                </span>
              </span>
            </div>
          </div>
        </div>
      </div>
    </aside>
  )
}
