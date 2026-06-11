import { createFileRoute } from '@tanstack/react-router'
import { Header } from '../components/Header'
import { Footer } from '../components/Footer'
import { GitHubIcon } from '../components/icons'
import { RELEASES, TAG_LABEL } from '../data/changelog'
import { GITHUB_URL, SITE_URL, VERSION } from '../data/site'

export const Route = createFileRoute('/changelog')({
  head: () => ({
    meta: [
      { title: 'honya 本屋 — บันทึกการเปลี่ยนแปลง (Changelog)' },
      {
        name: 'description',
        content:
          'บันทึกการเปลี่ยนแปลงของ honya (本屋): สิ่งที่เพิ่ม ปรับ และแก้ในแต่ละเวอร์ชัน เรียงจากรีลีสล่าสุดไปยังรีลีสแรก',
      },
      { property: 'og:title', content: 'honya — บันทึกการเปลี่ยนแปลง' },
      {
        property: 'og:description',
        content:
          'รายการเปลี่ยนแปลงของ honya ในแต่ละเวอร์ชัน ตั้งแต่ฟีเจอร์ใหม่ไปจนถึงบั๊กที่แก้แล้ว',
      },
      { property: 'og:url', content: `${SITE_URL}/changelog` },
    ],
    links: [{ rel: 'canonical', href: `${SITE_URL}/changelog` }],
  }),
  component: Changelog,
})

function Changelog() {
  return (
    <>
      <Header page="changelog" />
      <main id="main">
        <section className="log-hero" aria-labelledby="log-h">
          <div className="wrap">
            <span className="eyebrow">บันทึกการเปลี่ยนแปลง</span>
            <h1 id="log-h">
              บันทึกการเปลี่ยนแปลงของ{' '}
              <span className="ja" lang="ja">
                本屋
              </span>
            </h1>
            <p>
              หน้านี้สรุปสิ่งที่เปลี่ยนในแต่ละเวอร์ชัน เรียงจากใหม่ไปเก่า
              เลขเวอร์ชันอ้างอิงจาก <code>Cargo.toml</code>
            </p>
            <span className="latest-pill">
              <span className="dot" aria-hidden="true" />
              เวอร์ชันล่าสุด <b>{VERSION}</b>
            </span>
          </div>
        </section>

        <hr className="divider" />

        <section className="log" aria-label="รายการเวอร์ชัน">
          <div className="wrap">
            {RELEASES.map((rel) => (
              <article className="release" key={rel.version}>
                <div className="rel-meta">
                  <span className="ver">
                    <span className="v-pre">v</span>
                    {rel.version}
                  </span>
                  <div className="rel-date">{rel.date}</div>
                  {rel.badge === 'latest' ? (
                    <span className="rel-badge">ล่าสุด</span>
                  ) : null}
                  {rel.badge === 'first' ? (
                    <span className="rel-badge first">รีลีสแรก</span>
                  ) : null}
                </div>
                <ul className="changes">
                  {rel.changes.map((c, i) => (
                    <li className="change" key={i}>
                      <span className={`tag ${c.tag}`}>{TAG_LABEL[c.tag]}</span>
                      <span
                        className="txt"
                        dangerouslySetInnerHTML={{ __html: c.html }}
                      />
                    </li>
                  ))}
                </ul>
              </article>
            ))}

            <div className="log-foot">
              <span>ดูไฟล์และรายละเอียดของแต่ละรีลีสได้ที่ GitHub</span>
              <a href={`${GITHUB_URL}/releases`} rel="noopener">
                <GitHubIcon />
                รีลีสทั้งหมดบน GitHub
              </a>
            </div>
          </div>
        </section>
      </main>
      <Footer page="changelog" />
    </>
  )
}
