//! Bounded previews of workspace files. Unreal editor packages index their own thumbnails
//! in FPackageFileSummary; reading the export data is unnecessary.
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const MAX_ENCODED: u64 = 8 * 1024 * 1024;
const MAX_DIMENSION: u32 = 4096;

/// A regular file below a canonical workspace root, even when a parent is a symlink.
pub(crate) fn local_file(root: &Path, path: &str) -> Option<PathBuf> {
    let path = lode_core::DepotPath::parse(path).ok()?;
    let file = root.join(path.as_str());
    if !file.symlink_metadata().ok()?.is_file() {
        return None;
    }
    let file = file.canonicalize().ok()?;
    file.starts_with(root).then_some(file)
}

fn integer(r: &mut impl Read) -> Option<i32> {
    let mut buf = [0; 4];
    r.read_exact(&mut buf).ok()?;
    Some(i32::from_le_bytes(buf))
}

fn string(r: &mut impl Read) -> Option<String> {
    let n = integer(r)?;
    let len = usize::try_from(n.unsigned_abs()).ok()?;
    if len > 2048 {
        return None;
    }
    if n < 0 {
        let mut bytes = vec![0; len * 2];
        r.read_exact(&mut bytes).ok()?;
        let wide: Vec<_> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .copied()
            .map(u16::from_le_bytes)
            .collect();
        String::from_utf16(wide.strip_suffix(&[0])?).ok()
    } else {
        let mut bytes = vec![0; len];
        r.read_exact(&mut bytes).ok()?;
        String::from_utf8(bytes.strip_suffix(&[0]).unwrap_or(&bytes).to_vec()).ok()
    }
}

// Version gates in Unreal's FPackageFileSummary up to the ThumbnailTableOffset field.
fn table_offset(r: &mut (impl Read + Seek)) -> Option<u64> {
    if integer(r)? as u32 != 0x9e2a83c1 {
        return None;
    }
    let legacy = integer(r)?;
    if !(-9..=-6).contains(&legacy) {
        return None;
    }
    integer(r)?; // legacy UE3 version
    let ue4 = integer(r)?;
    let ue5 = if legacy <= -8 { integer(r)? } else { 0 };
    if !(214..=522).contains(&ue4) || !(ue5 == 0 || (1000..=1018).contains(&ue5)) {
        return None;
    }
    integer(r)?; // licensee version
    if ue5 >= 1016 {
        r.seek_relative(24).ok()?;
    } // saved hash + total header size
    let custom = integer(r)?;
    if !(0..=4096).contains(&custom) {
        return None;
    }
    r.seek_relative(i64::from(custom) * 20).ok()?; // GUID and version per entry
    if ue5 < 1016 {
        integer(r)?;
    }
    string(r)?;
    let flags = integer(r)? as u32;
    if flags & 0x80000000 != 0 {
        return None;
    } // editor data was stripped
    r.seek_relative(8).ok()?; // name map
    if ue5 >= 1008 {
        r.seek_relative(8).ok()?;
    }
    if ue4 >= 516 {
        string(r)?;
    }
    if ue4 >= 459 {
        r.seek_relative(8).ok()?;
    }
    r.seek_relative(16).ok()?; // export and import maps
    if ue5 >= 1015 {
        r.seek_relative(16).ok()?;
    } // Verse cells
    if ue5 >= 1014 {
        integer(r)?;
    } // metadata offset
    integer(r)?; // depends offset
    if ue4 >= 384 {
        r.seek_relative(8).ok()?;
    }
    if ue4 >= 510 {
        integer(r)?;
    }
    u64::try_from(integer(r)?).ok().filter(|n| *n > 0)
}

fn embedded(r: &mut (impl Read + Seek), name: &str) -> Option<Vec<u8>> {
    let table = table_offset(r)?;
    r.seek(SeekFrom::Start(table)).ok()?;
    let count = integer(r)?;
    if !(1..=256).contains(&count) {
        return None;
    }
    for _ in 0..count {
        let _class = string(r)?;
        let object = string(r)?;
        let offset = u64::try_from(integer(r)?).ok()?;
        if object != name {
            continue;
        }
        if offset.checked_add(12)? > table {
            return None;
        }
        r.seek(SeekFrom::Start(offset)).ok()?;
        let width = integer(r)?;
        let height = integer(r)?.unsigned_abs();
        let n = u64::try_from(integer(r)?).ok()?;
        if width <= 0
            || width as u32 > MAX_DIMENSION
            || height == 0
            || height > MAX_DIMENSION
            || n > MAX_ENCODED
            || offset.checked_add(12 + n)? > table
        {
            return None;
        }
        let mut bytes = vec![0; usize::try_from(n).ok()?];
        r.read_exact(&mut bytes).ok()?;
        return Some(bytes);
    }
    None
}

fn decode<R: std::io::BufRead + Seek>(reader: R) -> Option<image::RgbaImage> {
    let mut reader = image::ImageReader::new(reader).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    Some(reader.decode().ok()?.thumbnail(180, 120).to_rgba8())
}

/// Returns a small image from a local bitmap or a versioned Unreal editor package.
pub(crate) fn load(file: &Path) -> Option<image::RgbaImage> {
    let ext = file.extension()?.to_str()?.to_ascii_lowercase();
    let mut reader = BufReader::new(File::open(file).ok()?);
    match ext.as_str() {
        "uasset" | "umap" => decode(Cursor::new(embedded(
            &mut reader,
            file.file_stem()?.to_str()?,
        )?)),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
            if reader.get_ref().metadata().ok()?.len() <= MAX_ENCODED =>
        {
            decode(reader)
        }
        _ => None,
    }
}
