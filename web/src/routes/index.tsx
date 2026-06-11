import { createFileRoute } from '@tanstack/react-router'
import type { ReactNode } from 'react'
import { Header } from '../components/Header'
import { Footer } from '../components/Footer'
import { AppDemo } from '../components/AppDemo'
import { ScreenTabs } from '../components/ScreenTabs'
import { PipelineDiagram } from '../components/PipelineDiagram'
import { InstallCard } from '../components/InstallCard'
import { Reveal } from '../components/Reveal'
import { ArrowIcon, ChevronIcon, GitHubIcon } from '../components/icons'
import { GITHUB_URL, SITE_URL, VERSION } from '../data/site'

const OG_IMAGE = `${SITE_URL}/og.png`

export const Route = createFileRoute('/')({
  head: () => ({
    meta: [
      {
        title: 'honya 本屋 — TUI สำหรับแปลไลต์โนเวลญี่ปุ่นเป็นไทย',
      },
      {
        name: 'description',
        content:
          'honya (本屋) เป็นแอปเทอร์มินัลสำหรับจัดโปรเจกต์แปลไลต์โนเวลญี่ปุ่นเป็นไทย: นำเข้า EPUB, แยกบท, เก็บไฟล์ Markdown, เรียกโมเดลผ่าน API ที่รองรับ OpenRouter และให้คุณตรวจงานใน TUI ใช้คีย์ของคุณเองและเก็บงานเป็นไฟล์บนดิสก์',
      },
      {
        property: 'og:title',
        content: 'honya — TUI สำหรับแปลไลต์โนเวลญี่ปุ่น→ไทย',
      },
      {
        property: 'og:description',
        content:
          'นำเข้า EPUB, แยกบทเป็น chunk, เรียกโมเดลผ่าน API ที่รองรับ OpenRouter และเก็บศัพท์ ตัวละคร โน้ต และคำแปลเป็นไฟล์ที่อ่านและแก้เองได้',
      },
      { property: 'og:url', content: `${SITE_URL}/` },
      { property: 'og:image', content: OG_IMAGE },
      {
        property: 'og:image:alt',
        content: 'honya 本屋 — TUI สำหรับแปลไลต์โนเวลญี่ปุ่นเป็นไทย',
      },
      { property: 'og:image:width', content: '1200' },
      { property: 'og:image:height', content: '630' },
      { property: 'og:image:type', content: 'image/png' },
      {
        name: 'twitter:title',
        content: 'honya — TUI สำหรับแปลไลต์โนเวลญี่ปุ่น→ไทย',
      },
      {
        name: 'twitter:description',
        content:
          'แอปเทอร์มินัลที่ช่วยนำเข้า EPUB, แปลทีละ chunk ผ่าน API ที่รองรับ OpenRouter และเก็บโปรเจกต์เป็นไฟล์ที่ตรวจเองได้',
      },
      { name: 'twitter:image', content: OG_IMAGE },
    ],
    links: [{ rel: 'canonical', href: `${SITE_URL}/` }],
  }),
  component: Home,
})

/** Editorial section header — folio number + vertical JP marker + heading. */
function EdHead({
  n,
  tate,
  eyebrow,
  title,
  lead,
  id,
}: {
  n: string
  tate: string
  eyebrow: string
  title: ReactNode
  lead: ReactNode
  id?: string
}) {
  return (
    <div className="edhead">
      <div className="edhead-rail" aria-hidden="true">
        <span className="folio">{n}</span>
        <span className="edhead-line" />
        <span className="edhead-tate ja" lang="ja">
          {tate}
        </span>
      </div>
      <div className="edhead-body">
        <span className="eyebrow">{eyebrow}</span>
        <h2 id={id}>{title}</h2>
        <p>{lead}</p>
      </div>
    </div>
  )
}

const FEATURES: Array<{ kanji: string; title: string; body: ReactNode }> = [
  {
    kanji: '鍵',
    title: 'ใช้คีย์ของคุณเอง',
    body: (
      <>
        ทำงานกับ API ที่รองรับ OpenRouter — ขอคีย์ครั้งแรกแล้วเก็บไว้ที่{' '}
        <code>~/.config/honya</code> ไม่มีบัญชี honya คำขอส่งตรงไปยัง provider ของคุณ
      </>
    ),
  },
  {
    kanji: '三',
    title: 'ไปป์ไลน์สามเอเจนต์',
    body: (
      <>
        Orchestrator, Translator และ Reviewer แบ่งงานทีละ chunk เป็นวงจรร่าง–ตรวจ–
        ลองใหม่ ก่อนบันทึกลงดิสก์
      </>
    ),
  },
  {
    kanji: '書',
    title: 'การเตรียมข้อมูล EPUB',
    body: (
      <>
        จัดบทตามลำดับสันหนังสือ ย้ายภาพประกอบ และล้าง HTML เป็น Markdown —
        โมเดลอ่านเนื้อหา ไม่ใช่มาร์กอัป
      </>
    ),
  },
  {
    kanji: '読',
    title: 'โหมดอ่านเทียบคู่',
    body: (
      <>
        พิสูจน์อักษร JA→TH แบบเทียบคู่ ซิงก์เลื่อนทีละย่อหน้า ให้สายตาไม่หลงตำแหน่ง
      </>
    ),
  },
  {
    kanji: '辞',
    title: 'ไฟล์บริบทที่แก้เองได้',
    body: (
      <>
        GLOSSARY, CHARACTERS และโน้ตอัปเดตผ่าน tool calls ระหว่างแปล
        และคุณเปิดแก้เองได้ทุกเมื่อ
      </>
    ),
  },
  {
    kanji: '匣',
    title: 'ทุกอย่างเป็นเพียงไฟล์',
    body: (
      <>
        PROJECT.md, GLOSSARY.md, STYLE.md และโฟลเดอร์ต่อเล่ม — อ่านได้ diff ได้ แก้ได้
        honya ทำงานต่อจากจุดที่ค้างไว้
      </>
    ),
  },
]

const TRUST: Array<[string, string]> = [
  ['3', 'เอเจนต์ทำงานร่วมกันต่อหนึ่ง chunk — Orchestrator · Translator · Reviewer'],
  ['5', 'หน้าจอใน TUI — 書架 棚 訳 読 辞 สลับด้วยปุ่ม 1–5'],
  ['8+', 'รูปแบบไฟล์ที่นำเข้าได้ — EPUB, PDF, DOCX, HTML, Markdown และอื่น ๆ'],
  ['100%', 'งานเป็นไฟล์บนเครื่องคุณ — ใช้คีย์ของคุณเอง ฟรีและโอเพนซอร์ส'],
]

const FAQ: Array<{ q: string; a: ReactNode }> = [
  {
    q: 'ต้องมีคีย์ API ไหม และใช้ผู้ให้บริการเจ้าไหน?',
    a: (
      <>
        ต้องมี honya ไม่มีโมเดลในตัว แต่ทำงานกับ API ที่รองรับ OpenRouter — เปิดครั้งแรก
        จะถามคีย์แล้วเก็บไว้ที่ <code>~/.config/honya</code> หรือตั้ง{' '}
        <code>HONYA_API_KEY</code> เอง (เลือกโมเดลแยกได้ต่อเอเจนต์)
      </>
    ),
  },
  {
    q: 'honya เก็บข้อมูลหรือคำแปลของฉันไหม?',
    a: (
      <>
        ไม่ ไม่มีบัญชีและไม่มีเซิร์ฟเวอร์ของ honya ทุกโปรเจกต์เป็นไฟล์อยู่บนเครื่องคุณ
        คำขอแปลถูกส่งตรงไปยัง provider ที่คุณเลือกด้วยคีย์ของคุณเอง
      </>
    ),
  },
  {
    q: 'นำเข้าไฟล์อะไรได้บ้าง?',
    a: (
      <>
        นอกจาก <code>EPUB</code> แล้วยังรองรับ <code>PDF</code>, <code>DOCX</code>,{' '}
        <code>HTML</code>, <code>CSV</code>, <code>XML</code> และไฟล์ข้อความ/
        <code>Markdown</code>/<code>JSON</code> ทั้งหมดจะถูกแปลงเป็น Markdown
        แล้วทำความสะอาดและแบ่งตอนตามขั้นตอนเดียวกัน
      </>
    ),
  },
  {
    q: 'คำแปลเชื่อถือได้แค่ไหน?',
    a: (
      <>
        honya ช่วย<b>จัดงาน</b>แปล ไม่รับประกันคำแปลสมบูรณ์ มีรอบตรวจ (Reviewer)
        และ audit ในเครื่อง แต่ควรอ่านตรวจเอง โดยเฉพาะชื่อเฉพาะและน้ำเสียง —
        โหมด Reader เทียบคู่มีไว้เพื่อการนี้
      </>
    ),
  },
  {
    q: 'ใช้บน Windows ได้ไหม?',
    a: (
      <>
        ได้ มีไบนารีสำเร็จรูปสำหรับ Linux, macOS และ Windows (ทั้ง <code>x86_64</code>{' '}
        และ <code>aarch64</code>) ติดตั้งบน Windows ด้วย{' '}
        <code>irm https://honya.altqx.com/install.ps1 | iex</code> และอัปเดตในตัวด้วย{' '}
        <code>honya update</code>
      </>
    ),
  },
]

function Home() {
  return (
    <>
      <Header page="home" />
      <main id="main">
        {/* ───── COVER ───── */}
        <section className="cover" aria-labelledby="hero-h">
          <div className="wrap">
            <div className="cover-masthead" aria-hidden="true">
              <span className="cm-l">
                <span className="ja" lang="ja">
                  本屋
                </span>{' '}
                — 日本語 → ไทย 翻訳
              </span>
              <span className="cm-r">No.01 — {VERSION}</span>
            </div>

            <div className="cover-grid">
              <div className="cover-tate" aria-hidden="true">
                <span className="ja" lang="ja">
                  一冊まるごと、訳す。
                </span>
              </div>

              <div className="cover-main">
                <span className="eyebrow">honya · 本屋</span>
                <h1 id="hero-h">
                  แปลไลต์โนเวล
                  <br />
                  ญี่ปุ่น
                  <span className="seal" aria-hidden="true">
                    →
                  </span>
                  ไทย
                  <br />
                  <span className="wordmark ja" lang="ja">
                    本屋
                  </span>
                </h1>
                <p className="lede">
                  <b>honya</b> นำเข้าไฟล์แล้วแตกเป็นโปรเจกต์บนดิสก์ — บท Markdown, ภาพ,
                  ศัพท์ ตัวละคร สไตล์ และคำแปลแยกตามเล่ม แล้วแปลทีละ chunk ผ่าน API
                  ที่รองรับ OpenRouter พร้อมหน้าจอตรวจงานเอง
                </p>
                <div className="cta-row">
                  <a className="btn btn-primary" href="#install">
                    <ArrowIcon />
                    ติดตั้ง
                  </a>
                  <a className="btn btn-ghost" href={GITHUB_URL} rel="noopener">
                    <GitHubIcon />
                    ดูบน GitHub
                  </a>
                </div>
                <ul className="cover-meta">
                  <li>
                    <span className="dot" />
                    ใช้คีย์ OpenRouter ของคุณเอง
                  </li>
                  <li>
                    <span className="dot" />
                    โปรเจกต์เป็นไฟล์ในเครื่อง
                  </li>
                  <li>
                    <span className="dot" />
                    ฟรีและโอเพนซอร์ส
                  </li>
                </ul>
              </div>
            </div>
          </div>
        </section>

        {/* ───── DEMO SPREAD (centerfold) ───── */}
        <Reveal as="section" className="spread" aria-label="ตัวอย่างแอป honya">
          <div className="wrap">
            <div className="spread-bar">
              <span className="folio">図 01</span>
              <span className="spread-cap">
                TUI จริงในเบราว์เซอร์ — คลิกแล้วกด{' '}
                <span className="kbd">1</span>–<span className="kbd">5</span> สลับหน้าจอ,{' '}
                <span className="kbd">t</span> เริ่มแปล
              </span>
              <span className="spread-edge ja" lang="ja">
                実演
              </span>
            </div>
            <AppDemo />
          </div>
        </Reveal>

        {/* ───── 01 · WHAT IT IS ───── */}
        <Reveal as="section" className="ed what" id="what" aria-labelledby="what-h">
          <div className="wrap">
            <EdHead
              n="01"
              tate="仕組み"
              eyebrow="คืออะไร"
              id="what-h"
              title="เครื่องมือสำหรับจัดงานแปลยาว ๆ ในเทอร์มินัล"
              lead="ไม่ได้แทนที่คนตรวจแปล แต่ช่วยจัดไฟล์ บริบท และรอบแปลให้ตามงานง่ายขึ้น เมื่อมีหลายบทหลายเล่ม"
            />
            <div className="what-grid">
              <div className="what-text">
                <p className="big">
                  เริ่มจาก EPUB แล้วได้โฟลเดอร์ที่อ่าน แก้ และ diff ได้ — ต้นฉบับญี่ปุ่นใน{' '}
                  <code>raw/</code> ผลแปลไทยใน <code>translated/</code>
                </p>
                <p>
                  ขั้นตอนนำเข้าจัดบทตามสันหนังสือ ย้ายภาพ และล้าง HTML เป็น Markdown
                  จากนั้นแบ่งแต่ละบทเป็น chunk ราว 1,000 โทเคน พร้อมส่งบริบทที่จำเป็น —
                  ศัพท์ ตัวละคร สไตล์ และข้อความก่อนหน้า
                </p>
                <p>
                  honya ไม่มีโมเดลในตัว ต้องใช้ <b>API ที่รองรับ OpenRouter</b>{' '}
                  เปิดครั้งแรกจะถามคีย์แล้วเก็บไว้ที่ <code>~/.config/honya</code>
                </p>
              </div>
              <div className="what-vis">
                <div className="layout-card" aria-label="โครงสร้างโปรเจกต์บนดิสก์">
                  <div className="lc-head">
                    <span>โครงสร้างโปรเจกต์</span>
                    <span aria-hidden="true">บนดิสก์</span>
                  </div>
                  <div className="tree-disk">
                    <div>
                      <span className="d">my-novel/</span>
                    </div>
                    <div>
                      <span className="ind">├─ </span>
                      <span className="f">PROJECT.md</span>{' '}
                      <span className="c"># สรุปและสถานะ</span>
                    </div>
                    <div>
                      <span className="ind">├─ </span>
                      <span className="f">CHARACTERS.md</span>
                    </div>
                    <div>
                      <span className="ind">├─ </span>
                      <span className="f">GLOSSARY.md</span>
                    </div>
                    <div>
                      <span className="ind">├─ </span>
                      <span className="f">STYLE.md</span>
                    </div>
                    <div>
                      <span className="ind">├─ </span>
                      <span className="d">images/</span>
                    </div>
                    <div>
                      <span className="ind">└─ </span>
                      <span className="d">Vol_01/</span>
                    </div>
                    <div>
                      <span className="ind">   ├─ </span>
                      <span className="f">VOLUME.md</span>
                    </div>
                    <div>
                      <span className="ind">   ├─ </span>
                      <span className="d">raw/</span>{' '}
                      <span className="c"># JA ที่ล้างแล้ว</span>
                    </div>
                    <div>
                      <span className="ind">   └─ </span>
                      <span className="d">translated/</span>{' '}
                      <span className="c"># ผลลัพธ์ TH</span>
                    </div>
                  </div>
                </div>
              </div>
            </div>
          </div>
        </Reveal>

        {/* ───── 02 · FIVE SCREENS ───── */}
        <Reveal as="section" className="ed" id="screens" aria-labelledby="screens-h">
          <div className="wrap">
            <EdHead
              n="02"
              tate="五画面"
              eyebrow="ห้าหน้าจอ"
              id="screens-h"
              title="ห้าหน้าจอสำหรับนำเข้า แปล ตรวจ และแก้บริบท"
              lead={
                <>
                  กด <span className="kbd">1</span>–<span className="kbd">5</span>{' '}
                  ในแอปเพื่อสลับมุมมอง แต่ละหน้าจอมีงานหลักของตัวเอง —
                  เลือกแท็บเพื่อดูตัวอย่าง
                </>
              }
            />
            <ScreenTabs />
            <div
              className="moon-legend"
              role="img"
              aria-label="คำอธิบายสถานะบทรูปดวงจันทร์ข้างขึ้น: วงกลมว่างคือรอดำเนินการ; จันทร์เสี้ยว ครึ่งดวง และค่อนดวงคือขั้นตอนกำลังทำงาน; วงกลมทึบคือเสร็จสิ้น; ธงคือผนวกแล้วแต่ยังต้องตรวจทาน; กากบาทคือล้มเหลว"
            >
              <span className="lbl">สถานะ</span>
              <span className="mi">
                <span className="g pending ja" aria-hidden="true">
                  ○
                </span>{' '}
                รอดำเนินการ
              </span>
              <span className="mi">
                <span className="g work ja" aria-hidden="true">
                  ◔
                </span>
                <span className="g work ja" aria-hidden="true">
                  ◐
                </span>
                <span className="g work ja" aria-hidden="true">
                  ◑
                </span>
                <span className="g work ja" aria-hidden="true">
                  ◕
                </span>{' '}
                กำลังทำงาน
              </span>
              <span className="mi">
                <span className="g done ja" aria-hidden="true">
                  ●
                </span>{' '}
                เสร็จสิ้น
              </span>
              <span className="mi">
                <span className="g review ja" aria-hidden="true">
                  ⚑
                </span>{' '}
                ต้องตรวจทาน
              </span>
              <span className="mi">
                <span className="g fail ja" aria-hidden="true">
                  ✗
                </span>{' '}
                ล้มเหลว
              </span>
            </div>
          </div>
        </Reveal>

        {/* ───── 03 · PIPELINE ───── */}
        <Reveal as="section" className="ed pipeline" id="pipeline" aria-labelledby="pipe-h">
          <div className="wrap">
            <EdHead
              n="03"
              tate="翻訳"
              eyebrow="วิธีแปล"
              id="pipe-h"
              title="แปลทีละ Chunk พร้อมบริบทที่จำเป็น"
              lead="แต่ละบทแบ่งเป็น chunk ราว 1,000 โทเคน — Orchestrator เตรียมบริบท, Translator ร่าง, Reviewer ตรวจ ก่อนเขียนลงไฟล์"
            />
            <div className="pipe-wrap">
              <PipelineDiagram />
              <div className="pipe-steps">
                <div className="pstep">
                  <span className="role">
                    <span className="ja" lang="ja">
                      指揮
                    </span>{' '}
                    Orchestrator
                  </span>
                  <h4>จัดเตรียมทุกอย่าง</h4>
                  <p>
                    เลือกบริบทที่ต้องส่งให้โมเดล — ศัพท์ ตัวละคร สไตล์ บทสรุป
                    และประโยคไทยท้าย chunk ก่อนหน้า
                  </p>
                </div>
                <div className="pstep">
                  <span className="role">
                    <span className="ja" lang="ja">
                      訳者
                    </span>{' '}
                    Translator
                  </span>
                  <h4>ร่างภาษาไทย</h4>
                  <p>
                    ร่างคำแปลของ chunk ปัจจุบัน สตรีมข้อความกลับมาให้เห็นระหว่างรัน
                  </p>
                </div>
                <div className="pstep">
                  <span className="role">
                    <span className="ja" lang="ja">
                      校正
                    </span>{' '}
                    Reviewer
                  </span>
                  <h4>อนุมัติหรือส่งกลับ</h4>
                  <p>
                    ตรวจความตรงต้นฉบับในรอบอัตโนมัติ ถ้าไม่ผ่าน ฟีดแบ็กกลับไปให้
                    Translator ลองใหม่
                  </p>
                </div>
              </div>
              <div className="pipe-note">
                อนุมัติแล้วแอปจะ
                <b>ผนวกคำแปลและบันทึกศัพท์ ตัวละคร หรือโน้ตใหม่</b> ลงไฟล์ ถ้าปฏิเสธ
                ยังไม่เขียนผล — <span className="vermilion">ฟีดแบ็ก</span> กลับไปให้
                Translator จนกว่าจะผ่าน
              </div>
            </div>
          </div>
        </Reveal>

        {/* ───── 04 · INSTALL ───── */}
        <Reveal as="section" className="ed install" id="install" aria-labelledby="install-h">
          <div className="wrap">
            <InstallCard />
          </div>
        </Reveal>

        {/* ───── FEATURES ───── */}
        <Reveal as="section" className="ed feats" aria-labelledby="feat-h">
          <div className="wrap">
            <EdHead
              n="05"
              tate="要点"
              eyebrow="สิ่งที่ควรรู้"
              id="feat-h"
              title="honya ช่วยจัดงาน ไม่ได้สัญญาว่าคำแปลสมบูรณ์"
              lead="ผลลัพธ์ยังควรอ่านตรวจเอง โดยเฉพาะชื่อเฉพาะ น้ำเสียง และประโยคที่พึ่งพาบริบทมาก"
            />
            <div className="feat-grid">
              {FEATURES.map((f, i) => (
                <div className="feat" key={f.title}>
                  <span className="feat-idx" aria-hidden="true">
                    {String(i + 1).padStart(2, '0')}
                  </span>
                  <span className="feat-kanji ja" lang="ja" aria-hidden="true">
                    {f.kanji}
                  </span>
                  <h3>{f.title}</h3>
                  <p>{f.body}</p>
                </div>
              ))}
            </div>
          </div>
        </Reveal>

        {/* ───── TRUST ───── */}
        <div className="trust">
          <div className="wrap">
            <div className="trust-row">
              {TRUST.map(([num, label]) => (
                <Reveal className="trust-item" key={label}>
                  <div className="tnum">{num}</div>
                  <div className="tlbl">{label}</div>
                </Reveal>
              ))}
            </div>
          </div>
        </div>

        {/* ───── FAQ ───── */}
        <Reveal as="section" className="ed" aria-labelledby="faq-h">
          <div className="wrap">
            <EdHead
              n="06"
              tate="質問"
              eyebrow="คำถามที่พบบ่อย"
              id="faq-h"
              title="FAQ"
              lead="สั้น ๆ เกี่ยวกับคีย์ ความเป็นส่วนตัว ไฟล์ที่รองรับ และคุณภาพคำแปล"
            />
            <div className="faq-grid">
              {FAQ.map((item) => (
                <details className="faq-item" key={item.q}>
                  <summary>
                    <span className="qmark" aria-hidden="true">
                      Q
                    </span>
                    {item.q}
                    <ChevronIcon className="chev" />
                  </summary>
                  <div className="faq-a">{item.a}</div>
                </details>
              ))}
            </div>
          </div>
        </Reveal>

        {/* ───── END BAND ───── */}
        <section className="endband" aria-labelledby="end-h">
          <div className="wrap">
            <div className="endband-tate ja" lang="ja" aria-hidden="true">
              読み継ぐ
            </div>
            <div className="endband-body">
              <span className="eyebrow">ลองใช้งาน</span>
              <h2 id="end-h">
                นำเข้า EPUB แล้วตรวจงานแปลในที่เดียว{' '}
                <span className="ja" lang="ja">
                  本屋
                </span>
              </h2>
              <p>
                เหมาะกับคนที่ต้องการโปรเจกต์แปลเป็นไฟล์ อ่าน diff ได้ และยอมตรวจคำแปลเอง
                ไม่ใช่บริการแปลสำเร็จรูป
              </p>
              <div className="cta-row">
                <a className="btn btn-primary" href="#install">
                  <ArrowIcon />
                  ติดตั้ง
                </a>
                <a className="btn btn-ghost" href={GITHUB_URL} rel="noopener">
                  <GitHubIcon />
                  ดูซอร์สโค้ด
                </a>
              </div>
            </div>
          </div>
        </section>
      </main>
      <Footer page="home" />
    </>
  )
}
