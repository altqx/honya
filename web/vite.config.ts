import { defineConfig } from 'vite'
import { tanstackStart } from '@tanstack/react-start/plugin/vite'
import viteReact from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import { nitro } from 'nitro/vite'

// Marketing site: every route is fully prerendered to static HTML, so the
// build output (.output/public) is a self-contained static site that drops
// straight onto Cloudflare Pages alongside install.sh / _headers / _redirects.
const config = defineConfig({
  resolve: { tsconfigPaths: true },
  plugins: [
    nitro(),
    tailwindcss(),
    tanstackStart({
      prerender: { enabled: true, crawlLinks: true },
      pages: [{ path: '/' }, { path: '/changelog' }],
    }),
    viteReact(),
  ],
})

export default config
