import { Outlet, createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/app')({
  head: () => ({
    meta: [
      { title: 'honya 本屋 — รีโมตคอนโทรล' },
      { name: 'robots', content: 'noindex' },
    ],
  }),
  component: () => <Outlet />,
})
