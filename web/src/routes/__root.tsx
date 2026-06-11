import { HeadContent, Scripts, createRootRoute } from '@tanstack/react-router'
import appCss from '../styles/app.css?url'

const FONTS_URL =
  'https://fonts.googleapis.com/css2?family=Zen+Kaku+Gothic+New:wght@400;500;700;900&family=Noto+Serif+JP:wght@500;600;700&family=Noto+Sans+Thai+Looped:wght@100..900&family=JetBrains+Mono:wght@400;500;700&family=Noto+Sans+Symbols+2&display=swap'

const FAVICON =
  "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 64 64'%3E%3Crect width='64' height='64' rx='12' fill='%23F3EFE6'/%3E%3Ccircle cx='32' cy='32' r='17' fill='none' stroke='%233A5078' stroke-width='3'/%3E%3Cpath d='M32 15 a17 17 0 0 1 0 34 a11 17 0 0 0 0 -34' fill='%233A5078'/%3E%3C/svg%3E"

export const Route = createRootRoute({
  head: () => ({
    meta: [
      { charSet: 'utf-8' },
      {
        name: 'viewport',
        content: 'width=device-width, initial-scale=1, viewport-fit=cover',
      },
      { name: 'color-scheme', content: 'light' },
      { name: 'theme-color', content: '#F3EFE6' },
      { property: 'og:type', content: 'website' },
      { property: 'og:site_name', content: 'honya 本屋' },
      { property: 'og:locale', content: 'th_TH' },
      { name: 'twitter:card', content: 'summary_large_image' },
    ],
    links: [
      { rel: 'preconnect', href: 'https://fonts.googleapis.com' },
      {
        rel: 'preconnect',
        href: 'https://fonts.gstatic.com',
        crossOrigin: 'anonymous',
      },
      { rel: 'stylesheet', href: FONTS_URL },
      { rel: 'icon', href: FAVICON },
      { rel: 'stylesheet', href: appCss },
    ],
  }),
  shellComponent: RootDocument,
})

function RootDocument({ children }: { children: React.ReactNode }) {
  return (
    <html lang="th" suppressHydrationWarning>
      <head>
        {/* Mark JS-capable before paint so the scroll-reveal enhancement only
            hides content when it can actually be animated back in. */}
        <script
          dangerouslySetInnerHTML={{
            __html: "document.documentElement.classList.add('js')",
          }}
        />
        <HeadContent />
      </head>
      <body>
        <a className="skip" href="#main">
          ข้ามไปยังเนื้อหา
        </a>
        {children}
        <Scripts />
      </body>
    </html>
  )
}
