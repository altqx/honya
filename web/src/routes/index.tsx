import { createFileRoute } from '@tanstack/react-router'
import { Header } from '../components/Header'
import { Footer } from '../components/Footer'
import { Terminal } from '../components/Terminal'
import { ScreenTabs } from '../components/ScreenTabs'
import { PipelineDiagram } from '../components/PipelineDiagram'
import { InstallCard } from '../components/InstallCard'
import { Reveal } from '../components/Reveal'
import { ArrowIcon, ChevronIcon, GitHubIcon } from '../components/icons'
import { GITHUB_URL, SITE_URL } from '../data/site'

const OG_IMAGE =
  "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='1200' height='630'%3E%3Crect width='1200' height='630' fill='%23F3EFE6'/%3E%3Crect x='40' y='40' width='1120' height='550' rx='14' fill='%23ECE7DC' stroke='%23CEC6B8'/%3E%3Ctext x='90' y='300' font-family='serif' font-size='180' fill='%232D2A26'%3E%E6%9C%AC%E5%B1%8B%3C/text%3E%3Ctext x='96' y='370' font-family='monospace' font-size='38' fill='%235C564E'%3Ehonya — JA%E2%86%92TH light-novel translation%3C/text%3E%3Ctext x='96' y='430' font-family='monospace' font-size='30' fill='%233A5078'%3EOrchestrator %C2%B7 Translator %C2%B7 Reviewer%3C/text%3E%3C/svg%3E"

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

const FEATURES: Array<{ icon: React.ReactNode; title: string; body: React.ReactNode }> = [
  {
    icon: (
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
        <path d="M12 2v4M12 18v4M4.9 4.9l2.9 2.9M16.2 16.2l2.9 2.9M2 12h4M18 12h4M4.9 19.1l2.9-2.9M16.2 7.8l2.9-2.9" />
      </svg>
    ),
    title: 'ใช้คีย์ของคุณเอง',
    body: (
      <>
        honya ทำงานกับ API ที่รองรับ OpenRouter ระบบจะขอคีย์เมื่อเปิดครั้งแรกและบันทึกไว้ในเครื่อง
        ที่ <code>~/.config/honya</code> — หรือจะอ่าน <code>HONYA_API_KEY</code> จากตัวแปร
        สภาพแวดล้อมก็ได้ ไม่มีบัญชีของ honya; คำขอแปลยังถูกส่งไปยัง provider ที่คุณเลือก
      </>
    ),
  },
  {
    icon: (
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
        <circle cx="6" cy="6" r="2.4" />
        <circle cx="18" cy="6" r="2.4" />
        <circle cx="12" cy="18" r="2.4" />
        <path d="M8.4 6h7.2M7.2 8 11 15.6M16.8 8 13 15.6" />
      </svg>
    ),
    title: 'ไปป์ไลน์สามเอเจนต์',
    body: (
      <>
        Orchestrator, Translator และ Reviewer แบ่งงานกันทีละ chunk เป็นวงจรร่าง ตรวจ
        และลองใหม่ก่อนที่ผลแปลจะถูกบันทึกลงดิสก์
      </>
    ),
  },
  {
    icon: (
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
        <path d="M4 4h11l5 5v11a0 0 0 0 1 0 0H4a0 0 0 0 1 0 0Z" />
        <path d="M15 4v5h5M8 13h8M8 17h6" />
      </svg>
    ),
    title: 'การเตรียมข้อมูล EPUB',
    body: (
      <>
        จัดบทตามลำดับสันหนังสือ ย้ายภาพประกอบ และล้าง HTML ให้เป็น Markdown
        เพื่อให้โมเดลอ่านเนื้อหา ไม่ใช่มาร์กอัปจาก EPUB
      </>
    ),
  },
  {
    icon: (
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
        <rect x="3" y="4" width="18" height="16" rx="2" />
        <path d="M12 4v16M6 9h3M6 13h3M15 9h3M15 13h3" />
      </svg>
    ),
    title: 'โหมดอ่านเทียบคู่',
    body: (
      <>
        พิสูจน์อักษร JA→TH แบบซิงก์กัน โดยล็อกคอลัมน์ให้ตรงกันทีละย่อหน้า
        เพื่อให้สายตาคุณไม่หลงตำแหน่งระหว่างการตรวจ
      </>
    ),
  },
  {
    icon: (
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
        <path d="M4 19.5V6a2 2 0 0 1 2-2h11v16H6a2 2 0 0 0-2 2Z" />
        <path d="M9 8h5M9 11h5" />
      </svg>
    ),
    title: 'ไฟล์บริบทที่แก้เองได้',
    body: (
      <>
        GLOSSARY, CHARACTERS และโน้ตถูกอัปเดตผ่าน tool calls ระหว่างแปล
        คุณยังเปิดไฟล์เหล่านี้มาแก้เองได้เมื่อคำศัพท์หรือชื่อตัวละครต้องล็อกให้ชัด
      </>
    ),
  },
  {
    icon: (
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
        <path d="M3 7l9-4 9 4-9 4-9-4Z" />
        <path d="M3 7v6l9 4 9-4V7" />
      </svg>
    ),
    title: 'ทุกอย่างเป็นเพียงไฟล์',
    body: (
      <>
        PROJECT.md, GLOSSARY.md, STYLE.md และโฟลเดอร์แยกตามเล่ม — อ่านได้ เทียบ diff ได้
        และเป็นของคุณ แก้ด้วยมือเมื่อไหร่ก็ได้ honya จะทำงานต่อจากจุดที่คุณค้างไว้
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

const FAQ: Array<{ q: string; a: React.ReactNode }> = [
  {
    q: 'ต้องมีคีย์ API ไหม และใช้ผู้ให้บริการเจ้าไหน?',
    a: (
      <>
        ต้องมี honya ไม่ได้มาพร้อมโมเดลในตัว แต่ทำงานกับ API ที่รองรับ OpenRouter
        เมื่อเปิดครั้งแรกโปรแกรมจะถามคีย์ของคุณและบันทึกไว้ที่ <code>~/.config/honya</code>{' '}
        หรือจะตั้งผ่านตัวแปร <code>HONYA_API_KEY</code> เองก็ได้
        คุณเลือกโมเดลแยกได้สำหรับแต่ละเอเจนต์
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
        honya ช่วย<b>จัดงาน</b>แปล ไม่ได้รับประกันคำแปลที่สมบูรณ์ ไปป์ไลน์มีรอบตรวจ
        (Reviewer) และการตรวจสอบในเครื่องอยู่แล้ว แต่ผลลัพธ์ยังควรอ่านตรวจเอง
        โดยเฉพาะชื่อเฉพาะ น้ำเสียง และประโยคที่พึ่งพาบริบทมาก — โหมด Reader
        แบบเทียบคู่มีไว้เพื่อการนี้
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
        {/* HERO */}
        <section className="hero" aria-labelledby="hero-h">
          <div className="wrap">
            <div className="hero-grid">
              <div className="hero-copy">
                <span className="eyebrow">Ratatui TUI · Rust · ใช้คีย์ของคุณเอง</span>
                <h1 id="hero-h">
                  แปลไลต์โนเวลญี่ปุ่นเป็นไทย <span className="accent">JA→TH</span>{' '}
                  ในเทอร์มินัลด้วย{' '}
                  <span className="wordmark" lang="ja">
                    本屋
                  </span>
                </h1>
                <p className="hero-sub">
                  <b>honya</b> นำเข้า EPUB แล้วแตกเป็นโปรเจกต์บนดิสก์: บทที่ล้างเป็น
                  Markdown, โฟลเดอร์ภาพ, ไฟล์ศัพท์ ตัวละคร สไตล์ และคำแปลแยกตามเล่ม
                  จากนั้นเรียกโมเดลผ่าน API ที่รองรับ OpenRouter เพื่อแปลทีละ chunk
                  พร้อมหน้าจอสำหรับตรวจงานเอง
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
                <div className="hero-meta">
                  <span>
                    <span className="dot" />
                    ใช้คีย์ OpenRouter ของคุณเอง
                  </span>
                  <span>
                    <span className="dot" />
                    โปรเจกต์เป็นไฟล์ในเครื่อง
                  </span>
                  <span>
                    <span className="dot" />
                    ฟรีและโอเพนซอร์ส
                  </span>
                </div>
              </div>
              <Terminal />
            </div>
          </div>
        </section>

        {/* WHAT IT IS */}
        <Reveal as="section" className="what" id="what" aria-labelledby="what-h">
          <div className="wrap">
            <div className="sec-head">
              <span className="eyebrow">คืออะไร</span>
              <h2 id="what-h">เครื่องมือสำหรับจัดงานแปลยาว ๆ ในเทอร์มินัล</h2>
              <p>
                honya ไม่ได้แทนที่คนตรวจแปล แต่ช่วยจัดไฟล์ บริบท และรอบแปลให้ตามงานได้ง่ายขึ้น
                เมื่อโปรเจกต์มีหลายบทหรือหลายเล่ม
              </p>
            </div>
            <div className="what-grid">
              <div className="what-text">
                <p className="big">
                  คุณเริ่มจาก EPUB แล้วได้โฟลเดอร์โปรเจกต์ที่เปิดอ่าน แก้ และ diff ได้ —
                  ต้นฉบับญี่ปุ่นอยู่ใน <code>raw/</code> และผลแปลไทยอยู่ใน{' '}
                  <code>translated/</code>
                </p>
                <p>
                  ขั้นตอนนำเข้าจะจัดบทตามลำดับสันหนังสือ ย้ายภาพประกอบ และล้าง HTML
                  ให้เหลือ Markdown จากนั้นแต่ละบทจะถูกแบ่งเป็น chunk ขนาดประมาณ 1,000
                  โทเคน พร้อมส่งบริบทที่จำเป็น เช่น ศัพท์ ตัวละคร สไตล์ และข้อความก่อนหน้า
                  ให้โมเดลในแต่ละรอบ
                </p>
                <p>
                  honya ไม่มาพร้อมโมเดลในตัว ต้องใช้{' '}
                  <b>API ที่รองรับ OpenRouter</b> เมื่อเปิดครั้งแรกโปรแกรมจะถามคีย์และ
                  บันทึกไว้ที่ <code>~/.config/honya</code> หรือจะตั้ง{' '}
                  <code>HONYA_API_KEY</code> เองก็ได้
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

        {/* FIVE SCREENS */}
        <Reveal as="section" id="screens" aria-labelledby="screens-h">
          <div className="wrap">
            <div className="sec-head">
              <span className="eyebrow">ห้าหน้าจอ</span>
              <h2 id="screens-h">ห้าหน้าจอสำหรับนำเข้า แปล ตรวจ และแก้บริบท</h2>
              <p>
                กด <span className="kbd">1</span>–<span className="kbd">5</span>{' '}
                ในแอปเพื่อสลับมุมมอง แต่ละหน้าจอมีงานหลักของตัวเอง —
                เลือกแท็บเพื่อดูตัวอย่าง
              </p>
            </div>
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

        {/* PIPELINE */}
        <Reveal as="section" className="pipeline" id="pipeline" aria-labelledby="pipe-h">
          <div className="wrap">
            <div className="sec-head">
              <span className="eyebrow">วิธีแปล</span>
              <h2 id="pipe-h">แปลทีละ Chunk พร้อมบริบทที่จำเป็น</h2>
              <p>
                แต่ละบทถูกแบ่งเป็น chunk ขนาดประมาณ 1,000 โทเคน Orchestrator เตรียมบริบท,
                Translator ร่างคำแปล และ Reviewer ตรวจเบื้องต้นก่อนที่แอปจะเขียนผลลงไฟล์
              </p>
            </div>
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
                    เลือกข้อมูลที่ควรส่งให้โมเดลในรอบนั้น เช่น ศัพท์ ตัวละคร สไตล์ บทสรุป
                    และประโยคภาษาไทยท้าย ๆ จาก chunk ก่อนหน้า
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
                    ร่างคำแปลของ chunk ปัจจุบันและสตรีมข้อความกลับมาให้เห็นระหว่างรัน
                    เพื่อให้รู้ว่างานยังเดินอยู่
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
                    ตรวจความตรงต้นฉบับและข้อผิดพลาดชัดเจนในรอบอัตโนมัติ ถ้าไม่ผ่าน
                    ฟีดแบ็กจะถูกส่งกลับไปให้ Translator ลองใหม่
                  </p>
                </div>
              </div>
              <div className="pipe-note">
                เมื่อ Reviewer อนุมัติ แอปจะ
                <b>ผนวกคำแปลและบันทึกศัพท์ ตัวละคร หรือโน้ตใหม่</b> ลงไฟล์โปรเจกต์
                จากนั้นจึงอัปเดตบทสรุป เมื่อปฏิเสธ จะยังไม่เขียนผลแปล:{' '}
                <span className="vermilion">ฟีดแบ็กของ Reviewer</span>
                จะถูกส่งกลับไปยัง Translator จนกว่า chunk จะผ่าน
              </div>
            </div>
          </div>
        </Reveal>

        {/* INSTALL */}
        <Reveal as="section" id="install" aria-labelledby="install-h">
          <div className="wrap">
            <InstallCard />
          </div>
        </Reveal>

        <hr className="divider" />

        {/* FEATURES */}
        <Reveal as="section" aria-labelledby="feat-h">
          <div className="wrap">
            <div className="sec-head">
              <span className="eyebrow">สิ่งที่ควรรู้</span>
              <h2 id="feat-h">honya ช่วยจัดงาน ไม่ได้สัญญาว่าคำแปลสมบูรณ์</h2>
              <p>
                ผลลัพธ์ยังควรอ่านตรวจเอง โดยเฉพาะชื่อเฉพาะ น้ำเสียง
                และประโยคที่พึ่งพาบริบทมาก
              </p>
            </div>
            <div className="feat-grid">
              {FEATURES.map((f) => (
                <div className="feat" key={f.title}>
                  <span className="ficon" aria-hidden="true">
                    {f.icon}
                  </span>
                  <h3>{f.title}</h3>
                  <p>{f.body}</p>
                </div>
              ))}
            </div>
          </div>
        </Reveal>

        {/* TRUST STRIP */}
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

        {/* FAQ */}
        <Reveal as="section" aria-labelledby="faq-h">
          <div className="wrap">
            <div className="sec-head">
              <span className="eyebrow">คำถามที่พบบ่อย</span>
              <h2 id="faq-h">FAQ</h2>
              <p>สั้น ๆ เกี่ยวกับคีย์ ความเป็นส่วนตัว ไฟล์ที่รองรับ และคุณภาพคำแปล</p>
            </div>
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

        {/* END BAND */}
        <section className="endband" aria-labelledby="end-h">
          <div className="wrap">
            <span className="eyebrow">ลองใช้งาน</span>
            <h2 id="end-h">นำเข้า EPUB แล้วตรวจงานแปลในที่เดียว</h2>
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
        </section>
      </main>
      <Footer page="home" />
    </>
  )
}
