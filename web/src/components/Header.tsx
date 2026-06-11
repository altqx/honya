import { Link } from '@tanstack/react-router'
import { Brand } from './Brand'
import { GitHubIcon } from './icons'
import { useScrolled } from '../hooks/useScrolled'
import { GITHUB_URL, VERSION } from '../data/site'

/**
 * Sticky site header. On the home page the section links are in-page anchors;
 * on other pages they point back to the home route's anchors.
 */
export function Header({ page }: { page: 'home' | 'changelog' }) {
  const scrolled = useScrolled()
  const base = page === 'home' ? '' : '/'

  return (
    <header className={`site${scrolled ? ' scrolled' : ''}`} id="top">
      <nav className="nav" aria-label="เมนูหลัก">
        <Brand />
        <span className="nav-edge" aria-hidden="true">
          <span className="ja" lang="ja">
            JA → TH
          </span>{' '}
          翻訳
        </span>
        <div className="nav-links">
          <Link className="nav-ver" to="/changelog" aria-label={`เวอร์ชัน ${VERSION}`}>
            {VERSION}
          </Link>
          <a className="lnk" href={`${base}#what`}>
            คืออะไร
          </a>
          <a className="lnk" href={`${base}#screens`}>
            หน้าจอ
          </a>
          <a className="lnk" href={`${base}#pipeline`}>
            ไปป์ไลน์
          </a>
          {page === 'home' ? (
            <a className="lnk" href="#install">
              ติดตั้ง
            </a>
          ) : null}
          <Link
            className="lnk"
            to="/changelog"
            aria-current={page === 'changelog' ? 'page' : undefined}
          >
            {page === 'changelog' ? 'บันทึกการเปลี่ยนแปลง' : 'บันทึก'}
          </Link>
          <a className="nav-cta" href={GITHUB_URL} rel="noopener">
            <GitHubIcon />
            GitHub
          </a>
        </div>
      </nav>
    </header>
  )
}
