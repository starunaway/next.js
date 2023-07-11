import type { Metadata } from 'next'
import Link from 'next/link'

export function generateMetadata(): Metadata {
  return {
    title: '/nested',
    description: 'desc',
    authors: {
      name: 'grzegorz',
      url: 'https://linkedin.com/in/grzegorz-pokorski',
    },
  }
}

export default function Page() {
  return (
    <div>
      <h1>current: /nested</h1>
      <Link href="/navigation">Link to / page</Link>
      <br />
      <Link href="/navigation/nested/inner">Link to /nested/route page</Link>
    </div>
  )
}
