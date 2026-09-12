import { describe, expect, test } from 'bun:test'
import { normalizeDaemonAddress, validateConnectionConfig } from './connection'

describe('normalizeDaemonAddress', () => {
  test('normalizes host, HTTP, and daemon paths', () => {
    expect(normalizeDaemonAddress('host.example:34123')).toBe(
      'ws://host.example:34123',
    )
    expect(normalizeDaemonAddress('https://fintwind.example/v1?token=nope')).toBe(
      'wss://fintwind.example',
    )
    expect(normalizeDaemonAddress('HTTP://FINTWIND.EXAMPLE/v1')).toBe(
      'ws://fintwind.example',
    )
  })

  test('rejects unsupported schemes and credentials', () => {
    expect(() => normalizeDaemonAddress('ftp://fintwind.example')).toThrow()
    expect(() => normalizeDaemonAddress('ws://token@fintwind.example')).toThrow()
  })

  test('requires a token without putting it in the address', () => {
    expect(() =>
      validateConnectionConfig({ address: 'fintwind.example', token: '  ' }),
    ).toThrow('token')
    expect(
      validateConnectionConfig({ address: 'fintwind.example', token: 'secret' }),
    ).toEqual({
      address: 'ws://fintwind.example',
      token: 'secret',
      remember: false,
    })
  })
})
