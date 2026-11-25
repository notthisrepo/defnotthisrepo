//! What the host driver does with the PC's memory for the kinds of objects
//! the runtime makes by the thousand, measured on the real adapter: after
//! each step, the process's write-combined memory (what the memory map of a
//! diagnostics build calls "buffers shared with the card") and its private
//! bytes.
//!
//! In the world the process commits 11 GB of write-combined memory, 6-7 GB
//! of it in 270 allocations of exactly 32 MiB, while the upload memory the
//! runtime itself holds is about 300 MB (`reports/triage.md`, U1). These
//! steps say which kind of object makes such allocations and whether
//! letting it go gives them back.
//!
//! What it printed on an RTX 4080 SUPER, driver 32.0.16.1088 (2026-11-24):
//! every byte in the card's memory is committed once more as write-combined
//! memory of the process. 17,000 textures (10.4 GB) took 10,624 MB in 331
//! allocations of 32 MiB; evicting them gave nothing back; letting them go
//! gave it back. The same textures placed in heaps of 256 MiB took the same
//! and kept it for as long as the heaps lived. A 1 GiB buffer in the card's
//! memory took 1,024 MB. Upload buffers took their size while alive and
//! nothing once let go. 760 lists and allocators took no write-combined
//! memory and 0.44 MB of other private memory each.
//!
//! Not a test of our code and it asserts nothing about the driver: it
//! prints. Ignored by default (it needs a hardware adapter and makes a few
//! GB of objects):
//!
//! ```text
//! cargo test -p victory-gpu --test driver_memory -- --ignored --nocapture
//! ```
#![cfg(windows)]

use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::Interface;
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_FREE, MEM_PRIVATE, MEMORY_BASIC_INFORMATION, PAGE_WRITECOMBINE, VirtualQuery,
};
use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// Write-combined private memory now: allocations, of them those whose
/// whole size (reserved parts included) is 32 MiB, committed bytes in all
/// and in the 32 MiB ones; then the process's private bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Seen {
    allocations: u64,
    of_32_mib: u64,
    committed: u64,
    committed_in_32_mib: u64,
    private: u64,
}

fn seen() -> Seen {
    use std::collections::HashMap;
    // Per allocation: its whole size, its committed write-combined bytes.
    let mut by_base: HashMap<usize, (usize, u64)> = HashMap::new();
    let mut at = 0usize;
    loop {
        let mut m = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: a query of our own address space into a structure of the
        // size given.
        let got = unsafe { VirtualQuery(at as *const _, &mut m, size_of::<MEMORY_BASIC_INFORMATION>()) };
        if got == 0 {
            break;
        }
        if m.State != MEM_FREE && m.Type == MEM_PRIVATE {
            let entry = by_base.entry(m.AllocationBase as usize).or_default();
            entry.0 += m.RegionSize;
            if m.State == MEM_COMMIT && m.Protect & PAGE_WRITECOMBINE != 0 {
                entry.1 += m.RegionSize as u64;
            }
        }
        let next = (m.BaseAddress as usize).saturating_add(m.RegionSize);
        if next <= at {
            break;
        }
        at = next;
    }
    let mut out = Seen::default();
    for &(whole, committed) in by_base.values() {
        if committed == 0 {
            continue;
        }
        out.allocations += 1;
        out.committed += committed;
        if whole == 32 << 20 {
            out.of_32_mib += 1;
            out.committed_in_32_mib += committed;
        }
    }
    let mut c = PROCESS_MEMORY_COUNTERS { cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
    // SAFETY: our own process and a sized structure.
    unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    out.private = c.PagefileUsage as u64;
    out
}

fn say(step: &str, before: Seen, now: Seen) {
    let mb = |a: u64, b: u64| (a as i64 - b as i64) / (1 << 20);
    println!(
        "{step:<58} write-combined {:>5} MB ({:+} MB) in {:>4} allocations, {:>3} of 32 MiB holding {:>5} MB ({:+}); private {:>6} MB ({:+} MB)",
        now.committed >> 20,
        mb(now.committed, before.committed),
        now.allocations,
        now.of_32_mib,
        now.committed_in_32_mib >> 20,
        now.of_32_mib as i64 - before.of_32_mib as i64,
        now.private >> 20,
        mb(now.private, before.private),
    );
}

fn buffer_desc(bytes: u64) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: bytes,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        ..Default::default()
    }
}

/// A committed buffer of `bytes` in the upload heap, mapped and written
/// from end to end as the runtime's are.
fn upload(device: &ID3D12Device, bytes: u64) -> ID3D12Resource {
    let heap = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_UPLOAD, ..Default::default() };
    let mut made: Option<ID3D12Resource> = None;
    // SAFETY: plain creation and mapping of an upload buffer; the mapping
    // holds `bytes` bytes.
    unsafe {
        device
            .CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &buffer_desc(bytes), D3D12_RESOURCE_STATE_GENERIC_READ, None, &mut made)
            .expect("an upload buffer");
        let buffer = made.expect("an upload buffer");
        let mut data = std::ptr::null_mut();
        buffer.Map(0, None, Some(&mut data)).expect("its mapping");
        std::ptr::write_bytes(data.cast::<u8>(), 0x5a, bytes as usize);
        buffer
    }
}

/// A committed texture in the card's memory: `size` by `size`, block
/// compressed, with all its mips, as most of the title's are.
fn texture(device: &ID3D12Device, size: u32) -> ID3D12Resource {
    let heap = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Width: u64::from(size),
        Height: size,
        DepthOrArraySize: 1,
        MipLevels: 0,
        Format: DXGI_FORMAT_BC7_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        ..Default::default()
    };
    let mut made: Option<ID3D12Resource> = None;
    // SAFETY: plain creation of a device-local texture.
    unsafe {
        device
            .CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, D3D12_RESOURCE_STATE_COMMON, None, &mut made)
            .expect("a texture");
    }
    made.expect("a texture")
}

fn texture_desc(size: u32) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Width: u64::from(size),
        Height: size,
        DepthOrArraySize: 1,
        MipLevels: 0,
        Format: DXGI_FORMAT_BC7_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        ..Default::default()
    }
}

/// `count` textures of the sizes [`texture`] is given, placed one after
/// the other in heaps of 256 MiB in the card's memory: the heaps and the
/// textures.
fn placed_textures(device: &ID3D12Device, count: usize) -> (Vec<ID3D12Heap>, Vec<ID3D12Resource>) {
    const HEAP: u64 = 256 << 20;
    let new_heap = || -> ID3D12Heap {
        let desc = D3D12_HEAP_DESC {
            SizeInBytes: HEAP,
            Properties: D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() },
            Alignment: 0,
            Flags: D3D12_HEAP_FLAG_ALLOW_ONLY_NON_RT_DS_TEXTURES,
        };
        let mut heap: Option<ID3D12Heap> = None;
        // SAFETY: plain creation of a heap in the card's memory.
        unsafe { device.CreateHeap(&desc, &mut heap) }.expect("a heap");
        heap.expect("a heap")
    };
    let (mut heaps, mut textures, mut used) = (vec![new_heap()], Vec::with_capacity(count), 0u64);
    for i in 0..count {
        let desc = texture_desc([256, 512, 1024][i % 3]);
        // SAFETY: a plain query of the device.
        let info = unsafe { device.GetResourceAllocationInfo(0, &[desc]) };
        let mut at = used.next_multiple_of(info.Alignment.max(1));
        if at + info.SizeInBytes > HEAP {
            heaps.push(new_heap());
            at = 0;
        }
        let mut made: Option<ID3D12Resource> = None;
        // SAFETY: the range lies in the heap and no other texture has it.
        unsafe {
            device
                .CreatePlacedResource(heaps.last().expect("a heap"), at, &desc, D3D12_RESOURCE_STATE_COMMON, None, &mut made)
                .expect("a placed texture");
        }
        textures.push(made.expect("a placed texture"));
        used = at + info.SizeInBytes;
    }
    (heaps, textures)
}

/// A committed buffer of `bytes` in the card's memory that shaders may
/// write.
fn card_buffer(device: &ID3D12Device, bytes: u64) -> ID3D12Resource {
    let heap = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() };
    let desc = D3D12_RESOURCE_DESC { Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, ..buffer_desc(bytes) };
    let mut made: Option<ID3D12Resource> = None;
    // SAFETY: plain creation of a device-local buffer.
    unsafe {
        device
            .CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, D3D12_RESOURCE_STATE_COMMON, None, &mut made)
            .expect("a buffer in the card's memory");
    }
    made.expect("a buffer in the card's memory")
}

/// A command allocator and a list on it, with `draws` empty state changes
/// recorded and the list closed: what a host list is.
fn list(device: &ID3D12Device, changes: u32) -> (ID3D12CommandAllocator, ID3D12GraphicsCommandList) {
    // SAFETY: plain creation; the list is recorded and closed by this
    // thread alone.
    unsafe {
        let allocator: ID3D12CommandAllocator =
            device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).expect("an allocator");
        let list: ID3D12GraphicsCommandList =
            device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None).expect("a list");
        for i in 0..changes {
            list.OMSetStencilRef(i);
            list.OMSetBlendFactor(Some(&[0.0, 0.0, 0.0, f32::from(i as u16)]));
        }
        list.Close().expect("a closed list");
        (allocator, list)
    }
}

/// Lets the driver finish what it defers: an empty submission waited for,
/// twice.
fn settle(device: &ID3D12Device, queue: &ID3D12CommandQueue) {
    // SAFETY: a fence of our own, signalled on our queue and polled.
    unsafe {
        let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).expect("a fence");
        for value in 1..=2 {
            queue.Signal(&fence, value).expect("a signal");
            while fence.GetCompletedValue() < value {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
}

#[test]
#[ignore = "needs a hardware adapter; prints what the driver keeps of the PC's memory"]
fn what_the_driver_keeps_for_each_kind_of_object() {
    let mut device: Option<ID3D12Device> = None;
    // SAFETY: plain device creation on the default adapter.
    if unsafe { D3D12CreateDevice(None, D3D_FEATURE_LEVEL_12_0, &mut device) }.is_err() {
        println!("no Direct3D 12 device: nothing measured");
        return;
    }
    let device = device.expect("a device");
    // SAFETY: plain queue creation.
    let queue: ID3D12CommandQueue = unsafe {
        device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })
    }
    .expect("a queue");
    settle(&device, &queue);
    let mut last = seen();
    say("at the start (a device and a queue)", last, last);
    let step = |what: &str, last: &mut Seen| {
        settle(&device, &queue);
        let now = seen();
        say(what, *last, now);
        *last = now;
    };

    // Textures in the card's memory, as many as the world has alive.
    let textures: Vec<ID3D12Resource> = (0..17_000).map(|i| texture(&device, [256, 512, 1024][i % 3])).collect();
    step("17,000 block-compressed textures alive (256 to 1024)", &mut last);
    // Evicted: the card's memory is given up, the contents kept.
    let pageable: Vec<Option<ID3D12Pageable>> = textures.iter().map(|t| t.cast::<ID3D12Pageable>().ok()).collect();
    // SAFETY: live resources of this device, none in use by a list.
    let evicted = unsafe { device.Evict(&pageable) };
    step(if evicted.is_ok() { "... evicted" } else { "... evict refused" }, &mut last);
    drop(pageable);
    drop(textures);
    step("... let go", &mut last);

    // The same textures placed in heaps of 256 MiB instead of committed
    // one by one.
    let (heaps, placed) = placed_textures(&device, 17_000);
    step(&format!("17,000 textures placed in {} heaps of 256 MiB", heaps.len()), &mut last);
    drop(placed);
    step("... the textures let go, the heaps kept", &mut last);
    drop(heaps);
    step("... the heaps let go", &mut last);

    // A buffer in the card's memory, as a title buffer's copy there is.
    let card = card_buffer(&device, 1 << 30);
    step("one 1 GiB buffer in the card's memory", &mut last);
    drop(card);
    step("... let go", &mut last);

    // Upload buffers made and let go one after the other, as a list's
    // texture uploads are (one buffer per copy).
    for i in 0..20_000u64 {
        drop(upload(&device, [64, 256, 1024, 4096][(i % 4) as usize] << 10));
    }
    step("20,000 upload buffers made and let go one by one", &mut last);

    // Upload buffers held together, as a burst of staging is: 192 MiB a
    // frame for a few frames in the air.
    let held: Vec<ID3D12Resource> = (0..3_000u64).map(|i| upload(&device, [64, 256, 1024][(i % 3) as usize] << 10)).collect();
    step("3,000 upload buffers alive (1.3 GB)", &mut last);
    drop(held);
    step("... let go", &mut last);

    // Host lists: the title makes 632 at its start and the runtime 130 more.
    let lists: Vec<_> = (0..760).map(|_| list(&device, 2_000)).collect();
    step("760 lists and allocators, 4,000 state changes each", &mut last);
    for (allocator, list) in &lists {
        // SAFETY: nothing of these lists was submitted.
        unsafe {
            allocator.Reset().expect("a reset allocator");
            list.Reset(allocator, None).expect("a reset list");
            list.Close().expect("a closed list");
        }
    }
    step("... reset", &mut last);
    drop(lists);
    step("... let go", &mut last);

    // What is left when the device goes.
    drop(queue);
    drop(device);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let now = seen();
    say("the device let go", last, now);
}
