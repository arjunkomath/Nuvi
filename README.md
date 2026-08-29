# Nuvi

Nuvi is a native, GPU-accelerated Neovim GUI for macOS, built with GPUI. Each
workspace tab runs an independent embedded Neovim process.

<img width="2636" height="1792" alt="CleanShot 2026-08-28 at 23 43 25@2x" src="https://github.com/user-attachments/assets/e17f704d-5179-4ae5-88b4-a3d1ebac2649" />

## Build

Install a current Rust toolchain and Neovim, then run:

```sh
cargo build
```

For a local macOS application bundle:

```sh
./scripts/build-app.sh
```

## Release

Direct-distribution releases require a Developer ID Application certificate and
a notarization profile stored in Keychain:

```sh
just signing-identities
just notary-profile
just release
```

The release script signs, notarizes, staples, and verifies the app, then writes
a ZIP and SHA-256 checksum to `target/release`. Upload both files to the GitHub
pre-release. Set `NUVI_SIGNING_IDENTITY` or `NUVI_NOTARY_PROFILE` to override
the `Developer ID Application` and `nuvi-notary` defaults. Use
`just release-debug` for command tracing and `just verify-release` to recheck
the finished artifacts.

Nuvi finds `nvim` on `PATH` and in the standard Homebrew locations. Set `NUVI_NVIM` to an absolute executable path to override it. Arguments passed to Nuvi are forwarded to Neovim.

## Configuration

Nuvi uses Neovim's standard `guifont` and `linespace` options. `vim.g.nuvi` and the `NUVI` environment variable are set before `init.lua` runs, so settings can be Nuvi-specific:

```lua
if vim.g.nuvi then
  vim.opt.guifont = "JetBrainsMono Nerd Font:h15"
  vim.opt.linespace = 2
end
```

Font options support point size (`h17`), relative cell width (`w-1.4`), bold (`b`), italic (`i`), and numeric weight (`W450`).

Set `NVIM_APPNAME=nuvi` before launching if you want an entirely separate Neovim configuration directory.

## Workspaces

Launch Nuvi without arguments to choose a recent folder or open one with the
native macOS picker. Use `⌘T` for a launcher tab, `⌘O` to open a folder, and
`⌘W` to close the active workspace. Passing a path opens it directly.

The first version remains macOS-only and uses the external line-grid UI
protocol. Multigrid windows, popups, and cross-platform support are deferred.
