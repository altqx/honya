import { Link } from '@tanstack/react-router'
import { MoonMark } from './icons'

export function Brand({ ariaLabel }: { ariaLabel?: string }) {
  return (
    <Link className="brand" to="/" aria-label={ariaLabel ?? 'honya — หน้าแรก'}>
      <span className="mark" aria-hidden="true">
        <MoonMark />
      </span>
      <span className="name">
        <span className="ja" lang="ja">
          本屋
        </span>
        honya
      </span>
    </Link>
  )
}
