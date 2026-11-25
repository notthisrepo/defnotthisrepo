//! The card's time by the kind of list it ran (`VICTORY_CARD_CLOCK=1`,
//! diagnostics).
//!
//! With submissions sent ahead the frame line's "on the GPU" is one wait a
//! frame for everything in the air, and the timing run
//! (`VICTORY_GPU_TIMING`) waits for every list by itself, which the card
//! never does otherwise: of 257 ms a frame there, the title's commands were
//! 106, and the rest was the layer's own lists and some two hundred waits.
//! Neither says what the card's time is made of in a run as the title is
//! played.
//!
//! Here every list the layer sends has a timestamp where it starts and one
//! where it ends, by its kind: the list that sends title bytes up to the
//! copies in the card's memory and hashes them, a title list's prelude
//! (states, the clamps of its indirect records), the title's list itself
//! (with the passes of ours recorded inside it), the compare of what its
//! shaders wrote, and the fetch of the pieces that differ. The line gives
//! each kind's milliseconds a frame and how many lists they were in. The
//! card runs one list of the queue at a time, so the kinds add up to the
//! time it was running lists of ours; what is missing to the frame is the
//! card idle, the present, and other programs.
//!
//! Two queries and a resolve of sixteen bytes a list: a few hundred a
//! frame.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;

/// The kinds of list.
pub(crate) const SEND: usize = 0;
pub(crate) const PRELUDE: usize = 1;
pub(crate) const TITLE: usize = 2;
pub(crate) const COMPARE: usize = 3;
pub(crate) const FETCH: usize = 4;
const NAMES: [&str; 5] = [
    "bytes sent up to copies in the card's memory, and their hashes",
    "preludes (states, clamps of indirect records)",
    "the title's lists",
    "compares of what shaders wrote",
    "fetches of the pieces that differ",
];

/// Lists with their two timestamps kept at a time: more in the air or not
/// read yet take the oldest's place.
const PAIRS: u32 = 32_768;

/// How long after it was recorded a list's timestamps are read: by then it
/// has run, or it never will.
const AFTER: std::time::Duration = std::time::Duration::from_secs(2);

struct Clock {
    heap: ID3D12QueryHeap,
    back: ID3D12Resource,
    /// The read-back buffer, mapped: two ticks a pair.
    data: *const u64,
    /// Ticks a second.
    frequency: u64,
    next: AtomicU32,
    pending: Mutex<Pending>,
}

struct Pending {
    /// Pairs recorded and not read yet: the pair, its kind, when.
    waiting: VecDeque<(u32, usize, std::time::Instant)>,
    /// Per pair, the first tick read from it the last time: the same again
    /// is a list that was recorded and never ran.
    seen: Vec<u64>,
}

// SAFETY: D3D12 objects are free-threaded; the mapping is only read.
unsafe impl Send for Clock {}
// SAFETY: as above.
unsafe impl Sync for Clock {}

fn clock() -> Option<&'static Clock> {
    static CLOCK: OnceLock<Option<Clock>> = OnceLock::new();
    CLOCK
        .get_or_init(|| {
            if std::env::var_os("VICTORY_CARD_CLOCK").is_none_or(|v| v != "1") {
                return None;
            }
            let host = crate::host::host()?;
            let mut heap: Option<ID3D12QueryHeap> = None;
            let mut back: Option<ID3D12Resource> = None;
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: u64::from(PAIRS) * 16,
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                ..Default::default()
            };
            // SAFETY: plain object creation; the buffer stays mapped for the
            // run and is only read here.
            unsafe {
                host.device
                    .CreateQueryHeap(
                        &D3D12_QUERY_HEAP_DESC { Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP, Count: PAIRS * 2, NodeMask: 0 },
                        &mut heap,
                    )
                    .ok()?;
                host.device
                    .CreateCommittedResource(
                        &D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_READBACK, ..Default::default() },
                        D3D12_HEAP_FLAG_NONE,
                        &desc,
                        D3D12_RESOURCE_STATE_COPY_DEST,
                        None,
                        &mut back,
                    )
                    .ok()?;
                let (heap, back) = (heap?, back?);
                let mut data = std::ptr::null_mut();
                back.Map(0, None, Some(&mut data)).ok()?;
                let frequency = host.queue().GetTimestampFrequency().ok()?.max(1);
                tracing::info!("VICTORY_CARD_CLOCK=1: every list of ours is timed on the card, by its kind");
                Some(Clock {
                    heap,
                    back,
                    data: data.cast::<u64>().cast_const(),
                    frequency,
                    next: AtomicU32::new(0),
                    pending: Mutex::new(Pending { waiting: VecDeque::new(), seen: vec![0; PAIRS as usize] }),
                })
            }
        })
        .as_ref()
}

/// A list of `kind` starts here: `list` is open and nothing of it is
/// recorded yet. The token for [`end`]; 0 without the clock.
pub(crate) fn begin(list: &ID3D12GraphicsCommandList, kind: usize) -> u32 {
    let Some(c) = clock() else { return 0 };
    let pair = c.next.fetch_add(1, Relaxed) % PAIRS;
    // SAFETY: an open list and our query heap.
    unsafe { list.EndQuery(&c.heap, D3D12_QUERY_TYPE_TIMESTAMP, pair * 2) };
    c.pending.lock().unwrap_or_else(|e| e.into_inner()).waiting.push_back((pair, kind, std::time::Instant::now()));
    pair + 1
}

/// The list [`begin`] gave `token` for ends here: it is closed next.
pub(crate) fn end(list: &ID3D12GraphicsCommandList, token: u32) {
    let (Some(c), Some(pair)) = (clock(), token.checked_sub(1)) else {
        return;
    };
    // SAFETY: an open list, our heap and read-back buffer; the pair's two
    // queries are this list's.
    unsafe {
        list.EndQuery(&c.heap, D3D12_QUERY_TYPE_TIMESTAMP, pair * 2 + 1);
        list.ResolveQueryData(&c.heap, D3D12_QUERY_TYPE_TIMESTAMP, pair * 2, 2, &c.back, u64::from(pair) * 16);
    }
}

/// The line, over the lists recorded up to two seconds ago and not read
/// before; `frames` and `ms` are the frame line's (frames since the last
/// one, milliseconds a frame).
pub(crate) fn line(frames: f64, ms: f64) {
    let Some(c) = clock() else { return };
    let mut sums = [(0u64, 0u64); 5];
    let (mut never, mut first, mut last) = (0u64, u64::MAX, 0u64);
    {
        let mut pending = c.pending.lock().unwrap_or_else(|e| e.into_inner());
        while pending.waiting.front().is_some_and(|w| w.2.elapsed() >= AFTER) {
            let Some((pair, kind, _)) = pending.waiting.pop_front() else { break };
            // SAFETY: inside the mapped buffer; the card wrote the two
            // ticks whole, or not yet.
            let (from, to) = unsafe { (c.data.add(pair as usize * 2).read_volatile(), c.data.add(pair as usize * 2 + 1).read_volatile()) };
            if from == 0 || from == pending.seen[pair as usize] || to < from {
                never += 1;
                continue;
            }
            pending.seen[pair as usize] = from;
            sums[kind].0 += 1;
            sums[kind].1 += to - from;
            first = first.min(from);
            last = last.max(to);
        }
    }
    let to_ms = |ticks: u64| ticks as f64 * 1e3 / c.frequency as f64;
    let all: u64 = sums.iter().map(|s| s.1).sum();
    if all == 0 {
        return;
    }
    let span = to_ms(last.saturating_sub(first)).max(1.0);
    // The frames those lists were in, by the frame line's rate.
    let in_frames = (span / ms.max(1.0)).max(1.0);
    let parts: Vec<String> = sums
        .iter()
        .zip(NAMES)
        .map(|(&(n, ticks), name)| format!("{name} {:.1} ms in {:.0} lists", to_ms(ticks) / in_frames, n as f64 / in_frames))
        .collect();
    tracing::info!(
        "the card's time by list per frame (VICTORY_CARD_CLOCK=1), over {span:.0} ms of the card's clock ({in_frames:.1} frames of {ms:.0} ms): {}; in all {:.1} ms a frame, {:.0}% of that time; {never} lists recorded and not run (or not yet); the frame line counts {frames:.0} frames",
        parts.join("; "),
        to_ms(all) / in_frames,
        to_ms(all) * 100.0 / span
    );
}
