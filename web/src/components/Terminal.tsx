import { useEffect, useRef } from 'react'

/* The five-tab TUI tab strip shown atop the mock (3 訳 active). */
const TABS: Array<[string, string]> = [
  ['1', '書架'],
  ['2', '棚'],
  ['3', '訳'],
  ['4', '読'],
  ['5', '辞'],
]
const TAB_LABELS = ['Shelf', 'Project', 'Translate', 'Reader', 'Lexicon']

const CHAPTERS: Array<{
  bar: string
  cls: string
  glyph: string
  i: number
  id: string
  title: string
  sel?: boolean
}> = [
  { bar: '▌', cls: 'done', glyph: '●', i: 0, id: 'ch_001', title: 'プロローグ' },
  { bar: '▌', cls: 'done', glyph: '●', i: 1, id: 'ch_002', title: '転生' },
  { bar: '▌', cls: 'work', glyph: '◐', i: 2, id: 'ch_003', title: '出会い', sel: true },
  { bar: '▌', cls: 'img', glyph: '▣', i: 3, id: 'ch_004', title: '口絵' },
  { bar: '▌', cls: 'pend', glyph: '○', i: 4, id: 'ch_005', title: '王都へ' },
  { bar: '▌', cls: 'pend', glyph: '○', i: 5, id: 'ch_006', title: '剣の稽古' },
]

/* bilingual lines (real JA, plausible literary TH) */
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
  [
    '風が頁をめくり、燭台の炎が静かに揺れた。',
    'สายลมพัดพลิกหน้ากระดาษ เปลวเทียนบนเชิงตะเกียงไหวระริกอย่างเงียบงัน',
  ],
  ['明日もまた、この物語の続きを訳すのだ。', 'พรุ่งนี้ก็จะแปลเรื่องราวบทต่อไปนี้อีกครั้ง'],
]

const MOONS = ['○', '◔', '◐', '◑', '◕', '●']
const SPIN = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏']
const TOTAL = 7
const GW = 22
const TOK_RATE = 0.0000018
const ORCH_TOOLS = [
  'term 月 → จันทร์',
  'character 02 (リオ → ริโอ)',
  'continuity [low] kept honorific',
  'term 王都 → เมืองหลวง',
]

function fmtTok(n: number) {
  return n >= 1000 ? (n / 1000).toFixed(1) + 'k' : String(Math.round(n))
}

/** Pull in trailing Thai combining marks so a base + its vowel/tone render together. */
function thaiClusterEnd(s: string, i: number) {
  while (i < s.length) {
    const c = s.charCodeAt(i)
    if (c === 0x0e31 || (c >= 0x0e34 && c <= 0x0e3a) || (c >= 0x0e47 && c <= 0x0e4e)) i++
    else break
  }
  return i
}

export function Terminal() {
  const rootRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const root = rootRef.current
    if (!root) return
    const $ = <T extends Element = HTMLElement>(sel: string) =>
      root.querySelector(sel) as T | null

    const gFill = $('#gauge-fill')
    const gTrack = $('#gauge-track')
    const gPct = $('#gauge-pct')
    const ag = [$('#ag0'), $('#ag1'), $('#ag2')]
    const mIn = $('#m-in')
    const mOut = $('#m-out')
    const mTok = $('#m-tok')
    const mTools = $('#m-tools')
    const mCost = $('#m-cost')
    const mRetry = $('#m-retry')
    const mcIn = $('#mc-in')
    const mcOut = $('#mc-out')
    const mcTotal = $('#mc-total')
    const mcTools = $('#mc-tools')
    const mcCost = $('#mc-cost')
    const srcActive = $('#src-active')
    const thStream = $('#th-stream')
    const caret = $<HTMLElement>('#caret')
    const histRow = $('#prow-hist')
    const histSrc = histRow ? (histRow.querySelector('.src') as HTMLElement) : null
    const histTh = histRow ? (histRow.querySelector('.th') as HTMLElement) : null
    const preview = $('#preview')
    const moon = $<HTMLElement>('.moon[data-i="2"]')
    if (!gFill || !gTrack || !gPct || !preview) return

    const reduce = window.matchMedia('(prefers-reduced-motion: reduce)').matches

    const setMsg = (i: number, text: string) => {
      const el = ag[i]
      if (!el) return
      el.classList.remove('idle')
      const m = el.querySelector('.msg') as HTMLElement
      m.textContent = text
      m.style.color = ''
    }
    const setIdle = (i: number, text?: string) => {
      const el = ag[i]
      if (!el) return
      el.classList.add('idle')
      const m = el.querySelector('.msg') as HTMLElement
      m.textContent = text || 'idle'
      m.style.color = ''
    }
    const setReview = (text: string, kind: 'approve' | 'reject' | null) => {
      const el = ag[2]
      if (!el) return
      el.classList.remove('idle')
      const m = el.querySelector('.msg') as HTMLElement
      m.textContent = text
      m.style.color =
        kind === 'approve'
          ? 'var(--sage-text)'
          : kind === 'reject'
            ? 'var(--vermilion)'
            : ''
    }
    const setSpin = (i: number, on: boolean) => {
      for (let k = 0; k < 3; k++) {
        if (!ag[k]) continue
        const sp = ag[k]!.querySelector('.sp') as HTMLElement
        sp.textContent = ' '
        delete sp.dataset.on
      }
      if (on && ag[i]) (ag[i]!.querySelector('.sp') as HTMLElement).dataset.on = '1'
    }
    const activeSpinIndex = () => {
      for (let k = 0; k < 3; k++)
        if (ag[k] && (ag[k]!.querySelector('.sp') as HTMLElement).dataset.on === '1')
          return k
      return -1
    }
    const drawGauge = (frac: number) => {
      const filled = Math.max(0, Math.min(GW, Math.round(frac * GW)))
      gFill.textContent = new Array(filled + 1).join('▰')
      gTrack.textContent = new Array(GW - filled + 1).join('▱')
    }
    const setMoon = (stage: number) => {
      if (!moon) return
      moon.textContent = MOONS[stage]
      moon.className = 'moon ' + (stage === 5 ? 'done' : 'work')
    }
    const setActive = (ja: string, th: string, showCaret: boolean) => {
      if (srcActive) srcActive.textContent = ja || ' '
      if (thStream) thStream.textContent = th || ''
      if (caret) caret.style.visibility = showCaret ? 'visible' : 'hidden'
    }
    const setHistory = (ja: string, th: string) => {
      if (histSrc) histSrc.textContent = ja || ' '
      if (histTh) histTh.textContent = th || ' '
    }

    const state = {
      chunk: 0,
      tin: 0,
      tout: 0,
      tools: 0,
      retry: 0,
      chunkOut: 0,
      toolIdx: 0,
      lineIdx: 0,
      charIdx: 0,
      phase: 'start',
      phaseT: 0,
    }
    const drawMeter = () => {
      const total = state.tin + state.tout
      const ti = fmtTok(state.tin)
      const to = fmtTok(state.tout)
      const tt = fmtTok(total)
      const cost = (total * TOK_RATE).toFixed(4)
      const tools = String(state.tools)
      if (mIn) mIn.textContent = ti
      if (mOut) mOut.textContent = to
      if (mTok) mTok.textContent = tt
      if (mTools) mTools.textContent = tools
      if (mCost) mCost.textContent = cost
      if (mRetry) mRetry.textContent = String(state.retry)
      if (mcIn) mcIn.textContent = ti
      if (mcOut) mcOut.textContent = to
      if (mcTotal) mcTotal.textContent = tt
      if (mcTools) mcTools.textContent = tools
      if (mcCost) mcCost.textContent = cost
    }

    if (reduce) {
      drawGauge(1)
      gPct.textContent = '7/7'
      setMsg(0, 'volume recap updated for ch 3')
      setMsg(1, 'returned · 1.4k tok')
      setReview('✓ approved', 'approve')
      if (mIn) mIn.textContent = '7.2k'
      if (mOut) mOut.textContent = '5.3k'
      if (mTok) mTok.textContent = '12.5k'
      if (mTools) mTools.textContent = '9'
      if (mCost) mCost.textContent = '0.0225'
      if (mRetry) mRetry.textContent = '1'
      if (mcIn) mcIn.textContent = '7.2k'
      if (mcOut) mcOut.textContent = '5.3k'
      if (mcTotal) mcTotal.textContent = '12.5k'
      if (mcTools) mcTools.textContent = '9'
      if (mcCost) mcCost.textContent = '0.0225'
      setMoon(5)
      setHistory(LINES[3][0], LINES[3][1])
      setActive(LINES[4][0], LINES[4][1], false)
      return
    }

    let spinFrame = 0
    let lastSpin = 0

    const startChunk = () => {
      state.charIdx = 0
      drawGauge(state.chunk / TOTAL)
      gPct.textContent = state.chunk + '/' + TOTAL
      if (state.chunk === 0) setMoon(1)
      else setMoon(Math.min(4, 1 + Math.floor((state.chunk / TOTAL) * 3)))
      setMsg(1, 'requesting chunk ' + (state.chunk + 1) + ' (attempt 1)')
      setIdle(2, 'waiting')
      setIdle(0, 'idle')
      setSpin(1, true)
      state.chunkOut = 0
      state.phase = 'translate'
      state.phaseT = 0
    }
    const beginType = () => {
      const ln = LINES[state.lineIdx % LINES.length]
      setActive(ln[0], '', true)
      state.phase = 'type'
      state.phaseT = 0
      state.charIdx = 0
    }

    const tick = (dt: number) => {
      const nowSpin = activeSpinIndex()
      lastSpin += dt
      if (lastSpin >= 95) {
        lastSpin = 0
        spinFrame = (spinFrame + 1) % SPIN.length
        if (nowSpin >= 0)
          (ag[nowSpin]!.querySelector('.sp') as HTMLElement).textContent = SPIN[spinFrame]
      }
      state.phaseT += dt

      switch (state.phase) {
        case 'start':
          if (state.phaseT > 350) startChunk()
          break
        case 'translate':
          state.tin += Math.round(dt * 0.9)
          drawMeter()
          if (state.phaseT > 620) beginType()
          break
        case 'type': {
          const ln = LINES[state.lineIdx % LINES.length]
          const th = ln[1]
          state.charIdx = Math.min(th.length, state.charIdx + 2)
          state.charIdx = thaiClusterEnd(th, state.charIdx)
          setActive(ln[0], th.slice(0, state.charIdx), true)
          const d = Math.round(dt * 0.5)
          state.tout += d
          state.chunkOut += d
          drawMeter()
          if (state.charIdx >= th.length) {
            setSpin(2, true)
            setMsg(2, 'reviewing …')
            setMsg(1, 'returned · ' + fmtTok(state.chunkOut) + ' tok')
            state.phase = 'review'
            state.phaseT = 0
          }
          break
        }
        case 'review': {
          if (state.phaseT > 760) {
            const doReject = state.chunk === 2 && state.retry === 0
            if (doReject) {
              state.retry += 1
              drawMeter()
              setReview('retry ' + state.retry + '/3 · pronoun consistency', 'reject')
              setMsg(1, 'requesting chunk ' + (state.chunk + 1) + ' (attempt 2)')
              setSpin(1, true)
              state.phase = 'retry'
              state.phaseT = 0
            } else {
              setReview('✓ approved', 'approve')
              setSpin(2, false)
              const ln = LINES[state.lineIdx % LINES.length]
              setHistory(ln[0], ln[1])
              state.lineIdx += 1
              const nextLn = LINES[state.lineIdx % LINES.length]
              setActive(nextLn[0], '', true)
              state.phase = 'commit'
              state.phaseT = 0
            }
          }
          break
        }
        case 'retry': {
          state.tin += Math.round(dt * 0.5)
          drawMeter()
          if (state.phaseT > 560) {
            setReview('reviewing …', null)
            setSpin(2, true)
            state.phase = 'review'
            state.phaseT = 0
          }
          break
        }
        case 'commit': {
          state.chunk += 1
          drawGauge(state.chunk / TOTAL)
          gPct.textContent = state.chunk + '/' + TOTAL
          setSpin(0, true)
          setMsg(0, ORCH_TOOLS[state.toolIdx % ORCH_TOOLS.length])
          state.toolIdx += 1
          state.tools += 1
          drawMeter()
          state.phase = state.chunk >= TOTAL ? 'orch' : 'next'
          state.phaseT = 0
          break
        }
        case 'next':
          if (state.phaseT > 420) {
            setSpin(0, false)
            setMoon(Math.min(4, 1 + Math.floor((state.chunk / TOTAL) * 3)))
            startChunk()
          }
          break
        case 'orch': {
          setSpin(0, true)
          setMsg(0, 'volume recap updated for ch 3')
          if (state.phaseT > 1000) {
            setSpin(0, false)
            state.tools += 1
            drawMeter()
            setMsg(1, 'done')
            setMsg(2, 'done')
            drawGauge(1)
            gPct.textContent = TOTAL + '/' + TOTAL
            setMoon(5)
            state.phase = 'done'
            state.phaseT = 0
          }
          break
        }
        case 'done':
          if (state.phaseT > 2600) {
            state.chunk = 0
            state.tin = 0
            state.tout = 0
            state.tools = 0
            state.retry = 0
            state.toolIdx = 0
            state.lineIdx = 0
            drawMeter()
            setIdle(0, 'idle')
            setIdle(1, 'idle')
            setIdle(2, 'idle')
            setSpin(0, false)
            setHistory(' ', ' ')
            setActive(LINES[0][0], '', true)
            setMoon(0)
            drawGauge(0)
            gPct.textContent = '0/' + TOTAL
            state.phase = 'start'
            state.phaseT = 0
          }
          break
      }
    }

    drawGauge(0)
    gPct.textContent = '0/' + TOTAL
    drawMeter()
    setHistory(' ', ' ')
    setActive(LINES[0][0], '', true)

    let last: number | null = null
    let running = true
    let raf = 0
    const loop = (ts: number) => {
      if (last === null) last = ts
      const dt = Math.min(80, ts - last)
      last = ts
      if (running) tick(dt)
      raf = requestAnimationFrame(loop)
    }
    raf = requestAnimationFrame(loop)

    let io: IntersectionObserver | null = null
    if ('IntersectionObserver' in window) {
      io = new IntersectionObserver(
        (entries) => {
          entries.forEach((e) => {
            running = e.isIntersecting
            if (running) last = null
          })
        },
        { threshold: 0.05 },
      )
      io.observe(preview)
    }
    const onVis = () => {
      if (document.hidden) running = false
      else {
        running = true
        last = null
      }
    }
    document.addEventListener('visibilitychange', onVis)

    return () => {
      cancelAnimationFrame(raf)
      io?.disconnect()
      document.removeEventListener('visibilitychange', onVis)
    }
  }, [])

  return (
    <aside className="hero-aside" aria-label="ตัวอย่างการแปลแบบสด" ref={rootRef}>
      <div className="term-shell">
        <div className="term-shadow" aria-hidden="true" />
        <div
          className="terminal"
          role="img"
          aria-label="ภาพจำลองหน้าจอ Translate ของ honya แสดงรายการบท สถานะการแปล ความคืบหน้าเป็น chunk เอเจนต์ Orchestrator, Translator, Reviewer จำนวนโทเคน ค่าใช้จ่ายโดยประมาณ และตัวอย่างภาษาไทยที่สตรีมกลับมาระหว่างแปล"
        >
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
              honya — <span lang="ja">訳</span> Translate
            </div>
            <div className="cwd">~/novels/Vol_01</div>
          </div>

          <div className="tui">
            <nav className="tabs" aria-hidden="true">
              {TABS.map(([num, gly], i) => (
                <span key={num} className={`tab${i === 2 ? ' active' : ''}`}>
                  <span className="num">{num}</span>
                  <span className="gly" lang="ja">
                    {gly}
                  </span>
                  <span className="label">{TAB_LABELS[i]}</span>
                </span>
              ))}
            </nav>

            <div className="tui-body">
              <div className="pane">
                <p className="pane-h">
                  <span className="ja" lang="ja">
                    棚
                  </span>{' '}
                  Vol_01 — chapters
                </p>
                <ul className="tree" aria-hidden="true">
                  <li className="vol">
                    <span className="moon">▾</span>
                    <span>Vol_01 · 精霊幻想記</span>
                  </li>
                  {CHAPTERS.map((c) => (
                    <li key={c.id} className={`row${c.sel ? ' sel' : ''}`}>
                      <span className="bar">{c.bar}</span>
                      <span className={`moon ${c.cls}`} data-i={c.i}>
                        {c.glyph}
                      </span>
                      <span className="cid">{c.id}</span>
                      <span className="ctitle">{c.title}</span>
                    </li>
                  ))}
                </ul>
              </div>

              <div className="pane">
                <p className="pane-h">
                  <span className="ja" lang="ja">
                    訳
                  </span>{' '}
                  ch_003 · 出会い
                </p>

                <div className="run-line">
                  <span className="k">chunk</span>
                  <span className="gauge" aria-hidden="true">
                    <span id="gauge-fill" className="fill" />
                    <span id="gauge-track" className="track">
                      ▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱
                    </span>
                    <span id="gauge-pct" className="pct">
                      0/7
                    </span>
                  </span>
                </div>

                <div className="agents" aria-hidden="true">
                  <div className="agent orch idle" id="ag0">
                    <span className="badge">◆ Orch</span>
                    <span className="sp"> </span>
                    <span className="msg">idle</span>
                  </div>
                  <div className="agent trans idle" id="ag1">
                    <span className="badge">▲ Trans</span>
                    <span className="sp"> </span>
                    <span className="msg">idle</span>
                  </div>
                  <div className="agent rev idle" id="ag2">
                    <span className="badge">■ Review</span>
                    <span className="sp"> </span>
                    <span className="msg">idle</span>
                  </div>
                </div>

                <div className="meter run" aria-hidden="true">
                  <span className="ml">run</span>
                  <span>
                    <b>in</b> <span className="v tk" id="m-in">0</span>
                  </span>
                  <span>
                    <b>out</b> <span className="v tk" id="m-out">0</span>
                  </span>
                  <span>
                    <b>total</b> <span className="v tk2" id="m-tok">0</span>
                  </span>
                  <span>
                    <b>tools</b> <span className="v n" id="m-tools">0</span>
                  </span>
                  <span>
                    <b>$</b>
                    <span className="v cost" id="m-cost">0.0000</span>
                  </span>
                  <span>
                    <b>retries</b> <span className="v n" id="m-retry">0</span>
                  </span>
                </div>
                <div className="meter chap" aria-hidden="true">
                  <span className="ml">chap</span>
                  <span>
                    <b>in</b> <span className="v tk" id="mc-in">0</span>
                  </span>
                  <span>
                    <b>out</b> <span className="v tk" id="mc-out">0</span>
                  </span>
                  <span>
                    <b>total</b> <span className="v tk2" id="mc-total">0</span>
                  </span>
                  <span>
                    <b>tools</b> <span className="v n" id="mc-tools">0</span>
                  </span>
                  <span>
                    <b>$</b>
                    <span className="v cost" id="mc-cost">0.0000</span>
                  </span>
                </div>

                <div className="preview" id="preview" aria-hidden="true">
                  <div className="prow" id="prow-hist">
                    <div className="src" lang="ja">
                      {' '}
                    </div>
                    <div className="th" lang="th">
                      {' '}
                    </div>
                  </div>
                  <div className="prow" id="prow-active">
                    <div className="src" lang="ja" id="src-active">
                      少年は静かに本を閉じ、窓の外の月を見上げた。
                    </div>
                    <div className="th" lang="th">
                      <span id="th-stream" />
                      <span className="caret" id="caret" />
                    </div>
                  </div>
                </div>
              </div>
            </div>

            <div className="tui-foot" aria-hidden="true">
              <span>
                <kbd>p</kbd> pause
              </span>
              <span className="opt">
                <kbd>s</kbd> stop
              </span>
              <span className="opt">
                <kbd>f</kbd> follow
              </span>
              <span className="spacer">
                <kbd>1</kbd>–<kbd>5</kbd> tabs · <kbd>?</kbd> help
              </span>
            </div>
          </div>
        </div>
      </div>
      <p className="term-cap">ตัวอย่างหน้าจอโดยย่อ — สี สัญลักษณ์ และข้อมูลอิงจากแอปจริง</p>
    </aside>
  )
}
