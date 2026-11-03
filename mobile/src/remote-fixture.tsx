import { useEffect } from "react";
import { createRoot } from "react-dom/client";
import { RemoteView } from "./app";
import { useRemote } from "./hooks/use-remote";
import { fixture } from "./fixtures/remote-client";
import "./index.css";

localStorage.setItem("fintwind-project", "fixture-project");
localStorage.setItem("fintwind-session", "fixture-session");
localStorage.setItem("fintwind-page", "conversation");

function Fixture() {
  const app = useRemote();
  useEffect(() => {
    document.documentElement.dataset.fixtureApp = JSON.stringify({ error: app.error, hasDetail: app.detail !== null, loading: app.loading });
  });
  const actions = {
    readError: () => { fixture.failRead(); app.refresh(); },
    heldRead: () => { fixture.holdRead(); app.refresh(); },
    releaseRead: fixture.releaseRead,
    hide: () => fixture.visibility(true), show: () => fixture.visibility(false),
    streamError: fixture.failWatch,
    completeActivity: fixture.completeActivity,
    appendActivity: fixture.appendActivity,
    finishTurn: fixture.finishTurn,
    waitForApproval: fixture.waitForApproval,
    acceptSteer: fixture.acceptSteer,
    rejectSteer: fixture.rejectSteer,
    mutationError: () => { fixture.failMutation(); void app.mutate("stop", { sessionId: "fixture-session" }); },
  };
  return <>
    <nav aria-label="隔离检查" style={{ position: "fixed", left: 0, top: 0, maxWidth: 120, display: "grid" }}>
      {Object.entries(actions).map(([name, action]) => <button key={name} data-fixture-action={name} onClick={action}>{name}</button>)}
    </nav>
    <div style={{ height: "100dvh", width: "min(100%, 390px)", marginInline: "auto" }}><RemoteView app={app} /></div>
  </>;
}
createRoot(document.getElementById("root")!).render(<Fixture />);
