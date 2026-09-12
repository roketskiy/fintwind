import { describe, expect, test } from 'bun:test'
import {
  formatDocumentTitle,
  FINTWIND_DOCUMENT_TITLE,
} from './use-document-title'

describe('formatDocumentTitle', () => {
  test('uses the product title without a section', () => {
    expect(formatDocumentTitle()).toBe(FINTWIND_DOCUMENT_TITLE)
    expect(formatDocumentTitle('   ')).toBe(FINTWIND_DOCUMENT_TITLE)
  })

  test('identifies the current browser surface', () => {
    expect(formatDocumentTitle('New Task')).toBe('New Task — Fintwind Web')
    expect(formatDocumentTitle('  General  ')).toBe('General — Fintwind Web')
  })

  test('does not duplicate the product title', () => {
    expect(formatDocumentTitle(FINTWIND_DOCUMENT_TITLE)).toBe(FINTWIND_DOCUMENT_TITLE)
  })
})
