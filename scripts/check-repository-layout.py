"""Check the active Go source tree and the source-only Rust archive."""
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parent.parent


def main():
    errors = []
    tracked = subprocess.check_output(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
        cwd=ROOT,
    ).decode("utf-8").split("\0")
    files = {name for name in tracked if name and (ROOT / name).is_file()}
    if not (ROOT / "go.mod").is_file():
        errors.append("The Go module must live at the repository root")
    commands = {p.name for p in (ROOT / "cmd").iterdir() if p.is_dir()}
    if commands != {"app", "daemon", "mygo"}:
        errors.append("cmd/ is reserved for app, daemon, and mygo; put maintenance commands in tools/")
    for name in sorted(files):
        path = Path(name)
        if path.suffix == ".rs" and not name.startswith("legacy/rust/"):
            errors.append(f"Rust source outside legacy/rust/: {name}")
        if name.startswith("legacy/rust/") and not (
            path.suffix in {".rs", ".hlsl", ".h", ".m", ".mm"}
            or path.name.startswith("LICENSE")
            or path.name.endswith("LICENSE")
            or path.name == "README.md"
        ):
            errors.append(f"Non-source file in the Rust archive: {name}")
        if path.name in {"Cargo.toml", "Cargo.lock"} or name.startswith(".cargo/"):
            errors.append(f"Retired Rust build input: {name}")
        if name.startswith("fintwind-go/"):
            errors.append(f"Old Go module location: {name}")
        if path.name == "go.mod" and name != "go.mod":
            errors.append(f"Unexpected nested Go module: {name}")
    icons = ROOT / "mobile/src/components/icons.tsx"
    for relative in re.findall(r'from "([^"?]+\.svg)\?raw"', icons.read_text(encoding="utf-8")):
        if not (icons.parent / relative).is_file():
            errors.append(f"Missing mobile icon: {relative}")
    for name in [".github/workflows/test.yml", ".github/workflows/release.yml", "scripts/mygo.ps1"]:
        if "fintwind-go/" in (ROOT / name).read_text(encoding="utf-8"):
            errors.append(f"Build entry still references the old Go directory: {name}")
    if errors:
        raise SystemExit("\n".join(errors))
    print("[OK] Root Go module, product commands, mobile assets, and source-only Rust archive")


if __name__ == "__main__":
    main()
