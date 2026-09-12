import { lazy, Suspense, useEffect } from 'react'
import { ConnectionPanel } from '@/components/connection-panel'
import { StartupScreen } from '@/components/startup-screen'
import { useDaemon } from '@/lib/daemon-context'

const loadConnectedApp = () => import('@/components/fintwind-app')
const ConnectedFintwindApp = lazy(() => loadConnectedApp().then((module) => ({
  default: module.FintwindApp,
})))

export function FintwindShell() {
  const { config, phase } = useDaemon()

  useEffect(() => {
    if (phase === 'connecting' && config) void loadConnectedApp()
  }, [config, phase])

  if (phase !== 'connected') return <ConnectionPanel />
  return (
    <Suspense fallback={<StartupScreen />}>
      <ConnectedFintwindApp />
    </Suspense>
  )
}
