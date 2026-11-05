import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "@/components/ui/popover";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { SettingsIcon, TrashIcon } from "@/components/icons";
import type { PhoneTheme } from "@/lib/theme";
import "./beautiful/segmented-control.css";

export function PhoneSettings({ onForget, theme, setTheme }: { onForget: () => Promise<void>; theme: PhoneTheme; setTheme: (value: PhoneTheme) => void }) {
  const [confirm, setConfirm] = useState(false);
  const [forgetting, setForgetting] = useState(false);
  return <Popover onOpenChange={() => setConfirm(false)}>
    <PopoverTrigger render={<Button variant="ghost" size="icon" aria-label="设置" />}><SettingsIcon /></PopoverTrigger>
    <PopoverContent align="end" className="phone-settings">
      <PopoverTitle>外观</PopoverTitle>
      <ToggleGroup className="bui-segments" spacing={0} size="sm" aria-label="配色" value={[theme]} onValueChange={(values) => { if (values[0]) setTheme(String(values[0]) as PhoneTheme); }}>
        <ToggleGroupItem value="light"><span className="bui-segment-face">日间</span></ToggleGroupItem>
        <ToggleGroupItem value="dark"><span className="bui-segment-face">夜间</span></ToggleGroupItem>
        <ToggleGroupItem value="auto" aria-label="自动（07:00 至 19:00 为日间）" title="07:00 至 19:00 使用日间模式"><span className="bui-segment-face">自动</span></ToggleGroupItem>
      </ToggleGroup>
      {confirm ? <div className="forget-confirm">
        <p>忘记后需重新扫码配对。</p>
        <div><Button variant="ghost" disabled={forgetting} onClick={() => setConfirm(false)}>取消</Button><Button variant="destructive" disabled={forgetting} onClick={() => { setForgetting(true); void onForget().finally(() => { setForgetting(false); setConfirm(false); }); }}>确认忘记</Button></div>
      </div> : <Button variant="ghost" className="forget-action" onClick={() => setConfirm(true)}><TrashIcon data-icon="inline-start" />忘记这台电脑</Button>}
    </PopoverContent>
  </Popover>;
}
