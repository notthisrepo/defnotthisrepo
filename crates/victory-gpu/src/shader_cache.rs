//! Translated shaders kept on disk (decision 0003): under the configured
//! cache folder, keyed by the container's content and by the VIctory build
//! that translated it (the BLAKE3 of the translator's sources), so a changed
//! translator never serves an old translation and one that did not change
//! keeps its own through a rebuild. Entries go through
//! `victory-pipeline`'s content-addressed cache, which writes them
//! atomically; an entry that does not read back is translated again.
//!
//! The entry is our own encoding of [`HostShader`]: a field added to it
//! must be added to `encode` and `decode` too (entries of older builds
//! are ignored anyway).

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use victory_pipeline::{Cache, Converter};
use victory_shader::emit::{HostShader, StaticSampler, UserData};
use victory_shader::resource::RootIndexedView;
use victory_shader::{HwStage, RootType};

/// Where translated shaders are kept ([`use_shader_cache`]).
static DIR: OnceLock<PathBuf> = OnceLock::new();
/// The cache, opened when the first shader asks for it ([`cache`]).
static CACHE: OnceLock<Option<Cache>> = OnceLock::new();
static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);

/// Keeps translated shaders under `dir` from now on, in a folder of this
/// build's own ([`build_folder`]), and lets go of the folders of builds
/// not used lately ([`stale`]).
///
/// Before, every build wrote into `dir` itself. An entry's key holds the
/// build, so a rebuilt runtime could read none of the old ones, and
/// nothing told them from the new: 8,872 entries and 503 MB after five
/// builds in an evening, of which one build's 3,000 were of use.
pub fn use_shader_cache(dir: PathBuf) {
    let _ = DIR.set(dir);
}

/// The cache under [`DIR`], opened at the first shader: the folder's name
/// holds [`build`], which asks the host device what its view heaps hold,
/// and at the start of a run (when [`use_shader_cache`] is called) there
/// is no device yet and none should be made for a folder's name.
fn cache() -> Option<&'static Cache> {
    CACHE
        .get_or_init(|| {
            let dir = DIR.get()?;
            let own = build_folder(build());
            // Opened first: it refuses a folder inside the repository, and
            // nothing is removed from a place that is not a cache.
            match Cache::open(dir.join(&own)) {
                Ok(cache) => {
                    mark_used(&dir.join(&own));
                    let (removed, failed) = prune(dir, &own);
                    if removed + failed > 0 {
                        tracing::info!(removed, failed, kept = KEPT_BUILDS, "shader cache: folders of other builds let go");
                    }
                    Some(cache)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "no shader cache");
                    None
                }
            }
        })
        .as_ref()
}

/// The builds whose folders are kept, this one included: a release and a
/// diagnostics build, each with one set of translation switches besides
/// the default.
const KEPT_BUILDS: usize = 4;

/// The file in a build's folder that holds when it was last used, in
/// seconds since 1970.
const LAST_USED: &str = "last-used";

/// The name of a build's folder: `build-` and the first eight bytes of
/// what identifies the build ([`build`]), in hex.
fn build_folder(build: &[u8; 32]) -> String {
    let hex: String = build[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("build-{hex}")
}

/// Whether `name` is `prefix` followed by exactly `digits` lower-case hex
/// digits.
fn hex_named(name: &str, prefix: &str, digits: usize) -> bool {
    name.strip_prefix(prefix)
        .is_some_and(|rest| rest.len() == digits && rest.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// Of the folders in the cache, each with when it was last used (0: not
/// known), those to remove, `own` being this build's:
///
/// * the folders of the layout before builds had their own (two hex
///   digits: the first byte of an entry's key), which no build reads any
///   more;
/// * builds' folders beyond the `keep` most lately used, `own` counted
///   among those kept whatever its date.
///
/// A folder of any other name is not ours to remove.
fn stale(folders: &[(String, u64)], own: &str, keep: usize) -> Vec<String> {
    let mut remove: Vec<String> =
        folders.iter().filter(|(name, _)| hex_named(name, "", 2)).map(|(name, _)| name.clone()).collect();
    let mut builds: Vec<&(String, u64)> =
        folders.iter().filter(|(name, _)| name != own && hex_named(name, "build-", 16)).collect();
    // The most lately used first; among equals, by name, so that the same
    // folders go whatever order the system lists them in.
    builds.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    remove.extend(builds.into_iter().skip(keep.saturating_sub(1)).map(|(name, _)| name.clone()));
    remove
}

/// Notes in `folder` that it was used now.
fn mark_used(folder: &std::path::Path) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    if let Err(e) = std::fs::write(folder.join(LAST_USED), now.to_string()) {
        tracing::debug!(error = %e, "shader cache: the folder's last use not noted");
    }
}

/// Removes the folders of `dir` that [`stale`] names. Returns how many
/// went and how many could not be removed (another run of ours may hold a
/// file open; they go at a later start).
fn prune(dir: &std::path::Path, own: &str) -> (usize, usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let folders: Vec<(String, u64)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let used = std::fs::read_to_string(e.path().join(LAST_USED)).ok().and_then(|s| s.trim().parse().ok());
            Some((name, used.unwrap_or(0)))
        })
        .collect();
    let (mut removed, mut failed) = (0, 0);
    for name in stale(&folders, own, KEPT_BUILDS) {
        match std::fs::remove_dir_all(dir.join(&name)) {
            Ok(()) => removed += 1,
            Err(e) => {
                failed += 1;
                tracing::debug!(error = %e, folder = %name, "shader cache: a folder could not be removed");
            }
        }
    }
    (removed, failed)
}

#[cfg(test)]
mod prune_tests {
    use super::*;

    fn folders(list: &[(&str, u64)]) -> Vec<(String, u64)> {
        list.iter().map(|&(name, used)| (name.to_string(), used)).collect()
    }

    const A: &str = "build-00000000000000aa";
    const B: &str = "build-00000000000000bb";
    const C: &str = "build-00000000000000cc";
    const D: &str = "build-00000000000000dd";
    const E: &str = "build-00000000000000ee";

    /// The old layout's folders always go; of the builds, this one and the
    /// most lately used others stay, up to the number kept.
    #[test]
    fn old_layout_and_least_lately_used_builds_go() {
        let all = folders(&[("00", 0), ("ff", 9), (A, 50), (B, 10), (C, 40), (D, 30), (E, 20), ("a7", 0)]);
        let mut gone = stale(&all, B, 4);
        gone.sort();
        // B is this build (the oldest by date, kept all the same); A, C and
        // D are the three others most lately used.
        assert_eq!(gone, ["00", "a7", E, "ff"]);
        assert!(stale(&all, B, 9).iter().all(|name| name.len() == 2), "room for every build: only the old layout goes");
        let mut only_own = stale(&all, B, 1);
        only_own.sort();
        assert_eq!(only_own, ["00", "a7", A, C, D, E, "ff"]);
    }

    /// A folder that is neither of the two kinds is never removed, whatever
    /// it looks like.
    #[test]
    fn folders_of_other_names_are_left_alone() {
        let all = folders(&[
            ("images", 0),
            ("pipelines", 0),
            ("0", 0),
            ("abc", 0),
            ("AB", 0),
            ("zz", 0),
            ("build-", 0),
            ("build-123", 0),
            ("build-00000000000000AA", 0),
            ("build-00000000000000aaa", 0),
            ("xbuild-00000000000000aa", 0),
        ]);
        assert!(stale(&all, A, 1).is_empty());
    }

    /// Builds with no date are the first to go, and the same ones whatever
    /// order they are listed in.
    #[test]
    fn undated_builds_go_first_and_the_listing_order_does_not_matter() {
        let one = folders(&[(B, 0), (C, 0), (D, 7)]);
        let other = folders(&[(D, 7), (C, 0), (B, 0)]);
        assert_eq!(stale(&one, A, 3), [C]);
        assert_eq!(stale(&other, A, 3), [C]);
        assert_eq!(stale(&one, A, 2), [B, C]);
    }

    /// The folder's name is the build's first eight bytes, in hex.
    #[test]
    fn a_builds_folder_is_named_by_its_first_eight_bytes() {
        let mut build = [0u8; 32];
        build[..9].copy_from_slice(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x55]);
        assert_eq!(build_folder(&build), "build-0123456789abcdef");
        assert!(hex_named(&build_folder(&build), "build-", 16));
    }

    /// On disk: stale folders are removed with what they hold, the others
    /// and loose files stay, and this build's folder is dated.
    #[test]
    fn pruning_removes_exactly_the_stale_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let make = |name: &str, used: Option<u64>| {
            let folder = dir.join(name);
            std::fs::create_dir_all(folder.join("3f")).unwrap();
            std::fs::write(folder.join("3f").join("entry"), b"x").unwrap();
            if let Some(used) = used {
                std::fs::write(folder.join(LAST_USED), used.to_string()).unwrap();
            }
        };
        make("0a", None);
        make("f3", None);
        make(A, None);
        make(B, Some(100));
        make(C, Some(300));
        make(D, Some(200));
        make(E, Some(50));
        make("pipelines", None);
        std::fs::write(dir.join("a-file"), b"x").unwrap();
        mark_used(&dir.join(A));
        assert!(std::fs::read_to_string(dir.join(A).join(LAST_USED)).unwrap().parse::<u64>().unwrap() > 1_700_000_000);

        // A is this build; of B, C, D, E the three most lately used stay.
        assert_eq!(prune(dir, A), (3, 0));
        let mut left: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        assert_eq!(left, ["a-file", A, B, C, D, "pipelines"]);
        assert!(dir.join(C).join("3f").join("entry").is_file());
        // Nothing more to remove the second time.
        assert_eq!(prune(dir, A), (0, 0));
    }
}

/// Shaders found in the cache and translated since the start.
pub(crate) fn counts() -> (u64, u64) {
    (HITS.load(Ordering::Relaxed), MISSES.load(Ordering::Relaxed))
}

/// What identifies this translator: the BLAKE3 of its sources, of this
/// file (the entries' encoding) and of the translation settings it runs
/// with (the loop limit, which a diagnostics build can change at run time,
/// and what the host's device said).
fn build() -> &'static [u8; 32] {
    static BUILD: OnceLock<[u8; 32]> = OnceLock::new();
    BUILD.get_or_init(|| {
        let limit = victory_shader::emit::loop_limit();
        if limit != victory_shader::emit::DEFAULT_LOOP_LIMIT {
            tracing::warn!(
                limit,
                "VICTORY_SHADER_LOOP_LIMIT: translated loops stop sooner"
            );
        }
        // The view heap's null slot is part of the translation (it differs
        // by host GPU): found before it goes into the key, so a translation
        // cached without the clamp is never served.
        crate::descriptor::host::probe_view_limit();
        let mut hash = blake3::Hasher::new();
        for source in victory_shader::sources().iter().copied().chain([include_str!("shader_cache.rs")]) {
            hash.update(&(source.len() as u64).to_le_bytes());
            hash.update(source.as_bytes());
        }
        hash.update(env!("CARGO_PKG_VERSION").as_bytes());
        hash.update(&[u8::from(cfg!(feature = "diagnostics"))]);
        for n in [
            limit,
            victory_shader::emit::view_heap_limit(),
            victory_shader::emit::host_max_wave(),
            // Which group barriers wait is part of the translation too.
            u32::from(victory_shader::emit::group_sync()),
            // And which clean-up passes ran over the translated code.
            victory_shader::emit::shader_opt(),
        ] {
            hash.update(&n.to_le_bytes());
        }
        // So are the switches that pick the form of a wave64 program:
        // without them here a run with `VICTORY_WAVE_WIDE=0` was served
        // the translations an earlier run had cached with the switch unset.
        for name in [
            "VICTORY_WAVE_WIDE",
            "VICTORY_WAVE_DEEP_SKIPS",
            "VICTORY_WAVE_OWN_BITS",
            "VICTORY_WAVE_LOCAL",
            "VICTORY_F16_NEAREST",
            "VICTORY_STRICT_JOINS",
            "VICTORY_MESH_OUTPUT_STRICT",
        ] {
            hash.update(name.as_bytes());
            hash.update(b"=");
            if let Some(value) = std::env::var_os(name) {
                hash.update(value.as_encoded_bytes());
            }
            hash.update(&[0]);
        }
        *hash.finalize().as_bytes()
    })
}

/// The translation of a console shader container, from the cache when
/// there is one.
pub(crate) fn host_shader(container: &[u8]) -> Result<HostShader, String> {
    let translate = |c: &[u8]| victory_shader::emit::translate(c).map_err(|e| e.to_string());
    match cache() {
        Some(cache) => cached(cache, build(), container, translate),
        None => translate(container),
    }
}

/// The cache entry's converter: the build and the container in, the
/// encoded translation out. `ran` tells a miss from a hit.
struct Translate<F> {
    translate: F,
    ran: AtomicBool,
}

impl<F: Fn(&[u8]) -> Result<HostShader, String> + Send + Sync> Converter for Translate<F> {
    fn id(&self) -> &'static str {
        "shader.dxil"
    }
    fn version(&self) -> u32 {
        // The build fingerprint in the input does the versioning.
        1
    }
    fn convert(&self, input: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.ran.store(true, Ordering::Relaxed);
        let shader = (self.translate)(&input[32..]).map_err(anyhow::Error::msg)?;
        Ok(encode(&shader))
    }
}

fn cached(
    cache: &Cache,
    build: &[u8; 32],
    container: &[u8],
    translate: impl Fn(&[u8]) -> Result<HostShader, String> + Send + Sync,
) -> Result<HostShader, String> {
    let input = [&build[..], container].concat();
    let converter = Translate {
        translate,
        ran: AtomicBool::new(false),
    };
    for _ in 0..2 {
        let path = cache
            .get_or_convert(&converter, &input)
            .map_err(|e| format!("{e:#}"))?;
        if let Some(shader) = std::fs::read(&path).ok().and_then(|b| decode(&b)) {
            let (counter, what) = if converter.ran.load(Ordering::Relaxed) {
                (&MISSES, "translated and cached")
            } else {
                (&HITS, "from the cache")
            };
            counter.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(path = %path.display(), "shader {what}");
            return Ok(shader);
        }
        tracing::debug!(path = %path.display(), "unreadable shader cache entry; translating again");
        let _ = std::fs::remove_file(&path);
    }
    MISSES.fetch_add(1, Ordering::Relaxed);
    (converter.translate)(container)
}

const MAGIC: &[u8; 4] = b"VSH1";
const STAGES: [HwStage; 8] = [
    HwStage::Ls,
    HwStage::Hs,
    HwStage::Es,
    HwStage::Gs,
    HwStage::Vs,
    HwStage::Ps,
    HwStage::Ms,
    HwStage::Cs,
];

fn root_code(r: RootType) -> u32 {
    match r {
        RootType::Constants => 0,
        RootType::Descriptor => 1,
        RootType::Table => 2,
        RootType::ImmediateSampler => 3,
        RootType::ResourceHeap => 4,
        RootType::SamplerHeap => 5,
        RootType::Other(x) => 6 + x,
    }
}

fn root_type(code: u32) -> RootType {
    match code {
        0 => RootType::Constants,
        1 => RootType::Descriptor,
        2 => RootType::Table,
        3 => RootType::ImmediateSampler,
        4 => RootType::ResourceHeap,
        5 => RootType::SamplerHeap,
        x => RootType::Other(x - 6),
    }
}

/// Little-endian words and length-prefixed lists.
fn encode(s: &HostShader) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    let word = |out: &mut Vec<u8>, v: u32| out.extend(v.to_le_bytes());
    let stage = STAGES.iter().position(|&x| x == s.stage).unwrap_or(0) as u32;
    word(&mut out, stage);
    word(&mut out, s.dxil.len() as u32);
    out.extend(&s.dxil);
    word(&mut out, s.user_data.len() as u32);
    for u in &s.user_data {
        for v in [u.index, u32::from(u.slot), root_code(u.root), u.dword] {
            word(&mut out, v);
        }
    }
    word(&mut out, s.samplers.len() as u32);
    for sampler in &s.samplers {
        for &v in sampler {
            word(&mut out, v);
        }
    }
    word(&mut out, s.static_samplers.len() as u32);
    for t in &s.static_samplers {
        let [u, v, w] = t.address;
        let words = [
            t.filter,
            u,
            v,
            w,
            t.mip_lod_bias.to_bits(),
            t.max_anisotropy,
            t.comparison,
            t.border,
            t.min_lod.to_bits(),
            t.max_lod.to_bits(),
        ];
        for v in words {
            word(&mut out, v);
        }
    }
    for slots in [&s.root_cbvs, &s.root_srvs, &s.root_uavs] {
        word(&mut out, slots.len() as u32);
        out.extend(slots);
    }
    word(&mut out, s.root_indexed_views.len() as u32);
    for view in &s.root_indexed_views {
        for value in [
            u32::from(view.table_slot),
            u32::from(view.constant_slot),
            view.constant_dword,
            view.byte_offset,
            u32::from(view.shift),
        ] {
            word(&mut out, value);
        }
    }
    out
}

fn decode(bytes: &[u8]) -> Option<HostShader> {
    struct Reader<'a>(&'a [u8]);
    impl<'a> Reader<'a> {
        fn take(&mut self, n: usize) -> Option<&'a [u8]> {
            let (head, rest) = self.0.split_at_checked(n)?;
            self.0 = rest;
            Some(head)
        }
        fn word(&mut self) -> Option<u32> {
            Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
        }
        fn list(&mut self) -> Option<Vec<u8>> {
            let n = self.word()? as usize;
            Some(self.take(n)?.to_vec())
        }
    }
    let mut r = Reader(bytes);
    if r.take(4)? != MAGIC {
        return None;
    }
    let stage = *STAGES.get(r.word()? as usize)?;
    let dxil = r.list()?;
    let n = r.word()? as usize;
    let mut user_data = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        user_data.push(UserData {
            index: r.word()?,
            slot: u8::try_from(r.word()?).ok()?,
            root: root_type(r.word()?),
            dword: r.word()?,
        });
    }
    let n = r.word()? as usize;
    let mut samplers = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        samplers.push([r.word()?, r.word()?, r.word()?, r.word()?]);
    }
    let n = r.word()? as usize;
    let mut static_samplers = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        static_samplers.push(StaticSampler {
            filter: r.word()?,
            address: [r.word()?, r.word()?, r.word()?],
            mip_lod_bias: f32::from_bits(r.word()?),
            max_anisotropy: r.word()?,
            comparison: r.word()?,
            border: r.word()?,
            min_lod: f32::from_bits(r.word()?),
            max_lod: f32::from_bits(r.word()?),
        });
    }
    let root_cbvs = r.list()?;
    let root_srvs = r.list()?;
    let root_uavs = r.list()?;
    let n = r.word()? as usize;
    let mut root_indexed_views = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        root_indexed_views.push(RootIndexedView {
            table_slot: u8::try_from(r.word()?).ok()?,
            constant_slot: u8::try_from(r.word()?).ok()?,
            constant_dword: r.word()?,
            byte_offset: r.word()?,
            shift: u8::try_from(r.word()?).ok().filter(|&s| s < 32)?,
        });
    }
    r.0.is_empty().then_some(HostShader {
        stage,
        dxil,
        user_data,
        samplers,
        static_samplers,
        root_cbvs,
        root_srvs,
        root_uavs,
        root_indexed_views,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shader() -> HostShader {
        HostShader {
            stage: HwStage::Ps,
            dxil: b"DXBC and the rest".to_vec(),
            user_data: vec![UserData {
                index: 8,
                slot: 1,
                root: RootType::Table,
                dword: 0,
            }],
            samplers: vec![[0x400, 0xc0_0000, 0x18f0_0000, 0]],
            static_samplers: vec![StaticSampler::from_words([
                0x400,
                0xc0_0000,
                0x18f0_0000,
                0,
            ])],
            root_cbvs: vec![2],
            root_srvs: vec![0, 3],
            root_uavs: vec![1],
            root_indexed_views: vec![RootIndexedView {
                table_slot: 1,
                constant_slot: 2,
                constant_dword: 3,
                byte_offset: 32,
                shift: 5,
            }],
        }
    }

    #[test]
    fn translations_come_back_from_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let build = [7u8; 32];
        let container = b"a console container".as_slice();
        let runs = AtomicU64::new(0);
        let translate = |_: &[u8]| {
            runs.fetch_add(1, Ordering::Relaxed);
            Ok(shader())
        };
        assert_eq!(cached(&cache, &build, container, translate), Ok(shader()));
        assert_eq!(cached(&cache, &build, container, translate), Ok(shader()));
        assert_eq!(runs.load(Ordering::Relaxed), 1, "the second came from disk");

        // Another build translates again.
        assert_eq!(cached(&cache, &[8; 32], container, translate), Ok(shader()));
        assert_eq!(runs.load(Ordering::Relaxed), 2);

        // A damaged entry is a miss, translated and stored again.
        for e in walk(dir.path()) {
            std::fs::write(e, b"VSH1 broken").unwrap();
        }
        assert_eq!(cached(&cache, &build, container, translate), Ok(shader()));
        assert_eq!(runs.load(Ordering::Relaxed), 3);
        assert_eq!(cached(&cache, &build, container, translate), Ok(shader()));
        assert_eq!(runs.load(Ordering::Relaxed), 3);

        // Failures are not stored.
        let fail = |_: &[u8]| Err::<HostShader, _>("unsupported".to_owned());
        assert!(cached(&cache, &build, b"other", fail).is_err());
    }

    fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            if e.path().is_dir() {
                files.extend(walk(&e.path()));
            } else {
                files.push(e.path());
            }
        }
        files
    }

    #[test]
    fn entries_round_trip_and_reject_damage() {
        let bytes = encode(&shader());
        assert_eq!(decode(&bytes), Some(shader()));
        assert_eq!(decode(&bytes[..bytes.len() - 1]), None);
        assert_eq!(decode(&[bytes.as_slice(), &[0]].concat()), None);
        assert_eq!(decode(b"VSH0"), None);
    }
}
