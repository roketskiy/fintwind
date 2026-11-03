import { useEffect } from "react";
import { createRoot } from "react-dom/client";
import { RemoteView } from "./app";
import { useRemote } from "./hooks/use-remote";
import { fixture } from "./fixtures/remote-client";
import { workflowActions, workflowEnabled } from "./fixtures/workflow";
import "./index.css";

localStorage.setItem("fintwind-project", "fixture-project");
localStorage.setItem("fintwind-session", "fixture-session");
localStorage.setItem("fintwind-page", "conversation");
const params = new URLSearchParams(location.search);
if (params.has("theme")) localStorage.setItem("fintwind-phone-theme", params.get("theme") === "dark" ? "dark" : "light");

if (workflowEnabled) {
  const violations: { directive: string; blocked: string }[] = [];
  document.addEventListener("securitypolicyviolation", event => {
    if (violations.length >= 32) return;
    violations.push({ directive: event.effectiveDirective, blocked: event.blockedURI });
    document.documentElement.dataset.fixtureCsp = JSON.stringify(violations);
  });
}

function Fixture() {
  const app = useRemote();
  useEffect(() => {
    document.documentElement.dataset.fixtureApp = JSON.stringify({ error: app.error, hasDetail: app.detail !== null, loading: app.loading, projectId: app.projectId, sessionId: app.sessionId });
  });
  const actions = {
    ...(workflowEnabled ? workflowActions : {}),
    failNextMutation: fixture.failMutation,
    readError: () => { fixture.failRead(); app.refresh(); },
    heldRead: () => { fixture.holdRead(); app.refresh(); },
    releaseRead: fixture.releaseRead,
    hide: () => fixture.visibility(true), show: () => fixture.visibility(false),
    streamError: fixture.failWatch,
    completeActivity: fixture.completeActivity,
    appendActivity: fixture.appendActivity,
    finishTurn: fixture.finishTurn,
    nextTurn: fixture.nextTurn,
    updateUsage: fixture.updateUsage,
    waitForApproval: fixture.waitForApproval,
    acceptSteer: fixture.acceptSteer,
    rejectSteer: fixture.rejectSteer,
    mutationError: () => { fixture.failMutation(); void app.mutate("stop", { sessionId: "fixture-session" }); },
  };
  return <>
    <div style={{ height: "100dvh", width: "min(100%, 390px)", marginInline: "auto" }}><RemoteView app={app} /></div>
    <nav hidden={params.has("frame")} aria-label="隔离检查" style={{ position: "fixed", left: 0, top: 0, width: 60, maxHeight: "100dvh", overflow: "auto", display: params.has("frame") ? "none" : "grid" }}>
      {Object.entries(actions).map(([name, action]) => <button type="button" style={{ overflowWrap: "anywhere" }} key={name} data-fixture-action={name} onClick={action}>{name}</button>)}
    </nav>
  </>;
}
createRoot(document.getElementById("root")!).render(<Fixture />);
