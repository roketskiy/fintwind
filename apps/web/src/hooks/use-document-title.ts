import { useEffect } from 'react'

export const FINTWIND_DOCUMENT_TITLE = 'Fintwind Web'

export function formatDocumentTitle(section?: string | null): string {
  const normalized = section?.trim()
  if (!normalized || normalized === FINTWIND_DOCUMENT_TITLE) return FINTWIND_DOCUMENT_TITLE
  return `${normalized} — ${FINTWIND_DOCUMENT_TITLE}`
}

export function useDocumentTitle(section?: string | null) {
  const title = formatDocumentTitle(section)
  useEffect(() => {
    document.title = title
  }, [title])
}
