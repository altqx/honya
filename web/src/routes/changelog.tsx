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
      { property: 'og:image', content: `${SITE_URL}/og.png` },
      { property: 'og:image:width', content: '1200' },
      { property: 'og:image:height', content: '630' },
      { property: 'og:image:type', content: 'image/png' },
      { name: 'twitter:image', content: `${SITE_URL}/og.png` },
    ],
    links: [{ rel: 'canonical', href: `${SITE_URL}/changelog` }],
  }),
  component: Changelog,
})

function Changelog() {
  const total = RELEASES.length
  return (
    <>
      <Header page="changelog" />
      <main id="main">
        <section className="log-hero cover" aria-labelledby="log-h">
          <div className="wrap">
            <div className="cover-masthead" aria-hidden="true">
              <span className="cm-l">
                <span className="ja" lang="ja">
                  本屋
                </span>{' '}
                — 変更履歴 / Changelog
              </span>
              <span className="cm-r">{total} releases · {VERSION}</span>
            </div>

            <div className="cover-grid">
              <div className="cover-tate" aria-hidden="true">
                <span className="ja" lang="ja">
                  更新の記録。
                </span>
              </div>

              <div className="cover-main">
                <span className="eyebrow">บันทึกการเปลี่ยนแปลง</span>
                <h1 id="log-h">
                  บันทึกการเปลี่ยนแปลงของ{' '}
                  <span className="wordmark ja" lang="ja">
                    本屋
                  </span>
                </h1>
                <p className="lede">
                  หน้านี้สรุปสิ่งที่เปลี่ยนในแต่ละเวอร์ชัน เรียงจากใหม่ไปเก่า
                  เลขเวอร์ชันอ้างอิงจาก <code>Cargo.toml</code>
                </p>
                <span className="latest-pill">
                  <span className="dot" aria-hidden="true" />
                  เวอร์ชันล่าสุด <b>{VERSION}</b>
                </span>
              </div>
            </div>
          </div>
        </section>

        <section className="log" aria-label="รายการเวอร์ชัน">
          <div className="wrap">
            {RELEASES.map((rel, i) => (
              <article className="release" key={rel.version}>
                <div className="rel-meta">
                  <span className="rel-issue" aria-hidden="true">
                    号 {String(total - i).padStart(2, '0')}
                  </span>
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
                  {rel.changes.map((c, j) => (
                    <li className="change" key={j}>
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
