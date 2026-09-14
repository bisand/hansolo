# HanSolo

A Bitcoin **solo lottery miner** in Rust, with a [DeniseUI](https://github.com/bisand/denise)
dashboard that runs in a window on macOS, Windows and Linux — and straight on the
display of a Linux machine with no desktop at all (DRM/KMS, or fbdev).

It inspects the machine it runs on, benchmarks every hashing path the hardware
supports (SHA-NI, ARMv8 SHA2, AVX2/AVX-512 multi-lane, GPU compute via wgpu,
AxeOS network ASICs), and mines with the fastest.

> **About the odds.** A CPU or GPU finding a Bitcoin block is a lottery ticket,
> and a long one: at 1.5 GH/s against today's network the expected wait is
> millions of years. The dashboard shows exactly how long, on purpose. People
> *do* win solo blocks with tiny miners every so often — which is the fun of it.

## Installing

Every [release](https://github.com/bisand/hansolo/releases) carries ready builds:

| Platform | Download |
| --- | --- |
| Debian, Ubuntu, Mint, Raspberry Pi OS (64-bit) | `hansolo_<version>-1_amd64.deb` / `_arm64.deb` |
| Fedora, RHEL, openSUSE | `hansolo-<version>-1.x86_64.rpm` / `.aarch64.rpm` |
| Arch, Manjaro | `hansolo-<version>-1-x86_64.pkg.tar.zst` / `-aarch64` |
| Any other Linux (glibc 2.31+) | `hansolo-<version>-<arch>-linux.tar.gz` |
| Bare-display panels (static, no winit) | `hansolo-kiosk-<version>-<arch>-linux-musl.tar.gz` (aarch64, armv7) |
| macOS 11+ (Apple Silicon and Intel) | `HanSolo-<version>.dmg`, or the `universal-macos.tar.gz` |
| Windows (x64 and Arm64) | `hansolo-<version>-<arch>-windows.msi` installer, or the `.zip` |

`SHA256SUMS` lists the checksums. The macOS app is not notarised, so the first
launch needs right-click → **Open**. The Windows installer is unsigned too, so
SmartScreen asks once (**More info** → **Run anyway**); it adds a Start menu
shortcut and puts `hansolo` on the `PATH`.

Publishing a GitHub release runs `.github/workflows/release.yml`, which builds all
of these and attaches them to it.

## Running

```bash
cargo run --release -p hansolo                 # window, or the bare display on Linux
cargo run --release -p hansolo -- --demo       # the dashboard with a synthetic miner
cargo run --release -p hansolo -- --ui headless
```

On first start the Settings page opens: enter a payout address, pick a solo pool
(public-pool.io, solo.ckpool.org) or point it at your own Bitcoin Core node, and
press **Start mining**. F5 starts and stops from anywhere.

### Which display

`--ui auto` (the default) opens a window when there is a desktop session
(`WAYLAND_DISPLAY`/`DISPLAY`, or any macOS/Windows), takes over the display when
there is none but `/dev/dri` or `/dev/fb0` exists, and otherwise runs headless.
`--ui window|kiosk|headless` overrides it.

A small panel-only build that never links winit:

```bash
cargo build --release -p hansolo --no-default-features --features kiosk \
    --target aarch64-unknown-linux-musl
```

On a bare display, F12 writes a screenshot to `/tmp/hansolo.ppm` and Escape quits.
On a Raspberry Pi, enable the vc4 KMS overlay first (see Denise's
`docs/raspberry-pi.md`).

### Themes

daisyUI's **dim** (dark) and **light**, with their exact colours and radii.
*System* follows the operating system and falls back to dim when it cannot tell —
which is always the case on a bare display.

### Fonts

The best installed face is picked per role (body, bold numbers, monospace hashes).
Set `HANSOLO_FONT`, `HANSOLO_FONT_BOLD` and `HANSOLO_FONT_MONO` to a `.ttf` path to
choose, which is how a panel image names the face it ships. With no fonts at all it
falls back to Denise's built-in bitmap font.

### Configuration

`hansolo.toml` in the platform config directory (printed at start-up), or
`--config FILE`. Everything in it is editable from the Settings page.

```toml
payout_address = "bc1q…"
worker_name = "hansolo"
autostart = false

[source]
kind = "stratum"
url = "stratum+tcp://public-pool.io:21496"

# or
# [source]
# kind = "node"
# rpc_url = "http://127.0.0.1:8332"
# cookie_file = "/home/me/.bitcoin/.cookie"

[cpu]
enabled = true
low_priority = true
# threads = 8
# backend = "armv8-sha2"

[gpu]
enabled = true
intensity = 6

[asic]
enabled = false
network_devices = ["192.168.1.50"]

[ui]
theme = "system"
```

## The dashboard

| Page | What it shows |
|---|---|
| Dashboard | Hashrate, best share, accepted/rejected, block odds today; a 15-minute hashrate chart; the *lottery ticket* (best share against the block target, on a log scale); devices; connection |
| Hardware | OS, CPU and instruction sets, the benchmark of every hashing path and which won, GPUs and ASICs |
| Work | The 80-byte header drawn to scale, the current job field by field, the coinbase transaction |
| Shares | Counters and the most recent shares |
| Log | Every event, newest first |
| Settings | Payout, work source, hardware, interface |

`--snapshot out.ppm [--page N] [--theme dark|light] [--size WxH] [--scale F]` draws
one frame of the demo into a file with no display, for reviewing a layout over SSH.

## Layout

| Crate | |
|---|---|
| `crates/hansolo-core` | The contract: SHA-256 reference and midstates, targets and difficulty, work units, the `Device` trait, configuration, the UI snapshot |
| `crates/hansolo-hash` | Hardware detection, the hashing backends, benchmarking and selection |
| `crates/hansolo-engine` | Stratum v1 and `getblocktemplate` work sources, share verification and submission, statistics |
| `app` | The DeniseUI application: views, custom widgets, window and kiosk backends, headless mode |

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Status

Measured on an Apple M5 Pro (18 cores): portable 3.5 MH/s, NEON 4-lane 7.4 MH/s,
ARMv8 SHA2 34 MH/s per thread; Metal GPU 1.0 GH/s; about 1.3 GH/s combined, with
shares accepted by public-pool.io.

| | |
|---|---|
| Stratum v1, Bitcoin Core `getblocktemplate` | ✅ tested against a fake pool and a fake regtest node; live pool shares accepted |
| CPU: portable, ARMv8 SHA2, NEON | ✅ verified against the reference hash |
| CPU: SHA-NI, AVX2, AVX-512 | ⚠️ round logic tested, intrinsics compile-checked only — needs a run on real x86 |
| GPU via wgpu | ✅ Metal verified; Vulkan and DX12 untested |
| ASIC | AxeOS network miners (Bitaxe, NerdQAxe) are monitored; USB BM13xx sticks are detected but not driven yet |
| Kiosk (DRM/KMS, fbdev) | compile-checked for x86_64/aarch64 Linux; not yet run on a panel |

Not yet: Stratum version rolling, GBT longpoll, an on-screen keyboard for
touch-only panels.

Cross-compiling needs a C compiler for the target (`ring` for TLS and
`secp256k1-sys` via the `bitcoin` crate); `cargo zigbuild` works.

## Licence

MIT
