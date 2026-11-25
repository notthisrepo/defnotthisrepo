# Triage: graphics, memory and performance

Date: 2026-11-24. Tree: `90b529f` plus the uncommitted changes listed in
section 7. One title, one PC, one scripted scene. Every figure below was
measured in the runs named beside it; nothing is estimated unless it says so.

## 1. Summary

| Problem | State | Main cause found |
|---|---|---|
| ~3 fps | 1.2 fps stock, 3.6 fps with the settings the launcher now applies (warm cache) | the submitting thread waits for the card on every submission; shader writes are found by comparing hundreds of MB a frame on the CPU; textures are refreshed on the submitting thread |
| Excessive RAM | not improved: 24 GB committed, 15-16.5 GB resident, 5-6 GB of the PC's memory shared with the card | 11 GB of the commit is write-combined upload memory of which 1.3 GB is ever touched; 3.4-3.8 GB of copies of title memory that could be in place |
| Missing textures and geometry | partly explained | about 350 draws a frame dropped (ray-tracing structure builder off, wave64 compute programs refused, untranslated shaders); texture uploads capped per frame with 340-390 left out each frame; no detiling or layout failure was logged |
| GPU hangs, device loss | none in 14 runs at default or launcher settings; reproduced once on purpose | `VICTORY_WAVE_WIDE=1` hangs the card (DXGI_ERROR_DEVICE_HUNG after 135 s) |

Nothing in section 8 is implemented. It waits for approval.

## 2. Environment, verified

| Item | Value | How verified |
|---|---|---|
| GPU | NVIDIA GeForce RTX 4080 SUPER, 16 GB | `Win32_VideoController`, `nvidia-smi` |
| Driver | 32.0.16.1088 (610.88) | same |
| Adapter in use | the hardware adapter | the runtime logs a local memory budget of 15,277 MB, "host mesh shader support mesh=true", "host wave lanes min=32 max=32"; the process holds 9.6-10.4 GB of dedicated GPU memory (performance counters) |
| WARP | not in use | only reachable in a diagnostics build with `VICTORY_D3D12_WARP` set (`crates/victory-gpu/src/host.rs`, `create`); the variable was never set |
| CPU, RAM | i9-12900K (16 cores, 24 threads), 31.7 GB | `Win32_Processor`, `Win32_OperatingSystem` |
| Display, back buffer | 3840x2160 at 119 Hz; back buffer 3840x2160 R8G8B8A8_UNORM, SDR | runtime log |
| Build | `release` profile: default optimisation level 3, `debug = "line-tables-only"`, no LTO, no `target-cpu` | `Cargo.toml`, `.cargo/config.toml` |

Gaps found while verifying:

* The device is created on the default adapter (`D3D12CreateDevice(None, ...)`).
  On this PC that is the only GPU. On a PC with an integrated GPU first in
  DXGI's order it would be the wrong one, and the adapter's name is not
  logged. Unverified on such a PC.
* DRED and the debug layer exist only in diagnostics builds. DRED is set
  before the device is created, as required.

Tools not available on this PC: PIX, PresentMon, xperf. Not used: `wpr`,
`DXCap` (present). Frame times were taken from the runtime's own per-present
log line instead (diagnostics build).

## 3. Method and its limits

* Benchmark: `target\diag\victory.exe` (the `diag` profile: release code
  generation plus the `diagnostics` feature), 360 s, Enter pressed every 8 s by
  `VICTORY_KEYS`, which takes the title from boot into the world. The window
  measured is 100 s to 240 s after the first frame of 500 draws or more.
* A plain `release` build was not benchmarked in the world: scripted input
  exists only in diagnostics builds, and nothing was injected into the desktop.
  The diagnostics feature adds accounting on the hot path, so release figures
  should be somewhat better. Unverified.
* Correctness runs (debug layer, GPU-based validation, DRED) were separate
  runs and none of their timings are quoted as performance.
* One run per configuration unless a table says otherwise. The scripted
  presses land at fixed times while loading time varies, so two runs of one
  configuration differ by up to 0.5 fps. Treat smaller differences as noise.
* Cold cache: an empty cache folder (prepared images, translated shaders,
  pipeline library). Warm: the same folder on the next run.

## 4. Baseline measurements

### 4.1 Frame rate and frame time (world, 140 s window)

| Run | Settings | Cache | Avg fps | p50 ms | p90 ms | p99 ms | Max ms | 1% low fps | Frames over 1 s |
|---|---|---|---|---|---|---|---|---|---|
| t-stock-warm | runtime defaults, old card limits (16 MiB, 512 MiB) | warm | 1.20 | 685 | 1,313 | 2,431 | 2,533 | 0.40 | 26 of 111 |
| t-default-warm | launcher defaults (relay 2, in place 1, card limits by budget) | warm | 3.61 | 238 | 390 | 868 | 1,108 | 0.98 | 4 of 503 |
| t-default-cold | launcher defaults | cold | 1.28 | 720 | 1,532 | 1,909 | 2,039 | 0.51 | 60 of 173 |

Earlier runs of the same script, medians of the 5-second frame line: stock
1.9 and 1.5; relay 2.6; relay and card limits 3.0; relay, card limits and in
place 4.3 and 3.8.

### 4.2 Where a frame goes (medians of the frame line, ms per frame)

| | Stock, warm | Launcher defaults, warm |
|---|---|---|
| Frame | 775 | 260 |
| Preparing submissions (CPU) | 399 | 120 |
| of that: copying title buffers | 53 (152 MB) | 41 (105 MB) |
| of that: refreshing textures | 225 (102 MB) | 43 (42 MB) |
| Waiting for the card | 227 | 115 |
| Copying back shader writes | 83 (69 MB) | 141 (60 MB), on its own thread |
| Submitting thread standing still for the card | 186 | 62-107 |
| Card busy | 39% | 76-82% |

### 4.3 Memory (medians in the window, MB)

| | Stock, warm | Launcher defaults, warm | Launcher defaults, cold |
|---|---|---|---|
| Private (committed) | 23,786 | 23,573 | 23,929 |
| Working set | 16,539 | 14,769 | 16,164 |
| GPU dedicated | 9,643 | 10,387 | 9,661 |
| GPU shared (the PC's memory) | 5,745 | 5,032 | 5,671 |
| Card budget (QueryVideoMemoryInfo, local) | 15,277 at start, 13,313 later in one run | | |
| Left to commit on the PC | 1.3 GB | 1.1-4.8 GB | |

What the 24 GB is, from the runtime's memory map (t-default-warm, in the
world; committed, with the part resident in brackets):

| Region | MB |
|---|---|
| Write-combined (buffers shared with the card) | 11,179 (1,291) |
| Views of the title's pages | 6,732 (3,713) |
| The title's page pool (mapped section) | 6,236 (0) |
| The title's heaps (private) | 4,629 (2,071) |
| Other private allocations of 1 MB and more | 2,439 (2,405) |
| Process heap segments | 2,409 (2,396) |
| Windows of title memory, other 4 MiB allocations | 2,171 (2,108) |
| Files and mappings, programs and libraries | 1,174 (561) |

Of the write-combined memory, 6,290 MB is 277 blocks of exactly 32 MB.

### 4.4 Counts per frame and per run

| Measure | Stock, warm | Launcher defaults |
|---|---|---|
| Submissions per frame | 43.9 | 36.7 |
| Refreshes of host copies per frame | 25,804 over 21.4 GB checked | 17,839 over 16.7 GB |
| Write-watch askings per frame | 2,826 (28.9 ms), 99% of bytes unchanged | 2,170 (23.0 ms) |
| Shader writes compared on the CPU per frame | 432.7 MB | 23-30 MB |
| Page faults per frame | 54,711-110,832 | 1,088 |
| Guard faults per frame | 220 | 189 |
| Texture uploads per frame | 36 recorded, 388 left out | 19 recorded, 342 left out |
| Draws dropped per frame without a host pipeline | 348 | 345 |
| Upload buffers made in 6 minutes | 33,976 (13.3 GB) | 57,904 (23.7 GB) |
| Host copies of textures made / let go / made again in 6 minutes | | 52,281 / 35,561 (16.3 GB) / 4,159 (3.5 GB) |
| Shaders translated on a cold cache | about 3,000 | |
| Shaders that do not translate | 7-10 | same |
| Host pipelines refused | 5-6 | 4-5 after the fix in section 7 |

## 5. Causes

Severity: S1 wrong picture or lost device, S2 large cost in time or memory,
S3 small or latent.

### 5.1 Confirmed

**C1. Every submission is waited for (S2).** With runtime defaults the
submitting thread stands 186 ms of a 775 ms frame waiting for the card, the
card is 39% busy, and 274 ms of its idle time is while that thread prepares
the next submission. With `VICTORY_RELAY=2` the card is 76-82% busy and the
frame is 260 ms. Files: `crates/victory-gpu/src/host.rs` (`execute`, `submit`,
`relay_worker`, `late_stage`), `shadow.rs` (`relay`). The faster path exists
and is off in the runtime.

**C2. Shader writes are found on the CPU for most buffers (S2).** The fixed
limits (title buffers of 16 MiB or more, 512 MiB in all) were filled by ten
buffers; everything else was snapshotted and compared on the CPU: 433 MB a
frame, 93% of it in copies of 1 MiB or more, with 55,000-110,000 page faults a
frame. With limits taken from the card's budget: 23-30 MB and 1,088 page
faults. File: `shadow.rs` (`card_limits`, `Shadow::writing`,
`copy_back_with`).

**C3. The PC runs out of commit (S2, and S1 when it fails).** 24 GB committed
by the process and 5-6 GB of shared GPU memory leave 1.1-1.3 GB to commit on a
32 GB PC with a 23 GB paging file. The README names the failure (error 1455).
Two parts are identified: 11.2 GB of write-combined memory of which 1.3 GB is
resident (the commit charged for what is on the card, C13), and 3.4-3.8 GB of
windows that copy title memory (C4).

**C4. Title memory is copied where it could be in place (S2).**
`VICTORY_INPLACE=1` put 80 windows (380 MB) in place and turned 1,440 down
because their memory is a view of the title's page pool, whose section is made
reserved and cannot be opened as a heap. 824-933 windows (3.4-3.8 GB) stay
copies, each refreshed through write watching on every submission that reads
it. Files: `crates/victory-gpu/src/inplace.rs`,
`crates/victory-hle/src/modules/xmem.rs`.

**C5. Draws are dropped because programs are missing (S1).** In the world
about 350 draws and dispatches a frame have no host pipeline. By reason,
summed over one run's samples: the ray-tracing structure builder, which is
switched off, 43,690; refused compute pipelines 174; pipelines still being
made 132; untranslated compute 41 and pixel 23; a refused mesh pipeline 7.
What the builder would have written is read by later passes (the README:
"anything in shadow is black"). Files: `pipeline.rs`, `pipeline/host.rs`,
`crates/victory-shader/src/emit`.

**C6. Five compute programs need 64-lane waves (S1).** The debug layer:
"Shader wants wave lane count of 64 but maximum wave lane count for device is
32", five times. The translator's forms for a 32-lane host do not take them.
The remaining form (`VICTORY_WAVE_WIDE=1`) accepted two of the five and hung
the card after 135 s (C7). File: `crates/victory-shader/src/emit/mod.rs`
(`emit`), `lower.rs`.

**C7. `VICTORY_WAVE_WIDE=1` hangs the card (S1).** DXGI_ERROR_DEVICE_HUNG,
13 s after the last frame. DRED's breadcrumbs were all complete and name no
command, so the program is not identified. No compute pipeline was first
bound in the 12 s before, so it was a program in use for about a minute. Off
by default; left off.

**C8. A cold cache costs two thirds of the frame rate and drops more draws
(S2).** 1.28 fps against 3.61, 850 draws a frame dropped against 345, 234
pipelines still queued four minutes into the world. About 3,000 shaders are
translated, and every rebuild of the runtime starts over: the cache key holds
the executable's hash. Entries of older builds are never removed (503 MB
after five builds). File: `crates/victory-gpu/src/shader_cache.rs`.

**C9. Texture uploads are capped and churn (S2).** Each frame 19-36 texture
uploads are recorded and 340-390 are left out by the per-frame budget
(192 MiB staged per frame, `draw.rs` `heap_textures`). In six minutes 52,281
host copies of textures were made, 35,561 let go by the idle sweep (5 s, 4
frames) and 4,159 made again. The queue holds 2,300-3,200 slots; 1,600 are
uploaded in part. Files: `draw.rs`, `resource.rs` (`let_go_idle`, `room`),
`texture.rs`.

**C10. A buffer view is bound where the descriptor holds a 1D texture (S1).**
GPU-based validation, during loading: "Dispatch, SRV resource dimensions
differs from that expected by shader: expected BUFFER, in descriptor
TEXTURE1D, descriptor heap index 0", compute stage. One message; the run did
not reach the world. File: `descriptor/host.rs` (unverified which path).
*Corrected 2026-11-24:* this was first put down to a null view of the wrong
dimension. That does not fit: a slot never written holds a null 2D view
(`null_view`), not a 1D one, and the code records that null buffer views in
such slots were tried and changed nothing. Slot 0 held a view the title wrote
as a 1D texture, so either the shader's index is wrong or it reads another
table than the one mirrored. Not fixed. To identify it: log, once, the view
the title writes to slot 0 and the pipeline of the dispatch that reads it.
*Diagnosis attempted 2026-11-24, not concluded:* diagnostics builds now list,
after any message of GPU-based validation, the dispatches of the lists just
run or landed (number in the list, pipeline, the views at the heap indices
given, and what slot 0 of the view heap holds), with device-removed data on
(`host.rs` `execute` and `relay_land`, `draw.rs` `op_text`). Two more runs
of 300 s with GPU-based validation, one with runtime defaults and one with
the launcher's settings, both reaching the same point of loading as the run
that reported it (frame 2,048, about 5,200 dispatches), reported nothing. The
mismatch has been seen once in three such runs: it is intermittent, and
neither tied to the relay nor excluded from it. The next run that shows it
will name the dispatch.

**C11. A method of the device that makes pipelines is not implemented (S1).**
"a pipeline creation method that is not implemented: the title goes without
this pipeline", slot 0x1c8, 16,384 calls counted in a run. File:
`crates/victory-gpu/src/device.rs`. Which pipelines these are is unverified.

**C12. Fixed and confirmed by the debug layer (S1), see section 7.** A mesh
pipeline refused for blending on an integer target, and a copy of a texture
onto itself recorded with the texture left a copy source (72 errors).

**C13. Everything in the card's memory is committed a second time in the
process (S2).** Added after fix 1 was approved; it replaces U1. An isolated
test on the real driver (`crates/victory-gpu/tests/driver_memory.rs`, run with
`cargo test -p victory-gpu --test driver_memory -- --ignored --nocapture`)
printed:

| Step | Write-combined commit | Of it in 32 MiB allocations |
|---|---|---|
| 17,000 block-compressed textures alive (10.4 GB) | +10,624 MB | 331 allocations |
| the same, evicted | no change | |
| the same, let go | -10,379 MB | |
| the same placed in 42 heaps of 256 MiB | +10,752 MB | none (heap-sized) |
| placed textures let go, heaps kept | no change | |
| one 1 GiB buffer in the card's memory | +1,024 MB | |
| 20,000 upload buffers made and let go one by one | +5 MB | |
| 3,000 upload buffers alive (1.3 GB) | +1,307 MB, back to +272 MB when let go | 40 allocations |
| 760 lists and allocators | 0 (333 MB of other private memory, 0.44 MB each) | |

In the title's own log the write-combined commit follows the bytes of host
textures and of title buffers' copies on the card: 9.1-9.6 GB of the
11.2-12.0 GB once the world is loaded, at every sample from 120 s on. About
2 GB is not attributed (upload chunks, staging and descriptor heaps account
for under 0.3 GB of it). One early sample, taken while loading, does not fit.

What follows from it:

* The commit is not a leak and not upload churn. It is the price of the bytes
  on the card, one for one, on this driver. Only 1.1-2.8 GB of it is resident,
  so it costs commit, not RAM.
* Pooling uploads, fewer lists, placed textures and eviction do not reduce it.
  Heaps would hold it longer.
* Moving title buffers to the card (section 7, card limits) costs their bytes
  in commit: up to 1.9 GB on this card where the old limit allowed 0.5 GB.
  The run with the higher limit committed about 1 GB more (24.7 against
  23.5 GB) on a PC that is 1.1-1.3 GB from its limit.
* What reduces it is fewer bytes on the card, or a higher commit limit (a
  larger paging file costs disk and, at 1.3 GB resident, almost no RAM).

### 5.2 Ruled out

* **Host lists or upload buffers as the owner of the write-combined commit.**
  C13: lists take none; upload buffers take their size only while alive
  (about 300 MB is alive in the world).

* **The software adapter or the wrong GPU on this PC.** Section 2.
* **Detiling or layout failures in this scene.** `texture.rs` logs "texture
  not detiled", "without a console layout" and "in an XOR mode not uploaded";
  none appeared in any run. The layout code follows AMD's address library and
  has round-trip and hand-checked tests (`crates/victory-formats/src/texture.rs`).
  This does not show the texels are right: no reference image was compared.
* **The card's memory budget.** 9.6-10.4 GB used of 13.3-15.3 GB; "passes that
  made none for lack of room: 0" in every run but one, which logged it once.
* **Logging.** About 14,000 records in 14 minutes at info level.
* **Shader cache not working.** It hits on every run of an unchanged binary
  (38-41 new entries on the second to fifth run of one build).
* **The debug layer's remaining messages.** A view heap asked one descriptor
  over the limit (the runtime falls back to the limit), pipeline library
  names not stored yet, clear values that differ from creation (slower clear).

### 5.3 Unverified

**U1. Who commits the 11.2 GB of write-combined memory.** Resolved on
2026-11-24: see C13. Both candidates named here first (the command allocators
of 665-761 host lists; the driver pooling upload buffers) were wrong.

**U2. Whether paging causes the long texture refreshes.** While the world
streams in, texture refresh takes 1.1-4.9 s a frame at about 46 MB/s, with
110,000-170,000 page faults a frame and 1-2 GB of RAM free. Test: the same run
with other programs closed and 16 GB more commit, or a hard-fault count per
frame.

**U3. Dropped notices of changed texture memory.** `descriptor/host.rs`
`remapped` kept 32,768 ranges and dropped them all when full; 65,536 were
dropped in a run of 1.6 million. *Resolved 2026-11-24:* the list now counts
when it fills and whether a pass had taken the ranges within two frames. In
two runs it filled 2 and 4 times, never while a pass was running: the drops
happened while loading, as the old comment said, not in the world. The
ranges are now joined into wider ones instead of dropped (56,643 and 103,101
joined), so none is lost in either case. Ruled out as a cause of stale
textures in this scene.

**U4. Texel correctness.** BC formats, sRGB views, mip tails, arrays, cube
and 3D textures were not compared with reference data. None is in the tree
and none may be added from the title (asset policy). Needs captures from a
console, or synthetic textures run through a reference implementation.

**U5. Indirect execution.** "indirect execution dropped" for 3-4 signatures a
run (signatures that change root arguments are dropped, `list.rs`). What
those draws are is not known.

**U6. Queries, depth bounds, HDR, MSAA.** Marked as shortcuts in the code:
query results are plausible values, not measured (`list.rs`
`resolve_queries`); the depth bounds test is left out of host pipelines (4
pipelines enable it, 12 warnings a run); presentation is one plane without
scaling; layouts for MSAA and the XOR tile modes are missing. None was
measured for visible effect.

**U7. Release against diagnostics build, LTO.** Not measured.

**U8. Cost of making 90-160 upload buffers a second.** Counted, not timed.

The tree marks 157 known shortcuts with `shortcut:` (40 in the shader
lowering, 12 in `list.rs`, 11 in `draw.rs`). They were read for sections 5.1
and 5.3 but not each tested.

## 6. Correctness runs

| Run | Tools | Result |
|---|---|---|
| debuglayer | debug layer | 100 messages: 72 copy-state errors, 5 wave64, 2 blend, 2 heap size, warnings |
| debuglayer2 (after section 7) | debug layer | 18 messages: 5 wave64, 2 heap size, warnings; no copy-state or blend error |
| wavewide | debug layer, DRED, `VICTORY_WAVE_WIDE=1` | device hung at 135 s; breadcrumbs complete |
| t-gbv | debug layer, GPU-based validation, DRED | no error from the title's draws in the world (not reached in 300 s: patched shaders load slowly); one binding mismatch (C10) |

No page fault, invalid GPU address or stale descriptor was reported in any
run. GPU-based validation ran without state tracking, as the runtime sets it.

## 7. Changes already in the working tree (uncommitted)

Made before this triage was asked for. Each can be reverted on its own.

| Change | File | Evidence |
|---|---|---|
| Card buffer limits follow the card's budget from 12 GiB on | `shadow.rs` | C2; unit test. Costs up to 1.4 GB more commit than before (C13) |
| Isolated driver-memory test (ignored by default; prints, asserts nothing) | `tests/driver_memory.rs` | C13 |
| Fix 11: the device is made on the adapter the system rates fastest (software adapters passed over, the default adapter as fallback), and its name is logged | `host.rs` | log: "adapter=NVIDIA GeForce RTX 4080 SUPER vendor=0x10de dedicated_mb=16045 software=false"; 2 unit tests. Unverified on a PC with two GPUs |
| Fix 7 (pruning only): each build's translated shaders in a folder of its own; old-layout folders and builds beyond the four most lately used are removed at start | `shader_cache.rs` | 256 old folders (150 MB) removed, 0 failed; second run wrote 13 entries against 2,595 in the first; 5 unit tests. The key still holds the executable's hash: every rebuild starts cold, as before |
| Fix 10: change notices are joined into wider ranges when the list fills, not dropped; counted with whether a pass was running | `descriptor/host.rs`, `host.rs` | U3; 4 unit tests |

Fix 8 was not made: its premise was wrong (C10, corrected).

After fixes 7, 10 and 11, one run each: debug layer 51 messages, all of
kinds seen before (pipeline library names 40, clear values 5, wave64 4, heap
size 2), no error, no device loss. Warm benchmark: average 4.20 fps, p50
237 ms, p99 283 ms, max 578 ms, 1% low 2.63 fps, no frame over 1 s; private
23.5 GB; least left to commit 30.7 GB. None of the three should change frame
times, and the difference from the run before them (4.18 fps, p99 467 ms) is
within what two runs of one configuration differ by.

One test fails now and then: `inplace::tests::
warp_a_title_buffer_in_place_shows_its_bytes_at_its_offset` failed once in
about 20 runs of the suite and passed 20 of 20 alone. It decommits memory
right after letting go of a heap opened on it, on the software adapter. Not
touched by any change here; not fixed.
| Blending off for integer render targets in host pipelines | `pipeline/host.rs` | debug layer 676 gone, pipeline made; 4 unit tests |
| A texture copied onto itself is not recorded (it changes nothing on the console); logged once | `list.rs` | debug layer 538 gone (72 to 0). Note: this leaves an operation out; it is correct only because source and destination are the same host texture |
| Diagnostics overlay (`VICTORY_OVERLAY=1`) | `overlay.rs`, `host.rs`, `pipeline.rs` | seen on screen; 7 unit tests |
| Launcher turns on relay 2 and in place 1, `-Safe` and `-Overlay` | `scripts/play.ps1` | section 4 |
| Launcher found its binary under Windows PowerShell 5.1 | `scripts/play.ps1` | reproduced and fixed |

`cargo test -p victory-gpu`: 133 passed, 1 ignored (see the note on one
intermittent test below). `cargo clippy -p victory-gpu --all-targets`: clean.
`cargo xtask policy`: ok.

## 8. Proposed fixes, by expected impact

Not started. Each would be one change with its own before and after run.

| # | Fix | Addresses | Expected effect | Risk |
|---|---|---|---|---|
| 1 | ~~Find the owner of the write-combined commit, then bound it with fewer lists or pooled upload memory~~ Owner found (C13); the bound proposed here does not apply. Replaced by 1a-1c, none started | C3, C13 | | |
| 1a | Raise the PC's commit limit (paging file); document the figure in the README: title memory plus twice the bytes on the card. **Approved and set on 2026-11-24:** C: paging file fixed at 49,152 MB (was system-managed: 10.5 GB idle, grown to 23 GB in play), commit limit 79.7 GB after the restart. **Verified in one run after the restart** (launcher defaults, warm cache, same script): least left to commit 32.3 GB (before: 1.1-1.3 GB); process private bytes unchanged at 24.6 GB; average 4.18 fps, p50 225 ms, p99 467 ms, max 791 ms, 1% low 1.77 fps, no frame over 1 s (before: 3.61 fps, 238, 868, 1,108, 0.98, four frames). The frame-time gain cannot be put down to the paging file alone: the run followed a fresh boot with 21.5 GB of RAM free at its start against about 15 GB before. RAM is still short in the world: least free 0.6 GB | C3, C13 | removes the commit failure; no RAM cost measured (1.3 GB of 11 GB resident) | low; a system setting, the user's to change |
| 1b | Count commit in the two budgets that put bytes on the card (`resource::room`, `shadow::card_for`): stop adding when the PC has less than a set amount left to commit | C3, C13 | no process death at the limit | medium: textures wait instead (shown late or missing) when commit is short, which on this PC is always |
| 1c | Put back the 512 MiB limit for title buffers on the card while commit is short, keeping 1 MiB as the smallest | C2, C13 | about 1 GB of commit back | low; costs some of C2's gain (not measured apart) |
| 2 | Make the title's page pool a committed section and open windows in it in place | C3, C4 | 3.4-3.8 GB less copied memory, fewer refreshes | high: commits the pool up front; touches the title memory manager |
| 3 | Translate the ray-tracing structure builder's stages, or run a host builder in its place | C5 | lighting in shadow; most dropped work | high: large |
| 4 | Take the five wave64 programs without `VICTORY_WAVE_WIDE`: first identify the hanging program of C7 (one at a time, with DRED), then fix its form | C6, C7 | five pipelines | medium: device loss while testing |
| 5 | Texture streaming: uploads by need instead of a flat per-frame cap; keep host copies under the budget instead of a 5 s idle sweep | C9 | fewer missing or late textures, less re-upload | medium |
| 6 | Move texture refresh (read, hash, detile) off the submitting thread | 4.2 | up to 43-225 ms a frame | medium: ordering with submissions |
| 7 | Shader cache: prune entries of other builds; key on the translator's version instead of the executable's hash where that is safe | C8 | no cold start after an unrelated rebuild; bounded disk | low |
| 8 | Null views of the dimension the shader expects | C10 | one validation error; possibly wrong reads | low |
| 9 | Implement or identify the missing pipeline creation method | C11 | unknown | unknown |
| 10 | Count and time dropped change notices; make the list coalesce instead of drop | U3 | stale textures, if confirmed | low |
| 11 | Choose the adapter by performance preference and log its name | section 2 | correct GPU on hybrid PCs | low |
| 12 | Make relay 2 the runtime's default once it has run for hours, not minutes | C1 | 3x for every user | medium: needs a long soak |

## 9. Manual commands

Benchmark (diagnostics build; frame lines every 5 s in the log on stderr):

```powershell
cargo build --profile diag -p victory --features diagnostics
$env:VICTORY_KEYS = (15..330 | ? { $_ % 8 -eq 0 } | % { "$($_):Enter" }) -join ","
$env:VICTORY_LOG = "info,victory_gpu::frame=debug"
$env:VICTORY_RELAY = "2"; $env:VICTORY_INPLACE = "1"
target\diag\victory.exe run "<game folder>\<exe>" --execute 2> run.log
```

Correctness (needs the Windows optional feature "Graphics Tools"):

```powershell
$env:VICTORY_D3D12_DEBUG = "1"; $env:VICTORY_D3D12_DRED = "1"
$env:VICTORY_D3D12_GBV = "1"   # much slower; leave out for a first pass
target\diag\victory.exe run "<game folder>\<exe>" --execute 2> debug.log
Select-String -Path debug.log -Pattern " D3D12: "
```

Play with the overlay:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\play.ps1 -Game "<game folder>\<exe>" -Overlay
```
