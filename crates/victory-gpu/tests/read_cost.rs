//! What it costs the card to read a buffer that is in the PC's memory, not
//! its own: measured on the real adapter, with no title.
//!
//! The runtime keeps most host copies of title memory in the PC's memory (a
//! custom heap with write-back pages in pool L0, `Host::shared_buffer`; a
//! heap opened on title memory in place is the same kind), so that our code
//! can fill and compare them, and the card reads them over the bus. In the
//! world about 3.8 GB of what shaders read was there and 0.9 GB in the
//! card's memory, and a draw took 15 to 40 microseconds on the card whatever
//! it drew. This says what part of that the place of the bytes is.
//!
//! Each variant is a batch of compute passes timed on the card by one pair
//! of timestamps around the whole batch. A pass is groups of 64 threads;
//! each thread adds up `words` dwords in a row from a place of its own in a
//! 256 MiB buffer (another place for every pass: nothing read twice), as a
//! mesh program reads its vertices. The same batch runs on a buffer in the
//! card's memory, on one made as the runtime's shared buffers are, and on
//! one in the upload heap.
//!
//! Not a test of our code and it asserts nothing: it prints. Ignored by
//! default (it needs a hardware adapter):
//!
//! ```text
//! cargo test -p victory-gpu --release --test read_cost -- --ignored --nocapture
//! ```
#![cfg(windows)]

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{D3D_FEATURE_LEVEL_12_0, ID3DBlob};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
use windows::core::{Interface, s};

/// The bytes of each buffer read.
const BYTES: u64 = 256 << 20;

const SOURCE: &str = r#"
#define RS "RootConstants(num32BitConstants=4, b0), SRV(t0), UAV(u0)"
cbuffer Batch : register(b0) {
    uint seed;
    uint words;
    uint span;
    uint unused;
};
ByteAddressBuffer data : register(t0);
RWByteAddressBuffer sums : register(u0);
[RootSignature(RS)]
[numthreads(64, 1, 1)]
void read(uint3 id : SV_DispatchThreadID) {
    // A place of the thread's own, another for every pass.
    uint h = (seed * 747796405u + id.x * 2891336453u) ^ 0x9e3779b9u;
    h ^= h >> 16;
    h *= 0x7feb352du;
    h ^= h >> 15;
    uint at = (h % (span - words * 4u)) & ~3u;
    uint sum = 0;
    for (uint i = 0; i < words; i++) {
        sum += data.Load(at + i * 4u);
    }
    // Every thread's sum is kept, so that no read is left out.
    sums.Store((id.x & 63u) * 4u, sum);
}
"#;

fn buffer_desc(bytes: u64, flags: D3D12_RESOURCE_FLAGS) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: bytes,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: flags,
        ..Default::default()
    }
}

/// A committed buffer; one our code can map is written from end to end, so
/// that every page of it is there.
fn buffer(
    device: &ID3D12Device,
    bytes: u64,
    heap: D3D12_HEAP_PROPERTIES,
    flags: D3D12_RESOURCE_FLAGS,
    state: D3D12_RESOURCE_STATES,
    fill: bool,
) -> windows::core::Result<ID3D12Resource> {
    let mut made: Option<ID3D12Resource> = None;
    // SAFETY: plain creation; the mapping holds `bytes` bytes.
    unsafe {
        device.CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &buffer_desc(bytes, flags), state, None, &mut made)?;
        let buffer = made.ok_or_else(windows::core::Error::empty)?;
        if fill {
            let mut data = std::ptr::null_mut();
            buffer.Map(0, None, Some(&mut data))?;
            let words = std::slice::from_raw_parts_mut(data.cast::<u32>(), (bytes / 4) as usize);
            for (i, w) in words.iter_mut().enumerate() {
                *w = i as u32;
            }
            buffer.Unmap(0, None);
        }
        Ok(buffer)
    }
}

struct Rig {
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    event: HANDLE,
    sent: u64,
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
    sums: ID3D12Resource,
    stamps: ID3D12QueryHeap,
    back: ID3D12Resource,
}

impl Rig {
    fn new() -> windows::core::Result<Rig> {
        // SAFETY: plain object creation with valid descriptions; the
        // shader's source and names live through the call.
        unsafe {
            let mut device: Option<ID3D12Device> = None;
            D3D12CreateDevice(None, D3D_FEATURE_LEVEL_12_0, &mut device)?;
            let device = device.ok_or_else(windows::core::Error::empty)?;
            let queue: ID3D12CommandQueue = device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                ..Default::default()
            })?;
            let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
            let list: ID3D12GraphicsCommandList =
                device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)?;
            list.Close()?;
            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            let event = CreateEventW(None, false, false, None)?;

            let mut code: Option<ID3DBlob> = None;
            let mut errors: Option<ID3DBlob> = None;
            let compiled = D3DCompile(
                SOURCE.as_ptr().cast(),
                SOURCE.len(),
                s!("read"),
                None,
                None,
                s!("read"),
                s!("cs_5_1"),
                0,
                0,
                &mut code,
                Some(&mut errors),
            );
            if let Err(e) = compiled {
                if let Some(errors) = errors {
                    let text = std::slice::from_raw_parts(errors.GetBufferPointer().cast::<u8>(), errors.GetBufferSize());
                    eprintln!("the read program did not compile: {}", String::from_utf8_lossy(text));
                }
                return Err(e);
            }
            let code = code.ok_or_else(windows::core::Error::empty)?;
            let bytes = std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize());
            let root: ID3D12RootSignature = device.CreateRootSignature(0, bytes)?;
            let pso: ID3D12PipelineState = device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                CS: D3D12_SHADER_BYTECODE { pShaderBytecode: bytes.as_ptr().cast(), BytecodeLength: bytes.len() },
                ..Default::default()
            })?;

            let card = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() };
            let sums = buffer(
                &device,
                64 << 10,
                card,
                D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                false,
            )?;
            let back = buffer(
                &device,
                16,
                D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_READBACK, ..Default::default() },
                D3D12_RESOURCE_FLAG_NONE,
                D3D12_RESOURCE_STATE_COPY_DEST,
                false,
            )?;
            let mut stamps: Option<ID3D12QueryHeap> = None;
            device.CreateQueryHeap(
                &D3D12_QUERY_HEAP_DESC { Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP, Count: 2, NodeMask: 0 },
                &mut stamps,
            )?;
            let stamps = stamps.ok_or_else(windows::core::Error::empty)?;
            Ok(Rig { device, queue, allocator, list, fence, event, sent: 0, root, pso, sums, stamps, back })
        }
    }

    /// `passes` passes of `groups` groups, each thread reading `words`
    /// dwords of `data`: the milliseconds the batch took on the card.
    fn batch(&mut self, data: &ID3D12Resource, passes: u32, groups: u32, words: u32, first_seed: u32) -> windows::core::Result<f64> {
        // SAFETY: our own list, closed and not running; live buffers; the
        // read-back buffer holds the two timestamps resolved into it.
        unsafe {
            self.allocator.Reset()?;
            self.list.Reset(&self.allocator, None)?;
            let l = &self.list;
            l.SetComputeRootSignature(&self.root);
            l.SetPipelineState(&self.pso);
            l.SetComputeRootShaderResourceView(1, data.GetGPUVirtualAddress());
            l.SetComputeRootUnorderedAccessView(2, self.sums.GetGPUVirtualAddress());
            l.EndQuery(&self.stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0);
            for pass in 0..passes {
                let constants = [first_seed.wrapping_add(pass), words, BYTES as u32, 0];
                l.SetComputeRoot32BitConstants(0, 4, constants.as_ptr().cast(), 0);
                l.Dispatch(groups, 1, 1);
            }
            l.EndQuery(&self.stamps, D3D12_QUERY_TYPE_TIMESTAMP, 1);
            l.ResolveQueryData(&self.stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0, 2, &self.back, 0);
            l.Close()?;
            let run: ID3D12CommandList = l.cast()?;
            self.queue.ExecuteCommandLists(&[Some(run)]);
            self.sent += 1;
            self.queue.Signal(&self.fence, self.sent)?;
            self.fence.SetEventOnCompletion(self.sent, self.event)?;
            WaitForSingleObject(self.event, INFINITE);
            self.device.GetDeviceRemovedReason()?;
            let mut at = std::ptr::null_mut();
            self.back.Map(0, Some(&D3D12_RANGE { Begin: 0, End: 16 }), Some(&mut at))?;
            let stamps = std::slice::from_raw_parts(at.cast::<u64>(), 2).to_vec();
            self.back.Unmap(0, None);
            let frequency = self.queue.GetTimestampFrequency().unwrap_or(1).max(1);
            Ok(stamps[1].saturating_sub(stamps[0]) as f64 * 1e3 / frequency as f64)
        }
    }
}

#[test]
#[ignore = "a measurement on the card: run it by name with --ignored --nocapture"]
fn read_cost() {
    let mut rig = match Rig::new() {
        Ok(rig) => rig,
        Err(e) => {
            eprintln!("no hardware adapter, or the read program did not compile ({e}); skipped");
            return;
        }
    };
    let card = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() };
    // As `Host::shared_buffer` makes the runtime's host copies.
    let shared = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_CUSTOM,
        CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_WRITE_BACK,
        MemoryPoolPreference: D3D12_MEMORY_POOL_L0,
        ..Default::default()
    };
    let upload = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_UPLOAD, ..Default::default() };
    let unordered = D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS;
    let places = [
        ("the card's memory", buffer(&rig.device, BYTES, card, unordered, D3D12_RESOURCE_STATE_COMMON, false)),
        ("the PC's memory (as shared buffers)", buffer(&rig.device, BYTES, shared, unordered, D3D12_RESOURCE_STATE_COMMON, true)),
        (
            "the PC's memory (upload heap)",
            buffer(&rig.device, BYTES, upload, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_GENERIC_READ, true),
        ),
    ];
    // (what, passes, groups a pass, dwords a thread)
    let batches = [
        ("8,000 passes of one group, 8 KiB read each (a small draw)", 8000u32, 1u32, 32u32),
        ("8,000 passes of one group, 64 KiB read each", 8000, 1, 256),
        ("8,000 passes of 8 groups, 512 KiB read each (a big draw)", 8000, 8, 256),
        ("one pass of 8,000 groups, 62 MiB read in all", 1, 8000, 32),
    ];
    eprintln!("read cost: a {} MiB buffer read by compute passes, three batches each (the least is given)", BYTES >> 20);
    let mut seed = 1u32;
    for (what, passes, groups, words) in batches {
        eprintln!("{what}:");
        let mut on_card = None;
        for (place, made) in &places {
            let data = match made {
                Ok(data) => data,
                Err(e) => {
                    eprintln!("  {place}: no such buffer ({e})");
                    continue;
                }
            };
            let mut least = f64::MAX;
            for _ in 0..3 {
                seed = seed.wrapping_add(passes);
                match rig.batch(data, passes, groups, words, seed) {
                    Ok(ms) => least = least.min(ms),
                    Err(e) => {
                        eprintln!("  {place}: the batch did not run ({e})");
                        return;
                    }
                }
            }
            let mib = f64::from(passes) * f64::from(groups) * 64.0 * f64::from(words) * 4.0 / f64::from(1u32 << 20);
            let times = on_card.map_or(String::new(), |c: f64| format!(", {:.1} times the card's own", least / c));
            eprintln!(
                "  {place}: {least:.2} ms, {:.2} microseconds a pass, {:.0} MiB a second{times}",
                least * 1e3 / f64::from(passes),
                mib / (least / 1e3),
            );
            on_card.get_or_insert(least);
        }
    }
}
