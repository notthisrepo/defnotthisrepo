# ztrx

**ztrx** runs the Xbox Series build of the game natively on Windows PCs with
NVIDIA graphics cards, from game files you supply yourself.

**This repository contains no game files or assets.** You need your own copy
of the game. Not affiliated with or endorsed by its developer or publisher.

## Status (2026-11-25)

Tested on Windows 11 with an **NVIDIA GeForce RTX 3050** (8 GB):

| | |
|---|---|
| Boot, first-boot screens, intro, opening credits | works |
| Prologue | playable from the cell to the boat chase: cell, cafeteria, robbery, police chase (wanted level, minimap), boat, binoculars |
| Stability | 1 h 30 min+ in one session without a crash |
| Frame rate | 1-3 fps in the world (the first run is slower while shaders translate; they are cached after that) |
| Known problems | anything in shadow is black and bright scenes overexpose (the game's ray-traced lighting does not run yet); some blocky artifacts; boats sit low in the water |

## Requirements

- Windows 10/11 x64, a CPU with AVX2
- A Direct3D 12 GPU with shader model 6.6 and mesh shaders (NVIDIA RTX 20 series or newer)
- Rust (the version in `rust-toolchain.toml`)
- About 30 GB of free Windows commit memory: the process commits about 24 GB in the world, and every byte it holds in the card's memory is committed once more in the process (10 GB of the 24 on a 16 GB card, almost none of it ever in RAM). Set a fixed paging file large enough rather than a system-managed one, which grows while you play; raise it if `crashes.log` reports error 1455

## Build

```powershell
cargo build --release -p victory
```

## Play

```powershell
powershell -ExecutionPolicy Bypass -File scripts\play.ps1 -Game "<your game folder>\<the game's exe>"
```

`scripts\play.ps1` keeps the shader cache and saves in folders you choose
(`-Cache`, `-Data`) and presses no buttons for you. Controller: XInput pads
(a DualShock/DualSense through DS4Windows works). With no pad connected the
keyboard acts as one while the game window has focus.

`-Overlay` shows a block of figures over the picture, renewed every second:
frame rate and time, where the time goes (preparing, the card, copying back,
waits), the process's and the card's memory, buffers allocated, textures
not there yet, the shader cache and what was dropped. `-Safe` runs without
the faster settings below.

## Faster settings

The runtime leaves them off; `scripts\play.ps1` turns these two on unless
you run it with `-Safe` or have set them yourself:

| Switch | Effect |
|---|---|
| `VICTORY_RELAY=2` | submissions are sent without waiting for the card; a thread of its own copies back what shaders wrote |
| `VICTORY_INPLACE=1` | windows of the title's private memory are opened as the card's memory instead of copied |

A card with a memory budget of 12 GiB or more also takes every title buffer
of 1 MiB or more into its own memory, up to an eighth of the budget (before:
16 MiB or more, 512 MiB in all, as on an 8 GB card; `VICTORY_CARD_BUFFERS_MIN_MB`
and `VICTORY_CARD_BUFFERS_MB` set both).

Measured on an RTX 4080 SUPER (16 GB) with 32 GB of RAM, one run each of the
same six scripted minutes, medians over the same 140 seconds in the world:

| Settings | fps | worst second |
|---|---|---|
| none (two runs) | 1.5 and 1.9 | 2.1 s and 7.0 s a frame |
| `VICTORY_RELAY=2` | 2.6 | 11.5 s |
| and card buffers from 1 MiB, 2 GiB in all | 3.0 | 3.3 s |
| and `VICTORY_INPLACE=1` (two runs) | 4.3 and 3.8 | 0.7 s and 1.3 s |

No run lost the device or logged a warning the plain run did not. Memory did
not change: about 24 GB committed, 15.5 GB of it in the PC's memory, 10 GB on
the card and 5 GB of the PC's memory shared with it. On a PC with 32 GB that
leaves almost nothing to commit; close other programs or raise the paging
file.

`VICTORY_LATE_WAIT=3 VICTORY_WIDE_BACK=1` is the older pair, without the
relay.

## Debugging switches

| Switch | Effect |
|---|---|
| `VICTORY_CAPTURE=<dir>` + `VICTORY_CAPTURE_FRAMES=<list>` | save those frames as PNG (diagnostics build) |
| `VICTORY_KEYS=30:Enter,35:X` | press keys at those seconds (diagnostics build) |
| `VICTORY_OVERLAY=1` | the figures of `-Overlay` (any build) |
| `VICTORY_D3D12_DEBUG=1` | Direct3D 12's debug layer names the failing command (diagnostics build; needs the Windows optional feature "Graphics Tools") |
| `VICTORY_D3D12_DRED=1` | on a GPU failure, report the commands in flight |
| `VICTORY_PIPELINE_WAIT_MS=24` | shorter per-frame wait for pipelines still being built |

Diagnostics build: `cargo build --profile diag -p victory --features diagnostics`

## License

GPL-3.0-or-later (see `LICENSE`). Based on VIctory (Marvisak) and victory-fork (fxceless).
