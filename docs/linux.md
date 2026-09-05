# fintwind on Linux

## Install

```sh
curl -fsSL https://fintwind.sh/install.sh | sh
```

The script needs no root. It unpacks the release tarball into
`~/.local/fintwind.app` and installs the desktop entry into
`~/.local/share/applications`, so **fintwind appears in your applications menu** —
you can also launch it from a terminal via `fintwind` command. Run the script again to
upgrade; it replaces the previous install rather than merging into it.

fintwind expects:

- **glibc 2.35 or newer** — Ubuntu 22.04, Debian 12, Fedora 36, and anything
  more recent. Releases are built on Ubuntu 22.04, so older distributions must
  build from source.
- **A working Vulkan or OpenGL driver.** fintwind renders through wgpu, which tries
  Vulkan first and falls back to GL. Software rasterizers (lavapipe, llvmpipe)
  are accepted, so it can run in a VM, but see the note below.
- **x86_64 or aarch64.** Other architectures build from source.
- `xdg-desktop-portal` for native file dialogs.

Set `FINTWIND_VERSION` to install a specific version rather than the latest.

## Installing manually

The script is a convenience, not a requirement. Download
`fintwind-<version>-<target>.tar.gz` from
[releases.fintwind.sh](https://releases.fintwind.sh) or the
[GitHub release](https://github.com/egoist/fintwind/releases), then unpack it
wherever you like:

```sh
mkdir -p ~/.local/fintwind.app
tar -xzf fintwind-<version>-<target>.tar.gz --strip-components=1 -C ~/.local/fintwind.app
ln -sf ~/.local/fintwind.app/bin/fintwind ~/.local/bin/fintwind   # optional
```

The archive uses an install-prefix layout (`bin/`, `share/`) beneath one
versioned directory, so `--strip-components=1` into a prefix such as
`/usr/local` works too.

**Keep `bin/` intact.** fintwind launches `fintwind-daemon` from its own directory, so
copying `bin/fintwind` somewhere on its own leaves it unable to start the daemon.
A symlink is fine — fintwind resolves it back to the real path.

Installing the desktop entry is the part that matters — it is how the app is
launched normally, and it is what associates the running window with its icon
and name (fintwind reports the Wayland `app_id` / X11 `WM_CLASS` `sh.fintwind`, which
matches the entry's filename). Install the packaged file and point it at the
install (the packaged copy uses bare `Exec=fintwind` and `Icon=sh.fintwind` names so it
can be relocated):

```sh
install -D ~/.local/fintwind.app/share/applications/sh.fintwind.desktop \
  -t ~/.local/share/applications
sed -i "s|^Exec=fintwind$|Exec=$HOME/.local/fintwind.app/bin/fintwind|" \
  ~/.local/share/applications/sh.fintwind.desktop
sed -i "s|^Icon=sh.fintwind$|Icon=$HOME/.local/fintwind.app/share/icons/hicolor/256x256/apps/sh.fintwind.png|" \
  ~/.local/share/applications/sh.fintwind.desktop
```

## Updating

fintwind does not update itself on Linux — Sparkle is macOS-only. Re-run the
install script to upgrade.

## Uninstalling

```sh
curl -fsSL https://fintwind.sh/install.sh | sh -s -- --uninstall
```

This removes `~/.local/fintwind.app`, the symlink, and the desktop entry. Projects
and settings stay in `~/.fintwind`; delete that directory to remove them too.

## Building from source

See [CONTRIBUTING.md](../CONTRIBUTING.md) for build prerequisites, then
produce the same archive this page installs with:

```sh
./scripts/bundle-linux.sh
```

To exercise the install script against that local build:

```sh
FINTWIND_BUNDLE_PATH=target/release/fintwind-<version>-<target>.tar.gz \
  sh scripts/install.sh
```

## Running in a virtual machine

VMs usually have no GPU passthrough, so Mesa falls back to a software
rasterizer. That works in principle — wgpu accepts a CPU adapter — but both
lavapipe (Vulkan) and llvmpipe (GL) JIT-compile shaders through LLVM, and that
path is fragile: on Fedora 44 aarch64 (mesa 26.0.3 + LLVM 22.1) it segfaults
inside `gallivm_jit_function` while compiling a fragment shader. The crash is
in the driver, not in fintwind, and no application-side setting avoids it.

If the app dies on its first frame in a VM, check `coredumpctl info` for a
backtrace through `libvulkan_lvp.so` or `libgallium`. The reliable fix is to
give the guest a real GL driver — on UTM that means the QEMU backend with
virtio-gpu-gl (virgl) rather than Apple Virtualization, which offers Linux
guests no 3D at all. `VK_DRIVER_FILES=/nonexistent.json` hides the software
Vulkan driver so wgpu takes the GL path instead.
