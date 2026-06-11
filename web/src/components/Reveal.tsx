import { useEffect, useRef, useState } from 'react'
import type { ElementType, ReactNode } from 'react'

/**
 * Fades + lifts its children in once they scroll into view. Honors
 * prefers-reduced-motion via the .reveal CSS (it ships visible there).
 */
export function Reveal({
  as,
  children,
  className,
  delay = 0,
  ...rest
}: {
  as?: ElementType
  children: ReactNode
  className?: string
  delay?: number
  [key: string]: unknown
}) {
  const Tag = (as ?? 'div') as ElementType
  const ref = useRef<HTMLElement | null>(null)
  const [shown, setShown] = useState(false)

  useEffect(() => {
    const el = ref.current
    if (!el || shown) return
    if (!('IntersectionObserver' in window)) {
      setShown(true)
      return
    }
    const io = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (e.isIntersecting) {
            setShown(true)
            io.disconnect()
            break
          }
        }
      },
      { threshold: 0.12, rootMargin: '0px 0px -8% 0px' },
    )
    io.observe(el)
    return () => io.disconnect()
  }, [shown])

  return (
    <Tag
      ref={ref}
      className={`reveal${shown ? ' in' : ''}${className ? ` ${className}` : ''}`}
      style={delay ? { transitionDelay: `${delay}ms` } : undefined}
      {...rest}
    >
      {children}
    </Tag>
  )
}
