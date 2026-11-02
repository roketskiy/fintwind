import { Component, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "@/components/ui/empty";

export class AppBoundary extends Component<
  { children: ReactNode },
  { failed: boolean }
> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  render() {
    if (!this.state.failed) return this.props.children;
    return (
      <main className="app-shell">
        <Empty className="h-full">
          <EmptyHeader>
            <EmptyTitle>页面未能加载</EmptyTitle>
            <EmptyDescription>
              请重新加载页面，已发送的操作不会自动重试。
            </EmptyDescription>
          </EmptyHeader>
          <EmptyContent>
            <Button onClick={() => location.reload()}>重新加载</Button>
          </EmptyContent>
        </Empty>
      </main>
    );
  }
}
