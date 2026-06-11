import { Link } from '@tanstack/react-router'
import { Brand } from './Brand'
import { GITHUB_URL } from '../data/site'

export function Footer({ page }: { page: 'home' | 'changelog' }) {
  const base = page === 'home' ? '' : '/'
  return (
    <footer
      className={`site${page === 'changelog' ? ' bordered' : ''}`}
      aria-labelledby="foot-h"
    >
      <h2 id="foot-h" className="vh">
        ส่วนท้ายเว็บไซต์
      </h2>
      <div className="wrap">
        <div className="foot-top">
          <div className="foot-brand">
            <Brand />
            <p>
              TUI สำหรับจัดโปรเจกต์แปลไลต์โนเวลญี่ปุ่นเป็นไทยผ่าน API ที่รองรับ
              OpenRouter สร้างด้วย Rust และ Ratatui
            </p>
          </div>

          <div className="foot-cols">
            <div className="foot-col">
              <h4>สำรวจ</h4>
              <a href={`${base}#what`}>คืออะไร</a>
              <a href={`${base}#screens`}>ห้าหน้าจอ</a>
              <a href={`${base}#pipeline`}>ไปป์ไลน์</a>
              <Link to="/changelog">บันทึกการเปลี่ยนแปลง</Link>
              <a href={`${base}#install`}>ติดตั้ง</a>
            </div>
            <div className="foot-col">
              <h4>โปรเจกต์</h4>
              <a href={GITHUB_URL} rel="noopener">
                คลัง GitHub
              </a>
              <a href={`${GITHUB_URL}#readme`} rel="noopener">
                อ่าน README
              </a>
              <a href={`${GITHUB_URL}/issues`} rel="noopener">
                รายงานปัญหา
              </a>
            </div>
            <div className="foot-col">
              <h4>เว็บไซต์</h4>
              <a href="https://honya.altqx.com">honya.altqx.com</a>
              <Link to="/changelog">บันทึกการเปลี่ยนแปลง</Link>
              {page === 'home' ? <a href="#install">install.sh</a> : null}
            </div>
          </div>
        </div>

        <div className="foot-bottom">
          <span>
            <span className="ja" lang="ja">
              本屋
            </span>{' '}
            — TUI สำหรับโปรเจกต์แปล JA→TH ฟรีและโอเพนซอร์ส
          </span>
          <span>
            <a href={GITHUB_URL} rel="noopener">
              github.com/altqx/honya
            </a>
          </span>
        </div>
      </div>
    </footer>
  )
}
