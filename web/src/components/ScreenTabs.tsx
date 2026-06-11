import { useId, useRef, useState } from 'react'
import type { ReactNode } from 'react'

type Screen = {
  key: string
  num: string
  ja: string
  en: string
  info: ReactNode
  mock: ReactNode
}

function Keys({ children }: { children: ReactNode }) {
  return (
    <div className="scr-keys">
      <span className="klbl">ปุ่ม</span>
      {children}
    </div>
  )
}
function Key({ k, children }: { k: ReactNode; children: ReactNode }) {
  return (
    <span className="kkey">
      {k} {children}
    </span>
  )
}
const Kbd = ({ children }: { children: ReactNode }) => (
  <span className="kbd">{children}</span>
)

const SCREENS: Screen[] = [
  {
    key: 'shelf',
    num: '1',
    ja: '書架',
    en: 'Shelf',
    info: (
      <>
        <p>
          Shelf คือรายการโปรเจกต์ทั้งหมดของคุณ เปิดงานเดิมได้ทันที หรือนำเข้า EPUB
          เพื่อสร้างโปรเจกต์ใหม่ ระหว่างนำเข้า honya จะจัดบทตามลำดับสันหนังสือ
          ย้ายภาพประกอบ และล้าง HTML ให้เป็น Markdown
        </p>
        <Keys>
          <Key k={<Kbd>↵</Kbd>}>เปิด</Key>
          <Key k={<Kbd>i</Kbd>}>นำเข้า</Key>
        </Keys>
      </>
    ),
    mock: (
      <>
        <div className="scr-mock-bar">
          <span className="dots">
            <i />
            <i />
            <i />
          </span>
          <span className="ja" lang="ja">
            書架
          </span>{' '}
          projects
        </div>
        <div className="scr-mock-body">
          <div className="scr-mock-row sel">
            <span className="mglyph done">●</span>
            <span className="mtitle">精霊幻想記</span>
            <span className="mmeta">· 2 vols</span>
          </div>
          <div className="scr-mock-row">
            <span className="mglyph work">◐</span>
            <span className="mtitle">転生したら</span>
            <span className="mmeta">· 1 vol</span>
          </div>
          <div className="scr-mock-row">
            <span className="mglyph pend">○</span>
            <span className="mtitle">本好きの記録</span>
            <span className="mmeta">· new</span>
          </div>
          <div className="scr-mock-row">
            <span className="mind">──────────────</span>
          </div>
          <div className="scr-mock-row">
            <span className="maccent">[ i ]</span>
            <span className="mmeta">import EPUB…</span>
          </div>
          <div className="scr-mock-row">
            <span className="msrc">drop a .epub into a folder</span>
          </div>
        </div>
      </>
    ),
  },
  {
    key: 'project',
    num: '2',
    ja: '棚',
    en: 'Project',
    info: (
      <>
        <p>
          Project แสดงเล่ม บท และสถานะของแต่ละบทด้วยสัญลักษณ์ —{' '}
          <span style={{ fontFamily: 'var(--moon)' }}>○ ◔ ◐ ◑ ◕ ●</span> หรือ{' '}
          <span className="mverm" style={{ color: 'var(--vermilion)' }}>
            ✗
          </span>{' '}
          เมื่องานล้มเหลว หน้านี้ยังพาไปดูไฟล์บริบทและรายละเอียดของบทที่เลือกได้
        </p>
        <Keys>
          <Key k={<Kbd>t</Kbd>}>แปลบท</Key>
          <Key k={<Kbd>T</Kbd>}>ทั้งเล่ม</Key>
        </Keys>
      </>
    ),
    mock: (
      <>
        <div className="scr-mock-bar">
          <span className="dots">
            <i />
            <i />
            <i />
          </span>
          <span className="ja" lang="ja">
            棚
          </span>{' '}
          Vol_01
        </div>
        <div className="scr-mock-body">
          <div className="scr-mock-row">
            <span className="mglyph">▾</span>
            <span className="mtitle">Vol_01 · 精霊幻想記</span>
          </div>
          <div className="scr-mock-row">
            <span className="mind">│</span> <span className="mglyph done">●</span>
            <span className="mid">ch_001</span>
            <span className="mtitle">プロローグ</span>
          </div>
          <div className="scr-mock-row sel">
            <span className="mind">│</span> <span className="mglyph work">◑</span>
            <span className="mid">ch_002</span> <span className="mtitle">転生</span>
          </div>
          <div className="scr-mock-row">
            <span className="mind">│</span> <span className="mglyph img">▣</span>
            <span className="mid">ch_003</span> <span className="mtitle">口絵</span>
          </div>
          <div className="scr-mock-row">
            <span className="mind">│</span> <span className="mglyph fail">✗</span>
            <span className="mid">ch_004</span>
            <span className="mtitle">王都へ</span>
          </div>
          <div className="scr-mock-row">
            <span className="mind">└</span> <span className="mglyph pend">○</span>
            <span className="mid">ch_005</span>
            <span className="mtitle">剣の稽古</span>
          </div>
        </div>
      </>
    ),
  },
  {
    key: 'translate',
    num: '3',
    ja: '訳',
    en: 'Translate',
    info: (
      <>
        <p>
          หน้า Translate แสดงความคืบหน้าเป็น chunk, สถานะของ{' '}
          <span className="mono">◆ Orchestrator</span>,{' '}
          <span className="mono">▲ Translator</span>,{' '}
          <span className="mono">■ Reviewer</span>, จำนวนโทเคน ค่าใช้จ่ายโดยประมาณ
          และตัวอย่างภาษาไทยที่สตรีมกลับมาระหว่างแปล
        </p>
        <Keys>
          <Key k={<Kbd>p</Kbd>}>หยุดชั่วคราว</Key>
          <Key k={<Kbd>s</Kbd>}>หยุด</Key>
          <Key k={<Kbd>f</Kbd>}>ติดตาม</Key>
        </Keys>
      </>
    ),
    mock: (
      <>
        <div className="scr-mock-bar">
          <span className="dots">
            <i />
            <i />
            <i />
          </span>
          <span className="ja" lang="ja">
            訳
          </span>{' '}
          ch_003 · live
        </div>
        <div className="scr-mock-body">
          <div className="scr-mock-row">
            <span className="mmeta">chunk</span>{' '}
            <span className="scr-mock-gauge">
              <span className="gf">▰▰▰▰▰▰▰▰▰▰▰▰▰</span>
              <span className="gt">▱▱▱▱▱▱▱▱▱</span>
            </span>{' '}
            <span className="mmeta">4/7</span>
          </div>
          <div className="scr-mock-row">
            <span className="maccent">◆ Orch</span>{' '}
            <span className="mmeta">term 月 → จันทร์</span>
          </div>
          <div className="scr-mock-row">
            <span className="maccent">▲ Trans</span>{' '}
            <span className="mmeta">returned · 1.4k tok</span>
          </div>
          <div className="scr-mock-row">
            <span className="maccent">■ Review</span>{' '}
            <span className="msage">✓ approved</span>
          </div>
          <div className="scr-mock-row">
            <span className="msrc" lang="ja">
              少年は静かに本を閉じた。
            </span>
          </div>
          <div className="scr-mock-row">
            <span className="mth" lang="th">
              เด็กหนุ่มปิดหนังสือลงอย่างเงียบ ๆ
            </span>
            <span className="maccent">▏</span>
          </div>
        </div>
      </>
    ),
  },
  {
    key: 'reader',
    num: '4',
    ja: '読',
    en: 'Reader',
    info: (
      <>
        <p>
          Reader ใช้อ่านตรวจแบบเทียบคู่ ภาษาญี่ปุ่นอยู่ทางซ้าย ภาษาไทยอยู่ทางขวา
          การเลื่อนแบบซิงก์ช่วยให้ย่อหน้าตรงกัน
          และสามารถปิดซิงก์หรือเปลี่ยนเลย์เอาต์ได้เมื่ออยากตรวจทีละฝั่ง
        </p>
        <Keys>
          <Key
            k={
              <>
                <Kbd>[</Kbd> <Kbd>]</Kbd>
              </>
            }
          >
            เลื่อนบท
          </Key>
          <Key k={<Kbd>z</Kbd>}>ซิงก์</Key>
          <Key k={<Kbd>o</Kbd>}>เลย์เอาต์</Key>
        </Keys>
      </>
    ),
    mock: (
      <>
        <div className="scr-mock-bar">
          <span className="dots">
            <i />
            <i />
            <i />
          </span>
          <span className="ja" lang="ja">
            読
          </span>{' '}
          ch_002 · <span className="maccent">z synced</span>
        </div>
        <div className="scr-mock-cols">
          <div className="col">
            <div className="ch">JA · 日本語</div>
            <div className="msrc" lang="ja">
              少年は静かに本を閉じ、窓の外の月を見上げた。
            </div>
            <div className="msrc" lang="ja">
              遠くで鐘が鳴っていた。
            </div>
          </div>
          <div className="col">
            <div className="ch">TH · ไทย</div>
            <div className="mth" lang="th">
              เด็กหนุ่มปิดหนังสือลงเงียบ ๆ แล้วเงยหน้ามองดวงจันทร์
            </div>
            <div className="mth" lang="th">
              เสียงระฆังดังก้องแต่ไกล
            </div>
          </div>
        </div>
      </>
    ),
  },
  {
    key: 'lexicon',
    num: '5',
    ja: '辞',
    en: 'Lexicon',
    info: (
      <>
        <p>
          Lexicon รวมศัพท์ ตัวละคร และสไตล์ไว้ในที่เดียว ใช้ค้นหา เพิ่ม แก้ หรือลบ
          รายการด้วยมือได้ ระหว่างแปล pipeline อาจเพิ่มข้อมูลใหม่ลงไฟล์เหล่านี้
          และคุณยังตรวจแก้ทีหลังได้
        </p>
        <Keys>
          <Key k={<Kbd>n</Kbd>}>ใหม่</Key>
          <Key k={<Kbd>e</Kbd>}>แก้ไข</Key>
          <Key k={<Kbd>d</Kbd>}>ลบ</Key>
          <Key k={<Kbd>/</Kbd>}>ค้นหา</Key>
        </Keys>
      </>
    ),
    mock: (
      <>
        <div className="scr-mock-bar">
          <span className="dots">
            <i />
            <i />
            <i />
          </span>
          <span className="ja" lang="ja">
            辞
          </span>{' '}
          Glossary · Characters · Style
        </div>
        <div className="scr-mock-body">
          <div className="scr-mock-row">
            <span className="maccent">/</span>
            <span className="mmeta">search… </span>
            <span className="msrc">精霊</span>
          </div>
          <div className="scr-mock-row sel">
            <span className="msrc" lang="ja">
              精霊
            </span>
            <span className="mind">→</span>
            <span className="mth" lang="th">
              ภูตธาตุ
            </span>
          </div>
          <div className="scr-mock-row">
            <span className="msrc" lang="ja">
              勇者
            </span>
            <span className="mind">→</span>
            <span className="mth" lang="th">
              ผู้กล้า
            </span>
          </div>
          <div className="scr-mock-row">
            <span className="msrc" lang="ja">
              リオ
            </span>
            <span className="mind">→</span>
            <span className="mth" lang="th">
              ริโอ
            </span>
            <span className="mmeta">· 主人公</span>
          </div>
          <div className="scr-mock-row">
            <span className="mind">──────────────</span>
          </div>
          <div className="scr-mock-row">
            <span className="maccent">[ n ]</span> new <span className="mmeta">·</span>{' '}
            <span className="maccent">[ e ]</span> edit{' '}
            <span className="mmeta">·</span> <span className="maccent">[ d ]</span>{' '}
            delete
          </div>
        </div>
      </>
    ),
  },
]

export function ScreenTabs() {
  const [active, setActive] = useState(2) // 訳 Translate is the default screen
  const [swapping, setSwapping] = useState(false)
  const tabRefs = useRef<Array<HTMLButtonElement | null>>([])
  const panelId = useId()
  const tabIdFor = (i: number) => `${panelId}-tab-${SCREENS[i].key}`

  const select = (i: number) => {
    if (i === active) return
    setSwapping(true)
    setActive(i)
    // brief opacity dip to echo the original htmx swap fade
    window.setTimeout(() => setSwapping(false), 130)
  }

  const onKeyDown = (e: React.KeyboardEvent) => {
    let next = -1
    if (e.key === 'ArrowRight' || e.key === 'ArrowDown')
      next = (active + 1) % SCREENS.length
    else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp')
      next = (active - 1 + SCREENS.length) % SCREENS.length
    else if (e.key === 'Home') next = 0
    else if (e.key === 'End') next = SCREENS.length - 1
    if (next > -1) {
      e.preventDefault()
      select(next)
      tabRefs.current[next]?.focus()
    }
  }

  const s = SCREENS[active]

  return (
    <>
      <div
        className="screens-tabbar"
        role="tablist"
        aria-label="ห้าหน้าจอของ honya"
        onKeyDown={onKeyDown}
      >
        {SCREENS.map((sc, i) => (
          <button
            key={sc.key}
            ref={(el) => {
              tabRefs.current[i] = el
            }}
            className="stab"
            type="button"
            role="tab"
            id={tabIdFor(i)}
            aria-selected={i === active}
            aria-controls={`${panelId}-panel`}
            tabIndex={i === active ? 0 : -1}
            onClick={() => select(i)}
          >
            <span className="stab-num">{sc.num}</span>
            <span className="stab-ja" lang="ja">
              {sc.ja}
            </span>
            <span className="stab-en">{sc.en}</span>
          </button>
        ))}
      </div>

      <div
        id={`${panelId}-panel`}
        role="tabpanel"
        aria-labelledby={tabIdFor(active)}
        tabIndex={0}
        className={`screen-panel${swapping ? ' is-swapping' : ''}`}
      >
        <div className="scr">
          <div className="scr-info">
            <div className="scr-head">
              <span className="scr-num">{s.num}</span>
              <span className="scr-ja" lang="ja">
                {s.ja}
              </span>
              <span className="scr-en">{s.en}</span>
            </div>
            {s.info}
          </div>
          <div className="scr-mock" aria-hidden="true">
            {s.mock}
          </div>
        </div>
      </div>
    </>
  )
}
