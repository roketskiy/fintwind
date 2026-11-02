import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "@/components/ui/popover";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { SettingsIcon, TrashIcon } from "@/components/icons";
import { usePhoneTheme, type PhoneTheme } from "@/lib/theme";

export function PhoneSettings({ onForget }: { onForget: () => Promise<void> }) {
  const { theme, setTheme } = usePhoneTheme();
  const [confirm, setConfirm] = useState(false);
  const [forgetting, setForgetting] = useState(false);
  return <Popover onOpenChange={() => setConfirm(false)}>
    <PopoverTrigger render={<Button variant="ghost" size="icon" aria-label="设置" />}><SettingsIcon /></PopoverTrigger>
    <PopoverContent align="end" className="phone-settings">
      <PopoverTitle>外观</PopoverTitle>
      <ToggleGroup aria-label="配色" value={[theme]} onValueChange={(values) => { if (values[0]) setTheme(String(values[0]) as PhoneTheme); }}>
        <ToggleGroupItem value="light">日间</ToggleGroupItem>
        <ToggleGroupItem value="dark">夜间</ToggleGroupItem>
        <ToggleGroupItem value="system">跟随系统</ToggleGroupItem>
      </ToggleGroup>
      {confirm ? <div className="forget-confirm">
        <p>忘记后需重新扫码配对。</p>
        <div><Button variant="ghost" disabled={forgetting} onClick={() => setConfirm(false)}>取消</Button><Button variant="destructive" disabled={forgetting} onClick={() => { setForgetting(true); void onForget().finally(() => { setForgetting(false); setConfirm(false); }); }}>确认忘记</Button></div>
      </div> : <Button variant="ghost" className="forget-action" onClick={() => setConfirm(true)}><TrashIcon data-icon="inline-start" />忘记这台电脑</Button>}
    </PopoverContent>
  </Popover>;
}
