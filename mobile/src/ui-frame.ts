// Explicit visual-fixture entry only. Never a production build input.
import "./ui-frame.css";
const params = new URLSearchParams(location.search);
const width = Math.min(430, Math.max(320, Number(params.get("width")) || 390));
const dark = params.get("theme") === "dark";
document.documentElement.classList.toggle("dark", dark);
const frame = document.querySelector("iframe")!;
frame.style.width = `${width}px`;
frame.src = `./remote-fixture.html?workflow=1&polish=1&frame=1&theme=${dark ? "dark" : "light"}${params.has("math") ? "&math=1" : ""}`;
