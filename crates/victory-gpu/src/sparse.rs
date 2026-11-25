//! Host copies of title textures that take the card's memory only for the
//! levels the title has loaded.
//!
//! The title streams its textures: a texture's small levels are in its
//! memory first, its view names the first level that is there, and the
//! large levels arrive later or never. A host copy was made with every
//! level of the description all the same, as a committed texture: in the
//! world 20,000 such copies took 6.2 to 6.9 GB of the card, of which 3.6 to
//! 4.0 GB were levels whose title memory is not mapped (2,400 to 2,900
//! textures, almost all of them missing their largest level), while the
//! textures waiting for room on the card needed 1.2 to 1.8 GB.
//!
//! Here such a host copy is a reserved (tiled) texture: the same resource
//! with the same levels, so views, level numbers and states are what they
//! were, but with memory only behind the levels that were uploaded. When a
//! view asks for the levels from `first` on (`crate::texture::upload_from`),
//! the tiles of those levels are mapped to a heap of their own before the
//! upload is recorded; when a later view asks for larger levels, theirs are
//! mapped then, to another heap. A level that was never mapped reads as
//! zeros and takes nothing.
//!
//! The heaps belong to the texture object (its private data), not to what
//! holds it here: a descriptor slot or a list keeps a host texture past
//! the title's release of it, by the object alone, and a heap freed under
//! a texture a draw still samples takes the device with it (the first run
//! with heaps held beside the texture lost it after 170 s).
//!
//! Only textures the title fills, one 2-D slice with several levels and at
//! least [`TEXELS_LEAST`] texels in the largest, on a device that maps
//! tiles of textures (tier 2 or more). Anything the host copies into by
//! other ways than the upload gets all its levels mapped first
//! ([`Sparse::map_from`] with 0; `crate::list::ensure_state`).
//!
//! Measured in the world over 300 s (same save, the card's budget 13.3 to
//! 13.9 GB): 19,300 textures made this way took 2.9 GB; the textures waiting
//! for room went from 10,349 to 321 and the card's memory in use from 13.0
//! to 11.9 GB, with 0.8 GB more of the title's buffers on the card. Making
//! the heaps and mapping took 6.6 s of the run in all (0.3 ms a heap). The
//! frame rate was the same.
//!
//! `VICTORY_SPARSE_TEXTURES=0`: every host copy whole, as before.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;

/// A tile of a reserved resource.
const TILE: u64 = 64 << 10;

/// The texels of the largest level from which a texture is made sparse:
/// below, its largest level is a few tiles and what could be saved is not
/// worth a heap of its own.
const TEXELS_LEAST: u64 = 1024 * 512;

/// Since the start: sparse textures made, their tiles mapped, heaps made
/// for them, and textures that could not be made sparse and were committed.
pub(crate) static COUNTS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// The microseconds spent making heaps and mapping tiles since the start.
static MAPPING_US: AtomicU64 = AtomicU64::new(0);

/// The name a sparse texture keeps its `n`th heap under.
fn heap_key(n: u32) -> windows::core::GUID {
    windows::core::GUID::from_values(0x5a1c_0000 | (n & 0xffff), 0x7a3e, 0x4b1d, [0x9c, 0x41, 0x76, 0x69, 0x63, 0x74, 0x6f, 0x72])
}

/// Whether host copies are made sparse where they can be.
pub(crate) fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_SPARSE_TEXTURES").is_none_or(|v| v != "0"))
}

/// Whether host copies are made sparse on `device`: the switch is on and
/// the device can ([`supported`]; asked once, and said once).
pub(crate) fn ready(device: &ID3D12Device) -> bool {
    static CAN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    on()
        && *CAN.get_or_init(|| {
            let can = supported(device);
            tracing::info!(
                can,
                "host copies of large textures the title fills take the card's memory for the levels read only (VICTORY_SPARSE_TEXTURES=0: whole), if the device maps texture tiles"
            );
            can
        })
}

/// Whether the device maps tiles of textures with the guarantees used here
/// (tier 2: a tile that is not mapped reads as zeros, and levels of at
/// least a tile are not packed with the smaller ones).
pub(crate) fn supported(device: &ID3D12Device) -> bool {
    let mut options = D3D12_FEATURE_DATA_D3D12_OPTIONS::default();
    // SAFETY: a plain query into a structure of the size given.
    let asked = unsafe {
        device.CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS, (&raw mut options).cast(), size_of_val(&options) as u32)
    };
    asked.is_ok() && options.TiledResourcesTier.0 >= D3D12_TILED_RESOURCES_TIER_2.0
}

/// Whether a host texture of this description is made sparse: one 2-D
/// slice, several levels, one sample, large enough, and nothing the host
/// renders to or writes through unordered access.
pub(crate) fn wanted(desc: &D3D12_RESOURCE_DESC) -> bool {
    desc.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE2D
        && desc.DepthOrArraySize == 1
        && desc.MipLevels > 1
        && desc.SampleDesc.Count == 1
        && desc.Flags == D3D12_RESOURCE_FLAG_NONE
        && desc.Width * u64::from(desc.Height) >= TEXELS_LEAST
}

/// The tiles of a sparse texture: per level that has tiles of its own its
/// width and height in tiles, then the levels packed together and the
/// tiles they take as one.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tiling {
    standard: Vec<(u32, u32)>,
    packed_tiles: u32,
}

impl Tiling {
    /// The regions to map so that the levels from `first` on are mapped,
    /// when those from `mapped` on are already (`mapped` the number of
    /// levels with tiles of their own and one more: nothing is): (level or
    /// packed group, tiles), largest level first, and the tiles in all.
    fn regions(&self, first: u32, mapped: u32) -> (Vec<(u32, u32)>, u32) {
        let levels = self.standard.len() as u32;
        let first = first.min(levels);
        let mut out = Vec::new();
        for level in first..mapped.min(levels) {
            let (w, h) = self.standard[level as usize];
            out.push((level, w * h));
        }
        // The packed levels with the first mapping, whatever it asks for.
        if mapped > levels && self.packed_tiles > 0 {
            out.push((levels, self.packed_tiles));
        }
        let tiles = out.iter().map(|r| r.1).sum();
        (out, tiles)
    }

    /// What `mapped` is while nothing is mapped.
    fn nothing(&self) -> u32 {
        self.standard.len() as u32 + 1
    }
}

/// A sparse host texture's memory: which levels have it, and the heaps.
pub(crate) struct Sparse {
    tiling: Tiling,
    /// The first level that is mapped ([`Tiling::nothing`]: none), and how
    /// many heaps the mapped tiles are in (the texture object holds them).
    state: Mutex<(u32, u32)>,
    /// The bytes of those heaps.
    bytes: AtomicU64,
}

// SAFETY: D3D12 objects are free-threaded; the rest is behind a lock.
unsafe impl Send for Sparse {}
// SAFETY: as above.
unsafe impl Sync for Sparse {}

impl Sparse {
    /// The bytes of the card the texture takes now.
    #[cfg(test)]
    fn bytes(&self) -> u64 {
        self.bytes.load(Relaxed)
    }

    /// Maps the levels from `first` on that are not mapped yet, on `queue`,
    /// before anything recorded from now on runs there. Returns the bytes
    /// newly taken; 0 if they were mapped already or no heap could be made
    /// (the levels then read as zeros and what is copied into them is
    /// dropped, as for a level that was never asked for).
    pub(crate) fn map_from(&self, device: &ID3D12Device, queue: &ID3D12CommandQueue, texture: &ID3D12Resource, first: u32) -> u64 {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (regions, tiles) = self.tiling.regions(first, state.0);
        if tiles == 0 {
            return 0;
        }
        let began = std::time::Instant::now();
        let bytes = u64::from(tiles) * TILE;
        let desc = D3D12_HEAP_DESC {
            SizeInBytes: bytes,
            Properties: D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() },
            Alignment: 0,
            Flags: D3D12_HEAP_FLAG_ALLOW_ONLY_NON_RT_DS_TEXTURES,
        };
        let mut heap: Option<ID3D12Heap> = None;
        // SAFETY: plain creation with a valid description.
        if let Err(e) = unsafe { device.CreateHeap(&desc, &mut heap) } {
            tracing::warn!(error = %e, bytes, "no heap for a sparse texture's levels: they stay unmapped");
            return 0;
        }
        let Some(heap) = heap else { return 0 };
        // The texture object holds the heap from here on, and lets go of
        // it when it is destroyed itself.
        // SAFETY: a live texture and heap; the key is this texture's own.
        if let Err(e) = unsafe { texture.SetPrivateDataInterface(&heap_key(state.1), &heap) } {
            tracing::warn!(error = %e, "a sparse texture could not be given its heap to hold: the levels stay unmapped");
            return 0;
        }
        let levels = self.tiling.standard.len() as u32;
        let coordinates: Vec<D3D12_TILED_RESOURCE_COORDINATE> =
            regions.iter().map(|&(level, _)| D3D12_TILED_RESOURCE_COORDINATE { X: 0, Y: 0, Z: 0, Subresource: level }).collect();
        let sizes: Vec<D3D12_TILE_REGION_SIZE> = regions
            .iter()
            .map(|&(level, tiles)| match self.tiling.standard.get(level as usize) {
                Some(&(w, h)) if level < levels => D3D12_TILE_REGION_SIZE {
                    NumTiles: tiles,
                    UseBox: true.into(),
                    Width: w,
                    Height: h as u16,
                    Depth: 1,
                },
                // The packed levels: so many tiles from the group's first.
                _ => D3D12_TILE_REGION_SIZE { NumTiles: tiles, UseBox: false.into(), Width: 0, Height: 0, Depth: 0 },
            })
            .collect();
        let (flags, offsets, counts) = ([D3D12_TILE_RANGE_FLAG_NONE], [0u32], [tiles]);
        // SAFETY: a live reserved texture and heap; the regions are the
        // texture's own levels as its tiling gave them, and the one range
        // is the whole heap, which holds exactly their tiles.
        unsafe {
            queue.UpdateTileMappings(
                texture,
                regions.len() as u32,
                Some(coordinates.as_ptr()),
                Some(sizes.as_ptr()),
                &heap,
                1,
                Some(flags.as_ptr()),
                Some(offsets.as_ptr()),
                Some(counts.as_ptr()),
                D3D12_TILE_MAPPING_FLAG_NONE,
            );
        }
        state.0 = state.0.min(first.min(levels));
        state.1 += 1;
        self.bytes.fetch_add(bytes, Relaxed);
        COUNTS[1].fetch_add(u64::from(tiles), Relaxed);
        let us = MAPPING_US.fetch_add(began.elapsed().as_micros() as u64, Relaxed);
        let heaps = COUNTS[2].fetch_add(1, Relaxed) + 1;
        if heaps.is_multiple_of(500) {
            let [made, tiles, _, refused] = COUNTS.each_ref().map(|c| c.load(Relaxed));
            tracing::info!(
                "sparse host textures since the start: {made} made, {tiles} tiles mapped ({} MB) in {heaps} heaps, {} ms making and mapping them, {refused} the host refused (made whole)",
                (tiles * 64) >> 10,
                us / 1000
            );
        }
        bytes
    }
}

/// A sparse texture of description `desc` (a description [`wanted`] says
/// yes to) in `state`, with nothing mapped. `None` if the device refuses
/// it or packs levels in a way this does not handle: the caller then makes
/// a committed texture, as before.
pub(crate) fn create(device: &ID3D12Device, desc: &D3D12_RESOURCE_DESC, state: D3D12_RESOURCE_STATES) -> Option<(ID3D12Resource, Sparse)> {
    let mut d = *desc;
    d.Layout = D3D12_TEXTURE_LAYOUT_64KB_UNDEFINED_SWIZZLE;
    d.Alignment = 0;
    d.SampleDesc = DXGI_SAMPLE_DESC { Count: 1, Quality: 0 };
    let mut texture: Option<ID3D12Resource> = None;
    // SAFETY: plain creation with a valid description; the out pointer is
    // ours.
    let made = unsafe { device.CreateReservedResource(&d, state, None, &mut texture) };
    let texture = match (made, texture) {
        (Ok(()), Some(texture)) => texture,
        _ => {
            COUNTS[3].fetch_add(1, Relaxed);
            return None;
        }
    };
    let levels = u32::from(d.MipLevels);
    let mut packed = D3D12_PACKED_MIP_INFO::default();
    let mut tilings = vec![D3D12_SUBRESOURCE_TILING::default(); levels as usize];
    let mut count = levels;
    // SAFETY: outputs for the texture's `levels` subresources.
    unsafe {
        device.GetResourceTiling(&texture, None, Some(&mut packed), None, Some(&mut count), 0, tilings.as_mut_ptr());
    }
    let standard = u32::from(packed.NumStandardMips);
    // Every level is either one with tiles of its own or packed.
    if standard + u32::from(packed.NumPackedMips) != levels || standard > count {
        COUNTS[3].fetch_add(1, Relaxed);
        return None;
    }
    let tiling = Tiling {
        standard: tilings[..standard as usize].iter().map(|t| (t.WidthInTiles, u32::from(t.HeightInTiles))).collect(),
        packed_tiles: packed.NumTilesForPackedMips,
    };
    COUNTS[0].fetch_add(1, Relaxed);
    let nothing = tiling.nothing();
    Some((texture, Sparse { tiling, state: Mutex::new((nothing, 0)), bytes: AtomicU64::new(0) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
    use windows::core::Interface;

    /// 2048x2048 of four bytes a texel: levels of 256, 64, 16, 4 and 1
    /// tiles, the rest packed in one.
    fn tiling() -> Tiling {
        Tiling { standard: vec![(16, 16), (8, 8), (4, 4), (2, 2), (1, 1)], packed_tiles: 1 }
    }

    /// The first mapping takes the levels asked for and the packed ones;
    /// a later one for larger levels takes just those.
    #[test]
    fn levels_are_mapped_once_each_from_the_first_asked_for() {
        let t = tiling();
        let nothing = t.nothing();
        assert_eq!(t.regions(3, nothing), (vec![(3, 4), (4, 1), (5, 1)], 6));
        assert_eq!(t.regions(1, 3), (vec![(1, 64), (2, 16)], 80));
        assert_eq!(t.regions(0, 1), (vec![(0, 256)], 256));
        // Already there, or smaller levels than those mapped.
        assert_eq!(t.regions(0, 0), (vec![], 0));
        assert_eq!(t.regions(4, 3), (vec![], 0));
    }

    /// A view of packed levels only maps the packed group; whole from the
    /// start maps everything.
    #[test]
    fn packed_levels_come_as_one() {
        let t = tiling();
        let nothing = t.nothing();
        assert_eq!(t.regions(9, nothing), (vec![(5, 1)], 1));
        assert_eq!(t.regions(5, nothing), (vec![(5, 1)], 1));
        assert_eq!(t.regions(0, nothing).1, 256 + 64 + 16 + 4 + 1 + 1);
        // After the packed group alone, the levels with tiles of their own.
        assert_eq!(t.regions(4, 5), (vec![(4, 1)], 1));
    }

    #[test]
    fn only_large_plain_2d_textures_with_levels_are_wanted() {
        let base = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: 2048,
            Height: 2048,
            DepthOrArraySize: 1,
            MipLevels: 12,
            Format: DXGI_FORMAT_BC7_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            ..Default::default()
        };
        assert!(wanted(&base));
        assert!(!wanted(&D3D12_RESOURCE_DESC { MipLevels: 1, ..base }));
        assert!(!wanted(&D3D12_RESOURCE_DESC { DepthOrArraySize: 6, ..base }));
        assert!(!wanted(&D3D12_RESOURCE_DESC { Width: 512, Height: 512, ..base }));
        assert!(!wanted(&D3D12_RESOURCE_DESC { Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, ..base }));
        assert!(!wanted(&D3D12_RESOURCE_DESC { Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE3D, ..base }));
    }

    /// A device, a queue and what one list needs, for the test on the card.
    struct Rig {
        device: ID3D12Device,
        queue: ID3D12CommandQueue,
        allocator: ID3D12CommandAllocator,
        list: ID3D12GraphicsCommandList,
        fence: ID3D12Fence,
        event: HANDLE,
        sent: u64,
    }

    impl Rig {
        fn new() -> Option<Rig> {
            // SAFETY: plain object creation with valid descriptions.
            unsafe {
                let mut device: Option<ID3D12Device> = None;
                D3D12CreateDevice(None, D3D_FEATURE_LEVEL_12_0, &mut device).ok()?;
                let device = device?;
                if !supported(&device) {
                    return None;
                }
                let queue: ID3D12CommandQueue = device
                    .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })
                    .ok()?;
                let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).ok()?;
                let list: ID3D12GraphicsCommandList =
                    device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None).ok()?;
                list.Close().ok()?;
                let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).ok()?;
                let event = CreateEventW(None, false, false, None).ok()?;
                Some(Rig { device, queue, allocator, list, fence, event, sent: 0 })
            }
        }

        fn buffer(&self, bytes: u64, heap: D3D12_HEAP_TYPE, state: D3D12_RESOURCE_STATES) -> ID3D12Resource {
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: bytes,
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                ..Default::default()
            };
            let mut made: Option<ID3D12Resource> = None;
            // SAFETY: plain creation.
            unsafe {
                self.device
                    .CreateCommittedResource(
                        &D3D12_HEAP_PROPERTIES { Type: heap, ..Default::default() },
                        D3D12_HEAP_FLAG_NONE,
                        &desc,
                        state,
                        None,
                        &mut made,
                    )
                    .unwrap();
            }
            made.unwrap()
        }

        /// Records with `record`, runs the list and waits for it.
        fn run(&mut self, record: impl FnOnce(&ID3D12GraphicsCommandList)) {
            // SAFETY: our own list, closed and not running.
            unsafe {
                self.allocator.Reset().unwrap();
                self.list.Reset(&self.allocator, None).unwrap();
                record(&self.list);
                self.list.Close().unwrap();
                let run: ID3D12CommandList = self.list.cast().unwrap();
                self.queue.ExecuteCommandLists(&[Some(run)]);
                self.sent += 1;
                self.queue.Signal(&self.fence, self.sent).unwrap();
                self.fence.SetEventOnCompletion(self.sent, self.event).unwrap();
                WaitForSingleObject(self.event, INFINITE);
                self.device.GetDeviceRemovedReason().unwrap();
            }
        }

        /// Fills level `level` of `texture` with `byte` in every channel.
        fn fill(&mut self, texture: &ID3D12Resource, desc: &D3D12_RESOURCE_DESC, level: u32, byte: u8) {
            let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
            let (mut rows, mut total) = (0u32, 0u64);
            // SAFETY: outputs for one subresource.
            unsafe { self.device.GetCopyableFootprints(desc, level, 1, 0, Some(&mut footprint), Some(&mut rows), None, Some(&mut total)) };
            let upload = self.buffer(total, D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_STATE_GENERIC_READ);
            // SAFETY: the mapping holds `total` bytes.
            unsafe {
                let mut data = std::ptr::null_mut();
                upload.Map(0, None, Some(&mut data)).unwrap();
                std::ptr::write_bytes(data.cast::<u8>(), byte, total as usize);
                upload.Unmap(0, None);
            }
            let texture = texture.clone();
            self.run(|l| {
                let to = D3D12_TEXTURE_COPY_LOCATION {
                    // SAFETY: a borrowed pointer the location never releases.
                    pResource: unsafe { std::mem::transmute_copy(&texture) },
                    Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: level },
                };
                let from = D3D12_TEXTURE_COPY_LOCATION {
                    // SAFETY: as above.
                    pResource: unsafe { std::mem::transmute_copy(&upload) },
                    Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: footprint },
                };
                // SAFETY: an open list; both locations name live resources.
                unsafe { l.CopyTextureRegion(&to, 0, 0, 0, &from, None) };
            });
        }

        /// The first row of level `level` of `texture`, read back.
        fn row(&mut self, texture: &ID3D12Resource, desc: &D3D12_RESOURCE_DESC, level: u32) -> Vec<u8> {
            let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
            let (mut rows, mut row_bytes, mut total) = (0u32, 0u64, 0u64);
            // SAFETY: outputs for one subresource.
            unsafe {
                self.device.GetCopyableFootprints(desc, level, 1, 0, Some(&mut footprint), Some(&mut rows), Some(&mut row_bytes), Some(&mut total))
            };
            let back = self.buffer(total, D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_STATE_COPY_DEST);
            let (texture, target) = (texture.clone(), back.clone());
            self.run(|l| {
                let from = D3D12_TEXTURE_COPY_LOCATION {
                    // SAFETY: a borrowed pointer the location never releases.
                    pResource: unsafe { std::mem::transmute_copy(&texture) },
                    Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: level },
                };
                let to = D3D12_TEXTURE_COPY_LOCATION {
                    // SAFETY: as above.
                    pResource: unsafe { std::mem::transmute_copy(&target) },
                    Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: footprint },
                };
                // SAFETY: an open list; both locations name live resources.
                unsafe {
                    l.ResourceBarrier(&[crate::host::transition(&texture, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
                    l.CopyTextureRegion(&to, 0, 0, 0, &from, None);
                    l.ResourceBarrier(&[crate::host::transition(&texture, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COPY_DEST)]);
                }
            });
            // SAFETY: the mapping holds the level's first row.
            unsafe {
                let mut data = std::ptr::null_mut();
                back.Map(0, None, Some(&mut data)).unwrap();
                let row = std::slice::from_raw_parts(data.cast::<u8>(), row_bytes as usize).to_vec();
                back.Unmap(0, None);
                row
            }
        }
    }

    /// On the card: a sparse texture takes memory for the levels mapped and
    /// no more, what is uploaded to a mapped level is there, a level that
    /// is not mapped reads as zeros, and mapping larger levels later leaves
    /// the smaller ones as they are. Skipped without a card that maps
    /// tiles.
    #[test]
    fn warp_or_card_a_sparse_texture_holds_what_is_uploaded_to_its_mapped_levels() {
        let Some(mut rig) = Rig::new() else {
            eprintln!("no device that maps texture tiles (tier 2); skipped");
            return;
        };
        for format in [DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_BC7_UNORM] {
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                Width: 2048,
                Height: 2048,
                DepthOrArraySize: 1,
                MipLevels: 12,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                ..Default::default()
            };
            assert!(wanted(&desc));
            let (texture, sparse) = create(&rig.device, &desc, D3D12_RESOURCE_STATE_COPY_DEST).expect("a reserved texture");
            assert_eq!(sparse.bytes(), 0);
            // SAFETY: a plain query of a live committed-size description.
            let whole = unsafe { rig.device.GetResourceAllocationInfo(0, &[desc]) }.SizeInBytes;

            // The levels from 3 on.
            let small = sparse.map_from(&rig.device, &rig.queue, &texture, 3);
            assert!(small > 0 && small < whole / 8, "levels from 3 on took {small} of {whole}");
            assert_eq!(sparse.map_from(&rig.device, &rig.queue, &texture, 3), 0, "mapped twice");
            assert_eq!(sparse.map_from(&rig.device, &rig.queue, &texture, 5), 0, "smaller levels mapped again");
            rig.fill(&texture, &desc, 3, 0x5A);
            rig.fill(&texture, &desc, 7, 0x33);
            assert!(rig.row(&texture, &desc, 3).iter().all(|&b| b == 0x5A));
            assert!(rig.row(&texture, &desc, 7).iter().all(|&b| b == 0x33));
            // The largest level has no memory: zeros.
            assert!(rig.row(&texture, &desc, 0).iter().all(|&b| b == 0), "an unmapped level did not read as zeros");

            // Then all of it.
            let rest = sparse.map_from(&rig.device, &rig.queue, &texture, 0);
            assert!(rest > small);
            assert!(sparse.bytes() <= whole + 4 * TILE, "{} mapped of {whole} committed", sparse.bytes());
            rig.fill(&texture, &desc, 0, 0xC3);
            assert!(rig.row(&texture, &desc, 0).iter().all(|&b| b == 0xC3));
            assert!(rig.row(&texture, &desc, 3).iter().all(|&b| b == 0x5A), "a level mapped before changed");
            eprintln!("format {}: levels from 3 on {small} bytes, all {} bytes, committed {whole} bytes", format.0, sparse.bytes());
        }
    }
}
