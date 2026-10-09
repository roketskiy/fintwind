// Wording pass on the desktop pairing card: drop the ambiguous
// "pairing code" phrasing (QR code, not a check code) in all locales.
import fs from "node:fs";

const replacements = [
  ["locales/app.yml", 'en: Show the pairing code to your phone.', 'en: Show the QR code to your phone.'],
  ["locales/app.yml", 'en: Show pairing code', 'en: Pair a phone'],
  ["locales/app.yml", 'en: Scan this code with the Fintwind app', 'en: Scan this QR code with your phone'],

  [
    "locales/zh-CN.yml",
    "有设备请求与本机配对。", // marker (already clean, keep as is)
    "有设备请求与本机配对。",
  ],
  [
    "locales/zh-CN.yml",
    "settings.devices_pair_description: 用手机扫描配对码。邀请一次性有效，五分钟后过期；在你于此确认之前，不会信任任何设备。",
    "settings.devices_pair_description: 用手机扫描二维码。邀请一次性有效，五分钟后过期；在你于此确认之前，不会信任任何设备。",
  ],
  ["locales/zh-CN.yml", "settings.devices_pair_button: 显示配对码", "settings.devices_pair_button: 配对新手机"],
  ["locales/zh-CN.yml", "settings.devices_scan_prompt: 用 Fintwind 应用扫描此码", "settings.devices_scan_prompt: 用手机扫描此二维码"],

  [
    "locales/ja.yml",
    "スマートフォンでペアリングコードをスキャンしてください。",
    "スマートフォンで QR コードをスキャンしてください。",
  ],
  ["locales/ja.yml", "settings.devices_pair_button: ペアリングコードを表示", "settings.devices_pair_button: スマートフォンをペアリング"],
  ["locales/ja.yml", "settings.devices_scan_prompt: Fintwind アプリでこのコードをスキャン", "settings.devices_scan_prompt: スマートフォンでこの QR コードをスキャン"],
];

for (const [file, from, to] of replacements) {
  if (from === to) continue;
  let t = fs.readFileSync(file, "utf8");
  if (!t.includes(from)) {
    console.log(`MISS  ${file}: ${from.slice(0, 40)}`);
    continue;
  }
  t = t.replace(from, to);
  fs.writeFileSync(file, t);
  console.log(`OK    ${file}: ${to.trim().slice(0, 50)}`);
}
console.log("done");
